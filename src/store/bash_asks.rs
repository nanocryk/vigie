//! The `bash_asks` table: the Bash commands the user was asked about, one row per
//! command and session folder. Read and cleared through vigie2's MCP tools.

use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::Deserialize;

use super::{Store, format_time, parse_time};

/// Creates the table and fills it once from the asks already in `events`.
pub const MIGRATION: &str = r#"
CREATE TABLE bash_asks (
    id INTEGER PRIMARY KEY,
    command TEXT NOT NULL,
    cwd TEXT NOT NULL,
    allowed INTEGER NOT NULL,
    denied INTEGER NOT NULL,
    first_at TEXT NOT NULL,
    last_at TEXT NOT NULL,
    description TEXT,
    denials TEXT NOT NULL,
    UNIQUE (command, cwd)
) STRICT;
WITH asks AS (
    SELECT ask.session_id, ask.payload,
        (SELECT MIN(answer.seq) FROM events answer
         WHERE answer.session_id = ask.session_id AND answer.kind = 'host'
             AND json_extract(answer.payload, '$.event') = 'ask_resolved'
             AND answer.seq > ask.seq
             AND json_extract(answer.payload, '$.request_id')
                 = json_extract(ask.payload, '$.request_id')) AS answer_seq
    FROM events ask
    WHERE ask.kind = 'line'
        AND json_extract(ask.payload, '$.type') = 'control_request'
        AND json_extract(ask.payload, '$.request.subtype') = 'can_use_tool'
        AND json_extract(ask.payload, '$.request.tool_name') = 'Bash'
),
answered AS (
    SELECT json_extract(asks.payload, '$.request.input.command') AS command,
        sessions.cwd AS cwd,
        json_extract(asks.payload, '$.request.input.description') AS description,
        json_extract(answer.payload, '$.resolution.answer.kind') AS kind,
        json_extract(answer.payload, '$.resolution.answer.message') AS message,
        answer.time AS at
    FROM asks
    JOIN sessions ON sessions.id = asks.session_id
    JOIN events answer ON answer.session_id = asks.session_id AND answer.seq = asks.answer_seq
),
ranked AS (
    SELECT *, first_value(description) OVER (PARTITION BY command, cwd
        ORDER BY description IS NULL, at DESC) AS latest_description
    FROM answered
    WHERE kind IN ('allow', 'deny') AND typeof(command) = 'text'
    ORDER BY at
)
INSERT INTO bash_asks (command, cwd, allowed, denied, first_at, last_at, description, denials)
SELECT command, cwd, SUM(kind = 'allow'), SUM(kind = 'deny'), MIN(at), MAX(at),
    MAX(latest_description),
    COALESCE(json_group_array(json_object('message', message, 'at', at))
        FILTER (WHERE kind = 'deny' AND trim(COALESCE(message, '')) <> ''
            AND message <> 'The user denied this request.'), '[]')
FROM ranked
GROUP BY command, cwd;
"#;

/// The deny message the web sends when none was typed, `DEFAULT_DENY` in `AskCard.tsx`;
/// kept out of `denials` like a blank one, and repeated in `MIGRATION`.
const DEFAULT_DENY: &str = "The user denied this request.";

