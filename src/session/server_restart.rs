//! Restarting the whole server, which only works under the `serve.sh` shim: the
//! server exits with `EXIT_CODE` and the shim runs it again.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Result, bail};
use parking_lot::Mutex;
use ring::digest;
use tokio::sync::watch;

/// Exit code asking the shim to run the server again.
pub const EXIT_CODE: u8 = 75;
/// Set by the shim on the server it runs.
const SUPERVISED_VAR: &str = "VIGIE_SUPERVISED";
/// Set by the shim when copying the new build failed and it ran the previous copy.
const STALE_COPY_VAR: &str = "VIGIE_STALE_COPY";
/// Set by the shim: the latest build, which it copies and runs on a restart.
const BUILT_VAR: &str = "VIGIE_BUILT";

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

/// Whether the latest build differs from the binary running now, by content.
#[derive(Debug)]
pub struct NewBuild {
    /// The latest build: what a restart would run.
    built: PathBuf,
    /// SHA-256 of the running binary, taken at startup.
    running: Vec<u8>,
    /// The last answer, for the build's size and modification time then.
    last: Mutex<Option<(u64, SystemTime, bool)>>,
}

impl NewBuild {
    /// Under the shim, which names the latest build in `BUILT_VAR`.
    pub fn from_env() -> Option<Self> {
        let built = PathBuf::from(std::env::var_os(BUILT_VAR)?);
        let running = std::env::current_exe().ok()?;
        match Self::new(built, &running) {
            Ok(check) => Some(check),
            Err(err) => {
                tracing::warn!("hash the running binary: {err}");
                None
            }
        }
    }

    pub fn new(built: PathBuf, running: &Path) -> io::Result<Self> {
        Ok(Self {
            built,
            running: file_hash(running)?,
            last: Mutex::new(None),
        })
    }

    /// Hashes the build again unless its size and modification time are unchanged.
    /// Blocking; false when the build cannot be read.
    pub fn ready(&self) -> bool {
        let Ok(meta) = std::fs::metadata(&self.built) else {
            return false;
        };
        let Ok(modified) = meta.modified() else {
            return false;
        };
        let mut last = self.last.lock();
        if let Some((len, time, ready)) = *last
            && len == meta.len()
            && time == modified
        {
            return ready;
        }
        let ready = file_hash(&self.built).is_ok_and(|hash| hash != self.running);
        *last = Some((meta.len(), modified, ready));
        ready
    }
}

fn file_hash(path: &Path) -> io::Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)?;
    let mut context = digest::Context::new(&digest::SHA256);
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(context.finish().as_ref().to_vec());
        }
        context.update(&buffer[..read]);
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
    new_build: Option<Arc<NewBuild>>,
    state: watch::Sender<RestartState>,
}

impl ServerRestart {
    pub fn new(supervision: Supervision) -> Self {
        Self {
            supervision,
            running_build: RunningBuild::Latest,
            new_build: None,
            state: watch::channel(RestartState::Idle).0,
        }
    }

    pub fn with_running_build(self, running_build: RunningBuild) -> Self {
        Self {
            running_build,
            ..self
        }
    }

    pub fn with_new_build(self, new_build: Option<NewBuild>) -> Self {
        Self {
            new_build: new_build.map(Arc::new),
            ..self
        }
    }

    /// The check for a build newer than the running binary, under the shim.
    pub fn new_build(&self) -> Option<Arc<NewBuild>> {
        self.new_build.clone()
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

    #[test]
    fn a_build_differing_from_the_running_binary_is_ready() {
        let tmp = tempfile::tempdir().unwrap();
        let running = tmp.path().join("running");
        let built = tmp.path().join("built");
        std::fs::write(&running, b"first").unwrap();
        std::fs::write(&built, b"first").unwrap();
        let check = NewBuild::new(built.clone(), &running).unwrap();
        assert!(!check.ready(), "the same bytes");
        std::fs::write(&built, b"second build").unwrap();
        assert!(check.ready());
        std::fs::remove_file(&built).unwrap();
        assert!(!check.ready(), "no build to run");
    }
}
