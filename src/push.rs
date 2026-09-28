//! Web Push: VAPID (RFC 8292) and aes128gcm message encryption (RFC 8291), on
//! `ring`, sent with `reqwest`. The P-256 key pair is generated on first start
//! and kept in the state directory; browsers subscribe with its public key.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use axum::{
    Extension, Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    aead, agreement, hkdf,
    rand::{SecureRandom, SystemRandom},
    signature::{self, EcdsaKeyPair, KeyPair},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::warn;

use tokio::sync::broadcast::error::RecvError;

use crate::auth::{Auth, AuthedDevice};
use crate::manager::{CACHE_WARNING_BEFORE, Notice, NoticeKind, SessionManager};
use crate::server::AppState;
use crate::store::Store;

const VAPID_JWT_TTL_SECS: i64 = 12 * 3600;
const AES128GCM_RECORD_SIZE: u32 = 4096;
const AES128GCM_TAG_LEN: usize = 16;
/// How long a push service keeps a message for an offline device.
const MESSAGE_TTL_SECS: &str = "3600";
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Vapid {
    keypair: EcdsaKeyPair,
    public_b64url: String,
    subject: String,
    rng: SystemRandom,
}

#[derive(Serialize, Deserialize)]
struct VapidFile {
    pkcs8_b64: String,
    subject: String,
}

impl Vapid {
    /// Reads `vapid.json` in `dir`, creating it with a new key pair when absent.
    pub fn load_or_generate(dir: &Path) -> Result<Self> {
        let path = dir.join("vapid.json");
        let rng = SystemRandom::new();
        let (pkcs8, subject) = match std::fs::read(&path) {
            Ok(bytes) => {
                let file: VapidFile = serde_json::from_slice(&bytes)
                    .with_context(|| format!("parse {}", path.display()))?;
                let pkcs8 = URL_SAFE_NO_PAD
                    .decode(file.pkcs8_b64.trim())
                    .context("decode vapid pkcs8")?;
                (pkcs8, file.subject)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
                let pkcs8 =
                    EcdsaKeyPair::generate_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
                        .map_err(|err| anyhow!("generate vapid pkcs8: {err}"))?;
                let subject = "mailto:vigie2@localhost".to_string();
                let file = VapidFile {
                    pkcs8_b64: URL_SAFE_NO_PAD.encode(pkcs8.as_ref()),
                    subject: subject.clone(),
                };
                std::fs::write(&path, serde_json::to_vec_pretty(&file)?)
                    .with_context(|| format!("write {}", path.display()))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
                }
                (pkcs8.as_ref().to_vec(), subject)
            }
            Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
        };
        let keypair =
            EcdsaKeyPair::from_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, &pkcs8, &rng)
                .map_err(|err| anyhow!("parse vapid pkcs8: {err}"))?;
        let public_b64url = URL_SAFE_NO_PAD.encode(keypair.public_key().as_ref());
        Ok(Self {
            keypair,
            public_b64url,
            subject,
            rng,
        })
    }

    pub fn public_key_b64url(&self) -> &str {
        &self.public_b64url
    }

    fn sign_jwt(&self, audience: &str) -> Result<String> {
        let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = json!({
            "aud": audience,
            "exp": chrono::Utc::now().timestamp() + VAPID_JWT_TTL_SECS,
            "sub": self.subject,
        });
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims)?);
        let message = format!("{header}.{payload}");
        let signature = self
            .keypair
            .sign(&self.rng, message.as_bytes())
            .map_err(|err| anyhow!("vapid sign: {err}"))?;
        Ok(format!(
            "{message}.{}",
            URL_SAFE_NO_PAD.encode(signature.as_ref())
        ))
    }
}

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

