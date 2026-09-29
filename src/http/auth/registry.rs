//! The paired devices, kept in `devices.json` under an advisory file lock.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::state_dir;

const LAST_SEEN_THROTTLE_SECS: i64 = 60;

#[derive(Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub label: String,
    pub issued_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    #[serde(default)]
    pub revoked: bool,
    #[serde(default)]
    pub last_ip: Option<String>,
}

impl Device {
    /// An active device issued and last seen at `now`, with no known address.
    pub fn new(id: String, label: String, now: DateTime<Utc>) -> Self {
        Self {
            id,
            label,
            issued_at: now,
            last_seen: now,
            revoked: false,
            last_ip: None,
        }
    }

    fn seen_stale(&self, now: DateTime<Utc>) -> bool {
        (now - self.last_seen).num_seconds() >= LAST_SEEN_THROTTLE_SECS
    }

    fn ip_changed(&self, ip: Option<&String>) -> bool {
        ip.is_some() && self.last_ip.as_ref() != ip
    }
}

#[derive(Default, Serialize, Deserialize)]
struct RegistryFile {
    devices: Vec<Device>,
}

pub enum RevokeResult {
    Revoked,
    NotFound,
    AlreadyRevoked,
}

pub enum RemoveResult {
    Removed,
    NotFound,
    NotRevoked,
}

pub struct Registry {
    path: PathBuf,
    devices: Mutex<Vec<Device>>,
}

impl Registry {
    pub fn load(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir {}", parent.display()))?;
        }
        let devices = read_locked(&path)?;
        Ok(Self {
            path,
            devices: Mutex::new(devices),
        })
    }

    /// Re-read the on-disk registry into memory. Required when a second
    /// process (`vigie2 auth revoke-all`) rewrites the file: without it the
    /// running server keeps honouring cookies that were revoked on disk.
    pub fn reload(&self) -> Result<()> {
        let devices = read_locked(&self.path)?;
        *self.devices.lock() = devices;
        Ok(())
    }

    pub fn find_active(&self, id: &str) -> Option<Device> {
        self.devices
            .lock()
            .iter()
            .find(|device| device.id == id && !device.revoked)
            .cloned()
    }

    #[cfg(test)]
    pub fn has_active(&self) -> bool {
        self.devices.lock().iter().any(|device| !device.revoked)
    }

    pub fn next_label(&self) -> String {
        let number = self.devices.lock().len() + 1;
        format!("device {number}")
    }

    pub fn insert(&self, device: Device) -> Result<()> {
        self.with_file(|devices| {
            devices.push(device);
            ((), true)
        })
    }

    pub fn list(&self) -> Vec<Device> {
        self.devices.lock().clone()
    }

    pub fn rename(&self, id: &str, label: &str) -> Result<bool> {
        self.with_file(
            |devices| match devices.iter_mut().find(|device| device.id == id) {
                Some(device) => {
                    device.label = label.to_string();
                    (true, true)
                }
                None => (false, false),
            },
        )
    }

    pub fn revoke(&self, id: &str) -> Result<RevokeResult> {
        self.with_file(
            |devices| match devices.iter_mut().find(|device| device.id == id) {
                None => (RevokeResult::NotFound, false),
                Some(device) if device.revoked => (RevokeResult::AlreadyRevoked, false),
                Some(device) => {
                    device.revoked = true;
                    (RevokeResult::Revoked, true)
                }
            },
        )
    }

    /// Permanently delete a device record. Only allowed for already-revoked
    /// entries; active devices must be revoked first.
    pub fn remove(&self, id: &str) -> Result<RemoveResult> {
        self.with_file(
            |devices| match devices.iter().position(|device| device.id == id) {
                None => (RemoveResult::NotFound, false),
                Some(index) if !devices[index].revoked => (RemoveResult::NotRevoked, false),
                Some(index) => {
                    devices.remove(index);
                    (RemoveResult::Removed, true)
                }
            },
        )
    }

    pub fn revoke_all(&self) -> Result<usize> {
        self.with_file(|devices| {
            let mut count = 0;
            for device in devices.iter_mut().filter(|device| !device.revoked) {
                device.revoked = true;
                count += 1;
            }
            (count, count > 0)
        })
    }

    /// Throttled: only takes the file lock if `last_seen` is stale beyond
    /// `LAST_SEEN_THROTTLE_SECS`, or if `ip` differs from the recorded one.
    /// Runs on every authed request, so the no-op path stays in memory.
    pub fn touch(&self, id: &str, ip: Option<String>) {
        let now = Utc::now();
        let stale = self.devices.lock().iter().any(|device| {
            device.id == id && (device.ip_changed(ip.as_ref()) || device.seen_stale(now))
        });
        if !stale {
            return;
        }
        let written = self.with_file(|devices| {
            let Some(device) = devices.iter_mut().find(|device| device.id == id) else {
                return (false, false);
            };
            let seen_stale = device.seen_stale(now);
            if !device.ip_changed(ip.as_ref()) && !seen_stale {
                return (false, false);
            }
            if seen_stale {
                device.last_seen = now;
            }
            if let Some(ip) = ip {
                device.last_ip = Some(ip);
            }
            (true, true)
        });
        if let Err(err) = written {
            warn!(%err, "registry persist (touch) failed");
        }
    }

    /// Read-modify-write the registry file under one exclusive advisory lock,
    /// then refresh the in-memory cache from the result. `apply` returns its
    /// own result plus whether the vector needs writing back.
    ///
    /// Re-reading inside the lock is load-bearing: without it a mutation here
    /// writes a stale in-memory copy over whatever `vigie2 auth revoke-all`
    /// just did from another process, silently un-revoking those devices.
    fn with_file<F, R>(&self, apply: F) -> Result<R>
    where
        F: FnOnce(&mut Vec<Device>) -> (R, bool),
    {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir {}", parent.display()))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&self.path)
            .with_context(|| format!("open {}", self.path.display()))?;
        file.lock().context("lock registry")?;
        let outcome = (|| -> Result<R> {
            let mut devices = read_devices(&mut file, &self.path)?;
            let (result, changed) = apply(&mut devices);
            if changed {
                let contents = serde_json::to_vec_pretty(&RegistryFile {
                    devices: devices.clone(),
                })?;
                file.set_len(0)?;
                file.seek(SeekFrom::Start(0))?;
                file.write_all(&contents)?;
                file.sync_all()?;
                state_dir::restrict_to_owner(&self.path)?;
            }
            *self.devices.lock() = devices;
            Ok(result)
        })();
        file.unlock().ok();
        outcome
    }
}

