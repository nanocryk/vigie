//! Restarting the whole server, which only works under the `serve.sh` shim: the
//! server exits with `EXIT_CODE` and the shim runs it again.

use anyhow::{Result, bail};
use tokio::sync::watch;

/// Exit code asking the shim to run the server again.
pub const EXIT_CODE: u8 = 75;
/// Set by the shim on the server it runs.
const SUPERVISED_VAR: &str = "VIGIE2_SUPERVISED";
/// Set by the shim when copying the new build failed and it ran the previous copy.
const STALE_COPY_VAR: &str = "VIGIE2_STALE_COPY";

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

/// Which build the shim runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunningBuild {
    Latest,
    /// Copying the latest build failed, so the previous copy runs.
    Previous,
}

impl RunningBuild {
    pub fn from_env() -> Self {
        if std::env::var_os(STALE_COPY_VAR).is_some_and(|value| value == "1") {
            Self::Previous
        } else {
            Self::Latest
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartState {
    Idle,
    /// Restart once no turn runs; messages sent meanwhile are held.
    Pending,
    /// Stop the server now and exit with `EXIT_CODE`.
    Now,
}

#[derive(Debug, Clone)]
pub struct ServerRestart {
    supervision: Supervision,
    running_build: RunningBuild,
    state: watch::Sender<RestartState>,
}

impl ServerRestart {
    pub fn new(supervision: Supervision) -> Self {
        Self {
            supervision,
            running_build: RunningBuild::Latest,
            state: watch::channel(RestartState::Idle).0,
        }
    }

    pub fn with_running_build(self, running_build: RunningBuild) -> Self {
        Self {
            running_build,
            ..self
        }
    }

    pub fn supervision(&self) -> Supervision {
        self.supervision
    }

    pub fn running_build(&self) -> RunningBuild {
        self.running_build
    }

    pub fn state(&self) -> RestartState {
        *self.state.borrow()
    }

    pub fn subscribe(&self) -> watch::Receiver<RestartState> {
        self.state.subscribe()
    }

    /// Asks for a restart now; refused when no shim would run the server again.
    pub fn now(&self) -> Result<()> {
        self.ensure_supervised()?;
        self.state.send_replace(RestartState::Now);
        Ok(())
    }

    /// Asks for a restart once no turn runs; a restart already under way stays as is.
    pub fn when_idle(&self) -> Result<()> {
        self.ensure_supervised()?;
        self.state.send_if_modified(|state| {
            let idle = *state == RestartState::Idle;
            if idle {
                *state = RestartState::Pending;
            }
            idle
        });
        Ok(())
    }

    /// Drops a pending restart; resolves to whether one was pending.
    pub fn cancel(&self) -> bool {
        self.state.send_if_modified(|state| {
            let pending = *state == RestartState::Pending;
            if pending {
                *state = RestartState::Idle;
            }
            pending
        })
    }

    /// Turns a pending restart into a restart now; resolves to whether one was pending.
    pub fn fire_pending(&self) -> bool {
        self.state.send_if_modified(|state| {
            let pending = *state == RestartState::Pending;
            if pending {
                *state = RestartState::Now;
            }
            pending
        })
    }

    fn ensure_supervised(&self) -> Result<()> {
        if self.supervision == Supervision::None {
            bail!("the server was not started by serve.sh, so it cannot restart itself");
        }
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

    #[test]
    fn a_pending_restart_can_be_cancelled_but_not_one_under_way() {
        let restart = ServerRestart::new(Supervision::Shim);
        assert!(ServerRestart::new(Supervision::None).when_idle().is_err());
        restart.when_idle().unwrap();
        assert_eq!(restart.state(), RestartState::Pending);
        assert!(restart.cancel());
        assert_eq!(restart.state(), RestartState::Idle);

        restart.now().unwrap();
        restart.when_idle().unwrap();
        assert!(!restart.cancel());
        assert_eq!(restart.state(), RestartState::Now);
    }
}
