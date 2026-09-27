use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::{Duration as StdDuration, Instant};

use anyhow::{Context, Result};
use axum::{
    Json,
    extract::{ConnectInfo, Path as AxumPath, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rand::TryRng;
use rand::rngs::SysRng;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::{BASE_PATH, BASE_PATH_SLASH};

pub const COOKIE_NAME: &str = "vigie2_session";
const COOKIE_MAX_AGE_DAYS: i64 = 60;
const LAST_SEEN_THROTTLE_SECS: i64 = 60;
const PAIR_TOKEN_TTL: StdDuration = StdDuration::from_secs(300);
const MAX_LABEL_LEN: usize = 64;
const RATE_WINDOW: StdDuration = StdDuration::from_secs(600);
const RATE_MAX_FAILS: usize = 5;
const RATE_LOCKOUT: StdDuration = StdDuration::from_secs(3600);

#[derive(Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub label: String,
    pub issued_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    #[serde(default)]
    pub revoked: bool,
    #[serde(default)]
    pub last_ip: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct RegistryFile {
    devices: Vec<Device>,
}

pub struct Registry {
    path: PathBuf,
    devices: Mutex<Vec<Device>>,
}

impl Registry {
    pub fn load(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir {}", parent.display()))?;
        }
        let devices = read_locked(&path)?;
        Ok(Self {
            path,
            devices: Mutex::new(devices),
        })
    }

    /// Re-read the on-disk registry into memory. Required when a second
    /// process (`vigie2 auth revoke-all`) rewrites the file: without it the
    /// running server keeps honouring cookies that were revoked on disk.
    pub fn reload(&self) -> Result<()> {
        let devices = read_locked(&self.path)?;
        *self.devices.lock() = devices;
        Ok(())
    }

    pub fn find_active(&self, id: &str) -> Option<Device> {
        self.devices
            .lock()
            .iter()
            .find(|d| d.id == id && !d.revoked)
            .cloned()
    }

    #[cfg(test)]
    pub fn has_active(&self) -> bool {
        self.devices.lock().iter().any(|d| !d.revoked)
    }

    pub fn next_label(&self) -> String {
        let n = self.devices.lock().len() + 1;
        format!("device {n}")
    }

    pub fn insert(&self, device: Device) -> Result<()> {
        self.with_file(|devices| {
            devices.push(device);
            ((), true)
        })
    }

    pub fn list(&self) -> Vec<Device> {
        self.devices.lock().clone()
    }

    pub fn rename(&self, id: &str, label: &str) -> Result<bool> {
        self.with_file(|devices| match devices.iter_mut().find(|d| d.id == id) {
            Some(d) => {
                d.label = label.to_string();
                (true, true)
            }
            None => (false, false),
        })
    }

    pub fn revoke(&self, id: &str) -> Result<RevokeResult> {
        self.with_file(|devices| match devices.iter_mut().find(|d| d.id == id) {
            None => (RevokeResult::NotFound, false),
            Some(d) if d.revoked => (RevokeResult::AlreadyRevoked, false),
            Some(d) => {
                d.revoked = true;
                (RevokeResult::Revoked, true)
            }
        })
    }

    /// Permanently delete a device record. Only allowed for already-revoked
    /// entries; active devices must be revoked first.
    pub fn remove(&self, id: &str) -> Result<RemoveResult> {
        self.with_file(|devices| match devices.iter().position(|d| d.id == id) {
            None => (RemoveResult::NotFound, false),
            Some(i) if !devices[i].revoked => (RemoveResult::NotRevoked, false),
            Some(i) => {
                devices.remove(i);
                (RemoveResult::Removed, true)
            }
        })
    }

    pub fn revoke_all(&self) -> Result<usize> {
        self.with_file(|devices| {
            let mut count = 0;
            for d in devices.iter_mut() {
                if !d.revoked {
                    d.revoked = true;
                    count += 1;
                }
            }
            (count, count > 0)
        })
    }

    /// Throttled: only takes the file lock if `last_seen` is stale beyond
    /// `LAST_SEEN_THROTTLE_SECS`, or if `ip` differs from the recorded one.
    /// Runs on every authed request, so the no-op path stays in memory.
    pub fn touch(&self, id: &str, ip: Option<String>) {
        let now = Utc::now();
        let stale = self.devices.lock().iter().any(|d| {
            d.id == id
                && ((ip.is_some() && d.last_ip != ip)
                    || (now - d.last_seen).num_seconds() >= LAST_SEEN_THROTTLE_SECS)
        });
        if !stale {
            return;
        }
        let written = self.with_file(|devices| {
            let Some(device) = devices.iter_mut().find(|d| d.id == id) else {
                return (false, false);
            };
            let ip_changed = ip.is_some() && device.last_ip != ip;
            let seen_stale = (now - device.last_seen).num_seconds() >= LAST_SEEN_THROTTLE_SECS;
            if !ip_changed && !seen_stale {
                return (false, false);
            }
            if seen_stale {
                device.last_seen = now;
            }
            if let Some(ip) = ip {
                device.last_ip = Some(ip);
            }
            (true, true)
        });
        if let Err(err) = written {
            warn!(%err, "registry persist (touch) failed");
        }
    }

    /// Read-modify-write the registry file under one exclusive advisory lock,
    /// then refresh the in-memory cache from the result. `apply` returns its
    /// own result plus whether the vector needs writing back.
    ///
    /// Re-reading inside the lock is load-bearing: without it a mutation here
    /// writes a stale in-memory copy over whatever `vigie2 auth revoke-all`
    /// just did from another process, silently un-revoking those devices.
    fn with_file<F, R>(&self, apply: F) -> Result<R>
    where
        F: FnOnce(&mut Vec<Device>) -> (R, bool),
    {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir {}", parent.display()))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&self.path)
            .with_context(|| format!("open {}", self.path.display()))?;
        file.lock().context("lock registry")?;
        let outcome = (|| -> Result<R> {
            let mut devices = read_devices(&mut file, &self.path)?;
            let (result, changed) = apply(&mut devices);
            if changed {
                let contents = serde_json::to_vec_pretty(&RegistryFile {
                    devices: devices.clone(),
                })?;
                file.set_len(0)?;
                file.seek(SeekFrom::Start(0))?;
                file.write_all(&contents)?;
                file.sync_all()?;
                secure_perms(&self.path)?;
            }
            *self.devices.lock() = devices;
            Ok(result)
        })();
        file.unlock().ok();
        outcome
    }
}

/// Read the registry from an already-open, already-locked handle.
fn read_devices(file: &mut File, path: &Path) -> Result<Vec<Device>> {
    file.seek(SeekFrom::Start(0)).context("seek registry")?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .with_context(|| format!("read {}", path.display()))?;
    if buf.is_empty() {
        return Ok(Vec::new());
    }
    let parsed: RegistryFile =
        serde_json::from_slice(&buf).with_context(|| format!("parse {}", path.display()))?;
    Ok(parsed.devices)
}

/// Read the registry under a shared advisory lock, so a reader never observes
/// the half-written file of a concurrent writer.
fn read_locked(path: &Path) -> Result<Vec<Device>> {
    let mut file = match OpenOptions::new().read(true).open(path) {
        Ok(f) => f,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("open {}", path.display())),
    };
    file.lock_shared().context("lock registry")?;
    let result = read_devices(&mut file, path);
    file.unlock().ok();
    result
}

