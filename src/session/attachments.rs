//! Files attached to a message. vigie2 records a message's text and its
//! attachments apart; Claude gets them together, as `compose` writes them.

/// A message as vigie2 records it: its text and the paths of its attached files.
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

#[cfg(test)]
mod tests {
    use super::*;

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
