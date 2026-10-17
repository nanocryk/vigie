//! The paired devices, kept in `devices.json` under an advisory file lock.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::tokens::{mint_token, same_token, secret_hash};
use crate::state_dir;

const LAST_SEEN_THROTTLE_SECS: i64 = 60;

/// A device's `id` is public: listed to every paired device and used in URLs.
/// Its cookie carries a separate secret, of which only the hash is kept.
#[derive(Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    /// Absent on a device whose cookie still carries its id, from before ids
    /// and secrets were split.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secret_hash: Option<String>,
    /// The secret issued to replace an id cookie, kept until the device first
    /// presents it, so concurrent requests carrying the id all get the same one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_secret: Option<String>,
    pub label: String,
    pub issued_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    #[serde(default)]
    pub revoked: bool,
    #[serde(default)]
    pub last_ip: Option<String>,
}

impl Device {
    /// An active device issued and last seen at `now`, with no known address,
    /// authenticated by `secret`.
    pub fn new(id: String, label: String, secret: &str, now: DateTime<Utc>) -> Self {
        Self {
            id,
            secret_hash: Some(secret_hash(secret)),
            pending_secret: None,
            label,
            issued_at: now,
            last_seen: now,
            revoked: false,
            last_ip: None,
        }
    }

    fn has_secret(&self, hash: &str) -> bool {
        self.secret_hash.as_deref() == Some(hash)
    }

    /// Whether a cookie carrying the device id still authenticates it.
    fn accepts_id_cookie(&self, presented: &str) -> bool {
        (self.secret_hash.is_none() || self.pending_secret.is_some())
            && same_token(&self.id, presented)
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
    /// process (`vigie auth revoke-all`) rewrites the file: without it the
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

    /// The active device a cookie value authenticates, without changing it.
    pub fn peek(&self, presented: &str) -> Option<Device> {
        let hash = secret_hash(presented);
        self.devices
            .lock()
            .iter()
            .find(|device| {
                !device.revoked && (device.has_secret(&hash) || device.accepts_id_cookie(presented))
            })
            .cloned()
    }

    /// The active device a cookie value authenticates, and the value its
    /// cookie carries from now on. An id cookie is answered with a fresh
    /// secret, and stops authenticating once the device presents that secret.
    pub fn authenticate(&self, presented: &str) -> Option<(Device, String)> {
        let device = self.peek(presented)?;
        let hash = secret_hash(presented);
        if device.has_secret(&hash) {
            if device.pending_secret.is_some() {
                let confirmed = self.with_file(|devices| {
                    match devices.iter_mut().find(|known| known.has_secret(&hash)) {
                        Some(known) if known.pending_secret.is_some() => {
                            known.pending_secret = None;
                            ((), true)
                        }
                        _ => ((), false),
                    }
                });
                if let Err(err) = confirmed {
                    warn!(%err, "registry persist (confirm secret) failed");
                }
            }
            return Some((device, presented.to_string()));
        }
        let rotated = self.with_file(|devices| {
            let Some(known) = devices
                .iter_mut()
                .find(|known| !known.revoked && known.accepts_id_cookie(presented))
            else {
                return (None, false);
            };
            if let Some(secret) = &known.pending_secret {
                return (Some((known.clone(), secret.clone())), false);
            }
            let secret = mint_token();
            known.secret_hash = Some(secret_hash(&secret));
            known.pending_secret = Some(secret.clone());
            (Some((known.clone(), secret)), true)
        });
        match rotated {
            Ok(rotated) => rotated,
            Err(err) => {
                warn!(%err, "registry persist (rotate secret) failed");
                Some((device, presented.to_string()))
            }
        }
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
    /// writes a stale in-memory copy over whatever `vigie auth revoke-all`
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
    //! really share `devices.json`: the running server, and a `vigie auth
    //! revoke-all` invocation.
    use super::*;

    pub fn device(id: &str) -> Device {
        Device::new(
            id.to_string(),
            id.to_string(),
            &format!("{id}-secret"),
            Utc::now(),
        )
    }

    /// A device whose cookie carries its id, as paired before secrets.
    fn id_cookie_device(id: &str) -> Device {
        Device {
            secret_hash: None,
            ..device(id)
        }
    }

    fn registry(dir: &tempfile::TempDir) -> Registry {
        Registry::load(dir.path().join("devices.json")).expect("load registry")
    }

    #[test]
    fn a_secret_authenticates_and_the_public_id_does_not() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry = registry(&tmp);
        registry.insert(device("phone")).expect("insert");

        let (found, cookie) = registry.authenticate("phone-secret").expect("secret");
        assert_eq!(found.id, "phone");
        assert_eq!(cookie, "phone-secret");
        assert!(registry.authenticate("phone").is_none(), "id authenticated");
    }

    #[test]
    fn an_id_cookie_is_swapped_for_a_secret_shared_until_presented() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry = registry(&tmp);
        registry.insert(id_cookie_device("phone")).expect("insert");

        let (_, first) = registry.authenticate("phone").expect("id cookie");
        assert_ne!(first, "phone");
        let (_, second) = registry
            .authenticate("phone")
            .expect("concurrent id cookie");
        assert_eq!(first, second, "concurrent requests got different secrets");

        let (_, kept) = registry.authenticate(&first).expect("new secret");
        assert_eq!(kept, first);
        assert!(
            registry.authenticate("phone").is_none(),
            "id cookie still accepted after the secret was presented",
        );
        let on_disk = Registry::load(tmp.path().join("devices.json")).expect("reload");
        assert!(
            on_disk.authenticate(&first).is_some(),
            "secret not persisted"
        );
    }

    #[test]
    fn a_revoked_device_authenticates_by_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry = registry(&tmp);
        registry.insert(device("phone")).expect("insert");
        registry.insert(id_cookie_device("tablet")).expect("insert");
        registry.revoke_all().expect("revoke");

        assert!(registry.authenticate("phone-secret").is_none());
        assert!(registry.authenticate("tablet").is_none());
        assert!(registry.peek("tablet").is_none());
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
