mod auth;
mod net;

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    Router,
    extract::Request,
    http::{HeaderValue, header},
    middleware::{Next, from_fn, from_fn_with_state},
    response::{IntoResponse, Redirect, Response},
    routing::{delete, get, post},
};
use clap::{Args, Parser, Subcommand};
use serde::{Deserialize, Serialize};
use tower_http::services::{ServeDir, ServeFile};
use tracing::{info, warn};

use crate::auth::{Auth, Registry, state_dir};

/// Path prefix everything is served under. Android builds a WebAPK's intent
/// filter from scheme, host and path, never the port, so two apps behind one
/// hostname collide at the root and only one can install.
pub const BASE_PATH: &str = "/vigie2";
/// `BASE_PATH` with the trailing slash, which is the form the app document is
/// actually served at.
pub const BASE_PATH_SLASH: &str = "/vigie2/";

const DEFAULT_BIND: &str = "127.0.0.1:7879";
/// Vite build output, relative to the working directory.
const WEB_DIST: &str = "web/dist";

#[derive(Parser, Debug)]
#[command(
    name = "vigie2",
    about = "Phone-reachable chat UI driving Claude Code headlessly"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the server.
    Serve(ServeArgs),
    /// Manage authentication and devices.
    Auth {
        #[command(subcommand)]
        cmd: AuthCmd,
    },
}

#[derive(Subcommand, Debug)]
enum AuthCmd {
    /// Revoke every paired device (panic button).
    RevokeAll,
}

#[derive(Args, Debug)]
struct ServeArgs {
    /// Address to bind the HTTPS server to. Defaults to config.bind then
    /// 127.0.0.1:7879.
    #[arg(long)]
    bind: Option<SocketAddr>,

    /// Caller-supplied TLS certificate (PEM). Requires --key.
    #[arg(long)]
    cert: Option<PathBuf>,

    /// Caller-supplied TLS private key (PEM). Requires --cert.
    #[arg(long)]
    key: Option<PathBuf>,

    /// Disable TLS. Refuses to start on a non-loopback bind.
    #[arg(long)]
    http: bool,

    /// Additional host[:port] entries to offer in the pair URL list. Repeatable.
    #[arg(long = "public-host")]
    public_host: Vec<String>,

    /// Skip printing the pair URL on startup.
    #[arg(long)]
    no_print_token: bool,
}

#[derive(Clone)]
pub struct AppState {
    pub auth: Arc<Auth>,
    pub pair_scheme: Arc<String>,
    pub pair_port: u16,
    pub pair_extra_hosts: Arc<Vec<String>>,
    pub web_dir: Arc<PathBuf>,
}

#[derive(Serialize, Deserialize, Default)]
struct ServerConfig {
    #[serde(default)]
    bind: Option<SocketAddr>,
    #[serde(default)]
    cert: Option<PathBuf>,
    #[serde(default)]
    key: Option<PathBuf>,
    #[serde(default)]
    public_hosts: Vec<String>,
}

fn load_config(path: &Path) -> Result<ServerConfig> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).unwrap_or_default()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(ServerConfig::default()),
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("install ring CryptoProvider");
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "vigie2=info,tower_http=info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Serve(args) => serve(args).await,
        Command::Auth {
            cmd: AuthCmd::RevokeAll,
        } => run_revoke_all(),
    }
}

fn run_revoke_all() -> Result<()> {
    let dir = state_dir()?;
    let registry = Registry::load(dir.join("devices.json"))?;
    let count = registry.revoke_all()?;
    println!("Revoked {count} device(s).");
    Ok(())
}

