//! Pair tokens, and the rate limit on failed attempts to spend one.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use parking_lot::Mutex;
use rand::TryRng;
use rand::rngs::SysRng;
use ring::digest;
use subtle::ConstantTimeEq;
use tracing::warn;

pub const PAIR_TOKEN_TTL: Duration = Duration::from_secs(300);
/// Lifetime of the tokens a local console gives: the startup one and `vigie auth pair`'s.
pub const LOCAL_TOKEN_TTL: Duration = Duration::from_secs(600);
const RATE_WINDOW: Duration = Duration::from_secs(600);
const RATE_MAX_FAILS: usize = 5;
const RATE_LOCKOUT: Duration = Duration::from_secs(3600);
/// Addresses tracked at once. Past it, new addresses go untracked: tokens are
/// 256-bit, so the limiter only trims noise, and memory must stay bounded.
const RATE_MAX_TRACKED: usize = 4096;

pub fn mint_token() -> String {
    let mut bytes = [0u8; 32];
    SysRng
        .try_fill_bytes(&mut bytes)
        .expect("OS random source unavailable");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Constant-time equality of two tokens.
pub fn same_token(token: &str, candidate: &str) -> bool {
    token.as_bytes().ct_eq(candidate.as_bytes()).into()
}

/// What the registry stores of a device secret: its SHA-256, base64.
pub fn secret_hash(secret: &str) -> String {
    URL_SAFE_NO_PAD.encode(digest::digest(&digest::SHA256, secret.as_bytes()))
}

struct PairToken {
    token: String,
    issued_by: String,
    expires_at: Instant,
}

/// Single-use tokens a paired device issues to pair another one.
#[derive(Default)]
pub struct PairTokens {
    tokens: Mutex<Vec<PairToken>>,
}

impl PairTokens {
    /// Replaces any token `device_id` issued before.
    pub fn issue(&self, device_id: &str) -> String {
        let now = Instant::now();
        let token = mint_token();
        let mut tokens = self.tokens.lock();
        tokens.retain(|pair| pair.expires_at > now && pair.issued_by != device_id);
        tokens.push(PairToken {
            token: token.clone(),
            issued_by: device_id.to_string(),
            expires_at: now + PAIR_TOKEN_TTL,
        });
        token
    }

    /// Consume a pair token if it matches and is still valid, returning the
    /// id of the device that issued it.
    pub fn consume(&self, candidate: &str) -> Option<String> {
        let now = Instant::now();
        let mut tokens = self.tokens.lock();
        tokens.retain(|pair| pair.expires_at > now);
        let index = tokens
            .iter()
            .position(|pair| same_token(&pair.token, candidate))?;
        Some(tokens.remove(index).issued_by)
    }
}

#[derive(Default)]
pub struct RateLimiter {
    addresses: Mutex<HashMap<IpAddr, IpState>>,
}

#[derive(Default)]
struct IpState {
    failures: Vec<Instant>,
    lockout_until: Option<Instant>,
}

pub enum RateCheck {
    Ok,
    Locked { retry_after_secs: u64 },
}

impl IpState {
    fn is_stale(&self, now: Instant) -> bool {
        self.lockout_until.is_none_or(|until| until <= now)
            && self
                .failures
                .iter()
                .all(|failure| now.duration_since(*failure) >= RATE_WINDOW)
    }
}

/// The key an address is limited under: an IPv6 address by its /64, which a
/// single host can rotate through freely.
fn rate_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => IpAddr::V6((u128::from(v6) & !u128::from(u64::MAX)).into()),
    }
}

impl RateLimiter {
    pub fn check(&self, ip: IpAddr) -> RateCheck {
        let now = Instant::now();
        let mut addresses = self.addresses.lock();
        let Some(state) = addresses.get_mut(&rate_key(ip)) else {
            return RateCheck::Ok;
        };
        state
            .failures
            .retain(|failure| now.duration_since(*failure) < RATE_WINDOW);
        if let Some(until) = state.lockout_until {
            if now < until {
                return RateCheck::Locked {
                    retry_after_secs: (until - now).as_secs().max(1),
                };
            }
            state.lockout_until = None;
            state.failures.clear();
        }
        RateCheck::Ok
    }

    pub fn record_failure(&self, ip: IpAddr) {
        let now = Instant::now();
        let mut addresses = self.addresses.lock();
        let key = rate_key(ip);
        if !addresses.contains_key(&key) {
            addresses.retain(|_, state| !state.is_stale(now));
            if addresses.len() >= RATE_MAX_TRACKED {
                return;
            }
        }
        let state = addresses.entry(key).or_default();
        state
            .failures
            .retain(|failure| now.duration_since(*failure) < RATE_WINDOW);
        state.failures.push(now);
        if state.failures.len() >= RATE_MAX_FAILS {
            state.lockout_until = Some(now + RATE_LOCKOUT);
            warn!(%ip, "rate-limit lockout: too many failed pair attempts");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locked(limiter: &RateLimiter, ip: &str) -> bool {
        matches!(
            limiter.check(ip.parse().expect("ip")),
            RateCheck::Locked { .. }
        )
    }

    #[test]
    fn an_ipv6_host_is_limited_across_its_whole_slash_64() {
        let limiter = RateLimiter::default();
        for suffix in 1..=RATE_MAX_FAILS {
            limiter.record_failure(format!("2001:db8:1:2::{suffix}").parse().expect("ip"));
        }
        assert!(locked(&limiter, "2001:db8:1:2:ffff::1"));
        assert!(!locked(&limiter, "2001:db8:1:3::1"));
    }

    #[test]
    fn tracked_addresses_stay_bounded() {
        let limiter = RateLimiter::default();
        for index in 0..RATE_MAX_TRACKED as u32 + 100 {
            limiter.record_failure(IpAddr::V4(index.into()));
        }
        assert_eq!(limiter.addresses.lock().len(), RATE_MAX_TRACKED);
    }

    #[test]
    fn a_consumed_pair_token_names_its_issuer_once() {
        let tokens = PairTokens::default();
        let token = tokens.issue("phone");
        assert_eq!(tokens.consume(&token).as_deref(), Some("phone"));
        assert_eq!(tokens.consume(&token), None);
    }
}