#[cfg(unix)]
fn secure_perms(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms).context("chmod registry")?;
    Ok(())
}

#[cfg(not(unix))]
fn secure_perms(_path: &Path) -> Result<()> {
    Ok(())
}

pub enum RevokeResult {
    Revoked,
    NotFound,
    AlreadyRevoked,
}

pub enum RemoveResult {
    Removed,
    NotFound,
    NotRevoked,
}

struct PairToken {
    token: String,
    issued_by: String,
    expires_at: Instant,
}

pub struct Auth {
    pub registry: Registry,
    pub startup_token: String,
    pub cookie_secure: bool,
    /// Set once the startup token has bootstrapped a device. It stays set for
    /// the life of the process, so the token is not resurrected by a later
    /// revoke emptying the registry.
    startup_spent: Mutex<bool>,
    pair_tokens: Mutex<Vec<PairToken>>,
    rate_limiter: RateLimiter,
    devices_changed: broadcast::Sender<()>,
}

impl Auth {
    pub fn new(registry: Registry, cookie_secure: bool) -> Self {
        let (tx, _) = broadcast::channel::<()>(16);
        Self {
            registry,
            startup_token: mint_token(),
            cookie_secure,
            startup_spent: Mutex::new(false),
            pair_tokens: Mutex::new(Vec::new()),
            rate_limiter: RateLimiter::new(),
            devices_changed: tx,
        }
    }

