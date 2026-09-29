//! The `events` table: every event of every session, and pages of them.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Store, format_time, parse_time};
use crate::claude::protocol::Inbound;
use crate::session::gauges::{MainRequest, Windows};
use crate::session::{Event, EventBody, HostEvent, StopReason};
use crate::usage::db::{MessageTokens, UsageDb};
use crate::usage::inputs::{self, Reading};
use crate::usage::units::Timestamp;

/// The seq of the `?3`-th latest (0 for the latest) `user_sent` of `?1` below `?2`
/// that started a turn: one sent mid-turn would split its turn at a page edge.
const NTH_USER_SENT: &str = "SELECT seq FROM events
    WHERE session_id = ?1 AND kind = 'host' AND json_extract(payload, '$.event') = 'user_sent'
        AND json_extract(payload, '$.mid_turn') IS NULL AND seq < ?2
    ORDER BY seq DESC LIMIT 1 OFFSET ?3";

/// The latest host event of `?1` named `?2` below `?3`.
const LATEST_HOST_EVENT: &str = "SELECT seq, time, kind, payload FROM events
    WHERE session_id = ?1 AND kind = 'host' AND json_extract(payload, '$.event') = ?2
        AND seq < ?3
    ORDER BY seq DESC LIMIT 1";

/// A `seq` bound above every stored one; SQLite integers are signed.
const NO_SEQ_LIMIT: u64 = i64::MAX as u64;

/// Host events whose latest one before a page sets fold state the page needs.
const PRELUDE_EVENTS: [&str; 4] = ["started", "permission_mode", "catalog", "restarted"];

/// Where a page of history starts, and what comes before it.
#[derive(Debug, Clone, PartialEq)]
pub struct PageStart {
    /// A `user_sent` event, or 1 when fewer exchanges precede the bound.
    pub first_seq: u64,
    /// No event precedes `first_seq`.
    pub start_reached: bool,
    /// The latest event of each `PRELUDE_EVENTS` kind before `first_seq`, in seq order.
    pub prelude: Vec<StoredEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// `payload` is a CLI stdout line exactly as received.
    Line,
    /// `payload` is a serialized `HostEvent`.
    Host,
}

impl EventKind {
    const ALL: [Self; 2] = [Self::Line, Self::Host];

    /// The `kind` column, as serde names the variant.
    fn as_str(self) -> &'static str {
        match self {
            Self::Line => "line",
            Self::Host => "host",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == text)
            .with_context(|| format!("unknown event kind {text}"))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    pub seq: u64,
    pub time: DateTime<Utc>,
    pub kind: EventKind,
    pub payload: String,
}

/// What a line tells about the gauges of the main conversation.
enum GaugeUpdate {
    Request(MainRequest),
    Windows(Windows),
}

impl GaugeUpdate {
    fn from_line(line: &Inbound) -> Option<Self> {
        match line {
            Inbound::Assistant(assistant) => {
                MainRequest::from_assistant(assistant).map(Self::Request)
            }
            Inbound::Result(result) => Some(Self::Windows(Windows::from_result(result))),
            _ => None,
        }
    }

    fn apply(self, connection: &Connection, session: &str, time: &str) -> Result<()> {
        match self {
            // SQLite evaluates every assignment against the row as it was, so the
            // time and the source only move on the first line of a response.
            Self::Request(request) => {
                connection.execute(
                    "UPDATE sessions SET
                         cache_request_at = CASE WHEN cache_message_id IS ?2
                             THEN cache_request_at ELSE ?3 END,
                         context_source = CASE WHEN cache_message_id IS ?2
                             THEN context_source ELSE 'estimate' END,
                         settings_changed_at = CASE WHEN cache_message_id IS ?2
                             THEN settings_changed_at ELSE NULL END,
                         cache_message_id = ?2,
                         cache_ttl_secs = COALESCE(?4, cache_ttl_secs),
                         context_model = COALESCE(?5, context_model),
                         context_tokens = ?6
                     WHERE id = ?1",
                    params![
                        session,
                        request.message_id,
                        time,
                        request.ttl_secs,
                        request.model,
                        request.prompt_tokens
                    ],
                )?;
            }
            Self::Windows(windows) => {
                let model: Option<String> = connection
                    .query_row(
                        "SELECT context_model FROM sessions WHERE id = ?1",
                        [session],
                        |row| row.get(0),
                    )
                    .optional()?
                    .flatten();
                if let Some(window) = windows.for_model(model.as_deref()) {
                    connection.execute(
                        "UPDATE sessions SET context_window = ?2 WHERE id = ?1",
                        params![session, window],
                    )?;
                }
            }
        }
        Ok(())
    }
}

