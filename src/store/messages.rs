//! The `history_messages` table: the text of each main-conversation message, the
//! user's and Claude's, for `history_search` and `history_read`.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde_json::Value;
use uuid::Uuid;

use super::{Store, format_time, parse_time};
use crate::claude::protocol::{ContentBlock, Inbound, MessageContent};
use crate::session::seed::{CLOSING, INTERRUPT_PREFIX, typed_command};

/// Creates the table; `migrate` then fills it once with `backfill`.
pub const MIGRATION: &str = r#"
CREATE TABLE history_messages (
    session_id TEXT NOT NULL REFERENCES sessions (id),
    seq INTEGER NOT NULL,
    time TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('user', 'claude')),
    text TEXT NOT NULL,
    folded TEXT NOT NULL,
    PRIMARY KEY (session_id, seq)
) STRICT;
CREATE INDEX history_messages_time ON history_messages (time);
"#;

/// Drops the synthetic user lines `MIGRATION` indexed before `message_of` skipped them.
pub const SYNTHETIC_MIGRATION: &str = r#"
DELETE FROM history_messages WHERE role = 'user' AND EXISTS (
    SELECT 1 FROM events
    WHERE events.session_id = history_messages.session_id AND events.seq = history_messages.seq
        AND json_extract(events.payload, '$.isSynthetic') = 1
);
"#;

/// The seed of the session's latest restart, NULL for one without.
const LATEST_SEED: &str = "SELECT json_extract(payload, '$.seed') FROM events
    WHERE session_id = ?1 AND kind = 'host' AND json_extract(payload, '$.event') = 'restarted'
    ORDER BY seq DESC LIMIT 1";

/// A time above every stored one: stored times start with the year.
const NO_TIME_LIMIT: &str = "~";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Claude,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Claude => "claude",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        [Self::User, Self::Claude]
            .into_iter()
            .find(|role| role.as_str() == text)
            .with_context(|| format!("unknown role {text}"))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredMessage {
    pub session: Uuid,
    pub seq: u64,
    pub time: DateTime<Utc>,
    pub role: Role,
    pub text: String,
}

/// What `history_search` filters on; times compare as stored text.
#[derive(Debug, Clone, Default)]
pub struct MessageQuery {
    pub sessions: Vec<Uuid>,
    /// Each must appear in the message, ignoring case.
    pub words: Vec<String>,
    pub since: Option<DateTime<Utc>>,
    pub before: Option<DateTime<Utc>>,
}

/// The message `line` adds to the history, before its seed is stripped: a
/// main-conversation user text, or Claude's text blocks.
pub fn message_of(line: &Inbound) -> Option<(Role, String)> {
    let (role, text) = match line {
        Inbound::User(user) if user.parent_tool_use_id.is_none() => {
            // Synthetic lines are the CLI's own: a loaded skill's text, an image's size note.
            let flagged = |key: &str| user.extra.get(key) == Some(&Value::Bool(true));
            if flagged("isMeta") || flagged("isSynthetic") {
                return None;
            }
            let text = match &user.message.content {
                MessageContent::Text(text) => text.clone(),
                MessageContent::Blocks(blocks) => joined_text(blocks)?,
            };
            let text = typed_command(&text).unwrap_or(text);
            if text.starts_with(INTERRUPT_PREFIX) {
                return None;
            }
            (Role::User, text)
        }
        Inbound::Assistant(assistant) if assistant.parent_tool_use_id.is_none() => {
            (Role::Claude, joined_text(&assistant.message.content)?)
        }
        _ => return None,
    };
    (!text.trim().is_empty()).then_some((role, text))
}

