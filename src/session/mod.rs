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
pub mod background;
pub mod danger;
mod events;
pub mod gauges;
pub mod grant;
mod handle;
pub mod manager;
pub mod plan;
pub mod seed;

pub use activity::{Activity, ActivityTracker, LiveStatus};
pub use events::{
    Answer, CommandInfo, Event, EventBody, GateFailure, HostEvent, Line, ModelChoice, Resolution,
    RestartMode, StopReason,
};
pub use handle::{Config, Session};
