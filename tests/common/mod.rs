//! Shared by the integration tests that drive the fake claude.

use std::path::Path;
use std::time::Duration;

use vigie2::child::Program;

pub const TIMEOUT: Duration = Duration::from_secs(20);

/// The fake claude replaying `recording`.
pub fn fake(recording: &Path) -> Program {
    Program {
        path: env!("CARGO_BIN_EXE_fake-claude").into(),
        leading_args: vec![recording.into()],
    }
}