impl Store {
    pub fn record_event(&self, session: Uuid, event: &Event) {
        let seq = event.seq;
        let time = format_time(event.time);
        let mut permission_mode = None;
        let mut readings = Vec::new();
        let mut message = None;
        let mut gauge = None;
        let (kind, payload) = match &event.body {
            EventBody::Line(line) => {
                if let Some(parsed) = &line.parsed {
                    match parsed {
                        Inbound::RateLimitEvent(limits) => {
                            readings = inputs::from_rate_limit_event(&limits.rate_limit_info);
                        }
                        Inbound::Assistant(assistant) => {
                            message = inputs::from_assistant(assistant)
                        }
                        _ => {}
                    }
                    gauge = GaugeUpdate::from_line(parsed);
                }
                (EventKind::Line, line.raw.clone())
            }
            EventBody::Host(host) => {
                if let HostEvent::PermissionMode { mode } = host {
                    permission_mode = Some(String::from(mode.clone()));
                }
                match serde_json::to_string(host) {
                    Ok(json) => (EventKind::Host, json),
                    Err(err) => {
                        tracing::warn!("serialize host event {seq}: {err}");
                        return;
                    }
                }
            }
        };
        let at = Timestamp::from_datetime(event.time);
        self.write("event", move |connection| {
            let id = session.to_string();
            connection.execute(
                "INSERT INTO events (session_id, seq, time, kind, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, seq, time, kind.as_str(), payload],
            )?;
            connection.execute(
                "UPDATE sessions SET last_seq = ?2, updated_at = ?3,
                     permission_mode = COALESCE(?4, permission_mode)
                 WHERE id = ?1",
                params![id, seq, time, permission_mode],
            )?;
            if let Some(gauge) = gauge {
                gauge.apply(connection, &id, &time)?;
            }
            record_usage_inputs(connection, session, at, &readings, message.as_ref())
        });
    }

    /// Why the discussion's latest child was stopped; `None` when it exited
    /// without being asked to, or never ran.
    pub async fn latest_stop_reason(&self, session: Uuid) -> Result<Option<StopReason>> {
        self.call(move |connection| {
            let Some(raw) = connection
                .query_row(
                    LATEST_HOST_EVENT,
                    params![session.to_string(), "child_exited", NO_SEQ_LIMIT],
                    RawEvent::read,
                )
                .optional()?
            else {
                return Ok(None);
            };
            match serde_json::from_str(&raw.parse()?.payload)? {
                HostEvent::ChildExited { reason, .. } => Ok(reason),
                _ => Ok(None),
            }
        })
        .await
    }

    /// Up to `limit` events with a seq above `after`, in seq order.
    pub async fn events_after(
        &self,
        session: Uuid,
        after: u64,
        limit: u32,
    ) -> Result<Vec<StoredEvent>> {
        self.call(move |connection| {
            let mut statement = connection.prepare(
                "SELECT seq, time, kind, payload FROM events
                 WHERE session_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
            )?;
            let rows =
                statement.query_map(params![session.to_string(), after, limit], RawEvent::read)?;
            rows.map(|row| row?.parse()).collect()
        })
        .await
    }

    /// Every event with a seq from `first` to below `before`, in seq order.
    pub async fn events_between(
        &self,
        session: Uuid,
        first: u64,
        before: u64,
    ) -> Result<Vec<StoredEvent>> {
        self.call(move |connection| {
            let mut statement = connection.prepare(
                "SELECT seq, time, kind, payload FROM events
                 WHERE session_id = ?1 AND seq >= ?2 AND seq < ?3 ORDER BY seq",
            )?;
            let rows = statement.query_map(
                params![
                    session.to_string(),
                    first.min(NO_SEQ_LIMIT),
                    before.min(NO_SEQ_LIMIT)
                ],
                RawEvent::read,
            )?;
            rows.map(|row| row?.parse()).collect()
        })
        .await
    }

