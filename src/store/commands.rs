//! The slash commands the user ran lately, read from the `events` table.

use std::collections::HashSet;

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};

use super::{Store, parse_time};
use crate::claude::protocol::{ContentBlock, Inbound};
use crate::session::HostEvent;

/// Every `user_sent` of `?1` holding a slash, in seq order. Repeats the
/// `events_host` index expression verbatim.
const USER_SENT_WITH_SLASH: &str = "SELECT seq, time, payload FROM events
    WHERE session_id = ?1 AND kind = 'host' AND json_extract(payload, '$.event') = 'user_sent'
        AND payload LIKE '%/%'
    ORDER BY seq";

/// The main-conversation lines of `?1` from `?2` to below `?3` that may call `Skill`.
const SKILL_LINES: &str = "SELECT payload FROM events
    WHERE session_id = ?1 AND seq > ?2 AND seq < ?3 AND kind = 'line'
        AND payload LIKE '%\"name\":\"Skill\"%'";

impl Store {
    /// Up to `limit` distinct command names, most recently run first. A command
    /// counts when it starts a message (the CLI runs it), or when it appears later
    /// in a message and Claude called `Skill` with it in that turn.
    pub async fn recent_commands(&self, limit: usize) -> Result<Vec<String>> {
        self.call(move |connection| {
            let mut uses = Vec::new();
            let mut sessions = connection.prepare("SELECT id FROM sessions")?;
            let ids = sessions
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for id in ids {
                session_uses(connection, &id, &mut uses)?;
            }
            uses.sort_by_key(|command| std::cmp::Reverse((command.at, command.seq)));
            let mut seen = HashSet::new();
            Ok(uses
                .into_iter()
                .map(|command| command.name)
                .filter(|name| seen.insert(name.clone()))
                .take(limit)
                .collect())
        })
        .await
    }
}

/// A command run by the message sent at `at`, event `seq` of its session.
struct CommandUse {
    at: DateTime<Utc>,
    seq: u64,
    name: String,
}

/// Appends the command uses of one session.
fn session_uses(connection: &Connection, session: &str, uses: &mut Vec<CommandUse>) -> Result<()> {
    let mut statement = connection.prepare(USER_SENT_WITH_SLASH)?;
    let sent = statement
        .query_map([session], |row| {
            Ok((
                row.get::<_, u64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut next_turns = connection.prepare(
        "SELECT seq FROM events
         WHERE session_id = ?1 AND kind = 'host' AND json_extract(payload, '$.event') = 'user_sent'
             AND seq > ?2
         ORDER BY seq LIMIT 1",
    )?;
    for (seq, time, payload) in sent {
        let Ok(HostEvent::UserSent { text, .. }) = serde_json::from_str(&payload) else {
            continue;
        };
        let at = parse_time(&time)?;
        let found = |name: &str| CommandUse {
            at,
            seq,
            name: name.to_string(),
        };
        let (leading, later) = slash_words(&text);
        uses.extend(leading.map(found));
        if later.is_empty() {
            continue;
        }
        let turn_end: u64 = next_turns
            .query_row(params![session, seq], |row| row.get(0))
            .optional()?
            .unwrap_or(i64::MAX as u64);
        let called = skills_called(connection, session, seq, turn_end)?;
        uses.extend(
            later
                .into_iter()
                .filter(|name| called.contains(*name))
                .map(found),
        );
    }
    Ok(())
}

/// The skills the main conversation called between two seqs.
fn skills_called(
    connection: &Connection,
    session: &str,
    after: u64,
    before: u64,
) -> Result<HashSet<String>> {
    let mut statement = connection.prepare(SKILL_LINES)?;
    let payloads = statement
        .query_map(params![session, after, before], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut called = HashSet::new();
    for payload in payloads {
        let Ok(Inbound::Assistant(assistant)) = serde_json::from_str(&payload) else {
            continue;
        };
        if assistant.parent_tool_use_id.is_some() {
            continue;
        }
        for block in assistant.message.content {
            if let ContentBlock::ToolUse { name, input, .. } = block
                && name == "Skill"
                && let Some(skill) = input.get("skill").and_then(|value| value.as_str())
            {
                called.insert(skill.to_string());
            }
        }
    }
    Ok(called)
}

/// The command a message starts with, and the command-like `/words` after it.
fn slash_words(text: &str) -> (Option<&str>, Vec<&str>) {
    let mut words = text.split_whitespace().map(|word| {
        word.strip_prefix('/').filter(|name| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|char| char.is_ascii_alphanumeric() || ":_.-".contains(char))
        })
    });
    let leading = words.next().flatten();
    (leading, words.flatten().collect())
}

#[cfg(test)]
mod tests {
    use super::super::testing::{Fixture, event, fixture, line};
    use super::*;
    use crate::session::EventBody;

    fn user_sent(seq: u64, text: &str) -> crate::session::Event {
        event(
            seq,
            EventBody::Host(HostEvent::UserSent {
                text: text.into(),
                attachments: Vec::new(),
                client_id: None,
                mid_turn: false,
            }),
        )
    }

    fn skill_call(seq: u64, skill: &str, parent: Option<&str>) -> crate::session::Event {
        let raw = serde_json::json!({
            "type": "assistant", "session_id": "s", "uuid": "u",
            "parent_tool_use_id": parent,
            "message": {"id": format!("m{seq}"), "model": "m", "content": [
                {"type": "tool_use", "id": format!("t{seq}"), "name": "Skill",
                    "input": {"skill": skill}}
            ]},
        });
        event(seq, line(&raw.to_string()))
    }

    #[test]
    fn slash_words_split_the_leading_command_from_later_ones() {
        assert_eq!(slash_words("/review high"), (Some("review"), vec![]));
        assert_eq!(
            slash_words("use /canif-suggest and /tmp/x then /a:b"),
            (None, vec!["canif-suggest", "a:b"])
        );
        assert_eq!(slash_words("  / alone"), (None, vec![]));
    }

    #[tokio::test]
    async fn counts_leading_commands_and_later_ones_claude_ran() {
        let Fixture {
            dir: _dir,
            store,
            id,
            ..
        } = fixture();
        let events = [
            user_sent(1, "/canif-suggest"),
            user_sent(2, "please /rust-dev and /never-called"),
            skill_call(3, "rust-dev", None),
            skill_call(4, "never-called", Some("agent")),
            user_sent(5, "plain text"),
            skill_call(6, "never-called", None),
            user_sent(7, "/review high"),
            user_sent(8, "/canif-suggest"),
        ];
        for event in &events {
            store.record_event(id, event);
        }
        store.flush().await.unwrap();
        assert_eq!(
            store.recent_commands(10).await.unwrap(),
            ["canif-suggest", "review", "rust-dev"]
        );
        assert_eq!(store.recent_commands(1).await.unwrap(), ["canif-suggest"]);
    }
}
