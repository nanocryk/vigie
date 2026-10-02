use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tracing::{info, warn};

use vigie::BASE_PATH_SLASH;
use vigie::claude::child::Program;
use vigie::http::auth::{Auth, Registry};
use vigie::http::net;
use vigie::http::push::{self, Pusher};
use vigie::http::server::{AppState, build_app};
use vigie::http::update::{self, Updater};
use vigie::session::manager::{Probe, SessionManager};
use vigie::session::server_restart::{
    self, RestartState, RunningBuild, ServerRestart, Supervision,
};
use vigie::state_dir;
use vigie::store::Store;

const DEFAULT_BIND: &str = "127.0.0.1:7879";
/// Vite build output, relative to the working directory.
const WEB_DIST: &str = "web/dist";
/// Time open connections get to finish after Ctrl+C.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

#[derive(Parser, Debug)]
#[command(
    name = "vigie",
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
    /// Prune stored history nothing reads anymore.
    Prune {
        #[command(subcommand)]
        what: PruneCmd,
    },
}

#[derive(Subcommand, Debug)]
enum AuthCmd {
    /// Revoke every paired device (panic button).
    RevokeAll,
}

#[derive(Subcommand, Debug)]
enum PruneCmd {
    /// Delete the stream lines of messages their assistant lines fully carry.
    Streams(PruneArgs),
    /// Replace tool outputs over 1 KB older than a day by a marker.
    ToolOutputs(PruneArgs),
}

#[derive(Args, Debug)]
struct PruneArgs {
    /// History database to prune; the live one is refused.
    #[arg(long)]
    db: PathBuf,
    /// Count what would be deleted, delete nothing.
    #[arg(long)]
    dry_run: bool,
    /// Rebuild the file afterwards, giving the freed space back.
    #[arg(long)]
    vacuum: bool,
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
        Ok(bytes) => {
            serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(ServerConfig::default()),
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }
}

/// How the server speaks TLS.
enum Tls {
    /// `--http`: plaintext, loopback only.
    Off,
    /// A certificate generated and kept in the state directory.
    SelfSigned,
    Provided {
        cert: PathBuf,
        key: PathBuf,
    },
}

/// `ServeArgs` over `config.json`: a flag wins over the config, which wins over the default.
struct Settings {
    bind: SocketAddr,
    tls: Tls,
    /// `host[:port]`, without scheme or trailing slash.
    public_hosts: Vec<String>,
    allow_origins: Vec<String>,
    print_token: bool,
}

impl Settings {
    fn resolve(args: ServeArgs, config: ServerConfig) -> Result<Self> {
        let bind = args
            .bind
            .or(config.bind)
            .unwrap_or_else(|| DEFAULT_BIND.parse().expect("valid default bind"));
        if args.http && !bind.ip().is_loopback() {
            bail!(
                "--http refused on non-loopback bind {bind}: vigie never carries session data over plaintext off-loopback",
            );
        }
        let provided = match (args.cert.or(config.cert), args.key.or(config.key)) {
            (Some(cert), Some(key)) => Some(Tls::Provided { cert, key }),
            (None, None) => None,
            _ => bail!(
                "cert and key must be provided together (either both flags or both in config.json)"
            ),
        };
        let tls = if args.http {
            Tls::Off
        } else {
            provided.unwrap_or(Tls::SelfSigned)
        };
        let public_hosts = if args.public_host.is_empty() {
            config.public_hosts
        } else {
            args.public_host
        };
        let allow_origins = if args.allow_origin.is_empty() {
            config.allow_origins
        } else {
            args.allow_origin
        };
        Ok(Self {
            bind,
            tls,
            public_hosts: public_hosts
                .iter()
                .map(|host| strip_url_scheme(host).trim_end_matches('/').to_string())
                .collect(),
            allow_origins,
            print_token: !args.no_print_token,
        })
    }

    fn tls_on(&self) -> bool {
        !matches!(self.tls, Tls::Off)
    }

    fn scheme(&self) -> &'static str {
        if self.tls_on() { "https" } else { "http" }
    }
}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("install ring CryptoProvider");
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "vigie=info,tower_http=info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Serve(args) => serve(args).await,
        Command::Auth {
            cmd: AuthCmd::RevokeAll,
        } => run_revoke_all(),
        Command::Prune { what } => run_prune(what),
    }
}