async fn serve(mut args: ServeArgs) -> Result<()> {
    let dir = state_dir()?;
    let config = load_config(&dir.join("config.json"))?;

    let bind: SocketAddr = args
        .bind
        .or(config.bind)
        .unwrap_or_else(|| DEFAULT_BIND.parse().expect("valid default bind"));

    if args.http && !is_loopback(&bind) {
        anyhow::bail!(
            "--http refused on non-loopback bind {bind}: vigie2 never carries session data over plaintext off-loopback",
        );
    }

    if args.cert.is_none() && config.cert.is_some() {
        args.cert = config.cert.clone();
    }
    if args.key.is_none() && config.key.is_some() {
        args.key = config.key.clone();
    }
    if args.cert.is_some() != args.key.is_some() {
        anyhow::bail!(
            "cert and key must be provided together (either both flags or both in config.json)"
        );
    }
    if args.public_host.is_empty() {
        args.public_host = config.public_hosts.clone();
    }
    for h in &mut args.public_host {
        *h = strip_url_scheme(h).trim_end_matches('/').to_string();
    }

    let web_dir = PathBuf::from(WEB_DIST);
    if !web_dir.join("index.html").is_file() {
        warn!(dir = %web_dir.display(), "no index.html in the web build; run the web build first");
    }

    let devices_path = dir.join("devices.json");
    let registry = Registry::load(devices_path.clone())?;
    let tls_on = !args.http;
    let auth = Arc::new(Auth::new(registry, tls_on));

    // `vigie2 auth revoke-all` rewrites devices.json from a second process.
    // Reloading here is what makes the panic button reach a running server.
    let _registry_watcher = spawn_file_watcher(&devices_path, "devices", {
        let auth = Arc::clone(&auth);
        move || match auth.registry.reload() {
            Ok(()) => {
                info!("device registry reloaded from disk");
                auth.notify_changed();
            }
            Err(err) => warn!(%err, "device registry reload failed"),
        }
    });

    let reachable_ips = net::all_reachable_ipv4();
    info!(interfaces = ?net::enumerate(), "network interfaces");

    let scheme = if tls_on { "https" } else { "http" };
    let loopback_origin = format!("{scheme}://127.0.0.1:{}", bind.port());

    let state = AppState {
        auth: Arc::clone(&auth),
        pair_scheme: Arc::new(scheme.to_string()),
        pair_port: bind.port(),
        pair_extra_hosts: Arc::new(args.public_host.clone()),
        web_dir: Arc::new(web_dir),
    };

    let app = build_app(state);

    if !args.no_print_token {
        eprintln!("\nvigie2 listening on {scheme}://{bind}{BASE_PATH_SLASH}");
        eprintln!("Pair URL (single use, valid until this process exits):");
        eprintln!(
            "  {loopback_origin}{BASE_PATH_SLASH}#auth={}",
            auth.startup_token
        );
        for host in &args.public_host {
            eprintln!(
                "  {scheme}://{host}{BASE_PATH_SLASH}#auth={}",
                auth.startup_token
            );
        }
        eprintln!();
        if !reachable_ips.is_empty() && !is_loopback(&bind) {
            eprintln!("Reachable interfaces:");
            for ip in &reachable_ips {
                eprintln!("  {scheme}://{ip}:{}{BASE_PATH_SLASH}", bind.port());
            }
            eprintln!();
        }
        if !tls_on {
            eprintln!("WARNING: --http is set; traffic is plaintext.\n");
        }
    }

    let sans_extra: Vec<String> = reachable_ips.iter().map(|ip| ip.to_string()).collect();

    if tls_on {
        let (cert_path, key_path) = resolve_tls(&args, bind, &dir, &sans_extra)?;
        let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert_path, &key_path)
            .await
            .with_context(|| format!("load TLS from {}", cert_path.display()))?;
        axum_server::bind_rustls(bind, config)
            .serve(app.into_make_service_with_connect_info::<SocketAddr>())
            .await?;
    } else {
        let listener = tokio::net::TcpListener::bind(bind).await?;
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await?;
    }
    Ok(())
}

async fn no_store(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

fn is_loopback(addr: &SocketAddr) -> bool {
    addr.ip().is_loopback()
}

/// Watch a single file for external mutation, running `on_change` on every
/// event whose path matches. `devices.json` is rewritten by
/// `vigie2 auth revoke-all` from a second process while a server is up.
///
/// Returned watcher must be kept alive for the process lifetime; dropping it
/// stops the watch.
fn spawn_file_watcher<F>(
    path: &Path,
    label: &'static str,
    mut on_change: F,
) -> Option<notify::RecommendedWatcher>
where
    F: FnMut() + Send + 'static,
{
    use notify::{RecursiveMode, Watcher};

    let path = path.to_path_buf();
    let Some(watch_dir) = path.parent().map(Path::to_path_buf) else {
        warn!(
            label,
            "watched path has no parent; skipping hot-reload watcher"
        );
        return None;
    };
    if let Err(err) = std::fs::create_dir_all(&watch_dir) {
        warn!(%err, label, dir = %watch_dir.display(), "cannot create dir; skipping watcher");
        return None;
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let target = path.clone();
    let watcher_result = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        if event.paths.iter().any(|p| p == &target) {
            let _ = tx.send(());
        }
    });
    let mut watcher = match watcher_result {
        Ok(w) => w,
        Err(err) => {
            warn!(%err, label, "watcher: build failed");
            return None;
        }
    };
    if let Err(err) = watcher.watch(&watch_dir, RecursiveMode::NonRecursive) {
        warn!(%err, label, dir = %watch_dir.display(), "watcher: watch failed");
        return None;
    }

    tokio::spawn(async move {
        while let Some(()) = rx.recv().await {
            on_change();
        }
    });

    info!(label, path = %path.display(), "file watcher started");
    Some(watcher)
}