    /// Start of the page holding the last `exchanges` exchanges below `before`
    /// (`None`: the latest ones). An exchange starts at a `user_sent` event.
    pub async fn page_start(
        &self,
        session: Uuid,
        before: Option<u64>,
        exchanges: u32,
    ) -> Result<PageStart> {
        let before = before.unwrap_or(NO_SEQ_LIMIT).min(NO_SEQ_LIMIT);
        self.call(move |connection| {
            let session = session.to_string();
            let first_seq: u64 = connection
                .query_row(
                    NTH_USER_SENT,
                    params![session, before, exchanges.max(1) - 1],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or(1);
            let mut latest = connection.prepare(LATEST_HOST_EVENT)?;
            let mut prelude = Vec::new();
            for name in PRELUDE_EVENTS {
                if let Some(raw) = latest
                    .query_row(params![session, name, first_seq], RawEvent::read)
                    .optional()?
                {
                    prelude.push(raw.parse()?);
                }
            }
            prelude.sort_by_key(|event| event.seq);
            Ok(PageStart {
                first_seq,
                start_reached: first_seq <= 1,
                prelude,
            })
        })
        .await
    }
}

#[cfg(test)]
impl Store {
    /// Stores the CLI stdout lines of recording `name` as events of `session`,
    /// numbered from `after_seq + 1`, each at `start` plus its recorded offset.
    /// Returns the last seq.
    pub fn record_recording(
        &self,
        session: Uuid,
        name: &str,
        after_seq: u64,
        start: DateTime<Utc>,
    ) -> u64 {
        use crate::claude::replay::{Direction, Recording};
        use crate::session::Line;

        let recording = Recording::named(name).expect("recording");
        let mut seq = after_seq;
        for step in recording
            .steps
            .iter()
            .filter(|step| step.dir == Direction::Out)
        {
            seq += 1;
            let raw = step.msg.to_string();
            let body = EventBody::Line(Box::new(Line {
                parsed: serde_json::from_str(&raw).ok(),
                raw,
            }));
            let time = start + chrono::Duration::milliseconds(step.t as i64);
            self.record_event(session, &Event { seq, time, body });
        }
        seq
    }
}

fn record_usage_inputs(
    connection: &Connection,
    session: Uuid,
    at: Timestamp,
    readings: &[Reading],
    message: Option<&MessageTokens>,
) -> Result<()> {
    let db = UsageDb::new(connection);
    for reading in readings {
        db.record(reading.kind, reading.state, Some(session), at)?;
    }
    if let Some(message) = message {
        db.record_message(session, at, message)?;
    }
    Ok(())
}

/// An `events` row, read by column name.
struct RawEvent {
    seq: u64,
    time: String,
    kind: String,
    payload: String,
}

impl RawEvent {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            seq: row.get("seq")?,
            time: row.get("time")?,
            kind: row.get("kind")?,
            payload: row.get("payload")?,
        })
    }

    fn parse(self) -> Result<StoredEvent> {
        Ok(StoredEvent {
            seq: self.seq,
            time: parse_time(&self.time)?,
            kind: EventKind::parse(&self.kind)?,
            payload: self.payload,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::{Fixture, event, fixture, line};
    use super::*;
    use crate::claude::child::Launch;
    use crate::session::gauges::{self, Cache};
    use crate::session::{Line, RestartMode};

    #[test]
    fn stored_kinds_are_the_serde_names() {
        for kind in EventKind::ALL {
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
            assert_eq!(EventKind::parse(kind.as_str()).unwrap(), kind);
        }
    }

    #[tokio::test]
    async fn gauges_follow_the_main_conversation_and_survive_a_reopen() {
        let Fixture {
            dir: _dir,
            path,
            store,
            id,
        } = fixture();
        let start = parse_time("2026-09-27T10:00:00Z").unwrap();
        store.record_recording(id, "cd", 0, start);
        let row = store.session(id).await.unwrap().unwrap();
        // The last response streams two lines, at 7557 and 7579 ms.
        assert_eq!(
            row.cache,
            Some(Cache {
                last_request_at: start + chrono::Duration::milliseconds(7557),
                ttl_secs: gauges::ONE_HOUR_TTL_SECS,
            })
        );
        let context = row.context.expect("a context estimate");
        // input 2, cache read 28990, cache creation 2056.
        assert_eq!(context.used_tokens, 31_048);
        assert_eq!(context.max_tokens, 1_000_000);

        // A response that writes nothing keeps the TTL; its time still counts.
        let empty_write = r#"{"type":"assistant","parent_tool_use_id":null,"session_id":"s","uuid":"u",
            "message":{"id":"msg_x","model":"claude-opus-5-5","content":[],
            "usage":{"input_tokens":5,"cache_read_input_tokens":40000,"cache_creation_input_tokens":0,
            "cache_creation":{"ephemeral_1h_input_tokens":0,"ephemeral_5m_input_tokens":0}}}}"#;
        let later = start + chrono::Duration::minutes(3);
        store.record_event(
            id,
            &Event {
                seq: 1000,
                time: later,
                body: EventBody::Line(Box::new(Line {
                    raw: empty_write.into(),
                    parsed: serde_json::from_str(empty_write).ok(),
                })),
            },
        );
        store.flush().await.unwrap();
        drop(store);

        let reopened = Store::open(&path).unwrap();
        let row = reopened.session(id).await.unwrap().unwrap();
        assert_eq!(
            row.cache,
            Some(Cache {
                last_request_at: later,
                ttl_secs: gauges::ONE_HOUR_TTL_SECS,
            })
        );
        assert_eq!(row.context.map(|context| context.used_tokens), Some(40_005));
    }

    fn host(seq: u64, host: HostEvent) -> Event {
        event(seq, EventBody::Host(host))
    }

    fn user_sent(seq: u64, text: &str) -> Event {
        host(
            seq,
            HostEvent::UserSent {
                text: text.into(),
                client_id: None,
                mid_turn: false,
            },
        )
    }

    fn mode(seq: u64, mode: &str) -> Event {
        host(seq, HostEvent::PermissionMode { mode: mode.into() })
    }

    #[tokio::test]
    async fn a_page_starts_at_a_user_message_after_the_latest_state_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let store = Store::open(&path).unwrap();
        let id = Uuid::new_v4();
        let cwd = dir.path().join("project");
        store.session_started(id, &cwd, Utc::now(), None);
        let started = |seq| {
            host(
                seq,
                HostEvent::Started {
                    launch: Launch::New(id),
                    cwd: cwd.clone(),
                },
            )
        };
        let events = [
            started(1),
            host(
                2,
                HostEvent::Catalog {
                    models: Vec::new(),
                    commands: Vec::new(),
                },
            ),
            mode(3, "default"),
            user_sent(4, "a"),
            event(5, line("not json")),
            mode(6, "plan"),
            user_sent(7, "b"),
            event(8, line(r#"{"type":"result"}"#)),
            host(
                9,
                HostEvent::Restarted {
                    mode: RestartMode::Blank,
                    exchanges: 0,
                    total_exchanges: 2,
                    seed: None,
                },
            ),
            started(10),
            user_sent(11, "c"),
            event(12, line(r#"{"type":"result"}"#)),
        ];
        for event in &events {
            store.record_event(id, event);
        }
        let seqs =
            |events: &[StoredEvent]| events.iter().map(|event| event.seq).collect::<Vec<_>>();

        let last = store.page_start(id, None, 1).await.unwrap();
        assert_eq!((last.first_seq, last.start_reached), (11, false));
        assert_eq!(seqs(&last.prelude), [2, 6, 9, 10]);

        let two = store.page_start(id, None, 2).await.unwrap();
        assert_eq!((two.first_seq, two.start_reached), (7, false));
        assert_eq!(seqs(&two.prelude), [1, 2, 6]);

        let before = store.page_start(id, Some(7), 5).await.unwrap();
        assert_eq!((before.first_seq, before.start_reached), (1, true));
        assert!(before.prelude.is_empty());
        let page = store.events_between(id, 1, 7).await.unwrap();
        assert_eq!(seqs(&page), [1, 2, 3, 4, 5, 6]);

        let exact = store.page_start(id, Some(7), 1).await.unwrap();
        assert_eq!((exact.first_seq, exact.start_reached), (4, false));
        assert_eq!(seqs(&exact.prelude), [1, 2, 3]);
        assert_eq!(
            store.page_start(Uuid::new_v4(), None, 20).await.unwrap(),
            PageStart {
                first_seq: 1,
                start_reached: true,
                prelude: Vec::new()
            }
        );

        // A message sent mid-turn starts no exchange: the page still starts at 11.
        store.record_event(
            id,
            &host(
                13,
                HostEvent::UserSent {
                    text: "also".into(),
                    client_id: None,
                    mid_turn: true,
                },
            ),
        );
        let queued = store.page_start(id, None, 1).await.unwrap();
        assert_eq!(queued.first_seq, 11);

        store.flush().await.unwrap();
        let connection = Connection::open(&path).unwrap();
        for sql in [NTH_USER_SENT, LATEST_HOST_EVENT] {
            let plan: Vec<String> = connection
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap()
                .query_map(params![id.to_string(), "user_sent", 7], |row| row.get(3))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            assert!(
                plan.iter().any(|step| step.contains("INDEX events_host")),
                "{sql}: {plan:?}"
            );
        }
    }
}
