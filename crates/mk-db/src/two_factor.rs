//! two-factor: the TOTP lane behind i/2fa/* (upstream endpoints/i/2fa/* +
//! core/UserAuthService.ts validateOtp/twoFactorAuthenticate).
//!
//! CRYPTO IS VENDORED, std-only: SHA-1 + HMAC-SHA1 + RFC 4648 base32 +
//! RFC 6238 TOTP live at the bottom of this file. Upstream pulls these from
//! the `otpauth` crate (HMAC-SHA1, 6 digits, period 30, window ±1); the port
//! hand-rolls the same primitives to avoid new dependency edges. The SHA-1 /
//! HMAC / TOTP code paths are unit-tested against the RFC 6238 / RFC 2202
//! vectors in the api tests tail (arena-2fa).
//!
//! Upstream validateOtp short-circuits in test mode (NODE_ENV=test &&
//! MISSKEY_TEST_CHECK_DUPLICATED_TOTP != '1' → `return true`), so the e2e's
//! `otpToken(...)` strings are only really checked for the reuse test; the
//! MISSKEY_TEST_CHECK_DUPLICATED_TOTP flag rides the harness /env controller
//! → POST /api/_test/env (config.test-gated) instead of the process env the
//! port never sees. The redis `2fa:used:...` SET-NX token-burn becomes an
//! in-memory registry keyed (userId, secret, step) with the same 90s TTL.

use std::io::Read;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use sqlx::PgPool;

pub const TOTP_PERIOD_SECS: u64 = 30;
/// timeStep * (validationWindow * 2 + 1) — upstream's redis EX ttl.
const USED_STEP_TTL: Duration = Duration::from_secs(90);

// ==== test-env toggle (MISSKEY_TEST_CHECK_DUPLICATED_TOTP) ====

static CHECK_DUP_TOTP: AtomicBool = AtomicBool::new(false);

/// POST /api/_test/env (config.test-gated): `value == "1"` enables the
/// duplicated-TOTP burn; any other value (the harness beforeEach `''` reset)
/// clears it.
pub fn set_check_duplicated_totp(on: bool) {
    CHECK_DUP_TOTP.store(on, Ordering::SeqCst);
}

pub fn check_duplicated_totp() -> bool {
    CHECK_DUP_TOTP.load(Ordering::SeqCst)
}

// ==== in-memory used-step burn (redis SET NX EX 90 parity) ====

static USED_STEPS: Mutex<Vec<((String, u64), Instant)>> = Mutex::new(Vec::new());

fn claim_used_step(user_id: &str, secret_b32: &str, step: u64) -> bool {
    let mut guard = USED_STEPS.lock().unwrap();
    let now = Instant::now();
    guard.retain(|(_, deadline)| *deadline > now);
    let user_key = format!("{}:{}", user_id, secret_b32);
    if guard.iter().any(|((u, s), _)| u == &user_key && *s == step) {
        return false;
    }
    guard.push(((user_key, step), now + USED_STEP_TTL));
    true
}

// ==== user_profile db paths (0001_init.sql L4050-4077) ====

/// The 2fa column subset of `user_profile` — kept off the wide PROFILE_COLS
/// select so profile readers never pull the TOTP secrets.
#[derive(Debug, sqlx::FromRow)]
pub struct TwoFactorSecrets {
    #[sqlx(rename = "twoFactorTempSecret")]
    pub two_factor_temp_secret: Option<String>,
    #[sqlx(rename = "twoFactorSecret")]
    pub two_factor_secret: Option<String>,
    #[sqlx(rename = "twoFactorBackupSecret")]
    pub two_factor_backup_secret: Option<Vec<String>>,
}