    pub fn subscribe_changes(&self) -> broadcast::Receiver<()> {
        self.devices_changed.subscribe()
    }

    pub fn notify_changed(&self) {
        let n = self.devices_changed.send(()).unwrap_or(0);
        tracing::info!(subscribers = n, "devices changed");
    }

    pub fn cookie_for(&self, id: String) -> Cookie<'static> {
        let mut cookie = Cookie::new(COOKIE_NAME, id);
        cookie.set_http_only(true);
        cookie.set_secure(self.cookie_secure);
        cookie.set_same_site(SameSite::Strict);
        // Scoped to the base path, not the root. Cookies are keyed by host and
        // ignore the port, so a root-scoped cookie would also be sent to
        // whatever else answers on this hostname.
        cookie.set_path(BASE_PATH);
        cookie.set_max_age(time::Duration::days(COOKIE_MAX_AGE_DAYS));
        cookie
    }

    /// Constant-time compare against the startup token, without spending it.
    fn matches_startup_token(&self, candidate: &str) -> bool {
        self.startup_token
            .as_bytes()
            .ct_eq(candidate.as_bytes())
            .into()
    }

    /// Spend the startup token. Returns false if it was already spent, which
    /// is what keeps it to the single bootstrap use it is documented as.
    fn spend_startup_token(&self) -> bool {
        let mut spent = self.startup_spent.lock();
        if *spent {
            false
        } else {
            *spent = true;
            true
        }
    }

    fn issue_pair_token(&self, device_id: &str) -> String {
        let now = Instant::now();
        let token = mint_token();
        let mut guard = self.pair_tokens.lock();
        guard.retain(|t| t.expires_at > now && t.issued_by != device_id);
        guard.push(PairToken {
            token: token.clone(),
            issued_by: device_id.to_string(),
            expires_at: now + PAIR_TOKEN_TTL,
        });
        token
    }

    /// Consume a pair token if it matches and is still valid. Returns true on match.
    fn consume_pair_token(&self, candidate: &str) -> bool {
        let now = Instant::now();
        let mut guard = self.pair_tokens.lock();
        guard.retain(|t| t.expires_at > now);
        let idx = guard
            .iter()
            .position(|t| bool::from(t.token.as_bytes().ct_eq(candidate.as_bytes())));
        if let Some(i) = idx {
            guard.remove(i);
            true
        } else {
            false
        }
    }
}

struct RateLimiter {
    inner: Mutex<HashMap<IpAddr, IpState>>,
}

struct IpState {
    failures: Vec<Instant>,
    lockout_until: Option<Instant>,
}

enum RateCheck {
    Ok,
    Locked { retry_after_secs: u64 },
}

impl RateLimiter {
    fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    fn check(&self, ip: IpAddr) -> RateCheck {
        let now = Instant::now();
        let mut inner = self.inner.lock();
        let Some(state) = inner.get_mut(&ip) else {
            return RateCheck::Ok;
        };
        state
            .failures
            .retain(|t| now.duration_since(*t) < RATE_WINDOW);
        if let Some(until) = state.lockout_until {
            if now < until {
                return RateCheck::Locked {
                    retry_after_secs: (until - now).as_secs().max(1),
                };
            } else {
                state.lockout_until = None;
                state.failures.clear();
            }
        }
        RateCheck::Ok
    }

