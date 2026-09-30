//! The endpoints a device uses to subscribe to notifications.

use axum::{
    Extension, Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

use super::{Message, PushSubscription, SendOutcome, validate_endpoint};
use crate::http::api::ApiError;
use crate::http::auth::AuthedDevice;
use crate::http::server::AppState;

#[derive(Serialize)]
struct PushConfig<'a> {
    vapid_public_key: &'a str,
    /// This device has a subscription.
    subscribed: bool,
}

pub async fn config(
    State(state): State<AppState>,
    Extension(device): Extension<AuthedDevice>,
) -> Result<Response, ApiError> {
    let subscription = state
        .manager
        .store()
        .push_subscription(&device.id)
        .await
        .map_err(|err| ApiError::internal("read the push subscription", err))?;
    Ok(Json(PushConfig {
        vapid_public_key: state.pusher.public_key(),
        subscribed: subscription.is_some(),
    })
    .into_response())
}

pub async fn subscribe(
    State(state): State<AppState>,
    Extension(device): Extension<AuthedDevice>,
    Json(subscription): Json<PushSubscription>,
) -> Result<StatusCode, ApiError> {
    if subscription.keys.p256dh.is_empty() || subscription.keys.auth.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "malformed subscription",
        ));
    }
    validate_endpoint(&subscription.endpoint)
        .map_err(|reason| ApiError::new(StatusCode::BAD_REQUEST, reason))?;
    state
        .manager
        .store()
        .set_push_subscription(&device.id, &subscription)
        .await
        .map_err(|err| ApiError::internal("store the push subscription", err))?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn unsubscribe(
    State(state): State<AppState>,
    Extension(device): Extension<AuthedDevice>,
) -> Result<StatusCode, ApiError> {
    state
        .manager
        .store()
        .remove_push_subscription(&device.id)
        .await
        .map_err(|err| ApiError::internal("drop the push subscription", err))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Sends a test notification to this device only.
pub async fn test(
    State(state): State<AppState>,
    Extension(device): Extension<AuthedDevice>,
) -> Result<StatusCode, ApiError> {
    let store = state.manager.store();
    let subscription = store
        .push_subscription(&device.id)
        .await
        .map_err(|err| ApiError::internal("read the push subscription", err))?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no_subscription"))?;
    let message = Message {
        title: "vigie".into(),
        body: "Test notification".into(),
        tag: "test".into(),
        url: String::new(),
    };
    let payload = serde_json::to_vec(&message)
        .map_err(|err| ApiError::internal("serialize the message", err))?;
    match state.pusher.send(&subscription, &payload).await {
        Ok(SendOutcome::Ok) => Ok(StatusCode::NO_CONTENT),
        Ok(SendOutcome::Gone) => {
            let _ = store.remove_push_subscription(&device.id).await;
            Err(ApiError::new(StatusCode::GONE, "subscription_gone"))
        }
        Ok(SendOutcome::Transient) => Err(ApiError::new(StatusCode::BAD_GATEWAY, "push_refused")),
        Err(err) => Err(ApiError::internal("send the test notification", err)),
    }
}
