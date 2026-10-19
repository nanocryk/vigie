//! REST routes over the sessions, the account usage and the shared catalog, and
//! the error every REST route answers with.

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Display;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;
use uuid::Uuid;

use crate::AppState;
use crate::http::wire::{CreateSession, SessionList, SessionView};
use crate::session::manager::SessionManager;
use crate::session::server_restart::{RestartState, RunningBuild, Supervision};
use crate::store::{MessageQuery, SessionRow, snippet};

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

#[derive(Debug, Deserialize)]
pub struct HistoryMessageQuery {
    session: Uuid,
    seq: u64,
}

/// `api/history/message`: one message of vigie's history, for the `history_read` ask.
/// The session is given without the message when it holds none at that seq.
pub async fn history_message(
    State(state): State<AppState>,
    Query(query): Query<HistoryMessageQuery>,
) -> Result<Response, ApiError> {
    let store = state.manager.store();
    let row = store
        .session(query.session)
        .await
        .map_err(|err| ApiError::internal("history session", err))?
        .ok_or(ApiError::new(StatusCode::NOT_FOUND, "no_session"))?;
    let message = store
        .message(query.session, query.seq)
        .await
        .map_err(|err| ApiError::internal("history message", err))?;
    Ok(Json(json!({
        "name": row.name,
        "cwd": row.cwd,
        "message": message.map(|message| json!({
            "role": message.role.as_str(),
            "time": message.time,
            "text": message.text,
        })),
    }))
    .into_response())
}

/// Hits per page of `api/history/search`.
const SEARCH_PAGE: usize = 20;

#[derive(Debug, Deserialize)]
pub struct HistorySearchQuery {
    q: String,
    /// Searches every discussion when absent.
    discussion: Option<Uuid>,
    #[serde(default)]
    offset: usize,
}

/// `api/history/search`: the messages holding every word of `q`, newest first.
pub async fn history_search(
    State(state): State<AppState>,
    Query(query): Query<HistorySearchQuery>,
) -> Result<Response, ApiError> {
    let words: Vec<String> = query.q.split_whitespace().map(str::to_owned).collect();
    if words.is_empty() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "no_words"));
    }
    let store = state.manager.store();
    let discussions: HashMap<Uuid, SessionRow> = store
        .sessions()
        .await
        .map_err(|err| ApiError::internal("history discussions", err))?
        .into_iter()
        .filter(|row| query.discussion.is_none_or(|id| row.id == id))
        .map(|row| (row.id, row))
        .collect();
    let message_query = MessageQuery {
        sessions: discussions.keys().copied().collect(),
        words: words.clone(),
        ..MessageQuery::default()
    };
    let (total, hits) = store
        .search_messages(message_query, query.offset, SEARCH_PAGE)
        .await
        .map_err(|err| ApiError::internal("history search", err))?;
    let hits: Vec<_> = hits
        .iter()
        .filter_map(|hit| {
            let row = discussions.get(&hit.session)?;
            Some(json!({
                "discussion": hit.session,
                "name": row.name,
                "cwd": row.cwd,
                "seq": hit.seq,
                "time": hit.time,
                "role": hit.role.as_str(),
                "snippet": snippet(&hit.text, &words),
            }))
        })
        .collect();
    Ok(Json(json!({ "total": total, "hits": hits })).into_response())
}

/// Exchanges `api/history/around` gives at most on either side of the hit.
const AROUND_MAX_EXCHANGES: u32 = 50;

#[derive(Debug, Deserialize)]
pub struct HistoryAroundQuery {
    discussion: Uuid,
    seq: u64,
    before: u32,
    after: u32,
}

/// `api/history/around`: the messages of the exchanges around `seq`, and whether
/// the discussion has messages past either end.
pub async fn history_around(
    State(state): State<AppState>,
    Query(query): Query<HistoryAroundQuery>,
) -> Result<Response, ApiError> {
    if query.before > AROUND_MAX_EXCHANGES || query.after > AROUND_MAX_EXCHANGES {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "too_many_exchanges"));
    }
    let store = state.manager.store();
    let row = store
        .session(query.discussion)
        .await
        .map_err(|err| ApiError::internal("history discussion", err))?
        .ok_or(ApiError::new(StatusCode::NOT_FOUND, "no_discussion"))?;
    let around = async {
        let messages = store
            .messages_around(query.discussion, query.seq, query.before, query.after)
            .await?;
        let (Some(first), Some(last)) = (messages.first(), messages.last()) else {
            return anyhow::Ok((messages, false, false));
        };
        let earlier = store
            .messages_around(query.discussion, first.seq, 1, 0)
            .await?
            .first()
            .is_some_and(|message| message.seq < first.seq);
        let later = store
            .messages_around(query.discussion, last.seq, 0, 1)
            .await?
            .last()
            .is_some_and(|message| message.seq > last.seq);
        Ok((messages, earlier, later))
    };
    let (messages, earlier, later) = around
        .await
        .map_err(|err| ApiError::internal("history around", err))?;
    let messages: Vec<_> = messages
        .iter()
        .map(|message| {
            json!({
                "seq": message.seq,
                "time": message.time,
                "role": message.role.as_str(),
                "text": message.text,
            })
        })
        .collect();
    Ok(Json(json!({
        "name": row.name,
        "cwd": row.cwd,
        "messages": messages,
        "earlier": earlier,
        "later": later,
    }))
    .into_response())
}

/// How many command names `api/commands/recent` returns at most.
const RECENT_COMMANDS: usize = 20;

pub async fn recent_commands(State(state): State<AppState>) -> Result<Response, ApiError> {
    let names = state
        .manager
        .store()
        .recent_commands(RECENT_COMMANDS)
        .await
        .map_err(|err| ApiError::internal("recent commands", err))?;
    Ok(Json(names).into_response())
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

/// How long before its expiry the TLS certificate is flagged.
pub const CERT_WARNING_BEFORE: chrono::TimeDelta = chrono::TimeDelta::days(14);

/// `expires_at` when it falls within `CERT_WARNING_BEFORE` of `now`, or already passed.
pub fn cert_expires_soon(
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    expires_at.filter(|expires_at| *expires_at - now <= CERT_WARNING_BEFORE)
}

/// Whether the server can restart itself, whether it runs a previous build because
/// copying the latest failed, whether a build differing from the running binary
/// waits for a restart, whether a restart waits for every turn to end, how
/// many turns run, and when a TLS certificate about to expire does.
pub async fn server(State(state): State<AppState>) -> Response {
    let restart = state.manager.server_restart();
    let supervised = restart.supervision() == Supervision::Shim;
    let stale_copy = restart.running_build() == RunningBuild::Previous;
    let new_build = match restart.new_build() {
        Some(check) => tokio::task::spawn_blocking(move || check.ready())
            .await
            .unwrap_or(false),
        None => false,
    };
    let pending = restart.state() == RestartState::Pending;
    let turns = state.manager.running_turns().await;
    Json(json!({
        "supervised": supervised,
        "stale_copy": stale_copy,
        "new_build": new_build,
        "pending": pending,
        "turns": turns,
        "cert_expires_soon": cert_expires_soon(state.cert_expires_at, chrono::Utc::now()),
    }))
    .into_response()
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
