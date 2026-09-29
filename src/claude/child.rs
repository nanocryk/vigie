//! The `claude` child process: argv, environment, and line-delimited pipes.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::str::FromStr;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::claude::protocol::PermissionMode;

/// Set on every child so hooks can tell which vigie2 session runs them.
pub const SESSION_ENV: &str = "VIGIE2_SESSION_ID";

/// Billing switches and parent-session markers that must not reach the child.
const STRIPPED_ENV: [&str; 5] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SSE_PORT",
];
/// Vigie's hook variables; without them its hook does nothing.
const STRIPPED_ENV_PREFIX: &str = "VIGIE_";
/// Variables `cargo run` injects, describing vigie2's own crate. Build scripts
/// tracking them (ring, among others) would see other values than in a plain
/// terminal and rebuild every crate above them on each switch.
const CARGO_RUN_ENV: [&str; 5] = [
    "CARGO",
    "CARGO_MANIFEST_DIR",
    "CARGO_MANIFEST_PATH",
    "CARGO_CRATE_NAME",
    "CARGO_BIN_NAME",
];
const CARGO_RUN_ENV_PREFIX: &str = "CARGO_PKG_";
/// Set to false so claude.ai connectors are not loaded; an unauthorized one otherwise gets reported by the model.
const CONNECTORS_ENV: &str = "ENABLE_CLAUDEAI_MCP_SERVERS";

const FLAGS: [&str; 11] = [
    "-p",
    "--input-format",
    "stream-json",
    "--output-format",
    "stream-json",
    "--verbose",
    "--include-partial-messages",
    "--permission-prompt-tool",
    "stdio",
    "--replay-user-messages",
    "--forward-subagent-text",
];

const LINE_BUFFER: usize = 1024;

/// The executable standing in for `claude`, with arguments placed before the CLI flags.
#[derive(Debug, Clone)]
pub struct Program {
    pub path: PathBuf,
    pub leading_args: Vec<OsString>,
}

impl Program {
    pub fn claude() -> Self {
        Self {
            path: PathBuf::from("claude"),
            leading_args: Vec::new(),
        }
    }
}

/// How the child picks its Claude session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "claude_session_id", rename_all = "snake_case")]
pub enum Launch {
    /// `--session-id`: a new Claude session with this id.
    New(Uuid),
    /// `--resume`: continue an existing Claude session.
    Resume(Uuid),
}

impl Launch {
    pub fn claude_session_id(&self) -> Uuid {
        match self {
            Self::New(id) | Self::Resume(id) => *id,
        }
    }
}

/// A level `--effort` accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

impl FromStr for Effort {
    type Err = anyhow::Error;

    fn from_str(level: &str) -> Result<Self> {
        Ok(match level {
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            "xhigh" => Self::XHigh,
            "max" => Self::Max,
            _ => bail!("unknown effort level {level}"),
        })
    }
}

/// The model and the effort level a discussion runs on; `None` lets the CLI pick.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSettings {
    pub model: Option<String>,
    pub effort: Option<Effort>,
}

/// The CLI arguments after `Program::leading_args`; `permission_mode`, the model
/// and the effort become `--permission-mode`, `--model` and `--effort` when known.
pub fn args(
    launch: Launch,
    permission_mode: Option<&PermissionMode>,
    settings: &ModelSettings,
) -> Vec<OsString> {
    let (flag, id) = match launch {
        Launch::New(id) => ("--session-id", id),
        Launch::Resume(id) => ("--resume", id),
    };
    let optional = [
        (
            "--permission-mode",
            permission_mode.map(PermissionMode::as_str),
        ),
        ("--model", settings.model.as_deref()),
        ("--effort", settings.effort.map(Effort::as_str)),
    ];
    let known = optional.into_iter().flat_map(|(name, value)| {
        value
            .into_iter()
            .flat_map(move |value| [OsString::from(name), value.into()])
    });
    FLAGS
        .iter()
        .map(OsString::from)
        .chain(known)
        .chain([flag.into(), id.to_string().into()])
        .collect()
}