fn joined_text(blocks: &[ContentBlock]) -> Option<String> {
    let texts: Vec<&str> = blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

/// A user message without the continue seed it starts with; `None` when only the
/// seed's closing line is left.
fn without_seed(text: String, seed: Option<&str>) -> Option<String> {
    let text = match seed.and_then(|seed| text.strip_prefix(seed)?.strip_prefix("\n\n")) {
        Some(rest) => rest.to_string(),
        None => text,
    };
    (text != CLOSING).then_some(text)
}

/// Stores the message of the event `seq` of `session`, which the same job stored.
pub fn record(
    connection: &Connection,
    session: &str,
    seq: u64,
    time: &str,
    role: Role,
    text: String,
) -> Result<()> {
    let text = match role {
        Role::User => {
            let seed: Option<String> = connection
                .query_row(LATEST_SEED, [session], |row| row.get(0))
                .optional()?
                .flatten();
            match without_seed(text, seed.as_deref()) {
                Some(text) => text,
                None => return Ok(()),
            }
        }
        Role::Claude => text,
    };
    insert(connection, session, seq, time, role, &text)
}

fn insert(
    connection: &Connection,
    session: &str,
    seq: u64,
    time: &str,
    role: Role,
    text: &str,
) -> Result<()> {
    connection.execute(
        "INSERT INTO history_messages (session_id, seq, time, role, text, folded)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![session, seq, time, role.as_str(), text, text.to_lowercase()],
    )?;
    Ok(())
}

/// Fills the table from the events already stored, as `record` would have.
pub fn backfill(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT session_id, seq, time, kind, payload,
             CASE kind WHEN 'host' THEN json_extract(payload, '$.seed') END AS seed
         FROM events
         WHERE (kind = 'host' AND json_extract(payload, '$.event') = 'restarted')
             OR (kind = 'line' AND json_extract(payload, '$.type') IN ('user', 'assistant'))
         ORDER BY session_id, seq",
    )?;
    let mut rows = statement.query([])?;
    let mut session = String::new();
    let mut seed: Option<String> = None;
    while let Some(row) = rows.next()? {
        let id: String = row.get("session_id")?;
        if id != session {
            session = id;
            seed = None;
        }
        if row.get::<_, String>("kind")? == "host" {
            seed = row.get("seed")?;
            continue;
        }
        let Ok(line) = serde_json::from_str::<Inbound>(&row.get::<_, String>("payload")?) else {
            continue;
        };
        let Some((role, text)) = message_of(&line) else {
            continue;
        };
        let text = match role {
            Role::User => match without_seed(text, seed.as_deref()) {
                Some(text) => text,
                None => continue,
            },
            Role::Claude => text,
        };
        let time: String = row.get("time")?;
        insert(connection, &session, row.get("seq")?, &time, role, &text)?;
    }
    Ok(())
}

impl Store {
    /// How many messages match, and the page of them from `offset`, newest first.
    pub async fn search_messages(
        &self,
        query: MessageQuery,
        offset: usize,
        limit: usize,
    ) -> Result<(usize, Vec<StoredMessage>)> {
        self.call(move |connection| {
            let ids: Vec<String> = query.sessions.iter().map(Uuid::to_string).collect();
            let mut values = vec![
                SqlValue::Text(serde_json::to_string(&ids)?),
                SqlValue::Text(query.since.map(format_time).unwrap_or_default()),
                SqlValue::Text(query.before.map_or(NO_TIME_LIMIT.to_string(), format_time)),
            ];
            let mut filter = String::from(
                "FROM history_messages
                 WHERE session_id IN (SELECT value FROM json_each(?1))
                     AND time >= ?2 AND time < ?3",
            );
            for word in &query.words {
                values.push(SqlValue::Text(word.to_lowercase()));
                filter.push_str(&format!(" AND instr(folded, ?{}) > 0", values.len()));
            }
            let total: usize = connection.query_row(
                &format!("SELECT COUNT(*) {filter}"),
                params_from_iter(&values),
                |row| row.get(0),
            )?;
            values.push(SqlValue::Integer(i64::try_from(limit).unwrap_or(i64::MAX)));
            values.push(SqlValue::Integer(i64::try_from(offset).unwrap_or(i64::MAX)));
            let page = read_messages(
                connection,
                &format!(
                    "SELECT session_id, seq, time, role, text {filter}
                     ORDER BY time DESC, seq DESC LIMIT ?{} OFFSET ?{}",
                    values.len() - 1,
                    values.len()
                ),
                params_from_iter(&values),
            )?;
            Ok((total, page))
        })
        .await
    }

