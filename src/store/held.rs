//! The `held_messages` table: messages kept for a discussion until sent.

use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{DateTime, SubsecRound, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Store, format_time, json_error, parse_time};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeldMode {
    /// Waits for the user to send it.
    Held,
    /// Sent when the main agent's turn ends normally.
    TurnEnd,
}

impl HeldMode {
    const ALL: [Self; 2] = [Self::Held, Self::TurnEnd];

    /// The `mode` column, as serde names the variant.
    fn as_str(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::TurnEnd => "turn_end",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|mode| mode.as_str() == text)
            .with_context(|| format!("unknown held mode {text}"))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeldMessage {
    pub id: Uuid,
    pub text: String,
    /// Absolute paths of the attached files.
    pub attachments: Vec<String>,
    pub mode: HeldMode,
    pub created_at: DateTime<Utc>,
}

/// A change to a held message; an absent field keeps its value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HeldEdit {
    pub text: Option<String>,
    pub attachments: Option<Vec<String>>,
    pub mode: Option<HeldMode>,
}

impl Store {
    /// Queues a message last on the discussion.
    pub async fn hold(
        &self,
        session: Uuid,
        text: String,
        attachments: Vec<String>,
        mode: HeldMode,
    ) -> Result<HeldMessage> {
        let held = HeldMessage {
            id: Uuid::new_v4(),
            text,
            attachments,
            mode,
            // The store keeps microseconds; a message reads back equal.
            created_at: Utc::now().trunc_subsecs(6),
        };
        let stored = held.clone();
        self.call(move |connection| {
            connection.execute(
                "INSERT INTO held_messages (id, session_id, position, text, attachments, mode,
                     created_at)
                 VALUES (?1, ?2,
                     (SELECT COALESCE(MAX(position), 0) + 1 FROM held_messages
                      WHERE session_id = ?2),
                     ?3, ?4, ?5, ?6)",
                params![
                    stored.id.to_string(),
                    session.to_string(),
                    stored.text,
                    serde_json::to_string(&stored.attachments).map_err(json_error)?,
                    stored.mode.as_str(),
                    format_time(stored.created_at),
                ],
            )?;
            Ok(())
        })
        .await?;
        Ok(held)
    }

    /// Resolves to whether the message was still queued.
    pub async fn edit_held(&self, session: Uuid, id: Uuid, edit: HeldEdit) -> Result<bool> {
        self.call(move |connection| {
            let attachments = edit
                .attachments
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(json_error)?;
            let changed = connection.execute(
                "UPDATE held_messages SET text = COALESCE(?3, text),
                     attachments = COALESCE(?4, attachments), mode = COALESCE(?5, mode)
                 WHERE session_id = ?1 AND id = ?2",
                params![
                    session.to_string(),
                    id.to_string(),
                    edit.text,
                    attachments,
                    edit.mode.map(HeldMode::as_str),
                ],
            )?;
            Ok(changed > 0)
        })
        .await
    }

    /// Removes one message; resolves to it, `None` when it was no longer queued.
    pub async fn take_held(&self, session: Uuid, id: Uuid) -> Result<Option<HeldMessage>> {
        self.call(move |connection| {
            let held = read_held(
                connection,
                "WHERE session_id = ?1 AND id = ?2",
                params![session.to_string(), id.to_string()],
            )?;
            connection.execute(
                "DELETE FROM held_messages WHERE session_id = ?1 AND id = ?2",
                params![session.to_string(), id.to_string()],
            )?;
            Ok(held.into_values().flatten().next())
        })
        .await
    }

    /// Removes the discussion's `TurnEnd` messages; resolves to them in queue order.
    pub async fn take_turn_end(&self, session: Uuid) -> Result<Vec<HeldMessage>> {
        self.call(move |connection| {
            let held = read_held(
                connection,
                "WHERE session_id = ?1 AND mode = 'turn_end'",
                params![session.to_string()],
            )?;
            connection.execute(
                "DELETE FROM held_messages WHERE session_id = ?1 AND mode = 'turn_end'",
                params![session.to_string()],
            )?;
            Ok(held.into_values().flatten().collect())
        })
        .await
    }

    /// Switches the discussion's `TurnEnd` messages to `Held`; resolves to whether any changed.
    pub async fn hold_turn_end(&self, session: Uuid) -> Result<bool> {
        self.call(move |connection| {
            let changed = connection.execute(
                "UPDATE held_messages SET mode = 'held'
                 WHERE session_id = ?1 AND mode = 'turn_end'",
                params![session.to_string()],
            )?;
            Ok(changed > 0)
        })
        .await
    }
}

