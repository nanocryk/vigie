//! Pairing and device management endpoints.

use std::collections::HashSet;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Extension, Json,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode, header},
    response::sse::{Event, KeepAlive, Sse},
    response::{IntoResponse, Response},
};
use axum_extra::extract::CookieJar;
use chrono::{DateTime, Utc};
use futures_util::stream::{Stream, unfold};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use tracing::{info, warn};

use super::registry::{Device, RemoveResult, RevokeResult};
use super::tokens::{PAIR_TOKEN_TTL, RateCheck, mint_token};
use super::{AuthedDevice, session_cookie_values};
use crate::BASE_PATH_SLASH;
use crate::http::api::ApiError;
use crate::http::net::{self, Interface};
use crate::http::server::AppState;

const MAX_LABEL_LEN: usize = 64;

#[derive(Deserialize)]
pub struct ExchangeReq {
    pub token: String,
}

#[derive(Serialize)]
struct ExchangeOk<'a> {
    label: &'a str,
}

#[derive(Serialize)]
pub struct WhoamiResp {
    pub authed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

pub async fn exchange(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    jar: CookieJar,
    Json(req): Json<ExchangeReq>,
) -> Response {
    let auth = &state.auth;
    let ip = peer.ip();

    if let RateCheck::Locked { retry_after_secs } = auth.rate_limiter.check(ip) {
        return (
            [(header::RETRY_AFTER, retry_after_secs.to_string())],
            ApiError::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
        )
            .into_response();
    }

    let via_startup = auth.matches_startup_token(&req.token);

    let (path, pair_matched) = if via_startup {
        ("startup", false)
    } else if let Some(issuer) = auth.pair_tokens.consume(&req.token) {
        // A device revoked after issuing a token must not pair a successor.
        if auth.registry.find_active(&issuer).is_none() {
            warn!(%ip, "pair token of a revoked device rejected");
            return ApiError::new(StatusCode::UNAUTHORIZED, "bad_token").into_response();
        }
        ("pair", true)
    } else if auth.local_token.consume(&req.token) {
        ("local", false)
    } else {
        auth.rate_limiter.record_failure(ip);
        return ApiError::new(StatusCode::UNAUTHORIZED, "bad_token").into_response();
    };

    if via_startup && !auth.spend_startup_token() {
        warn!(%ip, "startup token reuse rejected");
        return ApiError::new(StatusCode::UNAUTHORIZED, "token_spent").into_response();
    }

    let secret = mint_token();
    let label = auth.registry.next_label();
    let mut device = Device::new(mint_token(), label.clone(), &secret, Utc::now());
    device.last_ip = Some(ip.to_string());
    if let Err(err) = auth.registry.insert(device) {
        warn!(%err, "registry insert failed");
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "registry_write").into_response();
    }
    auth.notify_changed();

    info!(via = path, pair = pair_matched, label = %label, "device paired");
    let cookie = auth.cookie_for(secret);
    (jar.add(cookie), Json(ExchangeOk { label: &label })).into_response()
}

pub async fn whoami(State(state): State<AppState>, headers: HeaderMap) -> Json<WhoamiResp> {
    match session_cookie_values(&headers).find_map(|value| state.auth.registry.peek(value)) {
        Some(device) => Json(WhoamiResp {
            authed: true,
            label: Some(device.label),
        }),
        None => Json(WhoamiResp {
            authed: false,
            label: None,
        }),
    }
}

#[derive(Serialize)]
pub struct DeviceView {
    id: String,
    label: String,
    issued_at: DateTime<Utc>,
    last_seen: DateTime<Utc>,
    revoked: bool,
    is_you: bool,
    is_connected: bool,
    last_ip: Option<String>,
}

#[derive(Serialize)]
pub struct DevicesResp {
    devices: Vec<DeviceView>,
}

pub async fn devices_list(
    State(state): State<AppState>,
    Extension(you): Extension<AuthedDevice>,
) -> Json<DevicesResp> {
    let devices = state
        .auth
        .registry
        .list()
        .into_iter()
        .map(|device| DeviceView {
            is_you: device.id == you.id,
            is_connected: state.auth.is_connected(&device.id),
            last_ip: device.last_ip,
            id: device.id,
            label: device.label,
            issued_at: device.issued_at,
            last_seen: device.last_seen,
            revoked: device.revoked,
        })
        .collect();
    Json(DevicesResp { devices })
}

#[derive(Deserialize)]
pub struct RenameReq {
    pub label: String,
}

pub async fn devices_rename(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<RenameReq>,
) -> Result<StatusCode, ApiError> {
    let label = req.label.trim();
    if label.is_empty() || label.len() > MAX_LABEL_LEN {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "bad_label"));
    }
    match state.auth.registry.rename(&id, label) {
        Ok(true) => {
            state.auth.notify_changed();
            Ok(StatusCode::NO_CONTENT)
        }
        Ok(false) => Err(ApiError::new(StatusCode::NOT_FOUND, "not_found")),
        Err(err) => Err(ApiError::internal("rename failed", err)),
    }
}