    /// The message of event `seq` of `session`, if the history holds one.
    pub async fn message(&self, session: Uuid, seq: u64) -> Result<Option<StoredMessage>> {
        self.call(move |connection| {
            let mut messages = read_messages(
                connection,
                "SELECT session_id, seq, time, role, text FROM history_messages
                 WHERE session_id = ?1 AND seq = ?2",
                params![session.to_string(), seq],
            )?;
            Ok(messages.pop())
        })
        .await
    }

    /// The messages of `before` exchanges before the one holding `seq`, that one,
    /// and `after` exchanges after it, in seq order. An exchange starts at a user message.
    pub async fn messages_around(
        &self,
        session: Uuid,
        seq: u64,
        before: u32,
        after: u32,
    ) -> Result<Vec<StoredMessage>> {
        self.call(move |connection| {
            let session = session.to_string();
            let user_seqs = |sql: &str, bound: u64, count: u32| -> Result<Vec<u64>> {
                let mut statement = connection.prepare(&format!(
                    "SELECT seq FROM history_messages
                     WHERE session_id = ?1 AND role = 'user' AND {sql} LIMIT ?3"
                ))?;
                let seqs = statement
                    .query_map(params![session, bound, count], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                Ok(seqs)
            };
            let start = user_seqs("seq <= ?2 ORDER BY seq DESC", seq, 1)?
                .first()
                .copied()
                .unwrap_or(0);
            let first = user_seqs("seq < ?2 ORDER BY seq DESC", start, before)?
                .last()
                .copied()
                .unwrap_or(start);
            let ends = user_seqs("seq > ?2 ORDER BY seq", start, after.saturating_add(1))?;
            let end = match ends.get(after as usize) {
                Some(end) => *end,
                None => i64::MAX as u64,
            };
            read_messages(
                connection,
                "SELECT session_id, seq, time, role, text FROM history_messages
                 WHERE session_id = ?1 AND seq >= ?2 AND seq < ?3 ORDER BY seq",
                params![session, first, end],
            )
        })
        .await
    }
}

fn read_messages(
    connection: &Connection,
    sql: &str,
    values: impl rusqlite::Params,
) -> Result<Vec<StoredMessage>> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(values, |row| {
        Ok((
            row.get::<_, String>("session_id")?,
            row.get::<_, u64>("seq")?,
            row.get::<_, String>("time")?,
            row.get::<_, String>("role")?,
            row.get::<_, String>("text")?,
        ))
    })?;
    rows.map(|row| {
        let (session, seq, time, role, text) = row?;
        Ok(StoredMessage {
            session: session.parse()?,
            seq,
            time: parse_time(&time)?,
            role: Role::parse(&role)?,
            text,
        })
    })
    .collect()
}

pub const SNIPPET_CHARS: usize = 200;
/// Characters a snippet shows before the first matched word.
const SNIPPET_LEAD: usize = 60;

