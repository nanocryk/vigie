//! Pair tokens, and the rate limit on failed attempts to spend one.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use parking_lot::Mutex;
use rand::TryRng;
use rand::rngs::SysRng;
use subtle::ConstantTimeEq;
use tracing::warn;

pub const PAIR_TOKEN_TTL: Duration = Duration::from_secs(300);
const RATE_WINDOW: Duration = Duration::from_secs(600);
const RATE_MAX_FAILS: usize = 5;
const RATE_LOCKOUT: Duration = Duration::from_secs(3600);

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

    /// Consume a pair token if it matches and is still valid. Returns true on match.
    pub fn consume(&self, candidate: &str) -> bool {
        let now = Instant::now();
        let mut tokens = self.tokens.lock();
        tokens.retain(|pair| pair.expires_at > now);
        match tokens
            .iter()
            .position(|pair| same_token(&pair.token, candidate))
        {
            Some(index) => {
                tokens.remove(index);
                true
            }
            None => false,
        }
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

impl RateLimiter {
    pub fn check(&self, ip: IpAddr) -> RateCheck {
        let now = Instant::now();
        let mut addresses = self.addresses.lock();
        let Some(state) = addresses.get_mut(&ip) else {
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
        let state = addresses.entry(ip).or_default();
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