/// Read the registry from an already-open, already-locked handle.
fn read_devices(file: &mut File, path: &Path) -> Result<Vec<Device>> {
    file.seek(SeekFrom::Start(0)).context("seek registry")?;
    let mut contents = Vec::new();
    file.read_to_end(&mut contents)
        .with_context(|| format!("read {}", path.display()))?;
    if contents.is_empty() {
        return Ok(Vec::new());
    }
    let parsed: RegistryFile =
        serde_json::from_slice(&contents).with_context(|| format!("parse {}", path.display()))?;
    Ok(parsed.devices)
}

/// Read the registry under a shared advisory lock, so a reader never observes
/// the half-written file of a concurrent writer.
fn read_locked(path: &Path) -> Result<Vec<Device>> {
    let mut file = match OpenOptions::new().read(true).open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("open {}", path.display())),
    };
    file.lock_shared().context("lock registry")?;
    let result = read_devices(&mut file, path);
    file.unlock().ok();
    result
}

#[cfg(test)]
pub mod tests {
    //! Two `Registry` values over one path stand in for the two processes that
    //! really share `devices.json`: the running server, and a `vigie2 auth
    //! revoke-all` invocation.
    use super::*;

    pub fn device(id: &str) -> Device {
        Device::new(id.to_string(), id.to_string(), Utc::now())
    }

    fn revoke_all_from_another_process(path: &Path) {
        Registry::load(path.to_path_buf())
            .expect("load cli registry")
            .revoke_all()
            .expect("revoke all");
    }

    #[test]
    fn external_revoke_survives_a_later_server_write() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("devices.json");

        let server = Registry::load(path.clone()).expect("load server registry");
        server.insert(device("phone")).expect("insert phone");

        revoke_all_from_another_process(&path);

        // Server writes again from its own, now stale, in-memory copy.
        server.insert(device("laptop")).expect("insert laptop");

        let on_disk = Registry::load(path).expect("reload from disk");
        let phone = on_disk
            .list()
            .into_iter()
            .find(|device| device.id == "phone")
            .expect("phone still present");
        assert!(
            phone.revoked,
            "server write clobbered an external revoke-all",
        );
    }

    #[test]
    fn touch_does_not_resurrect_an_externally_revoked_device() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("devices.json");

        let server = Registry::load(path.clone()).expect("load server registry");
        let mut stale = device("phone");
        // Stale enough that touch takes the file lock instead of no-opping.
        stale.last_seen = Utc::now() - chrono::Duration::seconds(LAST_SEEN_THROTTLE_SECS * 2);
        server.insert(stale).expect("insert phone");

        revoke_all_from_another_process(&path);
        server.touch("phone", Some("10.0.0.1".into()));

        let on_disk = Registry::load(path).expect("reload from disk");
        assert!(on_disk.find_active("phone").is_none());
    }

    #[test]
    fn reload_picks_up_an_external_revoke() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("devices.json");

        let server = Registry::load(path.clone()).expect("load server registry");
        server.insert(device("phone")).expect("insert phone");
        assert!(server.find_active("phone").is_some());

        revoke_all_from_another_process(&path);
        assert!(
            server.find_active("phone").is_some(),
            "in-memory copy should still be stale before reload",
        );

        server.reload().expect("reload");
        assert!(server.find_active("phone").is_none());
    }

    #[test]
    fn revoke_all_reports_only_devices_it_actually_revoked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("devices.json");

        let registry = Registry::load(path).expect("load registry");
        registry.insert(device("phone")).expect("insert phone");
        registry.insert(device("laptop")).expect("insert laptop");

        assert_eq!(registry.revoke_all().expect("first revoke-all"), 2);
        assert_eq!(registry.revoke_all().expect("second revoke-all"), 0);
        assert!(!registry.has_active());
    }
}
