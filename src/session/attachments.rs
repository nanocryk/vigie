//! Files attached to a message. vigie records a message's text and its
//! attachments apart; Claude gets them together, as `compose` writes them.

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::Path;

use ring::digest;

/// A message as vigie records it: its text and the paths of its attached files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Message {
    pub text: String,
    pub attachments: Vec<String>,
}

impl Message {
    pub fn new(text: String, attachments: Vec<String>) -> Self {
        Self { text, attachments }
    }

    /// No text and no file.
    pub fn is_blank(&self) -> bool {
        self.text.trim().is_empty() && self.attachments.is_empty()
    }

    /// What Claude gets, see `compose`.
    pub fn composed(&self) -> String {
        compose(&self.text, &self.attachments)
    }
}

impl From<String> for Message {
    fn from(text: String) -> Self {
        Self::new(text, Vec::new())
    }
}

impl From<&str> for Message {
    fn from(text: &str) -> Self {
        Self::new(text.to_string(), Vec::new())
    }
}

/// The text Claude gets for `text` with `attachments`: the text, a blank line,
/// then one `- path` line per file under `Attachments:`. The block stands alone
/// when the text is blank.
pub fn compose(text: &str, attachments: &[String]) -> String {
    if attachments.is_empty() {
        return text.to_string();
    }
    let list: String = attachments
        .iter()
        .map(|path| format!("\n- {path}"))
        .collect();
    if text.trim().is_empty() {
        format!("Attachments:{list}")
    } else {
        format!("{text}\n\nAttachments:{list}")
    }
}

/// SHA-256 of `bytes`, lowercase hex.
pub fn digest_hex(bytes: &[u8]) -> String {
    hex(digest::digest(&digest::SHA256, bytes).as_ref())
}

/// SHA-256 of the file at `path`, lowercase hex, read in chunks.
pub fn file_hash(path: &Path) -> io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut context = digest::Context::new(&digest::SHA256);
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        context.update(&buffer[..read]);
    }
    Ok(hex(context.finish().as_ref()))
}

/// The hash of each readable file of `paths`, keyed by path.
pub fn file_hashes(paths: &[String]) -> BTreeMap<String, String> {
    paths
        .iter()
        .filter_map(|path| Some((path.clone(), file_hash(Path::new(path)).ok()?)))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_hash_follows_the_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        std::fs::write(&path, b"first").unwrap();
        let first = file_hash(&path).unwrap();
        assert_eq!(first, digest_hex(b"first"));
        assert_eq!(first.len(), 64);
        std::fs::write(&path, b"second").unwrap();
        assert_ne!(file_hash(&path).unwrap(), first);

        let missing = dir.path().join("gone.png").display().to_string();
        let hashes = file_hashes(&[path.display().to_string(), missing]);
        assert_eq!(hashes.len(), 1);
        assert_eq!(hashes.values().next(), Some(&digest_hex(b"second")));
    }

    /// `web/src/attachments.test.ts` checks the same strings.
    #[test]
    fn compose_appends_the_block() {
        assert_eq!(compose("hi", &[]), "hi");
        let files = ["C:\\u\\a.png".to_string(), "/tmp/b.txt".to_string()];
        assert_eq!(
            compose("look", &files),
            "look\n\nAttachments:\n- C:\\u\\a.png\n- /tmp/b.txt"
        );
        assert_eq!(compose(" \n", &files[..1]), "Attachments:\n- C:\\u\\a.png");
    }
}