/// The held messages matching `filter`, per session in queue order.
pub fn read_held(
    connection: &Connection,
    filter: &str,
    params: impl rusqlite::Params,
) -> Result<HashMap<Uuid, Vec<HeldMessage>>> {
    let mut statement = connection.prepare(&format!(
        "SELECT session_id, id, text, attachments, mode, created_at FROM held_messages
         {filter} ORDER BY session_id, position"
    ))?;
    let rows = statement.query_map(params, |row| {
        Ok((
            row.get::<_, String>("session_id")?,
            row.get::<_, String>("id")?,
            row.get::<_, String>("text")?,
            row.get::<_, String>("attachments")?,
            row.get::<_, String>("mode")?,
            row.get::<_, String>("created_at")?,
        ))
    })?;
    let mut held: HashMap<Uuid, Vec<HeldMessage>> = HashMap::new();
    for row in rows {
        let (session, id, text, attachments, mode, created_at) = row?;
        held.entry(Uuid::parse_str(&session)?)
            .or_default()
            .push(HeldMessage {
                id: Uuid::parse_str(&id)?,
                text,
                attachments: serde_json::from_str(&attachments)?,
                mode: HeldMode::parse(&mode)?,
                created_at: parse_time(&created_at)?,
            });
    }
    Ok(held)
}

#[cfg(test)]
mod tests {
    use super::super::testing::fixture;
    use super::*;

    #[tokio::test]
    async fn held_messages_queue_edit_and_leave() {
        let fixture = fixture();
        let (store, session) = (&fixture.store, fixture.id);
        let first = store
            .hold(session, "one".into(), vec![], HeldMode::TurnEnd)
            .await
            .unwrap();
        let second = store
            .hold(
                session,
                "two".into(),
                vec!["C:\\a.png".into()],
                HeldMode::Held,
            )
            .await
            .unwrap();
        let third = store
            .hold(session, "three".into(), vec![], HeldMode::TurnEnd)
            .await
            .unwrap();
        let row = store.require_session(session).await.unwrap();
        assert_eq!(row.held, [first.clone(), second.clone(), third.clone()]);

        let edit = HeldEdit {
            text: Some("one!".into()),
            ..HeldEdit::default()
        };
        assert!(store.edit_held(session, first.id, edit).await.unwrap());
        let taken = store.take_turn_end(session).await.unwrap();
        assert_eq!(
            taken
                .iter()
                .map(|held| held.text.as_str())
                .collect::<Vec<_>>(),
            ["one!", "three"]
        );
        assert!(
            !store
                .edit_held(session, first.id, HeldEdit::default())
                .await
                .unwrap()
        );

        let edit = HeldEdit {
            mode: Some(HeldMode::TurnEnd),
            ..HeldEdit::default()
        };
        assert!(store.edit_held(session, second.id, edit).await.unwrap());
        assert!(store.hold_turn_end(session).await.unwrap());
        assert!(!store.hold_turn_end(session).await.unwrap());
        assert_eq!(
            store.require_session(session).await.unwrap().held,
            std::slice::from_ref(&second)
        );
        assert_eq!(
            store.take_held(session, second.id).await.unwrap(),
            Some(second.clone())
        );
        assert_eq!(store.take_held(session, second.id).await.unwrap(), None);
        assert!(
            store
                .require_session(session)
                .await
                .unwrap()
                .held
                .is_empty()
        );
    }
}