/// Sends messages to every subscribed device.
pub struct Pusher {
    vapid: Vapid,
    client: reqwest::Client,
    store: Store,
    auth: Arc<Auth>,
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
        })
    }

    /// Sends `message` to every subscription of an active device; drops dead ones.
    pub async fn broadcast(&self, message: &Message) {
        let subscriptions = match self.store.push_subscriptions().await {
            Ok(subscriptions) => subscriptions,
            Err(err) => {
                warn!("read push subscriptions: {err:#}");
                return;
            }
        };
        let payload = match serde_json::to_vec(message) {
            Ok(payload) => payload,
            Err(err) => {
                warn!("serialize push message: {err}");
                return;
            }
        };
        let sends = subscriptions
            .into_iter()
            .filter(|(device, _)| self.auth.registry.find_active(device).is_some())
            .map(|(device, subscription)| {
                let payload = &payload;
                async move {
                    let outcome = self.send(&subscription, payload).await;
                    (device, outcome)
                }
            });
        for (device, outcome) in futures_util::future::join_all(sends).await {
            match outcome {
                Ok(SendOutcome::Ok | SendOutcome::Transient) => {}
                Ok(SendOutcome::Gone) => {
                    if let Err(err) = self.store.remove_push_subscription(&device).await {
                        warn!("drop the push subscription of a device: {err:#}");
                    }
                }
                Err(err) => warn!("push send: {err:#}"),
            }
        }
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
        let body = encrypt_aes128gcm(&self.vapid.rng, &ua_public, &auth_secret, payload)
            .context("aes128gcm encrypt")?;
        let audience = endpoint_origin(&subscription.endpoint)?;
        let jwt = self.vapid.sign_jwt(&audience)?;
        let response = self
            .client
            .post(&subscription.endpoint)
            .header("TTL", MESSAGE_TTL_SECS)
            .header("Content-Encoding", "aes128gcm")
            .header("Content-Type", "application/octet-stream")
            .header(
                "Authorization",
                format!("vapid t={jwt}, k={}", self.vapid.public_b64url),
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

/// RFC 8291 aes128gcm: salt (16) || rs (4, BE) || idlen (1) || keyid (65) || ciphertext and tag.
fn encrypt_aes128gcm(
    rng: &SystemRandom,
    ua_public: &[u8],
    auth_secret: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let mut salt = [0u8; 16];
    rng.fill(&mut salt)
        .map_err(|err| anyhow!("rng salt: {err}"))?;
    let as_private = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, rng)
        .map_err(|err| anyhow!("generate ephemeral key: {err}"))?;
    let as_public = as_private
        .compute_public_key()
        .map_err(|err| anyhow!("ephemeral public key: {err}"))?;
    let as_public = as_public.as_ref().to_vec();
    let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, ua_public);
    let shared = agreement::agree_ephemeral(as_private, &peer, |raw| raw.to_vec())
        .map_err(|err| anyhow!("ecdh: {err}"))?;

    // RFC 8291 3.4: IKM = HKDF(salt = auth_secret, ikm = shared, info = "WebPush: info\0" || ua || as).
    let prk_key = hkdf::Salt::new(hkdf::HKDF_SHA256, auth_secret).extract(&shared);
    let mut key_info = Vec::with_capacity(14 + 65 + 65);
    key_info.extend_from_slice(b"WebPush: info\0");
    key_info.extend_from_slice(ua_public);
    key_info.extend_from_slice(&as_public);
    let ikm = hkdf_expand(&prk_key, &[&key_info], 32)?;

    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &salt).extract(&ikm);
    let cek = hkdf_expand(&prk, &[b"Content-Encoding: aes128gcm\0"], 16)?;
    let nonce = hkdf_expand(&prk, &[b"Content-Encoding: nonce\0"], 12)?;

    // One record: the plaintext, then the last-record delimiter, no padding.
    let mut in_out = Vec::with_capacity(plaintext.len() + 1 + AES128GCM_TAG_LEN);
    in_out.extend_from_slice(plaintext);
    in_out.push(0x02);
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_128_GCM, &cek).map_err(|err| anyhow!("aes key: {err}"))?,
    );
    let nonce =
        aead::Nonce::try_assume_unique_for_key(&nonce).map_err(|err| anyhow!("nonce: {err}"))?;
    key.seal_in_place_append_tag(nonce, aead::Aad::empty(), &mut in_out)
        .map_err(|err| anyhow!("aes seal: {err}"))?;

    let mut body = Vec::with_capacity(16 + 4 + 1 + 65 + in_out.len());
    body.extend_from_slice(&salt);
    body.extend_from_slice(&AES128GCM_RECORD_SIZE.to_be_bytes());
    body.push(65);
    body.extend_from_slice(&as_public);
    body.extend_from_slice(&in_out);
    Ok(body)
}