/// How the user answered one Bash ask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BashAnswer {
    Allowed,
    Denied { message: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct BashAsk {
    pub id: i64,
    pub command: String,
    pub cwd: PathBuf,
    pub allowed: u64,
    pub denied: u64,
    pub first_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
    pub description: Option<String>,
    /// The typed deny messages, oldest first.
    pub denials: Vec<Denial>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Denial {
    pub message: String,
    pub at: DateTime<Utc>,
}

impl Store {
    /// Counts one answered Bash ask on its command's row; a failure is logged.
    pub fn record_bash_ask(
        &self,
        command: String,
        cwd: &Path,
        description: Option<String>,
        answer: BashAnswer,
        at: DateTime<Utc>,
    ) {
        let cwd = cwd.display().to_string();
        self.write("bash ask", move |connection| {
            let at = format_time(at);
            let (allowed, denied, denial) = match answer {
                BashAnswer::Allowed => (1, 0, None),
                BashAnswer::Denied { message } => {
                    let denial = (!message.trim().is_empty() && message != DEFAULT_DENY)
                        .then(|| serde_json::json!({"message": message, "at": at}).to_string());
                    (0, 1, denial)
                }
            };
            connection.execute(
                "INSERT INTO bash_asks (command, cwd, allowed, denied, first_at, last_at,
                     description, denials)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6,
                     CASE WHEN ?7 IS NULL THEN '[]' ELSE json_array(json(?7)) END)
                 ON CONFLICT (command, cwd) DO UPDATE SET
                     allowed = allowed + excluded.allowed,
                     denied = denied + excluded.denied,
                     first_at = min(first_at, excluded.first_at),
                     last_at = max(last_at, excluded.last_at),
                     description = COALESCE(excluded.description, description),
                     denials = CASE WHEN ?7 IS NULL THEN denials
                         ELSE json_insert(denials, '$[#]', json(?7)) END",
                params![command, cwd, allowed, denied, at, description, denial],
            )?;
            Ok(())
        });
    }

    /// Every row, by folder then command.
    pub async fn bash_asks(&self) -> Result<Vec<BashAsk>> {
        self.call(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, command, cwd, allowed, denied, first_at, last_at, description, denials
                 FROM bash_asks ORDER BY cwd, command",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, i64>("id")?,
                    row.get::<_, String>("command")?,
                    row.get::<_, String>("cwd")?,
                    row.get::<_, u64>("allowed")?,
                    row.get::<_, u64>("denied")?,
                    row.get::<_, String>("first_at")?,
                    row.get::<_, String>("last_at")?,
                    row.get::<_, Option<String>>("description")?,
                    row.get::<_, String>("denials")?,
                ))
            })?;
            let mut asks = Vec::new();
            for row in rows {
                let (id, command, cwd, allowed, denied, first_at, last_at, description, denials) =
                    row?;
                asks.push(BashAsk {
                    id,
                    command,
                    cwd: cwd.into(),
                    allowed,
                    denied,
                    first_at: parse_time(&first_at)?,
                    last_at: parse_time(&last_at)?,
                    description,
                    denials: serde_json::from_str(&denials)?,
                });
            }
            Ok(asks)
        })
        .await
    }

    /// Deletes these rows; resolves to how many were there.
    pub async fn delete_bash_asks(&self, ids: Vec<i64>) -> Result<usize> {
        self.call(move |connection| {
            let mut statement = connection.prepare("DELETE FROM bash_asks WHERE id = ?1")?;
            let mut deleted = 0;
            for id in ids {
                deleted += statement.execute([id])?;
            }
            Ok(deleted)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use chrono::SubsecRound;
    use rusqlite::Connection;

    use super::super::testing::fixture;
    use super::super::{MIGRATIONS, migrate};
    use super::*;

    #[tokio::test]
    async fn answers_add_up_on_one_row_per_command_and_folder() {
        let fixture = fixture();
        let store = &fixture.store;
        let other = fixture.dir.path().join("other");
        let [first, second, third] =
            [0, 1, 2].map(|hours| Utc::now().trunc_subsecs(6) + chrono::Duration::hours(hours));
        store.record_bash_ask(
            "ls".into(),
            fixture.dir.path(),
            Some("List".into()),
            BashAnswer::Allowed,
            first,
        );
        store.record_bash_ask(
            "ls".into(),
            fixture.dir.path(),
            None,
            BashAnswer::Denied {
                message: "use Glob".into(),
            },
            second,
        );
        store.record_bash_ask(
            "ls".into(),
            fixture.dir.path(),
            Some("List files".into()),
            BashAnswer::Denied {
                message: "  ".into(),
            },
            third,
        );
        store.record_bash_ask(
            "ls".into(),
            fixture.dir.path(),
            None,
            BashAnswer::Denied {
                message: DEFAULT_DENY.into(),
            },
            second,
        );
        store.record_bash_ask("ls".into(), &other, None, BashAnswer::Allowed, first);

        let asks = store.bash_asks().await.unwrap();
        assert_eq!(asks.len(), 2);
        let row = asks
            .iter()
            .find(|ask| ask.cwd == fixture.dir.path())
            .unwrap();
        assert_eq!((row.allowed, row.denied), (1, 3));
        assert_eq!((row.first_at, row.last_at), (first, third));
        assert_eq!(row.description.as_deref(), Some("List files"));
        assert_eq!(
            row.denials,
            [Denial {
                message: "use Glob".into(),
                at: second
            }]
        );

        let other_id = asks.iter().find(|ask| ask.cwd == other).unwrap().id;
        assert_eq!(
            store.delete_bash_asks(vec![other_id, 999]).await.unwrap(),
            1
        );
        assert_eq!(store.bash_asks().await.unwrap(), std::slice::from_ref(row));
    }

    #[tokio::test]
    async fn migration_fills_the_table_from_answered_bash_asks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let version = MIGRATIONS
            .iter()
            .position(|migration| *migration == MIGRATION)
            .unwrap();
        {
            let connection = Connection::open(&path).unwrap();
            for migration in &MIGRATIONS[..version] {
                connection.execute_batch(migration).unwrap();
            }
            connection
                .pragma_update(None, "user_version", version)
                .unwrap();
            connection
                .execute(
                    "INSERT INTO sessions (id, cwd, created_at, updated_at, state)
                     VALUES ('s', 'C:\\p', 't', 't', 'stopped')",
                    [],
                )
                .unwrap();
            let ask = |id: &str, tool: &str, command: &str| {
                format!(
                    r#"{{"type":"control_request","request_id":"{id}","request":{{"subtype":"can_use_tool","tool_name":"{tool}","input":{{"command":"{command}","description":"Run {id}"}}}}}}"#
                )
            };
            let resolved = |id: &str, resolution: &str| {
                format!(
                    r#"{{"event":"ask_resolved","request_id":"{id}","resolution":{resolution}}}"#
                )
            };
            let allow = r#"{"kind":"answered","answer":{"kind":"allow"}}"#;
            let deny = r#"{"kind":"answered","answer":{"kind":"deny","message":"no"}}"#;
            let events = [
                ("line", ask("a", "Bash", "cargo test")),
                ("host", resolved("a", allow)),
                ("line", ask("b", "Bash", "cargo test")),
                ("host", resolved("b", deny)),
                // Only the first resolution after the ask counts.
                ("host", resolved("b", allow)),
                ("line", ask("c", "Bash", "rm x")),
                ("host", resolved("c", r#"{"kind":"cancelled"}"#)),
                ("line", ask("d", "Read", "cargo test")),
                ("host", resolved("d", allow)),
                ("line", ask("e", "Bash", "git push")),
                ("line", ask("f", "Bash", "cargo test")),
                (
                    "host",
                    resolved(
                        "f",
                        &format!(
                            r#"{{"kind":"answered","answer":{{"kind":"deny","message":"{DEFAULT_DENY}"}}}}"#
                        ),
                    ),
                ),
            ];
            for (seq, (kind, payload)) in events.iter().enumerate() {
                connection
                    .execute(
                        "INSERT INTO events (session_id, seq, time, kind, payload)
                         VALUES ('s', ?1, ?2, ?3, ?4)",
                        params![
                            seq + 1,
                            format!("2026-09-30T10:00:{:02}.000000Z", seq + 1),
                            kind,
                            payload
                        ],
                    )
                    .unwrap();
            }
            migrate(&connection).unwrap();
        }
        let store = Store::open(&path).unwrap();
        let asks = store.bash_asks().await.unwrap();
        assert_eq!(asks.len(), 1, "{asks:?}");
        let row = &asks[0];
        assert_eq!(row.command, "cargo test");
        assert_eq!(row.cwd, Path::new("C:\\p"));
        assert_eq!((row.allowed, row.denied), (1, 2));
        assert_eq!(row.first_at, parse_time("2026-09-30T10:00:02Z").unwrap());
        assert_eq!(row.last_at, parse_time("2026-09-30T10:00:12Z").unwrap());
        assert_eq!(row.description.as_deref(), Some("Run f"));
        assert_eq!(
            row.denials,
            [Denial {
                message: "no".into(),
                at: parse_time("2026-09-30T10:00:04Z").unwrap()
            }]
        );
    }
}