    fn record_failure(&self, ip: IpAddr) {
        let now = Instant::now();
        let mut inner = self.inner.lock();
        let state = inner.entry(ip).or_insert(IpState {
            failures: Vec::new(),
            lockout_until: None,
        });
        state
            .failures
            .retain(|t| now.duration_since(*t) < RATE_WINDOW);
        state.failures.push(now);
        if state.failures.len() >= RATE_MAX_FAILS {
            state.lockout_until = Some(now + RATE_LOCKOUT);
            warn!(%ip, "rate-limit lockout: too many failed pair attempts");
        }
    }
}

fn mint_token() -> String {
    let mut bytes = [0u8; 32];
    SysRng
        .try_fill_bytes(&mut bytes)
        .expect("OS random source unavailable");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn mint_device_id() -> String {
    mint_token()
}

#[derive(Deserialize)]
pub struct ExchangeReq {
    pub token: String,
}

#[derive(Serialize)]
struct ExchangeOk<'a> {
    label: &'a str,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
}

#[derive(Serialize)]
pub struct WhoamiResp {
    pub authed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

pub async fn exchange(
    State(state): State<crate::AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    jar: CookieJar,
    Json(req): Json<ExchangeReq>,
) -> Response {
    let auth = &state.auth;
    let ip = peer.ip();

    if let RateCheck::Locked { retry_after_secs } = auth.rate_limiter.check(ip) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(
                axum::http::header::RETRY_AFTER,
                retry_after_secs.to_string(),
            )],
            Json(ErrorBody {
                error: "rate_limited",
            }),
        )
            .into_response();
    }

    let via_startup = auth.matches_startup_token(&req.token);

    let (path, pair_matched) = if via_startup {
        ("startup", false)
    } else if auth.consume_pair_token(&req.token) {
        ("pair", true)
    } else {
        auth.rate_limiter.record_failure(ip);
        return (
            StatusCode::UNAUTHORIZED,
            Json(ErrorBody { error: "bad_token" }),
        )
            .into_response();
    };

    if via_startup && !auth.spend_startup_token() {
        warn!(%ip, "startup token reuse rejected");
        return (
            StatusCode::UNAUTHORIZED,
            Json(ErrorBody {
                error: "token_spent",
            }),
        )
            .into_response();
    }

    let id = mint_device_id();
    let now = Utc::now();
    let label = auth.registry.next_label();
    let device = Device {
        id: id.clone(),
        label: label.clone(),
        issued_at: now,
        last_seen: now,
        revoked: false,
        last_ip: Some(ip.to_string()),
    };
    if let Err(err) = auth.registry.insert(device) {
        warn!(%err, "registry insert failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody {
                error: "registry_write",
            }),
        )
            .into_response();
    }
    auth.notify_changed();

    info!(via = path, pair = pair_matched, label = %label, "device paired");
    let cookie = auth.cookie_for(id);
    (jar.add(cookie), Json(ExchangeOk { label: &label })).into_response()
}

