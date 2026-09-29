//! The `claude` CLI: its stream-json wire, its child process, and a fake that
//! replays recordings in its place.

pub mod child;
pub mod protocol;
#[cfg(any(test, feature = "fake-claude"))]
pub mod replay;