fn resolve_tls(
    args: &ServeArgs,
    bind: SocketAddr,
    state_dir: &Path,
    extra_sans: &[String],
) -> Result<(PathBuf, PathBuf)> {
    if let (Some(cert), Some(key)) = (args.cert.clone(), args.key.clone()) {
        return Ok((cert, key));
    }
    let tls_dir = state_dir.join("tls");
    std::fs::create_dir_all(&tls_dir).with_context(|| format!("mkdir {}", tls_dir.display()))?;
    let cert_path = tls_dir.join("cert.pem");
    let key_path = tls_dir.join("key.pem");
    let sans_path = tls_dir.join("sans.json");

    let mut desired: BTreeSet<String> = BTreeSet::new();
    desired.insert("localhost".to_string());
    desired.insert("127.0.0.1".to_string());
    desired.insert("::1".to_string());
    desired.insert(bind.ip().to_string());
    for san in extra_sans {
        desired.insert(san.clone());
    }
    for host in &args.public_host {
        if let Some(host) = origin_host(host) {
            desired.insert(host);
        }
    }

    let cached: Option<BTreeSet<String>> = std::fs::read(&sans_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());

    let need_regen =
        !cert_path.exists() || !key_path.exists() || cached.as_ref().is_none_or(|c| c != &desired);

    if need_regen {
        let names: Vec<String> = desired.iter().cloned().collect();
        info!(sans = ?names, "generating self-signed TLS cert");
        let generated = rcgen::generate_simple_self_signed(names).context("rcgen")?;
        std::fs::write(&cert_path, generated.cert.pem())?;
        std::fs::write(&key_path, generated.signing_key.serialize_pem())?;
        std::fs::write(&sans_path, serde_json::to_vec_pretty(&desired)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok((cert_path, key_path))
}

fn strip_url_scheme(s: &str) -> &str {
    s.strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s)
}

fn origin_host(origin: &str) -> Option<String> {
    let rest = strip_url_scheme(origin);
    let host = rest.split('/').next()?;
    let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
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

/// Send `/vigie2` to `/vigie2/`. The document addresses its assets relative to
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
mod tests {
    use super::*;

    #[test]
    fn strip_url_scheme_drops_http_and_https() {
        assert_eq!(strip_url_scheme("http://host"), "host");
        assert_eq!(strip_url_scheme("https://host:8080/x"), "host:8080/x");
    }

    #[test]
    fn strip_url_scheme_leaves_other_input_alone() {
        assert_eq!(strip_url_scheme("host:8080"), "host:8080");
        assert_eq!(strip_url_scheme("ftp://host"), "ftp://host");
        assert_eq!(strip_url_scheme(""), "");
    }

    #[test]
    fn origin_host_strips_scheme_port_and_path() {
        assert_eq!(
            origin_host("https://example.com:8443/path"),
            Some("example.com".into())
        );
        assert_eq!(
            origin_host("http://127.0.0.1:7879"),
            Some("127.0.0.1".into())
        );
    }

    #[test]
    fn origin_host_bare_host_without_scheme() {
        assert_eq!(origin_host("example.com"), Some("example.com".into()));
        assert_eq!(origin_host("host:9000"), Some("host".into()));
    }

    #[test]
    fn origin_host_empty_returns_none() {
        assert_eq!(origin_host(""), None);
        assert_eq!(origin_host("http://"), None);
    }
}

#[cfg(test)]
mod auth_gate_tests {
    //! Verify every route registered in `build_app` is on the correct side of
    //! the `require_auth` middleware. Adding a route to `build_app` without
    //! adding it here means the coverage silently gaps, so keep the two
    //! tables in sync (one entry per route/method registered in that fn).
    use super::*;
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

    fn build_test_app() -> TestApp {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_path_buf();
        let registry = Registry::load(dir.join("devices.json")).expect("registry load");
        let device_cookie = "test-device-cookie".to_string();
        let now = chrono::Utc::now();
        registry
            .insert(auth::Device {
                id: device_cookie.clone(),
                label: "test".into(),
                issued_at: now,
                last_seen: now,
                revoked: false,
                last_ip: None,
            })
            .expect("insert device");
        let web_dir = dir.join("web");
        std::fs::create_dir_all(web_dir.join("assets")).expect("mkdir web");
        std::fs::write(web_dir.join("index.html"), "<!doctype html>").expect("write index");
        std::fs::write(web_dir.join("assets").join("app.js"), "").expect("write asset");
        let auth = Arc::new(Auth::new(registry, false));
        let startup_token = auth.startup_token.clone();
        let state = AppState {
            auth,
            pair_scheme: Arc::new("http".into()),
            pair_port: 0,
            pair_extra_hosts: Arc::new(vec![]),
            web_dir: Arc::new(web_dir),
        };
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
        let app = build_test_app();
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
        let app = build_test_app();
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
        let app = build_test_app();
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
        let app = build_test_app();
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
        let app = build_test_app();
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
        let app = build_test_app();
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
        let app = build_test_app();
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
        let app = build_test_app();
        let mut served = Vec::new();
        for uri in [BASE_PATH_SLASH, "/vigie2/assets/app.js"] {
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
        let app = build_test_app();
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
