//! The HTTP router and the state its handlers share.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Router,
    extract::{DefaultBodyLimit, Request},
    http::{HeaderValue, header},
    middleware::{Next, from_fn, from_fn_with_state},
    response::{IntoResponse, Redirect, Response},
    routing::{delete, get, post},
};
use tower_http::services::{ServeDir, ServeFile};

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
}

#[cfg(any(test, feature = "test-support"))]
impl AppState {
    /// A state kept in `dir` that runs `program`, with `device` paired, served
    /// over plain HTTP on `port` to `http://127.0.0.1:{port}`.
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
            chrono::Utc::now(),
        ))?;
        let auth = Arc::new(Auth::new(registry, false));
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
            updater: Arc::new(Updater::new(dir.to_path_buf())),
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
        .route("/api/fs/list", get(files::list_route))
        .route("/api/fs/places", get(files::places_route))
        .route("/api/fs/file", get(files::file_route))
        .route("/api/fs/raw", get(files::raw_route))
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
        .layer(from_fn(no_store))
        .layer(from_fn(redirect_bare_base))
}

async fn no_store(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
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
            (Method::POST, "/api/sessions/anything/upload"),
            (Method::GET, "/api/usage"),
            (Method::GET, "/api/catalog"),
            (Method::GET, "/api/commands/recent"),
            (Method::GET, "/api/fs/list"),
            (Method::GET, "/api/fs/places"),
            (Method::GET, "/api/fs/file"),
            (Method::GET, "/api/fs/raw"),
            (Method::POST, "/api/fs/mkdir"),
            (Method::GET, "/api/git/root"),
            (Method::GET, "/api/git/diff"),
            (Method::GET, "/api/git/status"),
            (Method::GET, "/api/git/file"),
            (Method::GET, "/api/ws"),
            (Method::GET, "/api/version"),
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
        _tmp: tempfile::TempDir,
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
            _tmp: tmp,
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
