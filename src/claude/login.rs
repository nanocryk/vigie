//! `claude auth`: the account the CLI is logged in to, and a login run from the
//! app through the manual code flow (open the URL, paste the code it shows).

use std::ffi::OsString;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc};
use tokio::time::timeout;

use crate::claude::child::{Child, Program};

const URL_TIMEOUT: Duration = Duration::from_secs(15);
const CODE_TIMEOUT: Duration = Duration::from_secs(60);
const STATUS_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a started login waits for its code before it is dropped.
const RUN_LIFETIME: Duration = Duration::from_secs(10 * 60);

/// Precedes the login URL in `claude auth login` output.
const URL_MARKER: &str = "visit: ";
/// Printed without a newline, so it prefixes the line that follows the code.
const CODE_PROMPT: &str = "Paste code here if prompted >";

/// The account `claude auth status` reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Account {
    pub logged_in: bool,
    pub email: Option<String>,
    pub subscription: Option<String>,
}

impl Account {
    fn parse(json: &str) -> Result<Self> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Status {
            logged_in: bool,
            email: Option<String>,
            subscription_type: Option<String>,
        }
        let status: Status = serde_json::from_str(json)?;
        Ok(Self {
            logged_in: status.logged_in,
            email: status.email,
            subscription: status.subscription_type,
        })
    }
}

/// How a code sent to the running login ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submitted {
    LoggedIn,
    /// The CLI's failure message.
    Rejected(String),
    /// No login runs: never started, expired, or already given a code.
    NoLogin,
}

/// At most one `claude auth login` at a time, waiting for its code.
pub struct Login {
    program: Program,
    run: Mutex<Option<Run>>,
    next_run: AtomicU64,
}

struct Run {
    id: u64,
    child: Child,
}

impl Login {
    pub fn new(program: Program) -> Self {
        Self {
            program,
            run: Mutex::new(None),
            next_run: AtomicU64::new(0),
        }
    }

    pub async fn account(&self) -> Result<Account> {
        let mut child = Child::spawn_plain(&self.program, args(&["auth", "status", "--json"]))?;
        let (stdout, stderr) = timeout(STATUS_TIMEOUT, async {
            tokio::join!(drain(&mut child.stdout), drain(&mut child.stderr))
        })
        .await
        .map_err(|_| anyhow!("claude auth status gave no answer within {STATUS_TIMEOUT:?}"))?;
        child.wait().await?;
        Account::parse(&stdout.join("\n"))
            .with_context(|| format!("read claude auth status: {}", stderr.join(" / ")))
    }

    /// Starts a login, dropping any earlier one, and returns the URL to open.
    pub async fn start(self: &Arc<Self>) -> Result<String> {
        let mut slot = self.run.lock().await;
        *slot = None;
        let mut child = Child::spawn_plain(&self.program, args(&["auth", "login", "--claudeai"]))?;
        let url = timeout(URL_TIMEOUT, read_url(&mut child))
            .await
            .map_err(|_| anyhow!("claude auth login gave no URL within {URL_TIMEOUT:?}"))??;
        let id = self.next_run.fetch_add(1, Ordering::Relaxed);
        *slot = Some(Run { id, child });
        drop(slot);
        let login = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(RUN_LIFETIME).await;
            let Some(login) = login.upgrade() else { return };
            let mut slot = login.run.lock().await;
            if slot.as_ref().is_some_and(|run| run.id == id) {
                *slot = None;
            }
        });
        Ok(url)
    }

    /// Sends `code` to the running login and waits for it to end.
    pub async fn submit(&self, code: &str) -> Result<Submitted> {
        let Some(mut run) = self.run.lock().await.take() else {
            return Ok(Submitted::NoLogin);
        };
        run.child.send(code.to_owned()).await?;
        let (success, last) = timeout(CODE_TIMEOUT, async {
            let mut last = None;
            while let Some(line) = next_line(&mut run.child).await {
                if let Some(text) = output_text(&line) {
                    last = Some(text.to_owned());
                }
            }
            let status = run.child.wait().await?;
            anyhow::Ok((status.success(), last))
        })
        .await
        .map_err(|_| anyhow!("claude auth login did not finish within {CODE_TIMEOUT:?}"))??;
        Ok(if success {
            Submitted::LoggedIn
        } else {
            Submitted::Rejected(last.unwrap_or_else(|| "Login failed".into()))
        })
    }
}

fn args(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

async fn drain(lines: &mut mpsc::Receiver<String>) -> Vec<String> {
    let mut all = Vec::new();
    while let Some(line) = lines.recv().await {
        all.push(line);
    }
    all
}

/// The next line of either stream, or `None` once both closed.
async fn next_line(child: &mut Child) -> Option<String> {
    tokio::select! {
        Some(line) = child.stdout.recv() => Some(line),
        Some(line) = child.stderr.recv() => Some(line),
        else => None,
    }
}

async fn read_url(child: &mut Child) -> Result<String> {
    let mut seen = Vec::new();
    while let Some(line) = next_line(child).await {
        if let Some(url) = login_url(&line) {
            return Ok(url.to_owned());
        }
        seen.push(line);
    }
    bail!(
        "claude auth login exited without a URL: {}",
        seen.join(" / ")
    )
}

fn login_url(line: &str) -> Option<&str> {
    let (_, url) = line.split_once(URL_MARKER)?;
    let url = url.trim();
    url.starts_with("https://").then_some(url)
}

/// `line` without the code prompt it may start with, or `None` when nothing is left.
fn output_text(line: &str) -> Option<&str> {
    let text = line
        .rsplit_once(CODE_PROMPT)
        .map_or(line, |(_, rest)| rest)
        .trim();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_url_line_gives_the_url() {
        let line = "If the browser didn't open, visit: https://claude.com/cai/oauth/authorize?code=true&state=x";
        assert_eq!(
            login_url(line),
            Some("https://claude.com/cai/oauth/authorize?code=true&state=x")
        );
        assert_eq!(login_url("Opening browser to sign in…"), None);
        assert_eq!(login_url("visit: not a url"), None);
    }

    #[test]
    fn output_text_drops_the_code_prompt() {
        assert_eq!(
            output_text(
                "Paste code here if prompted > Login failed: Request failed with status code 400"
            ),
            Some("Login failed: Request failed with status code 400")
        );
        assert_eq!(output_text("Paste code here if prompted > "), None);
        assert_eq!(output_text("Login successful."), Some("Login successful."));
    }

    #[test]
    fn account_reads_auth_status() {
        let json = r#"{
            "loggedIn": true,
            "authMethod": "claude.ai",
            "apiProvider": "firstParty",
            "email": "someone@example.com",
            "orgName": "Org",
            "subscriptionType": "max"
        }"#;
        assert_eq!(
            Account::parse(json).unwrap(),
            Account {
                logged_in: true,
                email: Some("someone@example.com".into()),
                subscription: Some("max".into()),
            }
        );
        assert_eq!(
            Account::parse(r#"{"loggedIn": false}"#).unwrap(),
            Account {
                logged_in: false,
                email: None,
                subscription: None,
            }
        );
    }
}
