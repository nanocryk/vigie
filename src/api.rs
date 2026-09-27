//! REST routes over the sessions.

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use tracing::warn;

use crate::AppState;
use crate::manager::SessionManager;
use crate::session::Activity;
use crate::wire::{CreateSession, SessionList, SessionView};

pub async fn list_sessions(State(state): State<AppState>) -> Response {
    match session_views(&state.manager).await {
        Ok(sessions) => Json(SessionList { sessions }).into_response(),
        Err(err) => internal_error("list sessions", &err),
    }
}

pub async fn create_session(
    State(state): State<AppState>,
    Json(request): Json<CreateSession>,
) -> Response {
    if !request.cwd.is_absolute() || !request.cwd.is_dir() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "bad_cwd"}))).into_response();
    }
    let created = async {
        let id = state.manager.create(request.cwd).await?.id();
        session_views(&state.manager)
            .await?
            .into_iter()
            .find(|view| view.id == id)
            .ok_or_else(|| anyhow::anyhow!("session {id} missing from the store"))
    };
    match created.await {
        Ok(view) => (StatusCode::CREATED, Json(view)).into_response(),
        Err(err) => internal_error("create session", &err),
    }
}

async fn session_views(manager: &SessionManager) -> anyhow::Result<Vec<SessionView>> {
    let activities = manager.activities().await;
    Ok(manager
        .list()
        .await?
        .into_iter()
        .map(|row| {
            let activity = activities.get(&row.id).copied();
            SessionView::new(row, activity.is_some(), activity == Some(Activity::Busy))
        })
        .collect())
}

fn internal_error(what: &str, err: &anyhow::Error) -> Response {
    warn!("{what}: {err:#}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": format!("{err:#}")})),
    )
        .into_response()
}
