//! One long-lived `claude` child per session, driven by an actor task.
//!
//! Commands go in through `Session`. Every stdout line and a few host events come
//! out on a broadcast channel, numbered by a per-session sequence. The
//! submodules track what the session runs and decides, and `manager` holds
//! every session.

mod activity;
mod actor;
pub mod advice;
pub mod agents;
pub mod attachments;
pub mod background;
pub mod danger;
mod events;
pub mod fanout;
pub mod gauges;
pub mod grant;
mod handle;
pub mod manager;
pub mod mcp;
pub mod plan;
pub mod scratch;
pub mod seed;
pub mod server_restart;

pub use activity::{Activity, ActivityTracker, LiveStatus};
pub use events::{
    Answer, CommandInfo, Event, EventBody, GateFailure, HostEvent, Line, ModelChoice, Resolution,
    RestartMode, StopReason,
};
pub use handle::{Config, Session};
