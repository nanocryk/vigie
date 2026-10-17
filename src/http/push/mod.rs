//! Web Push: VAPID (RFC 8292) and aes128gcm message encryption (RFC 8291), on
//! `ring`, sent with `reqwest`. The P-256 key pair is generated on first start
//! and kept in the state directory; browsers subscribe with its public key.

mod crypto;
mod routes;

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use parking_lot::Mutex;
use reqwest::Url;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
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
            // Through a proxy the resolver would vet the proxy, not the endpoint.
            .no_proxy()
            .dns_resolver(Arc::new(PublicOnly))
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
        let endpoint = validate_endpoint(&subscription.endpoint)
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
        let audience = crypto::endpoint_origin(&endpoint);
        let jwt = self.vapid.sign_jwt(&audience)?;
        let response = self
            .client
            .post(endpoint)
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
/// public HTTPS hostnames, so plaintext, IP literals and local names go. The
/// parse is the one `reqwest` connects with, so both see the same host; names
/// resolving to private addresses are caught by [`PublicOnly`].
fn validate_endpoint(endpoint: &str) -> std::result::Result<Url, &'static str> {
    let url = Url::parse(endpoint).map_err(|_| "endpoint is not a URL")?;
    if url.scheme() != "https" {
        return Err("endpoint must use https");
    }
    let host = url.host_str().ok_or("endpoint has no host")?;
    if host.starts_with('[') || host.parse::<IpAddr>().is_ok() {
        return Err("endpoint host must be a hostname, not an IP literal");
    }
    let lower = host.trim_end_matches('.').to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") || lower.ends_with(".local") {
        return Err("endpoint host is not publicly routable");
    }
    Ok(url)
}

/// Resolves names to their public addresses only, so a name pointing at a
/// private or loopback address cannot reach the server's own network.
struct PublicOnly;

impl Resolve for PublicOnly {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|address| is_public(address.ip()))
                .collect();
            if addresses.is_empty() {
                return Err(format!("{host} has no public address").into());
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

/// Whether `ip` is routable on the public internet.
fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_v4(ip),
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            // Addresses embedding an IPv4 one: mapped, NAT64 and 6to4.
            let embedded = if let Some(v4) = ip.to_ipv4_mapped() {
                Some(v4)
            } else if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                Some(Ipv4Addr::from_bits(ip.to_bits() as u32))
            } else if segments[0] == 0x2002 {
                Some(Ipv4Addr::from_bits((ip.to_bits() >> 80) as u32))
            } else {
                None
            };
            if let Some(v4) = embedded {
                return is_public_v4(v4);
            }
            !(ip.is_unspecified()
                || ip.is_loopback()
                || ip.is_multicast()
                || segments[0] & 0xfe00 == 0xfc00
                || segments[0] & 0xffc0 == 0xfe80
                || segments[0] & 0xffc0 == 0xfec0
                || segments[..2] == [0x2001, 0x0db8]
                || segments[..6] == [0, 0, 0, 0, 0, 0])
        }
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [first, second, third, _] = ip.octets();
    !(first == 0
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_documentation()
        || first >= 224
        || (first == 100 && second & 0xc0 == 64)
        || (first == 192 && second == 0 && third == 0)
        || (first == 198 && second & 0xfe == 18))
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
        assert!(validate_endpoint(r"https://127.0.0.1\@fcm.googleapis.com/x").is_err());
        assert!(validate_endpoint("https://2130706433/x").is_err());
        assert!(validate_endpoint("https://0x7f.1/x").is_err());
        assert!(validate_endpoint("https://LOCALHOST./x").is_err());
    }

    #[test]
    fn only_public_addresses_pass() {
        let public = |text: &str| is_public(text.parse().unwrap());
        assert!(public("142.250.74.110"));
        assert!(public("2a00:1450:4007:80e::200a"));
        for private in [
            "0.0.0.0",
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "198.18.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "fd00::1",
            "fe80::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:192.168.0.1",
            "64:ff9b::a00:1",
            "2002:7f00:1::",
        ] {
            assert!(!public(private), "{private}");
        }
    }

    #[tokio::test]
    async fn the_resolver_drops_private_addresses() {
        let resolved = PublicOnly.resolve("localhost".parse().unwrap()).await;
        assert!(resolved.is_err());
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
            Path::new("/work/vigie"),
        );
        assert_eq!(message.title, "vigie");
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
        let message = notice_message(notice, Some("parser idea"), Path::new("/work/vigie"));
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
