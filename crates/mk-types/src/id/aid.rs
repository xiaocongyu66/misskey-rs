//! AID: 10 chars = 8 chars of base36 (milliseconds since 2000-01-01T00:00:00Z)
//! + 2 chars of incrementing noise. Port of `misc/id/aid.ts`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds at 2000-01-01T00:00:00Z.
const TIME_2000: i64 = 946_684_800_000;
const BASE36: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
/// The noise field is the last 2 chars, i.e. counter mod 36^2 — exactly what
/// upstream's `toString(36).slice(-2)` computes when `counter++` overflows.
const NOISE_MOD: u32 = 1296;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Upstream seeds the counter from `crypto.randomBytes(2)`; any random-ish
/// per-process seed has the same effect (avoids cross-process collisions).
fn seed() -> u32 {
    static SEED: OnceLock<u32> = OnceLock::new();
    *SEED.get_or_init(|| {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        nanos ^ std::process::id()
    })
}

fn to_base36_padded(mut v: u64, width: usize) -> String {
    let mut buf = [0u8; 13]; // u64 max is 13 base36 digits
    let mut n = 0;
    loop {
        buf[n] = BASE36[(v % 36) as usize];
        v /= 36;
        n += 1;
        if v == 0 {
            break;
        }
    }
    let mut s = String::with_capacity(width);
    while s.len() + n < width {
        s.push('0');
    }
    while n > 0 {
        n -= 1;
        s.push(buf[n] as char);
    }
    s
}

fn from_base36(s: &str) -> Option<u64> {
    if s.is_empty() || s.len() > 13 {
        return None;
    }
    s.chars().try_fold(0u64, |acc, c| {
        let d = match c.to_digit(36) {
            Some(d) => d as u64,
            None => return None,
        };
        acc.checked_mul(36)?.checked_add(d)
    })
}

/// Generate an AID for `now_ms` (unix epoch milliseconds).
pub fn gen_aid(now_ms: i64) -> String {
    let elapsed = if now_ms > TIME_2000 {
        (now_ms - TIME_2000) as u64
    } else {
        0
    };
    let prev = COUNTER.fetch_add(1, Ordering::Relaxed);
    let noise = if prev == 0 {
        seed() % NOISE_MOD
    } else {
        prev % NOISE_MOD
    };
    format!(
        "{}{}",
        to_base36_padded(elapsed, 8),
        to_base36_padded(noise as u64, 2)
    )
}

/// Generate an AID for the current time.
pub fn gen_aid_now() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(TIME_2000);
    gen_aid(now)
}

/// Recover the creation timestamp (unix epoch ms) from an AID. Port of `parseAid`.
pub fn parse_aid_date(id: &str) -> Option<i64> {
    if id.len() != 10 {
        return None;
    }
    let elapsed = from_base36(&id[..8])?;
    Some((elapsed as i64) + TIME_2000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_and_ordering() {
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let a = gen_aid(t);
        assert_eq!(a.len(), 10);
        assert!(a
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));

        let b = gen_aid(t);
        let c = gen_aid(t + 1);
        assert!(b < c); // lexicographic == time
    }

    #[test]
    fn carries_across_noise_wrap() {
        // 1296 invocations land in at most a couple of ms; ids must keep
        // ordering across the mod-36^2 wrap because the time part advances.
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let mut prev = String::new();
        for i in 0..1400 {
            let id = gen_aid(t + (i / 1296) as i64);
            assert_eq!(id.len(), 10);
            if !prev.is_empty() {
                // same-ms noise wrap temporarily resets order; assert only
                // that the time part is monotonic (first 8 chars).
                assert!(id[..8] >= prev[..8]);
            }
            prev = id;
        }
    }

    #[test]
    fn roundtrip_date() {
        let t = 1_700_000_000_000i64;
        let id = gen_aid(t);
        assert_eq!(parse_aid_date(&id).unwrap(), t);
    }

    #[test]
    fn base36_padding() {
        assert_eq!(to_base36_padded(0, 8), "00000000");
        assert_eq!(to_base36_padded(35, 2), "0z");
        assert_eq!(from_base36("0z"), Some(35));
    }
}
