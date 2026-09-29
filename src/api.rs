//! REST routes over the sessions, the account usage and the shared catalog.

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;

use crate::AppState;
use crate::manager::SessionManager;
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

pub async fn usage(State(state): State<AppState>) -> Response {
    match state.manager.usage().await {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(err) => internal_error("usage snapshot", &err),
    }
}

pub async fn catalog(State(state): State<AppState>) -> Response {
    match state.manager.catalog().await {
        Ok(catalog) => Json(catalog).into_response(),
        Err(err) => internal_error("shared catalog", &err),
    }
}

/// Differs from process to process, so a server restart counts as a new build.
static PROCESS_STAMP: LazyLock<u128> = LazyLock::new(|| {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos())
});

/// Build id of what is served now: the web build's `index.html` (its asset names
/// carry content hashes) and the server process. Clients reload when it changes.
pub async fn version(State(state): State<AppState>) -> Response {
    let index = tokio::fs::read(state.web_dir.join("index.html"))
        .await
        .unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    index.hash(&mut hasher);
    PROCESS_STAMP.hash(&mut hasher);
    Json(json!({"build": format!("{:016x}", hasher.finish())})).into_response()
}

async fn session_views(manager: &SessionManager) -> anyhow::Result<Vec<SessionView>> {
    let statuses = manager.statuses().await;
    let mut agents = manager.agents().await;
    let mut background = manager.background().await;
    Ok(manager
        .list()
        .await?
        .into_iter()
        .map(|row| {
            let status = statuses.get(&row.id).copied();
            let row_agents = agents.remove(&row.id).unwrap_or_default();
            let row_background = background.remove(&row.id).unwrap_or_default();
            SessionView::new(row, status, row_agents, row_background)
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