pub async fn fetch_secrets(pool: &PgPool, user_id: &str) -> Result<TwoFactorSecrets, sqlx::Error> {
    sqlx::query_as::<_, TwoFactorSecrets>(
        r#"SELECT "twoFactorTempSecret", "twoFactorSecret", "twoFactorBackupSecret"
           FROM "user_profile" WHERE "userId" = $1"#,
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
}

/// register.ts: `userProfilesRepository.update(me.id, { twoFactorTempSecret: secret.base32 })`.
pub async fn set_temp_secret(
    pool: &PgPool,
    user_id: &str,
    secret_b32: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "user_profile" SET "twoFactorTempSecret" = $2 WHERE "userId" = $1"#)
        .bind(user_id)
        .bind(secret_b32)
        .execute(pool)
        .await?;
    Ok(())
}

/// done.ts: `twoFactorSecret: profile.twoFactorTempSecret,
/// twoFactorBackupSecret: backupCodes, twoFactorEnabled: true`.
pub async fn confirm(
    pool: &PgPool,
    user_id: &str,
    secret_b32: &str,
    backup_codes: &[String],
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"UPDATE "user_profile"
           SET "twoFactorSecret" = $2, "twoFactorBackupSecret" = $3, "twoFactorEnabled" = true
           WHERE "userId" = $1"#,
    )
    .bind(user_id)
    .bind(secret_b32)
    .bind(backup_codes.to_vec())
    .execute(pool)
    .await?;
    Ok(())
}

/// unregister.ts: `twoFactorSecret: null, twoFactorBackupSecret: null,
/// twoFactorEnabled: false, usePasswordLessLogin: false` (twoFactorTempSecret
/// is deliberately NOT cleared upstream).
pub async fn unregister(pool: &PgPool, user_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"UPDATE "user_profile"
           SET "twoFactorSecret" = NULL, "twoFactorBackupSecret" = NULL,
               "twoFactorEnabled" = false, "usePasswordLessLogin" = false
           WHERE "userId" = $1"#,
    )
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// UserAuthService.twoFactorAuthenticate's backup-code branch: membership
/// consumes the code (`twoFactorBackupSecret.filter((s) => s !== token)`).
pub async fn consume_backup_code(
    pool: &PgPool,
    user_id: &str,
    backup: &[String],
    token: &str,
) -> Result<bool, sqlx::Error> {
    let Some(pos) = backup.iter().position(|s| s == token) else {
        return Ok(false);
    };
    let mut rest = backup.to_vec();
    rest.remove(pos);
    sqlx::query(r#"UPDATE "user_profile" SET "twoFactorBackupSecret" = $2 WHERE "userId" = $1"#)
        .bind(user_id)
        .bind(&rest)
        .execute(pool)
        .await?;
    Ok(true)
}

// ==== TOTP facade (upstream UserAuthService.validateOtp shape) ====

/// `OtpEnv` mirrors validateOtp's gates: `test_mode` = NODE_ENV=test
/// (config.test on the port) and `check_dup` = the MISSKEY_TEST env var.
#[derive(Clone, Copy, Debug)]
pub struct OtpEnv {
    pub test_mode: bool,
    pub check_dup: bool,
}

impl OtpEnv {
    pub fn from_config_test_and_flag(test_mode: bool) -> Self {
        Self {
            test_mode,
            check_dup: check_duplicated_totp(),
        }
    }
}

/// UserAuthService.validateOtp: test-mode-without-the-flag accepts anything;
/// otherwise a real ±1-window TOTP check whose accepted (user, secret, step)
/// is burned for 90s (redis SET NX EX parity).
pub fn validate_otp(user_id: &str, secret_b32: &str, token: &str, env: OtpEnv) -> bool {
    if env.test_mode && !env.check_dup {
        return true;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let Some(delta) = verify_totp(secret_b32, token, now) else {
        return false;
    };
    let step = (now / TOTP_PERIOD_SECS) as i64 + delta;
    claim_used_step(user_id, secret_b32, step as u64)
}

/// Upstream `new OTPAuth.TOTP({ secret, digits: 6, period: 30 }).validate({
/// token, window: 1 })` → matched delta, else null.
pub fn verify_totp(secret_b32: &str, token: &str, unix_secs: u64) -> Option<i64> {
    let t = token.trim();
    if t.len() != 6 || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    for delta in -1i64..=1 {
        let sec = unix_secs as i64 + delta;
        if sec < 0 {
            continue;
        }
        if totp_code_at(secret_b32, sec as u64).as_deref() == Some(t) {
            return Some(delta);
        }
    }
    None
}

/// 6-digit TOTP-SHA1 over `floor(unix_secs / 30)` (OTPAuth defaults; RFC 6238).
pub fn totp_code_at(secret_b32: &str, unix_secs: u64) -> Option<String> {
    let key = base32_decode(secret_b32)?;
    let mac = hmac_sha1(&key, &((unix_secs / TOTP_PERIOD_SECS).to_be_bytes()));
    let off = (mac[19] & 0x0f) as usize;
    let bin = u32::from_be_bytes([mac[off] & 0x7f, mac[off + 1], mac[off + 2], mac[off + 3]]);
    Some(format!("{:06}", bin % 1_000_000))
}

/// `new OTPAuth.Secret()` — 20 random bytes as unpadded base32, entropy from
/// /dev/urandom with a time+address fold fallback so the call never panics on
/// unusual filesystems.
pub fn generate_secret_base32() -> String {
    let mut buf = [0u8; 20];
    // /dev/urandom must be read bound (read_exact) — a whole-file read would
    // drain the infinite stream.
    match std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)) {
        Ok(()) => {}
        Err(_) => {
            // fallback: fold timing + address entropy through sha1 (never the
            // path on the Linux runners, but keeps the fn total)
            let mut seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64
                ^ (&mut buf as *mut _ as u64);
            for chunk in buf.chunks_mut(8) {
                let digest = sha1(&seed.to_be_bytes());
                seed = u64::from_be_bytes(digest[..8].try_into().unwrap_or([0u8; 8]));
                chunk.copy_from_slice(&seed.to_be_bytes()[..chunk.len()]);
            }
        }
    }
    base32_encode(&buf)
}

