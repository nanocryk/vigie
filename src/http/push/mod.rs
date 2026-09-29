//! Web Push: VAPID (RFC 8292) and aes128gcm message encryption (RFC 8291), on
//! `ring`, sent with `reqwest`. The P-256 key pair is generated on first start
//! and kept in the state directory; browsers subscribe with its public key.

mod crypto;
mod routes;

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use tracing::warn;

pub use crypto::Vapid;
pub use routes::{config, subscribe, test, unsubscribe};

use crate::http::auth::Auth;
use crate::session::manager::{CACHE_WARNING_BEFORE, Notice, NoticeKind, SessionManager};
use crate::store::Store;

/// How long a push service keeps a message for an offline device.
const MESSAGE_TTL_SECS: &str = "3600";
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushKeys {
    pub p256dh: String,
    pub auth: String,
}

/// What `pushManager.subscribe` returns, as the browser serializes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushSubscription {
    pub endpoint: String,
    pub keys: PushKeys,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOutcome {
    Ok,
    /// The subscription is dead (404 or 410): drop it.
    Gone,
    /// A 5xx or another refusal; logged.
    Transient,
}

/// What a notification says; the service worker shows it, one per `tag`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub title: String,
    pub body: String,
    /// A new message with the same tag replaces the shown one.
    pub tag: String,
    /// Opened on a tap, relative to the app.
    pub url: String,
}

/// Closes the notification tagged `clear` without showing anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clear {
    pub clear: String,
}

/// Sends messages to every subscribed device.
pub struct Pusher {
    vapid: Vapid,
    client: reqwest::Client,
    store: Store,
    auth: Arc<Auth>,
    holding: Mutex<Holding>,
}

/// The devices each tag was delivered to since its last clear.
#[derive(Debug, Default)]
pub struct Holding {
    devices: HashMap<String, HashSet<String>>,
}

impl Holding {
    pub fn delivered(&mut self, tag: &str, device: String) {
        self.devices
            .entry(tag.to_string())
            .or_default()
            .insert(device);
    }

    /// The devices to clear `tag` on; forgets them.
    pub fn take(&mut self, tag: &str) -> HashSet<String> {
        self.devices.remove(tag).unwrap_or_default()
    }
}

impl Pusher {
    pub fn new(vapid: Vapid, store: Store, auth: Arc<Auth>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(SEND_TIMEOUT)
            // Push services answer directly; a redirect could lead past the endpoint check.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("build the push client")?;
        Ok(Self {
            vapid,
            client,
            store,
            auth,
            holding: Mutex::new(Holding::default()),
        })
    }

    /// The VAPID public key browsers subscribe with.
    pub fn public_key(&self) -> &str {
        self.vapid.public_key_b64url()
    }

    /// Sends `message` to every subscription of an active device; drops dead ones.
    pub async fn broadcast(&self, message: &Message) {
        let delivered = self.send_all(message, |_| true).await;
        let mut holding = self.holding.lock();
        for device in delivered {
            holding.delivered(&message.tag, device);
        }
    }

    /// Closes notification `tag` on the devices it was delivered to since its last clear.
    pub async fn clear(&self, tag: &str) {
        let devices = self.holding.lock().take(tag);
        if devices.is_empty() {
            return;
        }
        let clear = Clear {
            clear: tag.to_string(),
        };
        self.send_all(&clear, |device| devices.contains(device))
            .await;
    }