fn run_prune(what: PruneCmd) -> Result<ExitCode> {
    use vigie::store::prune::{self, Mode};

    let (PruneCmd::Streams(args) | PruneCmd::ToolOutputs(args)) = &what;
    let live = Store::default_path()?;
    if live.exists() && std::fs::canonicalize(&args.db)? == std::fs::canonicalize(&live)? {
        bail!("{} is the live history; prune a copy", args.db.display());
    }
    if !args.db.is_file() {
        bail!("{} is not a file", args.db.display());
    }
    let mode = if args.dry_run {
        Mode::DryRun
    } else {
        Mode::Delete
    };
    let file_mb =
        |path: &Path| -> Result<f64> { Ok(std::fs::metadata(path)?.len() as f64 / 1_048_576.0) };
    let started = std::time::Instant::now();
    let size_before = file_mb(&args.db)?;
    let connection = rusqlite::Connection::open(&args.db)?;
    let before = prune::event_counts(&connection)?;
    let mb = |bytes: u64| bytes as f64 / 1_048_576.0;
    match &what {
        PruneCmd::Streams(_) => {
            let report = prune::prune_all_streams(&connection, mode)?;
            let verb = match mode {
                Mode::DryRun => "Would delete",
                Mode::Delete => "Deleted",
            };
            println!(
                "{verb} the stream lines of {} messages: {} rows, {:.1} MB of payload.",
                report.messages,
                report.rows,
                mb(report.bytes)
            );
            println!(
                "Kept whole: {} messages; {} stray rows.",
                report.kept.values().sum::<usize>(),
                report.stray_rows
            );
            let mut reasons: Vec<_> = report.kept.iter().collect();
            reasons.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
            for (reason, count) in reasons {
                let (session, message) = &report.examples[reason];
                println!("  {count:>5}  {reason}  [e.g. {session} {message}]");
            }
        }
        PruneCmd::ToolOutputs(_) => {
            let cutoff = chrono::Utc::now() - chrono::Duration::days(1);
            let transaction = connection.unchecked_transaction()?;
            let report = prune::prune_tool_results(&transaction, cutoff)?;
            let verb = match mode {
                Mode::DryRun => "Would replace",
                Mode::Delete => "Replaced",
            };
            println!(
                "{verb} {} tool output copies in {} lines before {cutoff}: \
                 {:.1} MB -> {:.1} MB of payload.",
                report.results,
                report.lines,
                mb(report.bytes_before),
                mb(report.bytes_after)
            );
            match mode {
                Mode::DryRun => transaction.rollback()?,
                Mode::Delete => transaction.commit()?,
            }
        }
    }
    if mode == Mode::Delete {
        let after = prune::event_counts(&connection)?;
        let mut unchanged = true;
        for name in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
            let (old, new) = (
                before.get(name).copied().unwrap_or(0),
                after.get(name).copied().unwrap_or(0),
            );
            if old != new {
                unchanged = false;
                println!("  {name}: {old} -> {new}");
            }
        }
        if unchanged {
            println!("  no row count changed");
        }
        if args.vacuum {
            connection.execute_batch("VACUUM")?;
        }
    }
    drop(connection);
    println!(
        "File: {size_before:.1} MB -> {:.1} MB, in {:.1}s.",
        file_mb(&args.db)?,
        started.elapsed().as_secs_f64()
    );
    Ok(ExitCode::SUCCESS)
}

fn run_revoke_all() -> Result<ExitCode> {
    let registry = Registry::load(state_dir::path()?.join("devices.json"))?;
    let count = registry.revoke_all()?;
    println!("Revoked {count} device(s).");
    Ok(ExitCode::SUCCESS)
}