// ==== vendored crypto (std-only) ====

/// SHA-1 (RFC 3174) over a complete message.
pub fn sha1(msg: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let ml = (msg.len() as u64) * 8;
    let mut data = Vec::with_capacity(msg.len() + 72);
    data.extend_from_slice(msg);
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&ml.to_be_bytes());

    for block in data.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, wi) in w.iter_mut().take(16).enumerate() {
            *wi = u32::from_be_bytes([
                block[4 * i],
                block[4 * i + 1],
                block[4 * i + 2],
                block[4 * i + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for (i, hi) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&hi.to_be_bytes());
    }
    out
}

/// HMAC-SHA1 (RFC 2104, block size 64).
pub fn hmac_sha1(key: &[u8], msg: &[u8]) -> [u8; 20] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..20].copy_from_slice(&sha1(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Vec::with_capacity(64 + msg.len());
    inner.extend_from_slice(&ipad);
    inner.extend_from_slice(msg);
    let ih = sha1(&inner);
    let mut outer = Vec::with_capacity(84);
    outer.extend_from_slice(&opad);
    outer.extend_from_slice(&ih);
    sha1(&outer)
}

const B32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// RFC 4648 base32 without padding (OTPAuth `Secret.base32` shape).
pub fn base32_encode(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(5) {
        let mut buf = [0u8; 5];
        buf[..chunk.len()].copy_from_slice(chunk);
        let bits = (buf[0] as u64) << 32
            | (buf[1] as u64) << 24
            | (buf[2] as u64) << 16
            | (buf[3] as u64) << 8
            | buf[4] as u64;
        let emitted = chunk.len() * 8 / 5 + usize::from(chunk.len() * 8 % 5 > 0);
        for i in 0..emitted {
            out.push(B32_ALPHABET[(bits >> (35 - 5 * i)) as usize & 31] as char);
        }
    }
    out
}

/// RFC 4648 base32 decode; `=` padding and whitespace are skipped, other
/// foreign characters reject (OTPAuth `Secret.fromBase32` parity).
pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    let mut out = Vec::new();
    for ch in s.chars() {
        if ch == '=' || ch.is_whitespace() {
            continue;
        }
        let up = ch.to_ascii_uppercase();
        let v = match up {
            'A'..='Z' => up as u32 - 'A' as u32,
            '2'..='7' => up as u32 - '2' as u32 + 26,
            _ => return None,
        };
        acc = (acc << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
            // keep only the surviving low bits so acc can never overflow u32
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}
