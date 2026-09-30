//! REST routes over the sessions, the account usage and the shared catalog, and
//! the error every REST route answers with.

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::borrow::Cow;
use std::fmt::Display;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;

use crate::AppState;
use crate::http::wire::{CreateSession, SessionList, SessionView};
use crate::session::manager::SessionManager;
use crate::session::server_restart::{RestartState, Supervision};

/// A failed request, answered `{"error": code}` with its status.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: Cow<'static, str>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str) -> Self {
        Self {
            status,
            code: Cow::Borrowed(code),
        }
    }

    /// A 500 carrying the error text, logged with what was being done.
    pub fn internal(what: &str, err: impl Display) -> Self {
        let message = format!("{err:#}");
        warn!("{what}: {message}");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: Cow::Owned(message),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.code }))).into_response()
    }
}

pub async fn list_sessions(State(state): State<AppState>) -> Result<Json<SessionList>, ApiError> {
    let sessions = session_views(&state.manager)
        .await
        .map_err(|err| ApiError::internal("list sessions", err))?;
    Ok(Json(SessionList { sessions }))
}

pub async fn create_session(
    State(state): State<AppState>,
    Json(request): Json<CreateSession>,
) -> Result<Response, ApiError> {
    if !request.cwd.is_absolute() || !request.cwd.is_dir() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "bad_cwd"));
    }
    let created = async {
        let session = state.manager.create(request.cwd).await?;
        created_view(&state.manager, session.id()).await
    };
    let view = created
        .await
        .map_err(|err| ApiError::internal("create session", err))?;
    Ok((StatusCode::CREATED, Json(view)).into_response())
}

pub async fn create_scratch(State(state): State<AppState>) -> Result<Response, ApiError> {
    let created = async {
        let session = state.manager.create_scratch().await?;
        created_view(&state.manager, session.id()).await
    };
    let view = created
        .await
        .map_err(|err| ApiError::internal("create scratch session", err))?;
    Ok((StatusCode::CREATED, Json(view)).into_response())
}

async fn created_view(manager: &SessionManager, id: uuid::Uuid) -> anyhow::Result<SessionView> {
    let row = manager.store().require_session(id).await?;
    let scratch = manager.is_scratch(&row.cwd);
    Ok(SessionView::new(row, manager.live_view(id).await, scratch))
}

pub async fn usage(State(state): State<AppState>) -> Result<Response, ApiError> {
    let snapshot = state
        .manager
        .usage()
        .await
        .map_err(|err| ApiError::internal("usage snapshot", err))?;
    Ok(Json(snapshot).into_response())
}

pub async fn catalog(State(state): State<AppState>) -> Result<Response, ApiError> {
    let catalog = state
        .manager
        .catalog()
        .await
        .map_err(|err| ApiError::internal("shared catalog", err))?;
    Ok(Json(catalog).into_response())
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

/// Whether the server can restart itself, whether a restart waits for every turn
/// to end, and how many turns run.
pub async fn server(State(state): State<AppState>) -> Response {
    let restart = state.manager.server_restart();
    let supervised = restart.supervision() == Supervision::Shim;
    let pending = restart.state() == RestartState::Pending;
    let turns = state.manager.running_turns().await;
    Json(json!({"supervised": supervised, "pending": pending, "turns": turns})).into_response()
}

/// Drops a restart waiting for every turn to end; messages held meanwhile stay held.
pub async fn cancel_restart(State(state): State<AppState>) -> Response {
    state.manager.server_restart().cancel();
    StatusCode::NO_CONTENT.into_response()
}

/// Stops the server now, stopping running turns, for the shim to run it again.
pub async fn restart_server(State(state): State<AppState>) -> Result<Response, ApiError> {
    if state.manager.server_restart().now().is_err() {
        return Err(ApiError::new(StatusCode::CONFLICT, "not_supervised"));
    }
    Ok(StatusCode::ACCEPTED.into_response())
}

async fn session_views(manager: &SessionManager) -> anyhow::Result<Vec<SessionView>> {
    let mut live = manager.live_views().await;
    Ok(manager
        .list()
        .await?
        .into_iter()
        .map(|row| {
            let live = live.remove(&row.id);
            let scratch = manager.is_scratch(&row.cwd);
            SessionView::new(row, live, scratch)
        })
        .collect())
}
