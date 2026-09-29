//! The `usage_samples` table: `get_usage` replies, and the usage snapshot fed from them.

use anyhow::Result;
use chrono::{DateTime, Local, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use super::{Store, format_time, parse_time};
use crate::claude::protocol::GetUsageResponse;
use crate::usage::db::UsageDb;
use crate::usage::inputs;
use crate::usage::units::Timestamp;
use crate::usage::{self, SessionContext, Snapshot};

#[derive(Debug, Clone, PartialEq)]
pub struct UsageSample {
    pub time: DateTime<Utc>,
    pub session_id: Option<Uuid>,
    /// The `get_usage` response payload as JSON text.
    pub response: String,
}

impl Store {
    pub fn record_usage(&self, time: DateTime<Utc>, session: Option<Uuid>, response: String) {
        let readings = match serde_json::from_str::<GetUsageResponse>(&response) {
            Ok(parsed) => inputs::from_get_usage(&parsed),
            Err(err) => {
                tracing::debug!("get_usage response without readings: {err}");
                Vec::new()
            }
        };
        self.write("usage sample", move |connection| {
            connection.execute(
                "INSERT INTO usage_samples (time, session_id, response) VALUES (?1, ?2, ?3)",
                params![
                    format_time(time),
                    session.map(|id| id.to_string()),
                    response
                ],
            )?;
            let db = UsageDb::new(connection);
            let at = Timestamp::from_datetime(time);
            for reading in readings {
                db.record(reading.kind, reading.state, session, at)?;
            }
            Ok(())
        });
    }

    pub async fn latest_usage(&self) -> Result<Option<UsageSample>> {
        self.call(|connection| {
            connection
                .query_row(
                    "SELECT time, session_id, response FROM usage_samples
                     ORDER BY time DESC, id DESC LIMIT 1",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>("time")?,
                            row.get::<_, Option<String>>("session_id")?,
                            row.get::<_, String>("response")?,
                        ))
                    },
                )
                .optional()?
                .map(|(time, session, response)| {
                    Ok(UsageSample {
                        time: parse_time(&time)?,
                        session_id: session.as_deref().map(Uuid::parse_str).transpose()?,
                        response,
                    })
                })
                .transpose()
        })
        .await
    }

    /// Folds what has elapsed and projects both windows, reporting on `sessions`.
    /// Local time decides which hour of the week a moment falls in.
    pub async fn usage_snapshot(
        &self,
        sessions: Vec<SessionContext>,
        now: DateTime<Utc>,
    ) -> Result<Snapshot> {
        self.call(move |connection| {
            usage::snapshot(
                &UsageDb::new(connection),
                &sessions,
                Timestamp::from_datetime(now),
                &Local,
            )
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::event;
    use super::*;
    use crate::claude::replay::Recording;
    use crate::session::{EventBody, Line};

    /// The `out` lines of the `basic` recording, and its `get_usage` answer.
    fn basic_recording() -> (Vec<String>, String) {
        let recording = Recording::named("basic").unwrap();
        let lines = recording.out().map(serde_json::Value::to_string).collect();
        let usage = recording
            .out()
            .find(|msg| msg["response"]["response"]["rate_limits_available"] == true)
            .map(|msg| msg["response"]["response"].to_string())
            .unwrap();
        (lines, usage)
    }

    #[tokio::test]
    async fn usage_inputs_are_recorded_as_they_arrive() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("history.db")).unwrap();
        let id = Uuid::new_v4();
        let cwd = dir.path().join("project");
        store.session_started(id, &cwd, Utc::now(), None);
        let (lines, usage) = basic_recording();
        for (seq, raw) in lines.iter().enumerate() {
            let body = EventBody::Line(Box::new(Line {
                raw: raw.clone(),
                parsed: serde_json::from_str(raw).ok(),
            }));
            store.record_event(id, &event(seq as u64 + 1, body));
        }
        store.record_usage(Utc::now(), Some(id), usage);

        let recorded_at = parse_time("2026-09-27T11:05:00Z").unwrap();
        let context = SessionContext {
            id,
            cwd,
            created_at: recorded_at,
            permission_mode: Some("bypassPermissions".into()),
        };
        let snapshot = store
            .usage_snapshot(vec![context], recorded_at)
            .await
            .unwrap();
        let five_hour = snapshot.five_hour.expect("a 5h window");
        assert_eq!(five_hour.used, 7.0);
        assert_eq!(
            five_hour.resets_at,
            parse_time("2026-09-27T13:00:00Z").unwrap()
        );
        assert!(five_hour.projected >= 7.0);
        let seven_day = snapshot.seven_day.expect("a 7d window");
        assert_eq!(seven_day.used, 51.0);
        assert_eq!(snapshot.history.readings, 2, "the sample repeats the event");
        let session = &snapshot.sessions[0];
        assert!(session.bypass);
        assert_eq!(session.model_mix.len(), 1);
        assert_eq!(session.model_mix[0].share, 1.0);
    }
}