    /// Sends `message` to the subscriptions of active devices `wanted` keeps;
    /// returns the devices it reached and drops dead subscriptions.
    async fn send_all(
        &self,
        message: &impl Serialize,
        wanted: impl Fn(&str) -> bool,
    ) -> Vec<String> {
        let subscriptions = match self.store.push_subscriptions().await {
            Ok(subscriptions) => subscriptions,
            Err(err) => {
                warn!("read push subscriptions: {err:#}");
                return Vec::new();
            }
        };
        let payload = match serde_json::to_vec(message) {
            Ok(payload) => payload,
            Err(err) => {
                warn!("serialize push message: {err}");
                return Vec::new();
            }
        };
        let sends = subscriptions
            .into_iter()
            .filter(|(device, _)| {
                wanted(device) && self.auth.registry.find_active(device).is_some()
            })
            .map(|(device, subscription)| {
                let payload = &payload;
                async move {
                    let outcome = self.send(&subscription, payload).await;
                    (device, outcome)
                }
            });
        let mut delivered = Vec::new();
        for (device, outcome) in futures_util::future::join_all(sends).await {
            match outcome {
                Ok(SendOutcome::Ok) => delivered.push(device),
                Ok(SendOutcome::Transient) => {}
                Ok(SendOutcome::Gone) => {
                    if let Err(err) = self.store.remove_push_subscription(&device).await {
                        warn!("drop the push subscription of a device: {err:#}");
                    }
                }
                Err(err) => warn!("push send: {err:#}"),
            }
        }
        delivered
    }

    /// Encrypts `payload` for `subscription` and posts it to its endpoint.
    pub async fn send(
        &self,
        subscription: &PushSubscription,
        payload: &[u8],
    ) -> Result<SendOutcome> {
        // Checked again here, so a stored subscription cannot aim the server at its own network.
        validate_endpoint(&subscription.endpoint)
            .map_err(|reason| anyhow!("bad endpoint: {reason}"))?;
        let ua_public = URL_SAFE_NO_PAD
            .decode(subscription.keys.p256dh.as_bytes())
            .context("decode p256dh")?;
        let auth_secret = URL_SAFE_NO_PAD
            .decode(subscription.keys.auth.as_bytes())
            .context("decode auth")?;
        if ua_public.len() != 65 || ua_public[0] != 0x04 {
            bail!("p256dh must be a 65-byte uncompressed P-256 point");
        }
        if auth_secret.len() < 16 {
            bail!("auth secret too short");
        }
        let body = self
            .vapid
            .encrypt(&ua_public, &auth_secret, payload)
            .context("aes128gcm encrypt")?;
        let audience = crypto::endpoint_origin(&subscription.endpoint)?;
        let jwt = self.vapid.sign_jwt(&audience)?;
        let response = self
            .client
            .post(&subscription.endpoint)
            .header("TTL", MESSAGE_TTL_SECS)
            .header("Content-Encoding", "aes128gcm")
            .header("Content-Type", "application/octet-stream")
            .header(
                "Authorization",
                format!("vapid t={jwt}, k={}", self.public_key()),
            )
            .body(body)
            .send()
            .await
            .context("push send")?;
        let status = response.status();
        if status.is_success() {
            return Ok(SendOutcome::Ok);
        }
        if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE {
            return Ok(SendOutcome::Gone);
        }
        let text = response.text().await.unwrap_or_default();
        warn!(%status, body = %text.chars().take(200).collect::<String>(), "push endpoint refused");
        Ok(SendOutcome::Transient)
    }
}

/// Refuses endpoints that would make the server probe its own network: the
/// endpoint comes from a client and the server posts to it. Push services are
/// public HTTPS hostnames, so plaintext, IP literals and local names go.
fn validate_endpoint(endpoint: &str) -> std::result::Result<(), &'static str> {
    let rest = endpoint
        .strip_prefix("https://")
        .ok_or("endpoint must use https")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if authority.is_empty() {
        return Err("endpoint has no host");
    }
    if authority.starts_with('[') {
        return Err("endpoint host must be a hostname, not an IP literal");
    }
    let host = authority.split(':').next().unwrap_or(authority);
    if host.parse::<IpAddr>().is_ok() {
        return Err("endpoint host must be a hostname, not an IP literal");
    }
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") || lower.ends_with(".local") {
        return Err("endpoint host is not publicly routable");
    }
    Ok(())
}

