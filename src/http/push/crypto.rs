//! VAPID (RFC 8292) signing and aes128gcm message encryption (RFC 8291), on `ring`.

use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::Url;
use ring::{
    aead, agreement, hkdf,
    rand::{SecureRandom, SystemRandom},
    signature::{self, EcdsaKeyPair, KeyPair},
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::state_dir;

const VAPID_JWT_TTL_SECS: i64 = 12 * 3600;
const AES128GCM_RECORD_SIZE: u32 = 4096;
const AES128GCM_TAG_LEN: usize = 16;

/// The server's P-256 key pair, generated on first start and kept in the state
/// directory; browsers subscribe with its public key.
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
            Err(err) if err.kind() == ErrorKind::NotFound => {
                state_dir::create_private_dir(dir)?;
                let pkcs8 =
                    EcdsaKeyPair::generate_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
                        .map_err(|err| anyhow!("generate vapid pkcs8: {err}"))?;
                let subject = "mailto:vigie@localhost".to_string();
                let file = VapidFile {
                    pkcs8_b64: URL_SAFE_NO_PAD.encode(pkcs8.as_ref()),
                    subject: subject.clone(),
                };
                state_dir::write_private(&path, serde_json::to_vec_pretty(&file)?)?;
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

    /// The JWT of the `Authorization: vapid` header for a push service at `audience`.
    pub fn sign_jwt(&self, audience: &str) -> Result<String> {
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

    /// Encrypts `plaintext` for the subscription keys `ua_public` and `auth_secret`.
    pub fn encrypt(
        &self,
        ua_public: &[u8],
        auth_secret: &[u8],
        plaintext: &[u8],
    ) -> Result<Vec<u8>> {
        encrypt_aes128gcm(&self.rng, ua_public, auth_secret, plaintext)
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

/// The VAPID audience: scheme and authority of the endpoint.
pub fn endpoint_origin(endpoint: &Url) -> String {
    endpoint.origin().ascii_serialization()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_audience_is_the_endpoint_origin() {
        assert_eq!(
            endpoint_origin(
                &Url::parse("https://updates.push.services.mozilla.com/wpush/v2/abc").unwrap()
            ),
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
