//! Fixed-window rate limiting shell.
//!
//! Parity note: upstream uses the `ratelimiter` npm package (sliding window,
//! Redis-backed, key = `<actor>:<endpoint>`, role `rateLimitFactor`
//! multiplier). The Redis-backed version with upstream's algorithm lands in
//! M2 once RoleService provides the factor; this in-memory shell keeps the
//! same interface so handlers don't change.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Default)]
pub struct RateLimiter {
    slots: Mutex<HashMap<(String, String), Slot>>,
}

#[derive(Debug)]
struct Slot {
    window_started_ms: u64,
    count: u32,
}

#[derive(Debug)]
pub struct Limit {
    /// Requests allowed per `duration_ms`.
    pub max: u32,
    pub duration_ms: u64,
}

pub enum LimitResult {
    Allowed,
    /// (info value, retry_after_secs)
    Limited {
        info: serde_json::Value,
        retry_after_secs: u64,
    },
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn check(&self, actor: &str, endpoint: &str, limit: &Limit) -> LimitResult {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let key = (actor.to_string(), endpoint.to_string());
        let t = now_ms();
        let slot = slots.entry(key).or_insert(Slot {
            window_started_ms: t,
            count: 0,
        });
        if t.saturating_sub(slot.window_started_ms) >= limit.duration_ms {
            slot.window_started_ms = t;
            slot.count = 0;
        }
        slot.count += 1;
        if slot.count > limit.max {
            let reset_ms = limit
                .duration_ms
                .saturating_sub(t.saturating_sub(slot.window_started_ms));
            return LimitResult::Limited {
                info: serde_json::json!({
                    "timeout": reset_ms,
                    "remaining": 0,
                }),
                retry_after_secs: (reset_ms / 1000).max(1),
            };
        }
        LimitResult::Allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_max_then_limits() {
        let rl = RateLimiter::new();
        let limit = Limit {
            max: 2,
            duration_ms: 60_000,
        };
        assert!(matches!(
            rl.check("u1", "ping", &limit),
            LimitResult::Allowed
        ));
        assert!(matches!(
            rl.check("u1", "ping", &limit),
            LimitResult::Allowed
        ));
        match rl.check("u1", "ping", &limit) {
            LimitResult::Limited {
                retry_after_secs, ..
            } => assert!(retry_after_secs >= 1),
            LimitResult::Allowed => panic!("expected limited"),
        }
        // independent actors
        assert!(matches!(
            rl.check("u2", "ping", &limit),
            LimitResult::Allowed
        ));
    }
}