/// Pushes every notice the manager raises, and clears a discussion's notification
/// once it is seen, until the manager is gone.
pub fn spawn_dispatcher(manager: &SessionManager, pusher: Arc<Pusher>) {
    let mut notices = manager.subscribe_notices();
    let mut seen = manager.subscribe_seen();
    let store = manager.store().clone();
    tokio::spawn(async move {
        loop {
            let notice = tokio::select! {
                notice = notices.recv() => match notice {
                    Ok(notice) => notice,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => return,
                },
                session = seen.recv() => {
                    match session {
                        Ok(session) => pusher.clear(&session.to_string()).await,
                        Err(RecvError::Lagged(_)) => {}
                        Err(RecvError::Closed) => return,
                    }
                    continue;
                }
            };
            let row = match store.session(notice.session).await {
                Ok(Some(row)) => row,
                Ok(None) => continue,
                Err(err) => {
                    warn!("read session {}: {err:#}", notice.session);
                    continue;
                }
            };
            pusher
                .broadcast(&notice_message(notice, row.name.as_deref(), &row.cwd))
                .await;
        }
    });
}

/// Titled by the discussion's name, else its folder; one notification per discussion.
fn notice_message(notice: Notice, name: Option<&str>, cwd: &Path) -> Message {
    let body = match notice.kind {
        NoticeKind::Ask => "Waiting for your approval".to_string(),
        NoticeKind::Finished => "Turn finished".to_string(),
        NoticeKind::Stopped => "Claude stopped unexpectedly".to_string(),
        NoticeKind::CacheSoonCold => format!(
            "Cache goes cold in {} min",
            CACHE_WARNING_BEFORE.num_minutes()
        ),
    };
    let title = name.map(str::to_string).unwrap_or_else(|| {
        cwd.file_name().map_or_else(
            || cwd.display().to_string(),
            |folder| folder.to_string_lossy().into_owned(),
        )
    });
    Message {
        title,
        body,
        tag: notice.session.to_string(),
        url: format!("#/s/{}", notice.session),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_must_be_public_https_hostnames() {
        assert!(validate_endpoint("https://fcm.googleapis.com/fcm/send/abc").is_ok());
        assert!(validate_endpoint("http://fcm.googleapis.com/x").is_err());
        assert!(validate_endpoint("https://127.0.0.1/x").is_err());
        assert!(validate_endpoint("https://[::1]/x").is_err());
        assert!(validate_endpoint("https://localhost:8080/x").is_err());
        assert!(validate_endpoint("https://printer.local/x").is_err());
        assert!(validate_endpoint("https://user@10.0.0.1/x").is_err());
    }

    #[test]
    fn a_notice_is_titled_by_its_folder_and_tagged_by_its_discussion() {
        let session = uuid::Uuid::new_v4();
        let message = notice_message(
            Notice {
                session,
                kind: NoticeKind::Finished,
            },
            None,
            Path::new("/work/vigie2"),
        );
        assert_eq!(message.title, "vigie2");
        assert_eq!(message.body, "Turn finished");
        assert_eq!(message.tag, session.to_string());
        assert_eq!(message.url, format!("#/s/{session}"));
    }

    #[test]
    fn a_named_discussion_titles_its_notices_by_name() {
        let notice = Notice {
            session: uuid::Uuid::new_v4(),
            kind: NoticeKind::Ask,
        };
        let message = notice_message(notice, Some("parser idea"), Path::new("/work/vigie2"));
        assert_eq!(message.title, "parser idea");
    }

    #[test]
    fn a_clear_goes_once_to_the_devices_holding_the_tag() {
        let mut holding = Holding::default();
        holding.delivered("s1", "phone".into());
        holding.delivered("s1", "desktop".into());
        holding.delivered("s2", "phone".into());
        assert_eq!(
            holding.take("s1"),
            HashSet::from(["phone".to_string(), "desktop".to_string()])
        );
        assert!(holding.take("s1").is_empty());
        assert_eq!(holding.take("s2"), HashSet::from(["phone".to_string()]));
        assert_eq!(
            serde_json::to_string(&Clear { clear: "s1".into() }).unwrap(),
            r#"{"clear":"s1"}"#
        );
    }
}
