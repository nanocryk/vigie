//! The HTTP router and the state its handlers share.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{Next, from_fn, from_fn_with_state},
    response::{IntoResponse, Redirect, Response},
    routing::{delete, get, post},
};
use tower_http::services::{ServeDir, ServeFile};
use tracing::info;

use crate::http::auth::{self, Auth};
use crate::http::push::{self, Pusher};
use crate::http::update::{self, Updater};
use crate::http::{api, files, git, upload, ws};
use crate::session::manager::SessionManager;

/// Path prefix everything is served under. Android builds a WebAPK's intent
/// filter from scheme, host and path, never the port, so two apps behind one
/// hostname collide at the root and only one can install.
pub const BASE_PATH: &str = "/vigie";
/// `BASE_PATH` with the trailing slash, which is the form the app document is
/// actually served at.
pub const BASE_PATH_SLASH: &str = "/vigie/";

#[derive(Clone)]
pub struct AppState {
    pub auth: Arc<Auth>,
    pub pair_scheme: Arc<String>,
    pub pair_port: u16,
    pub pair_extra_hosts: Arc<Vec<String>>,
    pub web_dir: Arc<PathBuf>,
    pub manager: SessionManager,
    /// Origins, without trailing slash, a WebSocket upgrade may come from.
    pub allowed_origins: Arc<BTreeSet<String>>,
    pub pusher: Arc<Pusher>,
    pub updater: Arc<Updater>,
    /// When the served TLS certificate stops being valid.
    pub cert_expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[cfg(any(test, feature = "test-support"))]
impl AppState {
    /// A state kept in `dir` that runs `program`, with `device` paired (as its
    /// id and its secret), served over plain HTTP on `port` to
    /// `http://127.0.0.1:{port}`.
    pub async fn for_tests(
        dir: &std::path::Path,
        program: crate::claude::child::Program,
        device: &str,
        port: u16,
    ) -> anyhow::Result<Self> {
        use crate::http::auth::{Device, Registry};
        use crate::http::push::Vapid;
        use crate::session::server_restart::{ServerRestart, Supervision};
        use crate::store::Store;

        let registry = Registry::load(dir.join("devices.json"))?;
        registry.insert(Device::new(
            device.to_string(),
            "test".into(),
            device,
            chrono::Utc::now(),
        ))?;
        let auth = Arc::new(Auth::new(registry, false, dir.join("pair-token")));
        let store = Store::open(&dir.join("history.db"))?;
        let vapid = Vapid::load_or_generate(dir)?;
        let pusher = Arc::new(Pusher::new(vapid, store.clone(), Arc::clone(&auth))?);
        let restart = ServerRestart::new(Supervision::Shim);
        let manager = SessionManager::start(store, program, dir, restart).await?;
        Ok(Self {
            auth,
            pair_scheme: Arc::new("http".into()),
            pair_port: port,
            pair_extra_hosts: Arc::new(Vec::new()),
            web_dir: Arc::new(dir.join("web")),
            manager,
            allowed_origins: Arc::new(BTreeSet::from([format!("http://127.0.0.1:{port}")])),
            pusher,
            updater: Arc::new(Updater::new(dir.to_path_buf(), dir.join("last-update"))),
            cert_expires_at: None,
        })
    }
}

/// Build the full HTTP router. Extracted from `serve` so tests can drive the
/// exact production wiring without duplicating the route table.
pub fn build_app(state: AppState) -> Router {
    let protected = Router::new()
        .route("/api/auth/pair", post(auth::pair))
        .route("/api/net/interfaces", get(auth::interfaces))
        .route("/api/devices", get(auth::devices_list))
        .route("/api/events/devices", get(auth::devices_events))
        .route("/api/devices/{id}/rename", post(auth::devices_rename))
        .route("/api/devices/{id}/revoke", post(auth::devices_revoke))
        .route("/api/devices/{id}", delete(auth::devices_delete))
        .route(
            "/api/sessions",
            get(api::list_sessions).post(api::create_session),
        )
        .route("/api/sessions/scratch", post(api::create_scratch))
        .route(
            "/api/sessions/{id}/upload",
            post(upload::upload_route).layer(DefaultBodyLimit::max(upload::MAX_UPLOAD_BYTES)),
        )
        .route("/api/usage", get(api::usage))
        .route("/api/catalog", get(api::catalog))
        .route("/api/commands/recent", get(api::recent_commands))
        .route("/api/history/message", get(api::history_message))
        .route("/api/history/search", get(api::history_search))
        .route("/api/history/around", get(api::history_around))
        .route("/api/fs/list", get(files::list_route))
        .route("/api/fs/places", get(files::places_route))
        .route("/api/fs/file", get(files::file_route))
        .route("/api/fs/raw", get(files::raw_route))
        .route("/api/fs/download", get(files::download_route))
        .route("/api/fs/hash", get(files::hash_route))
        .route("/api/fs/kinds", post(files::kinds_route))
        .route("/api/fs/mkdir", post(files::mkdir_route))
        .route("/api/git/root", get(git::root_route))
        .route("/api/git/diff", get(git::diff_route))
        .route("/api/git/status", get(git::status_route))
        .route("/api/git/file", get(git::file_route))
        .route("/api/ws", get(ws::handler))
        .route("/api/version", get(api::version))
        .route("/api/server", get(api::server))
        .route("/api/server/restart", post(api::restart_server))
        .route("/api/server/restart/cancel", post(api::cancel_restart))
        .route("/api/update", get(update::status_route))
        .route("/api/update/check", post(update::check_route))
        .route("/api/update/run", post(update::run_route))
        .route("/api/push/config", get(push::config))
        .route("/api/push/subscribe", post(push::subscribe))
        .route("/api/push/unsubscribe", post(push::unsubscribe))
        .route("/api/push/test", post(push::test))
        .layer(from_fn_with_state(state.clone(), auth::require_auth));

    let public = Router::new()
        .route("/api/auth/exchange", post(auth::exchange))
        .route("/api/whoami", get(auth::whoami));

    let web_dir = state.web_dir.as_ref().clone();
    let host_check = from_fn_with_state(state.clone(), known_host);
    let served = Router::new()
        .merge(protected)
        .merge(public)
        .fallback_service(ServeDir::new(&web_dir))
        .with_state(state);

    Router::new()
        // `nest` matches the prefix bare and the prefix followed by a
        // non-empty path, but not the prefix with a bare trailing slash, which
        // is exactly where the document lives. So it is routed out here.
        .route_service(BASE_PATH_SLASH, ServeFile::new(web_dir.join("index.html")))
        .nest(BASE_PATH, served)
        .layer(from_fn(common_headers))
        .layer(from_fn(redirect_bare_base))
        .layer(host_check)
}

/// A page on a domain rebound to this machine's address reaches vigie with
/// that domain as its Host, so only the hosts of the allowed origins are
/// served. A request without one is not from a browser.
async fn known_host(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .or_else(|| req.uri().authority().map(|authority| authority.as_str()));
    if let Some(host) = host
        && !host_allowed(host, &state.allowed_origins)
    {
        info!(host = ?host, "forbidden (unknown host)");
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(req).await
}

fn host_allowed(host: &str, allowed_origins: &BTreeSet<String>) -> bool {
    allowed_origins.iter().any(|origin| {
        origin
            .split_once("://")
            .is_some_and(|(_, authority)| authority.eq_ignore_ascii_case(host))
    })
}

/// The app renders untrusted content (Claude's replies, files, diffs), and a
/// script running on its origin could drive Claude, so the CSP confines
/// scripts, connections and images to the origin itself.
const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self'; \
    style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; \
    connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; \
    frame-ancestors 'none'";

async fn common_headers(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let headers = resp.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONTENT_SECURITY_POLICY),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    resp
}

/// Send `/vigie` to `/vigie/`. The document addresses its assets relative to
/// itself, so without the trailing slash every one of them would resolve
/// against the root. Runs as a layer rather than a route because the nest
/// claims the bare prefix for itself.
async fn redirect_bare_base(req: Request, next: Next) -> Response {
    if req.uri().path() == BASE_PATH {
        return Redirect::permanent(BASE_PATH_SLASH).into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod auth_gate_tests {
    //! Verify every route registered in `build_app` is on the correct side of
    //! the `require_auth` middleware. Adding a route to `build_app` without
    //! adding it here means the coverage silently gaps, so keep the two
    //! tables in sync (one entry per route/method registered in that fn).
    use super::*;
    use crate::claude::child::Program;
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::{Method, Request, StatusCode};
    use std::net::SocketAddr;
    use tower::ServiceExt;

    fn protected_routes() -> Vec<(Method, &'static str)> {
        vec![
            (Method::POST, "/api/auth/pair"),
            (Method::GET, "/api/net/interfaces"),
            (Method::GET, "/api/devices"),
            (Method::GET, "/api/events/devices"),
            (Method::POST, "/api/devices/anything/rename"),
            (Method::POST, "/api/devices/anything/revoke"),
            (Method::DELETE, "/api/devices/anything"),
            (Method::GET, "/api/sessions"),
            (Method::POST, "/api/sessions"),
            (Method::POST, "/api/sessions/scratch"),
            (Method::POST, "/api/sessions/anything/upload"),
            (Method::GET, "/api/usage"),
            (Method::GET, "/api/catalog"),
            (Method::GET, "/api/commands/recent"),
            (Method::GET, "/api/history/message"),
            (Method::GET, "/api/history/search"),
            (Method::GET, "/api/history/around"),
            (Method::GET, "/api/fs/list"),
            (Method::GET, "/api/fs/places"),
            (Method::GET, "/api/fs/file"),
            (Method::GET, "/api/fs/raw"),
            (Method::GET, "/api/fs/download"),
            (Method::GET, "/api/fs/hash"),
            (Method::POST, "/api/fs/kinds"),
            (Method::POST, "/api/fs/mkdir"),
            (Method::GET, "/api/git/root"),
            (Method::GET, "/api/git/diff"),
            (Method::GET, "/api/git/status"),
            (Method::GET, "/api/git/file"),
            (Method::GET, "/api/ws"),
            (Method::GET, "/api/version"),
            (Method::GET, "/api/server"),
            (Method::POST, "/api/server/restart"),
            (Method::POST, "/api/server/restart/cancel"),
            (Method::GET, "/api/update"),
            (Method::POST, "/api/update/check"),
            (Method::POST, "/api/update/run"),
            (Method::GET, "/api/push/config"),
            (Method::POST, "/api/push/subscribe"),
            (Method::POST, "/api/push/unsubscribe"),
            (Method::POST, "/api/push/test"),
        ]
    }

    fn public_routes() -> Vec<(Method, &'static str)> {
        vec![
            (Method::POST, "/api/auth/exchange"),
            (Method::GET, "/api/whoami"),
        ]
    }

    struct TestApp {
        tmp: tempfile::TempDir,
        router: Router,
        device_cookie: String,
        startup_token: String,
    }

    async fn build_test_app() -> TestApp {
        let tmp = tempfile::tempdir().expect("tempdir");
        let device_cookie = "test-device-cookie".to_string();
        let state = AppState::for_tests(tmp.path(), Program::claude(), &device_cookie, 0)
            .await
            .expect("test state");
        let web_dir = state.web_dir.as_ref();
        std::fs::create_dir_all(web_dir.join("assets")).expect("mkdir web");
        std::fs::write(web_dir.join("index.html"), "<!doctype html>").expect("write index");
        std::fs::write(web_dir.join("assets").join("app.js"), "").expect("write asset");
        let startup_token = state.auth.startup_token.clone();
        let router = build_app(state);
        TestApp {
            tmp,
            router,
            device_cookie,
            startup_token,
        }
    }

    /// Route tables list paths as `build_app` registers them, which is inside
    /// the nest and so without the prefix. Requests carry it.
    fn under_base(path: &str) -> String {
        format!("{BASE_PATH}{path}")
    }

    fn peer() -> ConnectInfo<SocketAddr> {
        ConnectInfo("127.0.0.1:9999".parse().expect("socket addr"))
    }

    async fn hit(router: &Router, method: Method, uri: &str) -> StatusCode {
        let mut req = Request::builder()
            .method(method)
            .uri(under_base(uri))
            .body(Body::empty())
            .expect("build request");
        // require_auth extracts ConnectInfo<SocketAddr>. In production this is
        // populated by `into_make_service_with_connect_info`; oneshot skips
        // that layer, so set it explicitly.
        req.extensions_mut().insert(peer());
        router.clone().oneshot(req).await.expect("oneshot").status()
    }

    async fn hit_with_cookie(router: &Router, method: Method, uri: &str, cookie: &str) -> Response {
        let mut req = Request::builder()
            .method(method)
            .uri(under_base(uri))
            .header("cookie", format!("{}={cookie}", auth::COOKIE_NAME))
            .body(Body::empty())
            .expect("build request");
        req.extensions_mut().insert(peer());
        router.clone().oneshot(req).await.expect("oneshot")
    }

    #[tokio::test]
    async fn protected_routes_return_401_without_cookie() {
        let app = build_test_app().await;
        for (method, path) in protected_routes() {
            let status = hit(&app.router, method.clone(), path).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "route {method} {path} did not require auth",
            );
        }
    }

    #[tokio::test]
    async fn protected_routes_return_401_with_bogus_cookie() {
        let app = build_test_app().await;
        for (method, path) in protected_routes() {
            let status = hit_with_cookie(&app.router, method.clone(), path, "not-a-real-device")
                .await
                .status();
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "route {method} {path} accepted a bogus cookie",
            );
        }
    }

    #[tokio::test]
    async fn protected_routes_pass_with_valid_cookie() {
        let app = build_test_app().await;
        for (method, path) in protected_routes() {
            let status = hit_with_cookie(&app.router, method.clone(), path, &app.device_cookie)
                .await
                .status();
            assert_ne!(
                status,
                StatusCode::UNAUTHORIZED,
                "route {method} {path} rejected a valid cookie (got 401)",
            );
        }
    }

    #[tokio::test]
    async fn download_serves_any_file_as_an_attachment() {
        let app = build_test_app().await;
        let file = app.tmp.path().join("notes.zip");
        std::fs::write(&file, b"zipped").expect("write");
        let encoded: String = file
            .to_str()
            .expect("utf-8 path")
            .bytes()
            .map(|byte| format!("%{byte:02X}"))
            .collect();
        let uri = format!("/api/fs/download?path={encoded}");
        let res = hit_with_cookie(&app.router, Method::GET, &uri, &app.device_cookie).await;

        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_DISPOSITION)
                .and_then(|value| value.to_str().ok()),
            Some("attachment; filename=\"notes.zip\"; filename*=UTF-8''notes.zip")
        );
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(&body[..], b"zipped");
    }

    /// The cookie `Max-Age` is the only expiry, so every authed response has to
    /// carry a fresh one or a device ages out while still in daily use.
    #[tokio::test]
    async fn an_authed_request_renews_the_session_cookie() {
        let app = build_test_app().await;
        let res =
            hit_with_cookie(&app.router, Method::GET, "/api/devices", &app.device_cookie).await;

        let renewed = res
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|v| v.starts_with(&format!("{}={}", auth::COOKIE_NAME, app.device_cookie)))
            .expect("authed response did not re-issue the session cookie");
        assert!(
            renewed.contains("Max-Age="),
            "renewed cookie carries no Max-Age: {renewed}",
        );
    }

    /// A browser holding the cookie at two paths sends both; the device list
    /// must mark the one that authenticated, whichever order they come in.
    #[tokio::test]
    async fn the_devices_list_marks_the_authenticated_device_among_two_cookies() {
        let app = build_test_app().await;
        let valid = &app.device_cookie;
        let name = auth::COOKIE_NAME;
        for header in [
            format!("{name}=stale; {name}={valid}"),
            format!("{name}={valid}; {name}=stale"),
        ] {
            let mut req = Request::builder()
                .uri(under_base("/api/devices"))
                .header("cookie", &header)
                .body(Body::empty())
                .expect("build request");
            req.extensions_mut().insert(peer());
            let res = app.router.clone().oneshot(req).await.expect("oneshot");
            assert_eq!(res.status(), StatusCode::OK, "cookies {header}");
            let body = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .expect("body");
            let list: serde_json::Value = serde_json::from_slice(&body).expect("json");
            let you: Vec<&str> = list["devices"]
                .as_array()
                .expect("devices")
                .iter()
                .filter(|device| device["is_you"] == true)
                .filter_map(|device| device["id"].as_str())
                .collect();
            assert_eq!(you, [valid.as_str()], "cookies {header}");
        }
    }

    async fn send(
        router: &Router,
        method: Method,
        uri: &str,
        cookie: &str,
        headers: &[(&str, &str)],
    ) -> Response {
        let mut builder = Request::builder()
            .method(method)
            .uri(under_base(uri))
            .header("cookie", format!("{}={cookie}", auth::COOKIE_NAME));
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let mut req = builder.body(Body::empty()).expect("build request");
        req.extensions_mut().insert(peer());
        router.clone().oneshot(req).await.expect("oneshot")
    }

    /// The SameSite cookie rides along on a form posted from another port of
    /// the same host, so a write is refused on its origin alone.
    #[tokio::test]
    async fn a_write_from_another_origin_is_refused_despite_a_valid_cookie() {
        let app = build_test_app().await;
        let cookie = &app.device_cookie;
        let evil = [("origin", "http://127.0.0.1:5173")];
        let refused = send(
            &app.router,
            Method::POST,
            "/api/server/restart/cancel",
            cookie,
            &evil,
        )
        .await;
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);

        let own = [("origin", "http://127.0.0.1:0")];
        let accepted = send(
            &app.router,
            Method::POST,
            "/api/server/restart/cancel",
            cookie,
            &own,
        )
        .await;
        assert_eq!(accepted.status(), StatusCode::NO_CONTENT);

        let read = send(&app.router, Method::GET, "/api/devices", cookie, &evil).await;
        assert_eq!(
            read.status(),
            StatusCode::OK,
            "a read was refused by origin"
        );
    }

    /// A rebound domain carries its own name as Host, on every route and the
    /// static files alike.
    #[tokio::test]
    async fn a_request_for_an_unknown_host_is_refused() {
        let app = build_test_app().await;
        let cookie = &app.device_cookie;
        for uri in ["/api/devices", "/api/whoami", "/assets/app.js"] {
            let rebound = send(
                &app.router,
                Method::GET,
                uri,
                cookie,
                &[("host", "evil.example:0")],
            )
            .await;
            assert_eq!(rebound.status(), StatusCode::FORBIDDEN, "{uri}");

            let own = send(
                &app.router,
                Method::GET,
                uri,
                cookie,
                &[("host", "127.0.0.1:0")],
            )
            .await;
            assert_eq!(own.status(), StatusCode::OK, "{uri}");
        }
    }

    #[tokio::test]
    async fn a_pair_token_dies_with_the_device_that_issued_it() {
        let app = build_test_app().await;
        let cookie = &app.device_cookie;
        let res = send(&app.router, Method::POST, "/api/auth/pair", cookie, &[]).await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .expect("body");
        let pair: serde_json::Value = serde_json::from_slice(&body).expect("json");
        let token = pair["token"].as_str().expect("token").to_string();

        let revoke = format!("/api/devices/{cookie}/revoke");
        let revoked = send(&app.router, Method::POST, &revoke, cookie, &[]).await;
        assert_eq!(revoked.status(), StatusCode::NO_CONTENT);

        assert_eq!(
            exchange(&app.router, &token).await,
            StatusCode::UNAUTHORIZED,
            "a revoked device's pair token paired a new device",
        );
    }

    async fn exchange(router: &Router, token: &str) -> StatusCode {
        let mut req = Request::builder()
            .method(Method::POST)
            .uri(under_base("/api/auth/exchange"))
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"token":"{token}"}}"#)))
            .expect("build request");
        req.extensions_mut().insert(peer());
        router.clone().oneshot(req).await.expect("oneshot").status()
    }

    /// A machine whose only device lost its cookie has no second device to pair
    /// from, so an already-populated registry must not block the launch token.
    #[tokio::test]
    async fn the_startup_token_pairs_despite_an_active_device() {
        let app = build_test_app().await;
        assert_eq!(
            exchange(&app.router, &app.startup_token).await,
            StatusCode::OK,
        );
        assert_eq!(
            exchange(&app.router, &app.startup_token).await,
            StatusCode::UNAUTHORIZED,
            "startup token paired a second device",
        );
    }

    /// Raw request, no `under_base`: these assert on the prefix itself.
    async fn get_raw(router: &Router, uri: &str) -> Response {
        let mut req = Request::builder()
            .method(Method::GET)
            .uri(uri)
            .body(Body::empty())
            .expect("build request");
        req.extensions_mut().insert(peer());
        router.clone().oneshot(req).await.expect("oneshot")
    }

    #[tokio::test]
    async fn nothing_is_served_at_the_root() {
        let app = build_test_app().await;
        for uri in ["/", "/assets/app.js", "/api/whoami"] {
            assert_eq!(
                get_raw(&app.router, uri).await.status(),
                StatusCode::NOT_FOUND,
                "{uri} answered outside the base path",
            );
        }
    }

    #[tokio::test]
    async fn the_bare_base_path_redirects_to_the_trailing_slash() {
        let app = build_test_app().await;
        let res = get_raw(&app.router, BASE_PATH).await;
        assert_eq!(res.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(
            res.headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok()),
            Some(BASE_PATH_SLASH),
        );
    }

    /// Nesting a router that answers static files from its fallback is the one
    /// piece of the base-path wiring that is not obvious from the axum API.
    #[tokio::test]
    async fn assets_are_served_under_the_base_path() {
        let app = build_test_app().await;
        let mut served = Vec::new();
        for uri in [BASE_PATH_SLASH, "/vigie/assets/app.js"] {
            served.push((uri, get_raw(&app.router, uri).await.status()));
        }
        let missing: Vec<_> = served
            .iter()
            .filter(|(_, s)| *s != StatusCode::OK)
            .collect();
        assert!(missing.is_empty(), "not served: {missing:?}");
    }

    #[tokio::test]
    async fn every_response_carries_the_security_headers() {
        let app = build_test_app().await;
        for uri in [BASE_PATH_SLASH, "/vigie/assets/app.js", "/vigie/api/whoami"] {
            let res = get_raw(&app.router, uri).await;
            let headers = res.headers();
            assert_eq!(
                headers.get(header::CONTENT_SECURITY_POLICY),
                Some(&HeaderValue::from_static(CONTENT_SECURITY_POLICY)),
                "{uri}",
            );
            assert_eq!(
                headers.get(header::X_CONTENT_TYPE_OPTIONS),
                Some(&HeaderValue::from_static("nosniff")),
                "{uri}",
            );
            assert!(headers.contains_key(header::X_FRAME_OPTIONS), "{uri}");
            assert!(headers.contains_key(header::REFERRER_POLICY), "{uri}");
        }
    }

    #[tokio::test]
    async fn public_routes_do_not_return_401() {
        let app = build_test_app().await;
        for (method, path) in public_routes() {
            let status = hit(&app.router, method.clone(), path).await;
            assert_ne!(
                status,
                StatusCode::UNAUTHORIZED,
                "public route {method} {path} returned 401 (should be reachable without cookie)",
            );
        }
    }
}