pub async fn whoami(State(state): State<crate::AppState>, headers: HeaderMap) -> Json<WhoamiResp> {
    match session_cookie_values(&headers).find_map(|value| state.auth.registry.find_active(value)) {
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
    last_ip: Option<String>,
}

#[derive(Serialize)]
pub struct DevicesResp {
    devices: Vec<DeviceView>,
}

pub async fn devices_list(
    State(state): State<crate::AppState>,
    jar: CookieJar,
) -> Json<DevicesResp> {
    let you = jar.get(COOKIE_NAME).map(|c| c.value().to_string());
    let devices = state
        .auth
        .registry
        .list()
        .into_iter()
        .map(|d| DeviceView {
            is_you: you.as_deref() == Some(d.id.as_str()),
            last_ip: d.last_ip.clone(),
            id: d.id,
            label: d.label,
            issued_at: d.issued_at,
            last_seen: d.last_seen,
            revoked: d.revoked,
        })
        .collect();
    Json(DevicesResp { devices })
}

#[derive(Deserialize)]
pub struct RenameReq {
    pub label: String,
}

pub async fn devices_rename(
    State(state): State<crate::AppState>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<RenameReq>,
) -> Response {
    let label = req.label.trim();
    if label.is_empty() || label.len() > MAX_LABEL_LEN {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorBody { error: "bad_label" }),
        )
            .into_response();
    }
    match state.auth.registry.rename(&id, label) {
        Ok(true) => {
            state.auth.notify_changed();
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(err) => {
            warn!(%err, "rename failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn devices_delete(
    State(state): State<crate::AppState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match state.auth.registry.remove(&id) {
        Ok(RemoveResult::Removed) => {
            state.auth.notify_changed();
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(RemoveResult::NotFound) => StatusCode::NOT_FOUND.into_response(),
        Ok(RemoveResult::NotRevoked) => (
            StatusCode::CONFLICT,
            Json(ErrorBody {
                error: "not_revoked",
            }),
        )
            .into_response(),
        Err(err) => {
            warn!(%err, "delete failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn devices_revoke(
    State(state): State<crate::AppState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match state.auth.registry.revoke(&id) {
        Ok(RevokeResult::Revoked) => {
            state.auth.notify_changed();
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(RevokeResult::NotFound) => StatusCode::NOT_FOUND.into_response(),
        Ok(RevokeResult::AlreadyRevoked) => StatusCode::CONFLICT.into_response(),
        Err(err) => {
            warn!(%err, "revoke failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
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
    State(state): State<crate::AppState>,
    jar: CookieJar,
    req: Option<Json<PairReq>>,
) -> Response {
    let Some(cookie) = jar.get(COOKIE_NAME) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(device) = state.auth.registry.find_active(cookie.value()) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let known_ips: std::collections::HashSet<String> = crate::net::all_reachable_ipv4()
        .into_iter()
        .map(|ip| ip.to_string())
        .collect();
    let known_hosts: std::collections::HashSet<String> =
        state.pair_extra_hosts.iter().cloned().collect();

    let requested = req.map(|Json(r)| r.hosts).unwrap_or_default();
    let selected: Vec<String> = if requested.is_empty() {
        let mut all: Vec<String> = known_ips.iter().cloned().collect();
        all.sort();
        all.extend(known_hosts.iter().cloned());
        all
    } else {
        requested
            .into_iter()
            .filter(|h| known_ips.contains(h) || known_hosts.contains(h))
            .collect()
    };

    if selected.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorBody {
                error: "no_valid_hosts",
            }),
        )
            .into_response();
    }

    let token = state.auth.issue_pair_token(&device.id);
    let scheme = state.pair_scheme.as_str();
    let port = state.pair_port;
    let urls: Vec<String> = selected
        .into_iter()
        .map(|host| {
            if host.contains(':') {
                format!("{scheme}://{host}{BASE_PATH_SLASH}#auth={token}")
            } else {
                format!("{scheme}://{host}:{port}{BASE_PATH_SLASH}#auth={token}")
            }
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
    interfaces: Vec<crate::net::Interface>,
    public_hosts: Vec<String>,
}

pub async fn devices_events(
    State(state): State<crate::AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    axum::Extension(device): axum::Extension<AuthedDevice>,
) -> axum::response::sse::Sse<
    impl futures_util::stream::Stream<
        Item = Result<axum::response::sse::Event, std::convert::Infallible>,
    >,
> {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use futures_util::stream::unfold;
    use std::time::Duration;
    use tokio::sync::broadcast::error::RecvError;

    let rx = state.auth.subscribe_changes();
    let auth = std::sync::Arc::clone(&state.auth);
    let device_id = device.id.clone();
    tracing::info!(device = %device.label, ip = %peer.ip(), "devices SSE client connected");
    // Every tick re-checks the streaming device against the registry, so a
    // revoke tears this stream down instead of leaving it live until the
    // client happens to disconnect.
    let stream = unfold(
        (rx, true, auth, device_id),
        |(mut rx, first, auth, device_id)| async move {
            if first {
                return Some((
                    Ok(Event::default().event("change").data("init")),
                    (rx, false, auth, device_id),
                ));
            }
            let data = match rx.recv().await {
                Ok(()) => "changed",
                Err(RecvError::Lagged(_)) => "lagged",
                Err(RecvError::Closed) => return None,
            };
            if auth.registry.find_active(&device_id).is_none() {
                tracing::info!("devices SSE closing: device revoked");
                return None;
            }
            Some((
                Ok(Event::default().event("change").data(data)),
                (rx, false, auth, device_id),
            ))
        },
    );
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

pub async fn interfaces(State(state): State<crate::AppState>) -> Json<InterfacesResp> {
    Json(InterfacesResp {
        port: state.pair_port,
        scheme: state.pair_scheme.as_str().to_string(),
        interfaces: crate::net::enumerate(),
        public_hosts: state.pair_extra_hosts.iter().cloned().collect(),
    })
}

#[derive(Clone)]
pub struct AuthedDevice {
    pub id: String,
    pub label: String,
}

/// Every value the request carries under `COOKIE_NAME`, in header order.
///
/// Read from the raw header rather than through `CookieJar`, which keys its
/// entries by cookie name alone and so collapses same-name cookies to one. A
/// browser that holds this cookie at two paths (a legacy `/` one next to the
/// current `BASE_PATH` one) sends both, and the jar would hide whichever the
/// header listed first, which is the one that matters: browsers order
/// same-name cookies longest-path-first.
fn session_cookie_values(headers: &HeaderMap) -> impl Iterator<Item = &str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|raw| raw.split(';'))
        .filter_map(|pair| pair.split_once('='))
        .filter(|(name, _)| name.trim() == COOKIE_NAME)
        .map(|(_, value)| value.trim())
}

pub async fn require_auth(
    State(state): State<crate::AppState>,
    jar: CookieJar,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let path = req.uri().path().to_string();
    let sent: Vec<String> = session_cookie_values(req.headers())
        .map(str::to_owned)
        .collect();
    let device = sent
        .iter()
        .find_map(|value| state.auth.registry.find_active(value));
    let sent = sent.len();
    if sent == 0 {
        info!(ip = %peer.ip(), path = %path, "unauthorized (no cookie)");
        return Err(StatusCode::UNAUTHORIZED);
    }
    let Some(device) = device else {
        info!(ip = %peer.ip(), path = %path, sent, "unauthorized (invalid cookie)");
        return Err(StatusCode::UNAUTHORIZED);
    };
    let device_id = device.id.clone();
    state
        .auth
        .registry
        .touch(&device_id, Some(peer.ip().to_string()));
    req.extensions_mut().insert(AuthedDevice {
        id: device.id,
        label: device.label,
    });
    // The cookie's Max-Age is the only expiry there is, and the browser never
    // reports how much of it is left. Re-issuing on every authed request is
    // what keeps a device that is in use from ageing out.
    let jar = jar.add(state.auth.cookie_for(device_id));
    Ok((jar, next.run(req).await).into_response())
}

pub fn state_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("no home dir")?;
    Ok(home.join(".vigie2"))
}

#[cfg(test)]
mod tests {
    //! Two `Registry` values over one path stand in for the two processes that
    //! really share `devices.json`: the running server, and a `vigie2 auth
    //! revoke-all` invocation.
    use super::*;

    /// A browser that holds the same cookie name at two paths sends both in
    /// one header. `CookieJar` keys its entries by name alone, so only one
    /// survives extraction: reading the jar cannot see the other.
    #[test]
    fn a_cookie_jar_keeps_one_cookie_per_name() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            "vigie2_session=from_prefix_path; vigie2_session=stale_root"
                .parse()
                .expect("header value"),
        );
        let jar = CookieJar::from_headers(&headers);
        let seen: Vec<&str> = jar
            .iter()
            .filter(|c| c.name() == COOKIE_NAME)
            .map(|c| c.value())
            .collect();
        assert_eq!(seen, vec!["stale_root"], "jar kept: {seen:?}");

        assert_eq!(
            session_cookie_values(&headers).collect::<Vec<_>>(),
            vec!["from_prefix_path", "stale_root"],
            "reading the header sees both, in the order the browser sent them",
        );
    }

    /// Several cookies of one name reach the server whenever the path the
    /// cookie is set at changes. Authenticating must not depend on which one
    /// the browser happened to list first.
    #[test]
    fn any_of_several_session_cookies_can_be_the_live_one() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry = Registry::load(tmp.path().join("devices.json")).expect("load");
        registry.insert(device("live")).expect("insert");

        for header in [
            "vigie2_session=live; vigie2_session=stale",
            "vigie2_session=stale; vigie2_session=live",
            "vigie_session=other; vigie2_session=stale; vigie2_session=live",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::COOKIE, header.parse().expect("header value"));
            let found = session_cookie_values(&headers)
                .find_map(|value| registry.find_active(value))
                .map(|d| d.id);
            assert_eq!(found.as_deref(), Some("live"), "for header {header:?}");
        }
    }

    fn device(id: &str) -> Device {
        let now = Utc::now();
        Device {
            id: id.to_string(),
            label: id.to_string(),
            issued_at: now,
            last_seen: now,
            revoked: false,
            last_ip: None,
        }
    }

    fn revoke_all_from_another_process(path: &Path) {
        Registry::load(path.to_path_buf())
            .expect("load cli registry")
            .revoke_all()
            .expect("revoke all");
    }

    #[test]
    fn external_revoke_survives_a_later_server_write() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("devices.json");

        let server = Registry::load(path.clone()).expect("load server registry");
        server.insert(device("phone")).expect("insert phone");

        revoke_all_from_another_process(&path);

        // Server writes again from its own, now stale, in-memory copy.
        server.insert(device("laptop")).expect("insert laptop");

        let on_disk = Registry::load(path).expect("reload from disk");
        let phone = on_disk
            .list()
            .into_iter()
            .find(|d| d.id == "phone")
            .expect("phone still present");
        assert!(
            phone.revoked,
            "server write clobbered an external revoke-all",
        );
    }

    #[test]
    fn touch_does_not_resurrect_an_externally_revoked_device() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("devices.json");

        let server = Registry::load(path.clone()).expect("load server registry");
        let mut stale = device("phone");
        // Stale enough that touch takes the file lock instead of no-opping.
        stale.last_seen = Utc::now() - chrono::Duration::seconds(LAST_SEEN_THROTTLE_SECS * 2);
        server.insert(stale).expect("insert phone");

        revoke_all_from_another_process(&path);
        server.touch("phone", Some("10.0.0.1".into()));

        let on_disk = Registry::load(path).expect("reload from disk");
        assert!(on_disk.find_active("phone").is_none());
    }

    #[test]
    fn reload_picks_up_an_external_revoke() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("devices.json");

        let server = Registry::load(path.clone()).expect("load server registry");
        server.insert(device("phone")).expect("insert phone");
        assert!(server.find_active("phone").is_some());

        revoke_all_from_another_process(&path);
        assert!(
            server.find_active("phone").is_some(),
            "in-memory copy should still be stale before reload",
        );

        server.reload().expect("reload");
        assert!(server.find_active("phone").is_none());
    }

    fn empty_auth() -> Auth {
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry = Registry::load(tmp.path().join("devices.json")).expect("load registry");
        // The tempdir guard drops here; the registry only touches the file on
        // mutation, and these cases never mutate.
        Auth::new(registry, false)
    }

    #[test]
    fn startup_token_is_spent_by_its_first_use() {
        let auth = empty_auth();
        assert!(auth.matches_startup_token(&auth.startup_token.clone()));
        assert!(auth.spend_startup_token());
        assert!(
            !auth.spend_startup_token(),
            "startup token accepted a second bootstrap",
        );
    }

    #[test]
    fn startup_token_rejects_a_wrong_candidate_without_spending_it() {
        let auth = empty_auth();
        assert!(!auth.matches_startup_token("not-the-token"));
        assert!(
            auth.spend_startup_token(),
            "a failed compare must not spend the token",
        );
    }

    #[test]
    fn revoke_all_reports_only_devices_it_actually_revoked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("devices.json");

        let registry = Registry::load(path).expect("load registry");
        registry.insert(device("phone")).expect("insert phone");
        registry.insert(device("laptop")).expect("insert laptop");

        assert_eq!(registry.revoke_all().expect("first revoke-all"), 2);
        assert_eq!(registry.revoke_all().expect("second revoke-all"), 0);
        assert!(!registry.has_active());
    }
}