fn hkdf_expand(prk: &hkdf::Prk, info: &[&[u8]], len: usize) -> Result<Vec<u8>> {
    struct Len(usize);
    impl hkdf::KeyType for Len {
        fn len(&self) -> usize {
            self.0
        }
    }
    let okm = prk
        .expand(info, Len(len))
        .map_err(|err| anyhow!("hkdf expand: {err}"))?;
    let mut out = vec![0u8; len];
    okm.fill(&mut out)
        .map_err(|err| anyhow!("hkdf fill: {err}"))?;
    Ok(out)
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
    if host.parse::<std::net::IpAddr>().is_ok() {
        return Err("endpoint host must be a hostname, not an IP literal");
    }
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") || lower.ends_with(".local") {
        return Err("endpoint host is not publicly routable");
    }
    Ok(())
}

/// The VAPID audience: scheme and authority of the endpoint.
fn endpoint_origin(endpoint: &str) -> Result<String> {
    let rest = endpoint
        .strip_prefix("https://")
        .context("endpoint must use https")?;
    let host = rest.split('/').next().unwrap_or(rest);
    Ok(format!("https://{host}"))
}

#[derive(Serialize)]
struct PushConfig<'a> {
    vapid_public_key: &'a str,
    /// This device has a subscription.
    subscribed: bool,
}

pub async fn config(
    State(state): State<AppState>,
    Extension(device): Extension<AuthedDevice>,
) -> Response {
    match state.pusher.store.push_subscription(&device.id).await {
        Ok(subscription) => Json(PushConfig {
            vapid_public_key: state.pusher.vapid.public_key_b64url(),
            subscribed: subscription.is_some(),
        })
        .into_response(),
        Err(err) => internal_error("read the push subscription", &err),
    }
}

pub async fn subscribe(
    State(state): State<AppState>,
    Extension(device): Extension<AuthedDevice>,
    Json(subscription): Json<PushSubscription>,
) -> Response {
    if subscription.keys.p256dh.is_empty() || subscription.keys.auth.is_empty() {
        return bad_request("malformed subscription");
    }
    if let Err(reason) = validate_endpoint(&subscription.endpoint) {
        return bad_request(reason);
    }
    match state
        .pusher
        .store
        .set_push_subscription(&device.id, &subscription)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => internal_error("store the push subscription", &err),
    }
}

pub async fn unsubscribe(
    State(state): State<AppState>,
    Extension(device): Extension<AuthedDevice>,
) -> Response {
    match state
        .pusher
        .store
        .remove_push_subscription(&device.id)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => internal_error("drop the push subscription", &err),
    }
}

/// Sends a test notification to this device only.
pub async fn test(
    State(state): State<AppState>,
    Extension(device): Extension<AuthedDevice>,
) -> Response {
    let subscription = match state.pusher.store.push_subscription(&device.id).await {
        Ok(Some(subscription)) => subscription,
        Ok(None) => return (StatusCode::NOT_FOUND, "no subscription").into_response(),
        Err(err) => return internal_error("read the push subscription", &err),
    };
    let message = Message {
        title: "vigie2".into(),
        body: "Test notification".into(),
        tag: "test".into(),
        url: String::new(),
    };
    let payload = match serde_json::to_vec(&message) {
        Ok(payload) => payload,
        Err(err) => return internal_error("serialize the message", &err.into()),
    };
    match state.pusher.send(&subscription, &payload).await {
        Ok(SendOutcome::Ok) => StatusCode::NO_CONTENT.into_response(),
        Ok(SendOutcome::Gone) => {
            let _ = state
                .pusher
                .store
                .remove_push_subscription(&device.id)
                .await;
            (StatusCode::GONE, "subscription gone").into_response()
        }
        Ok(SendOutcome::Transient) => {
            (StatusCode::BAD_GATEWAY, "the push service refused").into_response()
        }
        Err(err) => internal_error("send the test notification", &err),
    }
}