pub async fn devices_delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    match state.auth.registry.remove(&id) {
        Ok(RemoveResult::Removed) => {
            state.auth.notify_changed();
            Ok(StatusCode::NO_CONTENT)
        }
        Ok(RemoveResult::NotFound) => Err(ApiError::new(StatusCode::NOT_FOUND, "not_found")),
        Ok(RemoveResult::NotRevoked) => Err(ApiError::new(StatusCode::CONFLICT, "not_revoked")),
        Err(err) => Err(ApiError::internal("delete failed", err)),
    }
}

pub async fn devices_revoke(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    match state.auth.registry.revoke(&id) {
        Ok(RevokeResult::Revoked) => {
            state.auth.notify_changed();
            Ok(StatusCode::NO_CONTENT)
        }
        Ok(RevokeResult::NotFound) => Err(ApiError::new(StatusCode::NOT_FOUND, "not_found")),
        Ok(RevokeResult::AlreadyRevoked) => {
            Err(ApiError::new(StatusCode::CONFLICT, "already_revoked"))
        }
        Err(err) => Err(ApiError::internal("revoke failed", err)),
    }
}

#[derive(Serialize)]
pub struct PairResp {
    token: String,
    expires_in_secs: u64,
    urls: Vec<String>,
}

#[derive(Deserialize, Default)]
pub struct PairReq {
    #[serde(default)]
    pub hosts: Vec<String>,
}

pub async fn pair(
    State(state): State<AppState>,
    Extension(device): Extension<AuthedDevice>,
    req: Option<Json<PairReq>>,
) -> Response {
    let known_ips: HashSet<String> = net::all_reachable_ipv4()
        .into_iter()
        .map(|ip| ip.to_string())
        .collect();
    let known_hosts: HashSet<String> = state.pair_extra_hosts.iter().cloned().collect();

    let requested = req.map(|Json(req)| req.hosts).unwrap_or_default();
    let selected: Vec<String> = if requested.is_empty() {
        let mut all: Vec<String> = known_ips.iter().cloned().collect();
        all.sort();
        all.extend(known_hosts.iter().cloned());
        all
    } else {
        requested
            .into_iter()
            .filter(|host| known_ips.contains(host) || known_hosts.contains(host))
            .collect()
    };

    if selected.is_empty() {
        return ApiError::new(StatusCode::BAD_REQUEST, "no_valid_hosts").into_response();
    }

    let token = state.auth.pair_tokens.issue(&device.id);
    let urls: Vec<String> = selected
        .iter()
        .map(|host| {
            let origin = net::origin(&state.pair_scheme, host, state.pair_port);
            format!("{origin}{BASE_PATH_SLASH}#auth={token}")
        })
        .collect();

    info!(device = %device.label, urls = urls.len(), "pair token issued");
    Json(PairResp {
        token,
        expires_in_secs: PAIR_TOKEN_TTL.as_secs(),
        urls,
    })
    .into_response()
}

#[derive(Serialize)]
pub struct InterfacesResp {
    port: u16,
    scheme: String,
    interfaces: Vec<Interface>,
    public_hosts: Vec<String>,
}

pub async fn interfaces(State(state): State<AppState>) -> Json<InterfacesResp> {
    Json(InterfacesResp {
        port: state.pair_port,
        scheme: state.pair_scheme.as_str().to_string(),
        interfaces: net::enumerate(),
        public_hosts: state.pair_extra_hosts.iter().cloned().collect(),
    })
}

pub async fn devices_events(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(device): Extension<AuthedDevice>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let changes = state.auth.subscribe_changes();
    let auth = Arc::clone(&state.auth);
    let device_id = device.id.clone();
    info!(device = %device.label, ip = %peer.ip(), "devices SSE client connected");
    // Every tick re-checks the streaming device against the registry, so a
    // revoke tears this stream down instead of leaving it live until the
    // client happens to disconnect.
    let stream = unfold(
        (changes, true, auth, device_id),
        |(mut changes, first, auth, device_id)| async move {
            if first {
                return Some((
                    Ok(Event::default().event("change").data("init")),
                    (changes, false, auth, device_id),
                ));
            }
            let data = match changes.recv().await {
                Ok(()) => "changed",
                Err(RecvError::Lagged(_)) => "lagged",
                Err(RecvError::Closed) => return None,
            };
            if auth.registry.find_active(&device_id).is_none() {
                info!("devices SSE closing: device revoked");
                return None;
            }
            Some((
                Ok(Event::default().event("change").data(data)),
                (changes, false, auth, device_id),
            ))
        },
    );
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}