/// About `SNIPPET_CHARS` characters of `text` on one line, from a little before
/// the first of `words` it contains.
pub fn snippet(text: &str, words: &[String]) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let folded = flat.to_lowercase();
    let first = words
        .iter()
        .filter_map(|word| folded.find(&word.to_lowercase()))
        .min()
        .unwrap_or(0);
    let start = folded[..first].chars().count().saturating_sub(SNIPPET_LEAD);
    let shown: String = flat.chars().skip(start).take(SNIPPET_CHARS).collect();
    let more = flat.chars().count() > start + SNIPPET_CHARS;
    format!(
        "{}{shown}{}",
        if start > 0 { "…" } else { "" },
        if more { "…" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use chrono::Utc;
    use serde_json::json;

    use super::super::testing::{Fixture, event, fixture};
    use super::*;
    use crate::session::seed::first_message;
    use crate::session::{Event, EventBody, HostEvent, Line, RestartMode};

    fn parsed(value: Value) -> EventBody {
        let raw = value.to_string();
        EventBody::Line(Box::new(Line {
            parsed: serde_json::from_str(&raw).ok(),
            raw,
        }))
    }

    fn user(text: &str) -> EventBody {
        parsed(
            json!({"type": "user", "message": {"role": "user", "content": text},
            "parent_tool_use_id": null, "session_id": "s"}),
        )
    }

    fn claude(blocks: Value) -> EventBody {
        parsed(
            json!({"type": "assistant", "message": {"id": "m", "content": blocks},
            "parent_tool_use_id": null, "session_id": "s", "uuid": "u"}),
        )
    }

    fn restarted(seed: &str) -> EventBody {
        EventBody::Host(HostEvent::Restarted {
            mode: RestartMode::Continue,
            exchanges: 1,
            total_exchanges: 1,
            seed: Some(seed.into()),
            moved_to: None,
        })
    }

    async fn all(store: &Store, id: Uuid) -> Vec<(u64, Role, String)> {
        store
            .messages_around(id, 0, u32::MAX, u32::MAX)
            .await
            .unwrap()
            .into_iter()
            .map(|message| (message.seq, message.role, message.text))
            .collect()
    }

    #[tokio::test]
    async fn messages_are_indexed_live_and_by_the_backfill_alike() {
        let Fixture {
            dir: _dir,
            path,
            store,
            id,
        } = fixture();
        let seed = "This is a new session. Its last exchanges.";
        let bodies = [
            user("Fix the parser"),
            claude(json!([{"type": "thinking", "thinking": "hidden"},
                {"type": "text", "text": "Reading it."},
                {"type": "tool_use", "id": "t", "name": "Read", "input": {}}])),
            parsed(
                json!({"type": "user", "parent_tool_use_id": null, "session_id": "s",
                "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t", "content": "file body"}]}}),
            ),
            claude(json!([{"type": "tool_use", "id": "t2", "name": "Bash", "input": {}}])),
            parsed(json!({"type": "assistant", "parent_tool_use_id": "t",
                "message": {"id": "a", "content": [{"type": "text", "text": "subagent"}]},
                "session_id": "s", "uuid": "u"})),
            parsed(
                json!({"type": "user", "isMeta": true, "parent_tool_use_id": null,
                "session_id": "s", "message": {"role": "user", "content": "meta"}}),
            ),
            user("[Request interrupted by user]"),
            user("<command-message>review</command-message>\n<command-name>/review</command-name>"),
            restarted(seed),
            user(&first_message(seed, "")),
            restarted(seed),
            user(&first_message(seed, "Go on")),
            claude(json!([{"type": "text", "text": "Done."}])),
            parsed(
                json!({"type": "user", "isSynthetic": true, "parent_tool_use_id": null,
                "session_id": "s", "message": {"role": "user",
                    "content": [{"type": "text", "text": "Base directory for this skill"}]}}),
            ),
        ];
        for (index, body) in bodies.into_iter().enumerate() {
            store.record_event(id, &event(index as u64 + 1, body));
        }
        let expected = vec![
            (1, Role::User, "Fix the parser".to_string()),
            (2, Role::Claude, "Reading it.".to_string()),
            (8, Role::User, "/review".to_string()),
            (12, Role::User, "Go on".to_string()),
            (13, Role::Claude, "Done.".to_string()),
        ];
        assert_eq!(all(&store, id).await, expected);
        store.flush().await.unwrap();
        drop(store);

        let connection = Connection::open(&path).unwrap();
        connection
            .execute("DELETE FROM history_messages", [])
            .unwrap();
        backfill(&connection).unwrap();
        // An index built before synthetic lines were skipped holds them until the migration.
        insert(&connection, &id.to_string(), 14, "t", Role::User, "skill").unwrap();
        connection.execute_batch(SYNTHETIC_MIGRATION).unwrap();
        drop(connection);
        let reopened = Store::open(&path).unwrap();
        assert_eq!(all(&reopened, id).await, expected);
    }

    #[tokio::test]
    async fn search_matches_every_word_ignoring_case_newest_first() {
        let Fixture {
            dir: _dir,
            store,
            id,
            ..
        } = fixture();
        let other = Uuid::new_v4();
        store.session_started(other, Path::new("/other"), Utc::now(), None);
        let day = |day: u32| format!("2026-09-{day:02}T10:00:00Z").parse().unwrap();
        let texts = [
            (id, 1, "Set up Prettier with the config file", 1),
            (id, 2, "prettier CONFIG done", 2),
            (id, 3, "Une écriture CONFIG Prettier", 3),
            (other, 1, "prettier config elsewhere", 4),
            (id, 4, "only prettier", 5),
        ];
        for (session, seq, text, at) in texts {
            store.record_event(
                session,
                &Event {
                    seq,
                    time: day(at),
                    body: user(text),
                },
            );
        }
        let query = |sessions: Vec<Uuid>, words: &[&str]| MessageQuery {
            sessions,
            words: words.iter().map(|word| word.to_string()).collect(),
            ..MessageQuery::default()
        };
        let seqs = |page: &[StoredMessage]| page.iter().map(|hit| hit.seq).collect::<Vec<_>>();

        let (total, page) = store
            .search_messages(query(vec![id], &["Config", "PRETTIER"]), 0, 10)
            .await
            .unwrap();
        assert_eq!((total, seqs(&page)), (3, vec![3, 2, 1]));
        let (total, page) = store
            .search_messages(query(vec![id, other], &["config"]), 1, 2)
            .await
            .unwrap();
        assert_eq!((total, seqs(&page)), (4, vec![3, 2]));
        let (_, page) = store
            .search_messages(query(vec![id], &["ÉCRITURE"]), 0, 10)
            .await
            .unwrap();
        assert_eq!(seqs(&page), [3]);
        let (total, page) = store
            .search_messages(
                MessageQuery {
                    since: Some(day(2)),
                    before: Some(day(3)),
                    ..query(vec![id], &["prettier"])
                },
                0,
                10,
            )
            .await
            .unwrap();
        assert_eq!((total, seqs(&page)), (1, vec![2]));
        let (total, page) = store
            .search_messages(query(vec![id], &["prettier"]), 10, 10)
            .await
            .unwrap();
        assert_eq!((total, page.len()), (4, 0));
    }

    #[tokio::test]
    async fn reading_around_a_message_counts_exchanges() {
        let Fixture {
            dir: _dir,
            store,
            id,
            ..
        } = fixture();
        // Exchanges start at 1, 3, 5, 7 and 9.
        for seq in 1..=10u64 {
            let body = if seq % 2 == 1 {
                user(&format!("ask {seq}"))
            } else {
                claude(json!([{"type": "text", "text": format!("answer {seq}")}]))
            };
            store.record_event(id, &event(seq, body));
        }
        let seqs = async |seq, before, after| {
            store
                .messages_around(id, seq, before, after)
                .await
                .unwrap()
                .iter()
                .map(|message| message.seq)
                .collect::<Vec<_>>()
        };
        assert_eq!(seqs(6, 1, 1).await, [3, 4, 5, 6, 7, 8]);
        assert_eq!(seqs(5, 0, 0).await, [5, 6]);
        assert_eq!(seqs(2, 5, 0).await, [1, 2]);
        assert_eq!(seqs(9, 1, 5).await, [7, 8, 9, 10]);
        let found = store.message(id, 4).await.unwrap().unwrap();
        assert_eq!(
            (found.role, found.text.as_str()),
            (Role::Claude, "answer 4")
        );
        assert_eq!(store.message(id, 11).await.unwrap(), None);
    }
}