async fn serve(args: ServeArgs) -> Result<ExitCode> {
    let dir = state_dir::path()?;
    let settings = Settings::resolve(args, load_config(&dir.join("config.json"))?)?;
    let bind = settings.bind;
    let scheme = settings.scheme();

    let web_dir = PathBuf::from(WEB_DIST);
    if !web_dir.join("index.html").is_file() {
        warn!(dir = %web_dir.display(), "no index.html in the web build; run the web build first");
    }

    let devices_path = dir.join("devices.json");
    let registry = Registry::load(devices_path.clone())?;
    let auth = Arc::new(Auth::new(registry, settings.tls_on()));

    // `vigie auth revoke-all` rewrites devices.json from a second process.
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

    let store_path = Store::default_path()?;
    let store = Store::open(&store_path)?;
    let restart =
        ServerRestart::new(Supervision::from_env()).with_running_build(RunningBuild::from_env());
    let manager = SessionManager::start(store, Program::claude(), &dir, restart).await?;
    let probe_dir = store_path.with_file_name("probe");
    tokio::spawn({
        let manager = manager.clone();
        async move {
            match manager.probe_catalog(probe_dir).await {
                Ok(Probe::Ran) => info!("catalog probe stored the shared catalog"),
                Ok(Probe::Skipped) => {}
                Err(err) => warn!("catalog probe: {err:#}"),
            }
        }
    });

    let vapid = push::Vapid::load_or_generate(&dir)?;
    let pusher = Arc::new(Pusher::new(
        vapid,
        manager.store().clone(),
        Arc::clone(&auth),
    )?);
    push::spawn_dispatcher(&manager, Arc::clone(&pusher));
    let updater = Arc::new(Updater::new(std::env::current_dir()?));
    update::spawn_checks(Arc::clone(&updater));

    let state = AppState {
        pusher,
        auth: Arc::clone(&auth),
        pair_scheme: Arc::new(scheme.to_string()),
        pair_port: bind.port(),
        pair_extra_hosts: Arc::new(settings.public_hosts.clone()),
        web_dir: Arc::new(web_dir),
        manager: manager.clone(),
        allowed_origins: Arc::new(allowed_origins(&settings, &reachable_ips)),
        updater,
    };

    let app = build_app(state);

    if settings.print_token {
        let loopback_origin = format!("{scheme}://127.0.0.1:{}", bind.port());
        eprintln!("\nvigie listening on {scheme}://{bind}{BASE_PATH_SLASH}");
        eprintln!("Pair URL (single use, valid until this process exits):");
        eprintln!(
            "  {loopback_origin}{BASE_PATH_SLASH}#auth={}",
            auth.startup_token
        );
        for host in &settings.public_hosts {
            eprintln!(
                "  {}{BASE_PATH_SLASH}#auth={}",
                net::origin(scheme, host, bind.port()),
                auth.startup_token
            );
        }
        eprintln!();
        if !reachable_ips.is_empty() && !bind.ip().is_loopback() {
            eprintln!("Reachable interfaces:");
            for ip in &reachable_ips {
                eprintln!("  {scheme}://{ip}:{}{BASE_PATH_SLASH}", bind.port());
            }
            eprintln!();
        }
        if !settings.tls_on() {
            eprintln!("WARNING: --http is set; traffic is plaintext.\n");
        }
    }

    let (stop_sender, stop) = watch::channel(false);
    let mut restart_state = manager.server_restart().subscribe();
    tokio::spawn(async move {
        let interrupted = async {
            match tokio::signal::ctrl_c().await {
                Ok(()) => info!("shutting down"),
                Err(err) => {
                    warn!(%err, "cannot listen for Ctrl+C");
                    std::future::pending::<()>().await;
                }
            }
        };
        let restarting = async {
            let requested = restart_state
                .wait_for(|state| *state == RestartState::Now)
                .await
                .is_ok();
            if requested {
                info!("restarting");
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            () = interrupted => {}
            () = restarting => {}
        }
        let _ = stop_sender.send(true);
    });

    let served: Result<()> = if settings.tls_on() {
        let extra_sans: Vec<String> = reachable_ips.iter().map(|ip| ip.to_string()).collect();
        let (cert_path, key_path) = resolve_tls(&settings, &dir, &extra_sans)?;
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
    served?;
    Ok(match manager.server_restart().state() {
        RestartState::Now => ExitCode::from(server_restart::EXIT_CODE),
        RestartState::Idle | RestartState::Pending => ExitCode::SUCCESS,
    })
}

async fn stopped(mut stop: watch::Receiver<bool>) {
    let _ = stop.wait_for(|stop| *stop).await;
}

/// Origins a WebSocket upgrade is accepted from: the bind address, loopback,
/// every reachable interface, the public hosts, and `--allow-origin`.
fn allowed_origins(settings: &Settings, reachable_ips: &[IpAddr]) -> BTreeSet<String> {
    let (scheme, bind) = (settings.scheme(), settings.bind);
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
    for origin in &settings.allow_origins {
        allowed.insert(origin.trim_end_matches('/').to_string());
    }
    for host in &settings.public_hosts {
        allowed.insert(net::origin(scheme, host, port));
    }
    allowed
}

/// Watch a single file for external mutation, running `on_change` on every
/// event whose path matches. `devices.json` is rewritten by
/// `vigie auth revoke-all` from a second process while a server is up.
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

    let (changed_sender, mut changed) = tokio::sync::mpsc::unbounded_channel::<()>();
    let target = path.clone();
    let watcher_result =
        notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
            let Ok(event) = result else { return };
            if event.paths.iter().any(|event_path| event_path == &target) {
                let _ = changed_sender.send(());
            }
        });
    let mut watcher = match watcher_result {
        Ok(watcher) => watcher,
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
        while let Some(()) = changed.recv().await {
            on_change();
        }
    });

    info!(label, path = %path.display(), "file watcher started");
    Some(watcher)
}

