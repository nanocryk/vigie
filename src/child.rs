//! The `claude` child process: argv, environment, and line-delimited pipes.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::mpsc;
use uuid::Uuid;

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
/// Set to false so claude.ai connectors are not loaded; an unauthorized one otherwise gets reported by the model.
const CONNECTORS_ENV: &str = "ENABLE_CLAUDEAI_MCP_SERVERS";

const FLAGS: [&str; 10] = [
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

impl Default for Program {
    fn default() -> Self {
        Self::claude()
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

/// The CLI arguments after `Program::leading_args`.
pub fn args(launch: Launch) -> Vec<OsString> {
    let (flag, id) = match launch {
        Launch::New(id) => ("--session-id", id),
        Launch::Resume(id) => ("--resume", id),
    };
    FLAGS
        .iter()
        .map(OsString::from)
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

fn is_stripped(name: &OsStr) -> bool {
    let upper = name.to_string_lossy().to_ascii_uppercase();
    upper.starts_with(STRIPPED_ENV_PREFIX) || STRIPPED_ENV.contains(&upper.as_str())
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
    pub fn spawn(
        program: &Program,
        cwd: &Path,
        launch: Launch,
        host_session: Uuid,
    ) -> Result<Self> {
        let mut process = Command::new(&program.path)
            .args(&program.leading_args)
            .args(args(launch))
            .current_dir(cwd)
            .env_clear()
            .envs(env(std::env::vars_os(), host_session))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
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
        ];
        let child = env(parent, session);
        assert_eq!(
            child,
            vec![
                var("PATH", "/bin"),
                var("CLAUDE_CONFIG_DIR", "/c"),
                var("ANTHROPIC_MODEL", "m"),
                var(SESSION_ENV, &session.to_string()),
                var(CONNECTORS_ENV, "false"),
            ]
        );
    }

    #[test]
    fn args_pick_new_or_resume() {
        let id = Uuid::new_v4();
        let new = args(Launch::New(id));
        assert_eq!(new[0], "-p");
        assert!(new.contains(&OsString::from("--replay-user-messages")));
        assert_eq!(
            new[new.len() - 2..],
            [OsString::from("--session-id"), id.to_string().into()]
        );
        let resume = args(Launch::Resume(id));
        assert_eq!(
            resume[resume.len() - 2..],
            [OsString::from("--resume"), id.to_string().into()]
        );
    }
}
