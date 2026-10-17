//! The pair token `vigie auth pair` mints from a second process. It reaches the
//! running server through a file in the owner-only state directory, holding
//! the token's hash and expiry; the server spends it by deleting the file.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::tokens::{LOCAL_TOKEN_TTL, mint_token, same_token, secret_hash};
use crate::state_dir;

#[derive(Serialize, Deserialize)]
struct TokenFile {
    hash: String,
    expires_at: DateTime<Utc>,
}

/// Writes a fresh token to `path`, replacing any earlier one; returns it.
pub fn issue(path: &Path) -> Result<String> {
    let token = mint_token();
    let file = TokenFile {
        hash: secret_hash(&token),
        expires_at: Utc::now() + LOCAL_TOKEN_TTL,
    };
    state_dir::write_private(path, serde_json::to_vec(&file)?)?;
    Ok(token)
}

/// The server's side: spends the token at `path` once.
pub struct LocalToken {
    path: PathBuf,
    /// Two exchanges racing on one token must not both read it before the delete.
    spending: Mutex<()>,
}

impl LocalToken {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            spending: Mutex::new(()),
        }
    }

    /// Whether `candidate` is the unexpired token on disk; a match deletes it.
    pub fn consume(&self, candidate: &str) -> bool {
        let _spending = self.spending.lock();
        let file = match self.read() {
            Ok(Some(file)) => file,
            Ok(None) => return false,
            Err(err) => {
                warn!("read the local pair token: {err:#}");
                return false;
            }
        };
        let expired = file.expires_at <= Utc::now();
        let matched = !expired && same_token(&file.hash, &secret_hash(candidate));
        if (expired || matched)
            && let Err(err) = std::fs::remove_file(&self.path)
            && err.kind() != ErrorKind::NotFound
        {
            warn!("delete the local pair token: {err}");
            return false;
        }
        matched
    }

    fn read(&self) -> Result<Option<TokenFile>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => {
                Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
                    format!("parse {}", self.path.display())
                })?))
            }
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err).with_context(|| format!("read {}", self.path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_issued_token_pairs_once() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("pair-token");
        let local = LocalToken::new(path.clone());
        assert!(!local.consume("anything"), "no file, no token");

        let token = issue(&path).unwrap();
        assert!(!local.consume("wrong"));
        assert!(path.exists(), "a wrong guess must not burn the token");
        assert!(local.consume(&token));
        assert!(!local.consume(&token), "spent");
        assert!(!path.exists());
    }

    #[test]
    fn a_new_token_replaces_the_old_one() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("pair-token");
        let local = LocalToken::new(path.clone());
        let old = issue(&path).unwrap();
        let new = issue(&path).unwrap();
        assert!(!local.consume(&old));
        assert!(local.consume(&new));
    }

    #[test]
    fn an_expired_token_is_refused_and_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("pair-token");
        let token = mint_token();
        let file = TokenFile {
            hash: secret_hash(&token),
            expires_at: Utc::now() - chrono::Duration::seconds(1),
        };
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        assert!(!LocalToken::new(path.clone()).consume(&token));
        assert!(!path.exists());
    }
}