/// The child environment: `parent` minus the stripped variables, plus `SESSION_ENV` and `CONNECTORS_ENV`.
/// Names compare case-insensitively, as Windows treats them.
pub fn env(
    parent: impl IntoIterator<Item = (OsString, OsString)>,
    host_session: Uuid,
) -> Vec<(OsString, OsString)> {
    let mut vars: Vec<(OsString, OsString)> = parent
        .into_iter()
        .filter(|(name, _)| {
            !is_stripped(name) && !same_name(name, SESSION_ENV) && !same_name(name, CONNECTORS_ENV)
        })
        .collect();
    vars.push((SESSION_ENV.into(), host_session.to_string().into()));
    vars.push((CONNECTORS_ENV.into(), "false".into()));
    vars
}

/// Keeps a Ctrl+C in the server's console from reaching the child, which would
/// exit before the server's shutdown could stop it with a reason.
#[cfg(windows)]
fn own_process_group(command: &mut Command) {
    // Win32 process creation flag from `winbase.h`, see
    // https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[cfg(unix)]
fn own_process_group(command: &mut Command) {
    command.process_group(0);
}

fn is_stripped(name: &OsStr) -> bool {
    let upper = name.to_string_lossy().to_ascii_uppercase();
    upper.starts_with(STRIPPED_ENV_PREFIX)
        || upper.starts_with(CARGO_RUN_ENV_PREFIX)
        || STRIPPED_ENV.contains(&upper.as_str())
        || CARGO_RUN_ENV.contains(&upper.as_str())
}

fn same_name(name: &OsStr, other: &str) -> bool {
    name.to_string_lossy().eq_ignore_ascii_case(other)
}

/// A running child. Stdout and stderr arrive as lines without their terminator;
/// lines sent are written to stdin with a newline.
pub struct Child {
    pub stdout: mpsc::Receiver<String>,
    pub stderr: mpsc::Receiver<String>,
    stdin: Option<mpsc::Sender<String>>,
    process: tokio::process::Child,
}

impl Child {
    /// Runs `program` with `cli_args` (see `args`) in `cwd`, `host_session`
    /// exported as `VIGIE2_SESSION_ID`.
    pub fn spawn(
        program: &Program,
        cli_args: Vec<OsString>,
        cwd: &Path,
        host_session: Uuid,
    ) -> Result<Self> {
        let mut command = Command::new(&program.path);
        command
            .args(&program.leading_args)
            .args(cli_args)
            .current_dir(cwd)
            .env_clear()
            .envs(env(std::env::vars_os(), host_session))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        own_process_group(&mut command);
        let mut process = command
            .spawn()
            .with_context(|| format!("spawn {}", program.path.display()))?;
        let stdin = process.stdin.take().context("child stdin")?;
        let stdout = process.stdout.take().context("child stdout")?;
        let stderr = process.stderr.take().context("child stderr")?;
        Ok(Self {
            stdout: read_lines(stdout),
            stderr: read_lines(stderr),
            stdin: Some(write_lines(stdin)),
            process,
        })
    }

    pub async fn send(&self, line: String) -> Result<()> {
        let stdin = self
            .stdin
            .as_ref()
            .ok_or_else(|| anyhow!("child stdin closed"))?;
        stdin
            .send(line)
            .await
            .map_err(|_| anyhow!("child stdin closed"))
    }

    /// Lines already queued are still written before the pipe closes.
    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }

    pub fn kill(&mut self) {
        if let Err(err) = self.process.start_kill() {
            tracing::debug!("kill child: {err}");
        }
    }

    pub async fn wait(&mut self) -> Result<ExitStatus> {
        Ok(self.process.wait().await?)
    }
}

fn read_lines(pipe: impl AsyncRead + Unpin + Send + 'static) -> mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::channel(LINE_BUFFER);
    tokio::spawn(async move {
        let mut reader = BufReader::new(pipe);
        let mut buffer = Vec::new();
        loop {
            buffer.clear();
            match reader.read_until(b'\n', &mut buffer).await {
                Ok(0) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buffer);
                    let line = line.trim_end_matches(['\r', '\n']);
                    if !line.is_empty() && sender.send(line.to_string()).await.is_err() {
                        break;
                    }
                }
                Err(err) => {
                    tracing::debug!("read child pipe: {err}");
                    break;
                }
            }
        }
    });
    receiver
}