/// The certificate and key to serve: the provided ones, or a self-signed pair
/// regenerated whenever the names it must cover change.
fn resolve_tls(
    settings: &Settings,
    state_dir: &Path,
    extra_sans: &[String],
) -> Result<(PathBuf, PathBuf)> {
    if let Tls::Provided { cert, key } = &settings.tls {
        return Ok((cert.clone(), key.clone()));
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
    desired.insert(settings.bind.ip().to_string());
    for san in extra_sans {
        desired.insert(san.clone());
    }
    for origin in &settings.allow_origins {
        if let Some(host) = origin_host(origin) {
            desired.insert(host);
        }
    }
    for host in &settings.public_hosts {
        if let Some(host) = origin_host(host) {
            desired.insert(host);
        }
    }

    let cached: Option<BTreeSet<String>> = std::fs::read(&sans_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());

    let need_regen = !cert_path.exists()
        || !key_path.exists()
        || cached.as_ref().is_none_or(|cached| cached != &desired);

    if need_regen {
        let names: Vec<String> = desired.iter().cloned().collect();
        info!(sans = ?names, "generating self-signed TLS cert");
        let generated = rcgen::generate_simple_self_signed(names).context("rcgen")?;
        std::fs::write(&cert_path, generated.cert.pem())?;
        state_dir::write_private(&key_path, generated.signing_key.serialize_pem())?;
        std::fs::write(&sans_path, serde_json::to_vec_pretty(&desired)?)?;
    }
    Ok((cert_path, key_path))
}

fn strip_url_scheme(text: &str) -> &str {
    text.strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"))
        .unwrap_or(text)
}

fn origin_host(origin: &str) -> Option<String> {
    let rest = strip_url_scheme(origin);
    let host = rest.split('/').next()?;
    let host = host.rsplit_once(':').map_or(host, |(host, _)| host);
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

    fn args() -> ServeArgs {
        ServeArgs {
            bind: None,
            cert: None,
            key: None,
            http: false,
            public_host: Vec::new(),
            allow_origin: Vec::new(),
            no_print_token: false,
        }
    }

    #[test]
    fn flags_win_over_the_config_and_hosts_lose_their_scheme() {
        let config = ServerConfig {
            bind: Some("0.0.0.0:9000".parse().unwrap()),
            cert: Some("config.pem".into()),
            key: Some("config.key".into()),
            public_hosts: vec!["https://config.example/".into()],
            allow_origins: vec!["https://config.example".into()],
        };
        let settings = Settings::resolve(
            ServeArgs {
                public_host: vec!["https://flag.example:8443/".into()],
                ..args()
            },
            config,
        )
        .unwrap();
        assert_eq!(settings.bind, "0.0.0.0:9000".parse().unwrap());
        assert!(
            matches!(&settings.tls, Tls::Provided { cert, .. } if cert == Path::new("config.pem"))
        );
        assert_eq!(settings.public_hosts, ["flag.example:8443"]);
        assert_eq!(settings.allow_origins, ["https://config.example"]);
    }

    #[test]
    fn settings_refuse_plaintext_off_loopback_and_a_lone_cert() {
        let plaintext = ServeArgs {
            bind: Some("0.0.0.0:7879".parse().unwrap()),
            http: true,
            ..args()
        };
        assert!(Settings::resolve(plaintext, ServerConfig::default()).is_err());
        let lone_cert = ServeArgs {
            cert: Some("cert.pem".into()),
            ..args()
        };
        assert!(Settings::resolve(lone_cert, ServerConfig::default()).is_err());
        let loopback = Settings::resolve(
            ServeArgs {
                http: true,
                ..args()
            },
            ServerConfig::default(),
        )
        .unwrap();
        assert_eq!(loopback.scheme(), "http");
    }

    #[test]
    fn a_malformed_config_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"bind": "not an address"}"#).unwrap();
        assert!(load_config(&path).is_err());
        assert!(load_config(&dir.path().join("absent.json")).is_ok());
    }
}
