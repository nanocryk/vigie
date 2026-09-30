//! Device authentication: a paired device holds a session cookie naming its
//! registry entry, and every protected route goes through `require_auth`.

mod registry;
mod routes;
mod tokens;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use parking_lot::Mutex;
use tokio::sync::broadcast;
use tracing::info;

pub use registry::{Device, Registry, RemoveResult, RevokeResult};
pub use routes::{
    devices_delete, devices_events, devices_list, devices_rename, devices_revoke, exchange,
    interfaces, pair, whoami,
};

use crate::BASE_PATH;
use crate::http::server::AppState;
use tokens::{PairTokens, RateLimiter, mint_token, same_token};

pub const COOKIE_NAME: &str = "vigie_session";
const COOKIE_MAX_AGE_DAYS: i64 = 60;

pub struct Auth {
    pub registry: Registry,
    pub startup_token: String,
    pub cookie_secure: bool,
    /// Set once the startup token has bootstrapped a device. It stays set for
    /// the life of the process, so the token is not resurrected by a later
    /// revoke emptying the registry.
    startup_spent: AtomicBool,
    pair_tokens: PairTokens,
    rate_limiter: RateLimiter,
    devices_changed: broadcast::Sender<()>,
    /// Open app connections per device id. A device with no entry is offline.
    connections: Mutex<HashMap<String, usize>>,
}

/// Holds one open connection of a device; dropping it closes the connection.
pub struct ConnectionGuard {
    auth: Arc<Auth>,
    device_id: String,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        let went_offline = {
            let mut connections = self.auth.connections.lock();
            match connections.get_mut(&self.device_id) {
                Some(count) if *count > 1 => {
                    *count -= 1;
                    false
                }
                Some(_) => {
                    connections.remove(&self.device_id);
                    true
                }
                None => false,
            }
        };
        if went_offline {
            self.auth.notify_changed();
        }
    }
}

impl Auth {
    pub fn new(registry: Registry, cookie_secure: bool) -> Self {
        let (devices_changed, _) = broadcast::channel::<()>(16);
        Self {
            registry,
            startup_token: mint_token(),
            cookie_secure,
            startup_spent: AtomicBool::new(false),
            pair_tokens: PairTokens::default(),
            rate_limiter: RateLimiter::default(),
            devices_changed,
            connections: Mutex::new(HashMap::new()),
        }
    }

    /// Count an open connection of `device_id` until the guard drops.
    pub fn track_connection(self: &Arc<Self>, device_id: &str) -> ConnectionGuard {
        let went_online = {
            let mut connections = self.connections.lock();
            let count = connections.entry(device_id.to_string()).or_insert(0);
            *count += 1;
            *count == 1
        };
        if went_online {
            self.notify_changed();
        }
        ConnectionGuard {
            auth: Arc::clone(self),
            device_id: device_id.to_string(),
        }
    }

    pub fn is_connected(&self, device_id: &str) -> bool {
        self.connections.lock().contains_key(device_id)
    }

    pub fn subscribe_changes(&self) -> broadcast::Receiver<()> {
        self.devices_changed.subscribe()
    }

    pub fn notify_changed(&self) {
        let subscribers = self.devices_changed.send(()).unwrap_or(0);
        info!(subscribers, "devices changed");
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
        same_token(&self.startup_token, candidate)
    }

    /// Spend the startup token. Returns false if it was already spent, which
    /// is what keeps it to the single bootstrap use it is documented as.
    fn spend_startup_token(&self) -> bool {
        !self.startup_spent.swap(true, Ordering::SeqCst)
    }
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
    State(state): State<AppState>,
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

#[cfg(test)]
mod tests {
    use super::registry::tests::device;
    use super::*;

    /// A browser that holds the same cookie name at two paths sends both in
    /// one header. `CookieJar` keys its entries by name alone, so only one
    /// survives extraction: reading the jar cannot see the other.
    #[test]
    fn a_cookie_jar_keeps_one_cookie_per_name() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            "vigie_session=from_prefix_path; vigie_session=stale_root"
                .parse()
                .expect("header value"),
        );
        let jar = CookieJar::from_headers(&headers);
        let seen: Vec<&str> = jar
            .iter()
            .filter(|cookie| cookie.name() == COOKIE_NAME)
            .map(|cookie| cookie.value())
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
            "vigie_session=live; vigie_session=stale",
            "vigie_session=stale; vigie_session=live",
            "other_session=other; vigie_session=stale; vigie_session=live",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::COOKIE, header.parse().expect("header value"));
            let found = session_cookie_values(&headers)
                .find_map(|value| registry.find_active(value))
                .map(|device| device.id);
            assert_eq!(found.as_deref(), Some("live"), "for header {header:?}");
        }
    }

    fn empty_auth() -> Auth {
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry = Registry::load(tmp.path().join("devices.json")).expect("load registry");
        // The tempdir guard drops here; the registry only touches the file on
        // mutation, and these cases never mutate.
        Auth::new(registry, false)
    }

    #[test]
    fn connection_count_notifies_only_on_online_and_offline() {
        let auth = Arc::new(empty_auth());
        let mut changes = auth.subscribe_changes();
        assert!(!auth.is_connected("phone"));

        let first = auth.track_connection("phone");
        let second = auth.track_connection("phone");
        assert!(auth.is_connected("phone"));
        assert!(changes.try_recv().is_ok(), "going online must notify");
        assert!(
            changes.try_recv().is_err(),
            "a second socket must not notify"
        );

        drop(first);
        assert!(auth.is_connected("phone"));
        assert!(
            changes.try_recv().is_err(),
            "closing one of two sockets must not notify"
        );

        drop(second);
        assert!(!auth.is_connected("phone"));
        assert!(changes.try_recv().is_ok(), "going offline must notify");
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
}