fn write_lines(mut stdin: ChildStdin) -> mpsc::Sender<String> {
    let (sender, mut receiver) = mpsc::channel::<String>(LINE_BUFFER);
    tokio::spawn(async move {
        while let Some(mut line) = receiver.recv().await {
            line.push('\n');
            let written = async {
                stdin.write_all(line.as_bytes()).await?;
                stdin.flush().await
            };
            if let Err(err) = written.await {
                tracing::debug!("write child stdin: {err}");
                break;
            }
        }
    });
    sender
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(name: &str, value: &str) -> (OsString, OsString) {
        (name.into(), value.into())
    }

    #[test]
    fn env_strips_billing_and_vigie_vars_and_sets_the_session() {
        let session = Uuid::new_v4();
        let parent = vec![
            var("PATH", "/bin"),
            var("ANTHROPIC_API_KEY", "sk"),
            var("Anthropic_Auth_Token", "tok"),
            var("CLAUDECODE", "1"),
            var("CLAUDE_CODE_ENTRYPOINT", "cli"),
            var("CLAUDE_CODE_SSE_PORT", "1234"),
            var("VIGIE_HOOK_URL", "https://x"),
            var("vigie_hook_secret", "s"),
            var("VIGIE2_SESSION_ID", "parent-session"),
            var("enable_claudeai_mcp_servers", "true"),
            var("CLAUDE_CONFIG_DIR", "/c"),
            var("ANTHROPIC_MODEL", "m"),
            var("CARGO", "cargo.exe"),
            var("CARGO_MANIFEST_DIR", "/vigie2"),
            var("CARGO_PKG_NAME", "vigie2"),
            var("CARGO_BIN_NAME", "vigie2"),
            var("CARGO_HOME", "/cargo"),
            var("CARGO_TARGET_DIR", "/target"),
        ];
        let child = env(parent, session);
        assert_eq!(
            child,
            vec![
                var("PATH", "/bin"),
                var("CLAUDE_CONFIG_DIR", "/c"),
                var("ANTHROPIC_MODEL", "m"),
                var("CARGO_HOME", "/cargo"),
                var("CARGO_TARGET_DIR", "/target"),
                var(SESSION_ENV, &session.to_string()),
                var(CONNECTORS_ENV, "false"),
            ]
        );
    }

    #[test]
    fn args_pick_new_or_resume() {
        let id = Uuid::new_v4();
        let new = args(Launch::New(id), None, &ModelSettings::default());
        assert_eq!(new[0], "-p");
        assert!(new.contains(&OsString::from("--replay-user-messages")));
        assert!(new.contains(&OsString::from("--forward-subagent-text")));
        for absent in ["--permission-mode", "--model", "--effort"] {
            assert!(!new.contains(&OsString::from(absent)), "{absent}");
        }
        assert_eq!(
            new[new.len() - 2..],
            [OsString::from("--session-id"), id.to_string().into()]
        );
        let resume = args(Launch::Resume(id), None, &ModelSettings::default());
        assert_eq!(
            resume[resume.len() - 2..],
            [OsString::from("--resume"), id.to_string().into()]
        );
    }

    #[test]
    fn args_carry_a_known_permission_mode() {
        let id = Uuid::new_v4();
        let resume = args(
            Launch::Resume(id),
            Some(&PermissionMode::Plan),
            &ModelSettings::default(),
        );
        let flag = resume
            .iter()
            .position(|arg| arg == "--permission-mode")
            .expect("--permission-mode present");
        assert_eq!(resume[flag + 1], "plan");
        assert_eq!(
            resume[resume.len() - 2..],
            [OsString::from("--resume"), id.to_string().into()]
        );
    }

    #[test]
    fn args_carry_a_known_model_and_effort() {
        let id = Uuid::new_v4();
        let settings = ModelSettings {
            model: Some("sonnet".into()),
            effort: Some(Effort::Max),
        };
        let resume = args(Launch::Resume(id), None, &settings);
        let value = |name: &str| {
            let flag = resume
                .iter()
                .position(|arg| arg == name)
                .unwrap_or_else(|| panic!("{name} present"));
            resume[flag + 1].clone()
        };
        assert_eq!(value("--model"), "sonnet");
        assert_eq!(value("--effort"), "max");
        let model_only = ModelSettings {
            model: Some("haiku".into()),
            effort: None,
        };
        assert!(!args(Launch::New(id), None, &model_only).contains(&OsString::from("--effort")));
    }
}
