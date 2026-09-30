//! Updating vigie from its git repository: counting the upstream commits HEAD
//! lacks, then pulling and rebuilding the web app and the server.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use parking_lot::Mutex;
use serde::Serialize;
use tracing::{info, warn};

use crate::AppState;
use crate::http::api::ApiError;
use crate::session::server_restart::Supervision;

const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);
const GIT_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const BUILD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Output lines kept from a failed step.
const OUTPUT_LINES: usize = 40;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Check {
    Unchecked,
    /// HEAD lacks `behind` commits of its upstream branch.
    Done {
        behind: u32,
    },
    Failed {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// No local change nor commit missing upstream.
    Clean,
    Pull,
    Install,
    Web,
    Server,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Run {
    Idle,
    Running {
        step: Step,
    },
    /// Every step passed; a server restart runs the new build.
    Built,
    Failed {
        step: Step,
        output: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub check: Check,
    pub run: Run,
}

pub struct Updater {
    repo: PathBuf,
    status: Mutex<Status>,
    /// Held by a check or a run: a fetch must not race a pull.
    git: tokio::sync::Mutex<()>,
}

impl Updater {
    pub fn new(repo: PathBuf) -> Self {
        Self {
            repo,
            status: Mutex::new(Status {
                check: Check::Unchecked,
                run: Run::Idle,
            }),
            git: tokio::sync::Mutex::new(()),
        }
    }

    pub fn status(&self) -> Status {
        self.status.lock().clone()
    }

    /// Fetches and counts the upstream commits HEAD lacks; skipped while an update runs.
    pub async fn check(&self) {
        let Ok(_git) = self.git.try_lock() else {
            return;
        };
        let check = check(&self.repo).await;
        self.status.lock().check = check;
    }

    /// Starts an update in the background; false when one runs already.
    pub fn start(self: &Arc<Self>) -> bool {
        {
            let mut status = self.status.lock();
            if matches!(status.run, Run::Running { .. }) {
                return false;
            }
            status.run = Run::Running { step: Step::Clean };
        }
        let updater = Arc::clone(self);
        tokio::spawn(async move { updater.run().await });
        true
    }

    async fn run(&self) {
        let _git = self.git.lock().await;
        let run = match self.steps().await {
            Ok(()) => {
                info!("update built");
                Run::Built
            }
            Err((step, output)) => {
                warn!(?step, "update failed: {output}");
                Run::Failed { step, output }
            }
        };
        let check = check(&self.repo).await;
        let mut status = self.status.lock();
        status.run = run;
        status.check = check;
    }

    async fn steps(&self) -> Result<(), (Step, String)> {
        let repo = self.repo.as_path();
        let web = repo.join("web");
        self.enter(Step::Clean);
        clean(repo).await.map_err(|output| (Step::Clean, output))?;
        let npm = npm().map_err(|output| (Step::Install, output))?;
        let steps: [(Step, &Path, &[&str], &Path, Duration); 4] = [
            (
                Step::Pull,
                Path::new("git"),
                &["pull", "--ff-only"],
                repo,
                GIT_TIMEOUT,
            ),
            (Step::Install, &npm, &["ci"], &web, BUILD_TIMEOUT),
            (Step::Web, &npm, &["run", "build"], &web, BUILD_TIMEOUT),
            (
                Step::Server,
                Path::new("cargo"),
                &["build", "--release", "--bin", "vigie"],
                repo,
                BUILD_TIMEOUT,
            ),
        ];
        for (step, program, args, dir, timeout) in steps {
            self.enter(step);
            info!(?step, "update step");
            run(program, args, dir, timeout)
                .await
                .map_err(|output| (step, output))?;
        }
        Ok(())
    }

    fn enter(&self, step: Step) {
        self.status.lock().run = Run::Running { step };
    }
}

/// Checks at launch, then every `CHECK_INTERVAL`.
pub fn spawn_checks(updater: Arc<Updater>) {
    tokio::spawn(async move {
        loop {
            updater.check().await;
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    });
}

async fn check(repo: &Path) -> Check {
    let counted = async {
        run("git", &["fetch", "--quiet"], repo, GIT_TIMEOUT).await?;
        count(repo, "HEAD..@{upstream}").await
    };
    match counted.await {
        Ok(behind) => Check::Done { behind },
        Err(message) => Check::Failed { message },
    }
}

/// Fails, with what is in the way, when the tree has changes or HEAD has commits upstream lacks.
async fn clean(repo: &Path) -> Result<(), String> {
    let changes = run(
        "git",
        &["status", "--porcelain", "--untracked-files=all"],
        repo,
        GIT_TIMEOUT,
    )
    .await?;
    if !changes.trim().is_empty() {
        return Err(format!("Local changes:\n{}", tail(&changes)));
    }
    let ahead = count(repo, "@{upstream}..HEAD").await?;
    if ahead > 0 {
        return Err(format!(
            "{ahead} local commits are not on the upstream branch"
        ));
    }
    Ok(())
}

async fn count(repo: &Path, range: &str) -> Result<u32, String> {
    let output = run("git", &["rev-list", "--count", range], repo, GIT_TIMEOUT).await?;
    output
        .trim()
        .parse()
        .map_err(|err| format!("git rev-list gave {:?}: {err}", output.trim()))
}

/// npm installs ship its launcher as `npm.cmd` or `npm.exe`: the first PATHEXT match on PATH.
#[cfg(windows)]
fn npm() -> Result<PathBuf, String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
    find_program("npm", &path, &pathext).ok_or_else(|| not_found(Path::new("npm")))
}

#[cfg(not(windows))]
fn npm() -> Result<PathBuf, String> {
    Ok(PathBuf::from("npm"))
}

#[cfg(windows)]
fn find_program(name: &str, path: &std::ffi::OsStr, pathext: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .flat_map(|dir| {
            pathext
                .split(';')
                .filter(|ext| !ext.is_empty())
                .map(move |ext| dir.join(format!("{name}{}", ext.to_ascii_lowercase())))
        })
        .find(|candidate| candidate.is_file())
}

fn not_found(program: &Path) -> String {
    let path = std::env::var_os("PATH").unwrap_or_default();
    format!(
        "{} not found on the server's PATH: {}",
        program.display(),
        path.to_string_lossy()
    )
}

/// Runs `program` in `dir`: its stdout on success, else the end of its output.
async fn run(
    program: impl AsRef<Path>,
    args: &[&str],
    dir: &Path,
    timeout: Duration,
) -> Result<String, String> {
    let program = program.as_ref();
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .current_dir(dir)
        // A credential prompt would wait on a terminal nobody sees.
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| {
            format!(
                "{} timed out after {} s",
                program.display(),
                timeout.as_secs()
            )
        })?
        .map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => not_found(program),
            _ => format!("cannot run {}: {err}", program.display()),
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(tail(&format!("{stdout}{stderr}")))
}

fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    lines[lines.len().saturating_sub(OUTPUT_LINES)..].join("\n")
}

pub async fn status_route(State(state): State<AppState>) -> Json<Status> {
    Json(state.updater.status())
}

pub async fn check_route(State(state): State<AppState>) -> Json<Status> {
    state.updater.check().await;
    Json(state.updater.status())
}

/// Starts an update; only a server under the shim can run the build it makes.
pub async fn run_route(State(state): State<AppState>) -> Result<Response, ApiError> {
    if state.manager.server_restart().supervision() == Supervision::None {
        return Err(ApiError::new(StatusCode::CONFLICT, "not_supervised"));
    }
    if !state.updater.start() {
        return Err(ApiError::new(StatusCode::CONFLICT, "running"));
    }
    Ok((StatusCode::ACCEPTED, Json(state.updater.status())).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .current_dir(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }

    fn commit(dir: &Path, file: &str) {
        std::fs::write(dir.join(file), file).expect("write");
        git(dir, &["add", file]);
        git(dir, &["commit", "-q", "-m", file]);
    }

    #[tokio::test]
    async fn counts_upstream_commits_and_refuses_local_changes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let origin = tmp.path().join("origin");
        let local = tmp.path().join("local");
        let other = tmp.path().join("other");
        std::fs::create_dir(&origin).expect("mkdir");
        git(&origin, &["init", "-q", "--bare", "-b", "main"]);
        let origin_url = origin.to_str().expect("utf-8 path");
        for clone in [&local, &other] {
            git(
                tmp.path(),
                &[
                    "clone",
                    "-q",
                    origin_url,
                    clone.to_str().expect("utf-8 path"),
                ],
            );
        }
        commit(&other, "first.txt");
        git(&other, &["push", "-q", "origin", "HEAD:main"]);
        git(&local, &["pull", "-q", "origin", "main"]);
        git(&local, &["branch", "-q", "--set-upstream-to=origin/main"]);

        assert_eq!(check(&local).await, Check::Done { behind: 0 });
        commit(&other, "second.txt");
        commit(&other, "third.txt");
        git(&other, &["push", "-q", "origin", "HEAD:main"]);
        assert_eq!(check(&local).await, Check::Done { behind: 2 });
        assert_eq!(clean(&local).await, Ok(()));

        std::fs::write(local.join("stray.txt"), "x").expect("write");
        let dirty = clean(&local).await.expect_err("untracked file");
        assert!(dirty.contains("stray.txt"), "{dirty}");
        std::fs::remove_file(local.join("stray.txt")).expect("remove");

        commit(&local, "mine.txt");
        let ahead = clean(&local).await.expect_err("local commit");
        assert!(ahead.starts_with("1 local commits"), "{ahead}");
    }

    #[cfg(windows)]
    #[test]
    fn finds_a_program_by_pathext() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let empty = tmp.path().join("empty");
        let volta = tmp.path().join("volta");
        let node = tmp.path().join("node");
        for dir in [&empty, &volta, &node] {
            std::fs::create_dir(dir).expect("mkdir");
        }
        std::fs::write(volta.join("npm.exe"), "").expect("write");
        std::fs::write(node.join("npm.cmd"), "").expect("write");
        let path = std::env::join_paths([&empty, &volta, &node]).expect("join");
        assert_eq!(
            find_program("npm", &path, ".COM;.EXE;.BAT;.CMD"),
            Some(volta.join("npm.exe"))
        );
        let path = std::env::join_paths([&empty, &node]).expect("join");
        assert_eq!(
            find_program("npm", &path, ".COM;.EXE;.BAT;.CMD"),
            Some(node.join("npm.cmd"))
        );
        let path = std::env::join_paths([&empty]).expect("join");
        assert_eq!(find_program("npm", &path, ".COM;.EXE;.BAT;.CMD"), None);
    }

    #[test]
    fn keeps_the_last_lines() {
        let text: String = (1..=50).map(|line| format!("{line}\n")).collect();
        let kept = tail(&text);
        assert!(kept.starts_with("11\n") && kept.ends_with("\n50"), "{kept}");
    }
}
