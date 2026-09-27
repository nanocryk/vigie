use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tracing::{info, warn};

use vigie2::BASE_PATH_SLASH;
use vigie2::auth::{Auth, Registry, state_dir};
use vigie2::child::Program;
use vigie2::manager::SessionManager;
use vigie2::net;
use vigie2::server::{AppState, build_app};
use vigie2::store::Store;

const DEFAULT_BIND: &str = "127.0.0.1:7879";
/// Vite build output, relative to the working directory.
const WEB_DIST: &str = "web/dist";
/// Time open connections get to finish after Ctrl+C.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

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

    /// Additional origins to allow on the WebSocket upgrade. Repeatable.
    #[arg(long = "allow-origin")]
    allow_origin: Vec<String>,

    /// Skip printing the pair URL on startup.
    #[arg(long)]
    no_print_token: bool,
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
    #[serde(default)]
    allow_origins: Vec<String>,
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
    if args.allow_origin.is_empty() {
        args.allow_origin = config.allow_origins.clone();
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

    let store = Store::open(&Store::default_path()?)?;
    let manager = SessionManager::start(store, Program::claude()).await?;

    let state = AppState {
        auth: Arc::clone(&auth),
        pair_scheme: Arc::new(scheme.to_string()),
        pair_port: bind.port(),
        pair_extra_hosts: Arc::new(args.public_host.clone()),
        web_dir: Arc::new(web_dir),
        manager: manager.clone(),
        allowed_origins: Arc::new(allowed_origins(scheme, bind, &reachable_ips, &args)),
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

    let (stop_sender, stop) = watch::channel(false);
    tokio::spawn(async move {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {
                info!("shutting down");
                let _ = stop_sender.send(true);
            }
            Err(err) => {
                warn!(%err, "cannot listen for Ctrl+C");
                // Keeps the sender alive, so the server is never told to stop.
                std::future::pending::<()>().await;
            }
        }
    });

    let served: Result<()> = if tls_on {
        let (cert_path, key_path) = resolve_tls(&args, bind, &dir, &sans_extra)?;
        let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert_path, &key_path)
            .await
            .with_context(|| format!("load TLS from {}", cert_path.display()))?;
        let handle = axum_server::Handle::new();
        tokio::spawn({
            let handle = handle.clone();
            let stop = stop.clone();
            async move {
                stopped(stop).await;
                handle.graceful_shutdown(Some(SHUTDOWN_GRACE));
            }
        });
        axum_server::bind_rustls(bind, config)
            .handle(handle)
            .serve(app.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .map_err(Into::into)
    } else {
        let listener = tokio::net::TcpListener::bind(bind).await?;
        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(stopped(stop.clone()));
        // Graceful shutdown waits for every connection, open WebSockets
        // included, so it gets a deadline.
        tokio::select! {
            served = server => served.map_err(Into::into),
            () = async {
                stopped(stop.clone()).await;
                tokio::time::sleep(SHUTDOWN_GRACE).await;
            } => Ok(()),
        }
    };
    if let Err(err) = manager.shutdown().await {
        warn!("session shutdown: {err:#}");
    }
    served
}

async fn stopped(mut stop: watch::Receiver<bool>) {
    let _ = stop.wait_for(|stop| *stop).await;
}

/// Origins a WebSocket upgrade is accepted from: the bind address, loopback,
/// every reachable interface, the public hosts, and `--allow-origin`.
fn allowed_origins(
    scheme: &str,
    bind: SocketAddr,
    reachable_ips: &[IpAddr],
    args: &ServeArgs,
) -> BTreeSet<String> {
    let port = bind.port();
    let mut allowed: BTreeSet<String> = [
        format!("{scheme}://{bind}"),
        format!("{scheme}://127.0.0.1:{port}"),
        format!("{scheme}://localhost:{port}"),
    ]
    .into_iter()
    .collect();
    for ip in reachable_ips {
        allowed.insert(format!("{scheme}://{ip}:{port}"));
    }
    for origin in &args.allow_origin {
        allowed.insert(origin.trim_end_matches('/').to_string());
    }
    for host in &args.public_host {
        if host.contains(':') {
            allowed.insert(format!("{scheme}://{host}"));
        } else {
            allowed.insert(format!("{scheme}://{host}:{port}"));
        }
    }
    allowed
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
    for origin in &args.allow_origin {
        if let Some(host) = origin_host(origin) {
            desired.insert(host);
        }
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
