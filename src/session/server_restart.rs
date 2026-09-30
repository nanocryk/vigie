//! Restarting the whole server, which only works under the `serve.sh` shim: the
//! server exits with `EXIT_CODE` and the shim runs it again.

use anyhow::{Result, bail};
use tokio::sync::watch;

/// Exit code asking the shim to run the server again.
pub const EXIT_CODE: u8 = 75;
/// Set by the shim on the server it runs.
const SUPERVISED_VAR: &str = "VIGIE2_SUPERVISED";

/// Whether a shim runs the server again when it exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Supervision {
    Shim,
    None,
}

impl Supervision {
    pub fn from_env() -> Self {
        if std::env::var_os(SUPERVISED_VAR).is_some_and(|value| value == "1") {
            Self::Shim
        } else {
            Self::None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartState {
    Idle,
    /// Stop the server now and exit with `EXIT_CODE`.
    Now,
}

#[derive(Clone)]
pub struct ServerRestart {
    supervision: Supervision,
    state: watch::Sender<RestartState>,
}

impl ServerRestart {
    pub fn new(supervision: Supervision) -> Self {
        Self {
            supervision,
            state: watch::channel(RestartState::Idle).0,
        }
    }

    pub fn supervision(&self) -> Supervision {
        self.supervision
    }

    pub fn state(&self) -> RestartState {
        *self.state.borrow()
    }

    pub fn subscribe(&self) -> watch::Receiver<RestartState> {
        self.state.subscribe()
    }

    /// Asks for a restart now; refused when no shim would run the server again.
    pub fn now(&self) -> Result<()> {
        if self.supervision == Supervision::None {
            bail!("the server was not started by serve.sh, so it cannot restart itself");
        }
        self.state.send_replace(RestartState::Now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_supervised_server_restarts() {
        let alone = ServerRestart::new(Supervision::None);
        assert!(alone.now().is_err());
        assert_eq!(alone.state(), RestartState::Idle);

        let supervised = ServerRestart::new(Supervision::Shim);
        let receiver = supervised.subscribe();
        supervised.now().unwrap();
        assert_eq!(*receiver.borrow(), RestartState::Now);
    }
}