/// Pushes every notice the manager raises, until the manager is gone.
pub fn spawn_dispatcher(manager: &SessionManager, pusher: Arc<Pusher>) {
    let mut notices = manager.subscribe_notices();
    let store = manager.store().clone();
    tokio::spawn(async move {
        loop {
            let notice = match notices.recv().await {
                Ok(notice) => notice,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return,
            };
            let cwd = match store.session(notice.session).await {
                Ok(Some(row)) => row.cwd,
                Ok(None) => continue,
                Err(err) => {
                    warn!("read session {}: {err:#}", notice.session);
                    continue;
                }
            };
            pusher.broadcast(&notice_message(notice, &cwd)).await;
        }
    });
}

/// Titled by the discussion's folder; one notification per discussion.
fn notice_message(notice: Notice, cwd: &Path) -> Message {
    let body = match notice.kind {
        NoticeKind::Ask => "Waiting for your approval".to_string(),
        NoticeKind::Finished => "Turn finished".to_string(),
        NoticeKind::Stopped => "Claude stopped unexpectedly".to_string(),
        NoticeKind::CacheSoonCold => format!(
            "Cache goes cold in {} min",
            CACHE_WARNING_BEFORE.num_minutes()
        ),
    };
    let title = cwd.file_name().map_or_else(
        || cwd.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    Message {
        title,
        body,
        tag: notice.session.to_string(),
        url: format!("#/s/{}", notice.session),
    }
}

fn bad_request(reason: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error": reason}))).into_response()
}

fn internal_error(what: &str, err: &anyhow::Error) -> Response {
    warn!("{what}: {err:#}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": format!("{err:#}")})),
    )
        .into_response()
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
            Path::new("/work/vigie2"),
        );
        assert_eq!(message.title, "vigie2");
        assert_eq!(message.body, "Turn finished");
        assert_eq!(message.tag, session.to_string());
        assert_eq!(message.url, format!("#/s/{session}"));
    }

    #[test]
    fn the_audience_is_the_endpoint_origin() {
        assert_eq!(
            endpoint_origin("https://updates.push.services.mozilla.com/wpush/v2/abc").unwrap(),
            "https://updates.push.services.mozilla.com"
        );
    }

    /// Decrypts as a browser would, with the subscription's private key.
    #[test]
    fn a_message_decrypts_with_the_subscription_keys() {
        let rng = SystemRandom::new();
        let ua_private =
            agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).unwrap();
        let ua_public = ua_private.compute_public_key().unwrap().as_ref().to_vec();
        let auth_secret = [7u8; 16];
        let body = encrypt_aes128gcm(&rng, &ua_public, &auth_secret, b"hello").unwrap();

        let (salt, rest) = body.split_at(16);
        assert_eq!(rest[..4], AES128GCM_RECORD_SIZE.to_be_bytes());
        assert_eq!(rest[4], 65);
        let (as_public, ciphertext) = rest[5..].split_at(65);
        let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, as_public);
        let shared = agreement::agree_ephemeral(ua_private, &peer, |raw| raw.to_vec()).unwrap();
        let prk_key = hkdf::Salt::new(hkdf::HKDF_SHA256, &auth_secret).extract(&shared);
        let key_info = [b"WebPush: info\0".as_slice(), &ua_public, as_public].concat();
        let ikm = hkdf_expand(&prk_key, &[&key_info], 32).unwrap();
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(&ikm);
        let cek = hkdf_expand(&prk, &[b"Content-Encoding: aes128gcm\0"], 16).unwrap();
        let nonce = hkdf_expand(&prk, &[b"Content-Encoding: nonce\0"], 12).unwrap();
        let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &cek).unwrap());
        let mut in_out = ciphertext.to_vec();
        let plain = key
            .open_in_place(
                aead::Nonce::try_assume_unique_for_key(&nonce).unwrap(),
                aead::Aad::empty(),
                &mut in_out,
            )
            .unwrap();
        assert_eq!(plain, b"hello\x02");
    }

    #[test]
    fn the_key_pair_survives_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let first = Vapid::load_or_generate(dir.path()).unwrap();
        let second = Vapid::load_or_generate(dir.path()).unwrap();
        assert_eq!(first.public_key_b64url(), second.public_key_b64url());
        assert_eq!(
            URL_SAFE_NO_PAD
                .decode(first.public_key_b64url())
                .unwrap()
                .len(),
            65
        );
    }
}
