//! webauthn: the security-key (passkey) lane behind i/2fa/{register-key,
//! key-done, update-key, remove-key, password-less} + the signin-flow passkey
//! step (upstream core/WebAuthnService.ts + endpoints/i/2fa/*.ts, pinned
//! ea2c3aed, @simplewebauthn/server 14.0.3).
//!
//! CRYPTO IS VENDORED, std-only (the 2fa-resume slice): SHA-256 (FIPS 180-4),
//! a CBOR slice reader (decodeAttestationObject / decodeCredentialPublicKey =
//! parseAuthenticatorData's int-key map), DER ECDSA-Sig-Value unwrap and full
//! ECDSA P-256 verification (WebCrypto `subtle.verify({ name: 'ECDSA', hash:
//! 'SHA-256' })` parity). The upstream test (test/e2e/2fa.ts) signs with a
//! real P-256 key, so the verification is the real protocol — no stub road.
//!
//! Redis setex/getdel (90s) challenges become an in-memory registry keyed
//! (kind, userId) with the same TTL — the same shape the TOTP used-step burn
//! rides in two_factor.rs.
//!
//! RP DEVIATION (documented): upstream `getRelyingParty()` splits
//! `rpId: config.hostname` (host WITHOUT port) from `origin: config.url`.
//! The pinned 2fa.ts builds authData.rpIdHash from `config.host` (host WITH
//! port) and clientDataJSON origin from `scheme + '://' + config.host`, and
//! the harness config URL carries the port (`http://127.0.0.1:3000/`), so the
//! port pins rpId = config.host() and origin = config.url minus the trailing
//! slash to keep both hashes/origins equal — upstream's own CI URL is
//! port-less, where host and hostname coincide.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use sqlx::PgPool;

// ==== challenge store (redis `webauthn:*Challenge:*` setex/getdel parity) ====

static CHALLENGES: Mutex<Vec<((&'static str, String), String, Instant)>> = Mutex::new(Vec::new());

/// upstream redis EX: 90 seconds.
const CHALLENGE_TTL: Duration = Duration::from_secs(90);

const REGISTRATION_KIND: &str = "registration";
const AUTHENTICATION_KIND: &str = "authentication";

/// `setex(webauthn:registrationChallenge:<userId>, 90, challenge)` shape — one
/// live challenge per (kind, key), the newest write wins.
pub fn put_challenge_kind(kind: &'static str, key: &str, challenge: &str) {
    let mut guard = CHALLENGES.lock().unwrap();
    let now = Instant::now();
    guard.retain(|(_, _, deadline)| *deadline > now);
    guard.retain(|((k_kind, k_key), _, _)| !(*k_kind == kind && k_key.as_str() == key));
    guard.push((
        (kind, key.to_string()),
        challenge.to_string(),
        now + CHALLENGE_TTL,
    ));
}

/// Get-and-delete (`getdel`) — upstream reads then deletes unconditionally
/// (verifyRegistration L95-101 / verifyAuthentication getdel).
pub fn take_challenge_kind(kind: &str, key: &str) -> Option<String> {
    let mut guard = CHALLENGES.lock().unwrap();
    let now = Instant::now();
    guard.retain(|(_, _, deadline)| *deadline > now);
    let pos = guard
        .iter()
        .position(|((k_kind, k_val), _, _)| *k_kind == kind && k_val.as_str() == key)?;
    let ((_, _), challenge, _) = guard.remove(pos);
    Some(challenge)
}

pub fn put_registration_challenge(user_id: &str, challenge: &str) {
    put_challenge_kind(REGISTRATION_KIND, user_id, challenge);
}

pub fn take_registration_challenge(user_id: &str) -> Option<String> {
    take_challenge_kind(REGISTRATION_KIND, user_id)
}

pub fn put_authentication_challenge(user_id: &str, challenge: &str) {
    put_challenge_kind(AUTHENTICATION_KIND, user_id, challenge);
}

pub fn take_authentication_challenge(user_id: &str) -> Option<String> {
    take_challenge_kind(AUTHENTICATION_KIND, user_id)
}

/// `generateChallenge()` → 32 random bytes, base64url (isoBase64URL.fromBuffer
/// over the WebCrypto random, upstream default challenge).
pub fn new_challenge_b64url() -> String {
    let mut buf = [0u8; 32];
    match std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf))
    {
        Ok(()) => {}
        Err(_) => {
            // same total-fn fallback the TOTP secret generator rides
            let mut seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64
                ^ (&mut buf as *mut _ as u64);
            for chunk in buf.chunks_mut(8) {
                let digest = crate::two_factor::sha1(&seed.to_be_bytes());
                seed = u64::from_be_bytes(digest[..8].try_into().unwrap_or([0u8; 8]));
                chunk.copy_from_slice(&seed.to_be_bytes()[..chunk.len()]);
            }
        }
    }
    b64url_encode(&buf)
}

// ==== base64url (isoBase64URL parity, -_ alphabet, no padding) ====

const B64URL_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub fn b64url_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64URL_ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(B64URL_ALPHABET[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(B64URL_ALPHABET[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(B64URL_ALPHABET[n as usize & 63] as char);
        }
    }
    out
}

/// Accepts the standard unpadded (client surface) form plus `=` padding;
/// `-`/`_` remap, whitespace skipped, other characters reject.
pub fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for ch in s.chars() {
        if ch == '=' || ch.is_whitespace() {
            continue;
        }
        let v = match ch {
            'A'..='Z' => ch as u32 - 'A' as u32,
            'a'..='z' => ch as u32 - 'a' as u32 + 26,
            '0'..='9' => ch as u32 - '0' as u32 + 52,
            '-' => 62,
            '_' => 63,
            _ => return None,
        };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

// ==== SHA-256 (FIPS 180-4) ====

/// ISO/IEC 1010-3 padding, 64×32-bit rounds over big-endian words.
pub fn sha256(msg: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
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
        let mut w = [0u32; 64];
        for (i, wi) in w.iter_mut().take(16).enumerate() {
            *wi = u32::from_be_bytes([
                block[4 * i],
                block[4 * i + 1],
                block[4 * i + 2],
                block[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for (i, &wi) in w.iter().enumerate() {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(wi);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, hi) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&hi.to_be_bytes());
    }
    out
}

// ==== CBOR (the subset decodeAttestationObject/decodeCredentialPublicKey ride) ====

/// RFC 8949 values the WebAuthn objects use: ints, byte strings, text,
/// maps, arrays, bool/null. Definite lengths only (cbor2 emits definite).
#[derive(Debug, Clone, PartialEq)]
pub enum Cbor {
    UInt(u64),
    /// decoded negative value (major type 1)
    Neg(i128),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Cbor>),
    Map(Vec<(Cbor, Cbor)>),
    Bool(bool),
    Null,
}

/// Returns (value, offset-after-value).
pub fn cbor_scan(buf: &[u8], pos: usize) -> Option<(Cbor, usize)> {
    cbor_scan_depth(buf, pos, 0)
}

fn cbor_scan_depth(buf: &[u8], pos: usize, depth: u32) -> Option<(Cbor, usize)> {
    if depth > 8 {
        return None;
    }
    let head = *buf.get(pos)?;
    let (mt, ai) = (head >> 5, head & 0x1f);
    let mut npos = pos + 1;
    let arg: u64 = match ai {
        0..=23 => ai as u64,
        24 => {
            let v = *buf.get(npos)? as u64;
            npos += 1;
            v
        }
        25 => {
            let v = u16::from_be_bytes([*buf.get(npos)?, *buf.get(npos + 1)?]) as u64;
            npos += 2;
            v
        }
        26 => {
            let v = u32::from_be_bytes([
                *buf.get(npos)?,
                *buf.get(npos + 1)?,
                *buf.get(npos + 2)?,
                *buf.get(npos + 3)?,
            ]) as u64;
            npos += 4;
            v
        }
        27 => {
            let mut b = [0u8; 8];
            for (i, slot) in b.iter_mut().enumerate() {
                *slot = *buf.get(npos + i)?;
            }
            npos += 8;
            u64::from_be_bytes(b)
        }
        _ => return None, // indefinite (additional info 31) not emitted by cbor2 here
    };
    match mt {
        0 => Some((Cbor::UInt(arg), npos)),
        1 => Some((Cbor::Neg(-1 - arg as i128), npos)),
        2 => {
            let end = npos.checked_add(arg as usize)?;
            if end > buf.len() {
                return None;
            }
            Some((Cbor::Bytes(buf[npos..end].to_vec()), end))
        }
        3 => {
            let end = npos.checked_add(arg as usize)?;
            if end > buf.len() {
                return None;
            }
            Some((
                Cbor::Text(String::from_utf8(buf[npos..end].to_vec()).ok()?),
                end,
            ))
        }
        4 => {
            let mut items = Vec::with_capacity(arg.min(16) as usize);
            for _ in 0..arg {
                let (v, end) = cbor_scan_depth(buf, npos, depth + 1)?;
                items.push(v);
                npos = end;
            }
            Some((Cbor::Array(items), npos))
        }
        5 => {
            let mut items = Vec::with_capacity(arg.min(16) as usize);
            for _ in 0..arg {
                let (k, end) = cbor_scan_depth(buf, npos, depth + 1)?;
                let (v, end) = cbor_scan_depth(buf, end, depth + 1)?;
                items.push((k, v));
                npos = end;
            }
            Some((Cbor::Map(items), npos))
        }
        7 => match arg {
            20 => Some((Cbor::Bool(false), npos)),
            21 => Some((Cbor::Bool(true), npos)),
            22 => Some((Cbor::Null, npos)),
            _ => None,
        },
        _ => None,
    }
}

/// COSE map int-key getter (COSEKEYS: 1=kty, 3=alg, -1=crv, -2=x, -3=y).
pub fn cbor_map_int<'a>(cb: &'a Cbor, key: i64) -> Option<&'a Cbor> {
    let Cbor::Map(items) = cb else {
        return None;
    };
    items
        .iter()
        .find(|(k, _)| match k {
            Cbor::UInt(v) => *v as i64 == key,
            Cbor::Neg(v) => *v as i64 == key,
            _ => false,
        })
        .map(|(_, v)| v)
}

/// Attestation-object text-key getter ("fmt"/"attStmt"/"authData").
pub fn cbor_map_text<'a>(cb: &'a Cbor, key: &str) -> Option<&'a Cbor> {
    let Cbor::Map(items) = cb else {
        return None;
    };
    items
        .iter()
        .find(|(k, _)| matches!(k, Cbor::Text(t) if t == key))
        .map(|(_, v)| v)
}

// ==== P-256 (FIPS 186-5) modular + ECDSA-256/SHA-256 verification ====

/// 256-bit integer as little-endian u64 limbs.
type U256 = [u64; 4];
const ZERO: U256 = [0, 0, 0, 0];
const ONE: U256 = [1, 0, 0, 0];

// NIST P-256 domain parameters (FIPS 186-5 §G.1.2)
/// p = 0xffffffff00000001000000000000000000000000ffffffffffffffffffffffff
const P: U256 = [
    0xFFFF_FFFF_FFFF_FFFF,
    0x0000_0000_FFFF_FFFF,
    0xFFFF_FFFF_0000_0000,
    0xFFFF_FFFF_0000_0001,
];
/// a = p - 3
const A_P256: U256 = [
    0xFFFF_FFFF_FFFF_FFFC,
    0x0000_0000_FFFF_FFFF,
    0xFFFF_FFFF_0000_0000,
    0xFFFF_FFFF_0000_0001,
];
/// the curve order n
const N_P256: U256 = [
    0xF3B9_CAC2_FC63_2551,
    0xBCE6_FAAD_A717_9E84,
    0xFFFF_FFFF_FFFF_FFFF,
    0xFFFF_FFFF_0000_0000,
];
/// Gx = 0x6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296
const GX_P256: U256 = [
    0xF4A1_3945_D898_C296,
    0x7703_7D81_2DEB_33A0,
    0xF8BC_E6E5_63A4_40F2,
    0x6B17_D1F2_E12C_4247,
];
/// Gy = 0x4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5
const GY_P256: U256 = [
    0xCBB6_4068_37BF_51F5,
    0x2BCE_3357_6B31_5ECE,
    0x8EE7_EB4A_7C0F_9E16,
    0x4FE3_42E2_FE1A_7F9B,
];

fn cmp256(a: &U256, b: &U256) -> std::cmp::Ordering {
    for i in (0..4).rev() {
        match a[i].cmp(&b[i]) {
            std::cmp::Ordering::Equal => {}
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

fn add256(a: &U256, b: &U256) -> (U256, u64) {
    let mut out = ZERO;
    let mut carry = 0u128;
    for i in 0..4 {
        let s = a[i] as u128 + b[i] as u128 + carry;
        out[i] = s as u64;
        carry = s >> 64;
    }
    (out, carry as u64)
}

fn sub256(a: &U256, b: &U256) -> (U256, u64) {
    let mut out = ZERO;
    let mut borrow = 0i128;
    for i in 0..4 {
        let d = a[i] as i128 - b[i] as i128 - borrow;
        if d < 0 {
            out[i] = d as u64;
            borrow = 1;
        } else {
            out[i] = d as u64;
            borrow = 0;
        }
    }
    (out, borrow as u64)
}

fn add_mod(a: &U256, b: &U256, m: &U256) -> U256 {
    let (s, carry) = add256(a, b);
    if carry == 1 || cmp256(&s, m) != std::cmp::Ordering::Less {
        sub256(&s, m).0
    } else {
        s
    }
}

fn sub_mod(a: &U256, b: &U256, m: &U256) -> U256 {
    let (d, borrow) = sub256(a, b);
    if borrow == 1 { add256(&d, m).0 } else { d }
}

fn mul_wide(a: &U256, b: &U256) -> [u64; 8] {
    let mut out = [0u64; 8];
    for i in 0..4 {
        let mut carry = 0u128;
        for j in 0..4 {
            let t = a[i] as u128 * b[j] as u128 + out[i + j] as u128 + carry;
            out[i + j] = t as u64;
            carry = t >> 64;
        }
        out[i + 4] = carry as u64;
    }
    out
}

fn shl1_carry(a: &U256) -> (U256, u64) {
    let mut out = ZERO;
    let mut carry = 0u64;
    for i in 0..4 {
        out[i] = (a[i] << 1) | carry;
        carry = a[i] >> 63;
    }
    (out, carry)
}

/// 512 % 256-bit modulus via binary long division (msb-first; the running
/// remainder stays < 2m at every step, so one conditional subtract restores
/// the < m invariant — the same schoolbook shape the TOTP lane vendors).
fn reduce_wide(t: &[u64; 8], m: &U256) -> U256 {
    let mut rem = ZERO;
    for bit in (0..512).rev() {
        let bbit = (t[bit / 64] >> (bit % 64)) & 1;
        let (shifted, mut ovf) = shl1_carry(&rem);
        if bbit == 1 {
            let (s, c) = add256(&shifted, &ONE);
            ovf += c;
            rem = s;
        } else {
            rem = shifted;
        }
        if ovf > 0 || cmp256(&rem, m) != std::cmp::Ordering::Less {
            rem = sub256(&rem, m).0;
        }
    }
    rem
}

fn mul_mod(a: &U256, b: &U256, m: &U256) -> U256 {
    reduce_wide(&mul_wide(a, b), m)
}

fn pow_mod(base: &U256, exp: &U256, m: &U256) -> U256 {
    let mut result = ONE;
    let mut acc = *base;
    for i in (0..256).rev() {
        result = mul_mod(&result, &result, m);
        if (exp[i / 64] >> (i % 64)) & 1 == 1 {
            result = mul_mod(&result, &acc, m);
        }
    }
    result
}

/// Fermat inverse over a prime field.
fn inv_mod(x: &U256, m: &U256) -> U256 {
    let (e, _) = sub256(m, &[2, 0, 0, 0]);
    pow_mod(x, &e, m)
}

/// Jacobian point (all coordinates already reduced mod p).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Point {
    x: U256,
    y: U256,
    z: U256,
}

const INF: Point = Point {
    x: ONE,
    y: ONE,
    z: ZERO,
};

/// General Jacobian doubling (works for a = -3 via the `a*Z⁴` term).
fn pt_double(p: &Point) -> Point {
    if p.z == ZERO {
        return INF;
    }
    let xx = mul_mod(&p.x, &p.x, &P);
    let yy = mul_mod(&p.y, &p.y, &P);
    let zz = mul_mod(&p.z, &p.z, &P);
    // S = 4*X*Y^2
    let xyy = mul_mod(&p.x, &yy, &P);
    let s = add_mod(&add_mod(&xyy, &xyy, &P), &add_mod(&xyy, &xyy, &P), &P);
    // M = 3*X^2 + a*Z^4
    let z4 = mul_mod(&zz, &zz, &P);
    let az4 = mul_mod(&A_P256, &z4, &P);
    let m3 = add_mod(&add_mod(&xx, &xx, &P), &xx, &P);
    let mm = add_mod(&m3, &az4, &P);
    // X' = M^2 - 2S
    let mm2 = mul_mod(&mm, &mm, &P);
    let x3 = sub_mod(&mm2, &add_mod(&s, &s, &P), &P);
    // Y' = M*(S - X') - 8*Y^4
    let y4 = mul_mod(&yy, &yy, &P);
    let y4x2 = add_mod(&y4, &y4, &P);
    let y4x4 = add_mod(&y4x2, &y4x2, &P);
    let y8 = add_mod(&y4x4, &y4x4, &P);
    let y3 = sub_mod(&mul_mod(&mm, &sub_mod(&s, &x3, &P), &P), &y8, &P);
    // Z' = 2*Y*Z
    let yz = mul_mod(&p.y, &p.z, &P);
    let z3 = add_mod(&yz, &yz, &P);
    Point {
        x: x3,
        y: y3,
        z: z3,
    }
}

/// General Jacobian addition; degenerate equal points double, opposites fall
/// back to infinity.
fn pt_add(p1: &Point, p2: &Point) -> Point {
    if p1.z == ZERO {
        return *p2;
    }
    if p2.z == ZERO {
        return *p1;
    }
    let z1z1 = mul_mod(&p1.z, &p1.z, &P);
    let z2z2 = mul_mod(&p2.z, &p2.z, &P);
    let u1 = mul_mod(&p1.x, &z2z2, &P);
    let u2 = mul_mod(&p2.x, &z1z1, &P);
    let s1 = mul_mod(&p1.y, &mul_mod(&z2z2, &p2.z, &P), &P);
    let s2 = mul_mod(&p2.y, &mul_mod(&z1z1, &p1.z, &P), &P);
    if cmp256(&u1, &u2) == std::cmp::Ordering::Equal {
        if cmp256(&s1, &s2) == std::cmp::Ordering::Equal {
            return pt_double(p1);
        }
        return INF;
    }
    let h = sub_mod(&u2, &u1, &P);
    let r = sub_mod(&s2, &s1, &P);
    let hh = mul_mod(&h, &h, &P);
    let hhh = mul_mod(&h, &hh, &P);
    let v = mul_mod(&u1, &hh, &P);
    // X3 = R^2 - H^3 - 2*V
    let r2 = mul_mod(&r, &r, &P);
    let two_v = add_mod(&v, &v, &P);
    let x3 = sub_mod(&sub_mod(&r2, &hhh, &P), &two_v, &P);
    // Y3 = R*(V - X3) - S1*H^3
    let y3 = sub_mod(
        &mul_mod(&r, &sub_mod(&v, &x3, &P), &P),
        &mul_mod(&s1, &hhh, &P),
        &P,
    );
    // Z3 = Z1*Z2*H
    let z3 = mul_mod(&mul_mod(&p1.z, &p2.z, &P), &h, &P);
    Point {
        x: x3,
        y: y3,
        z: z3,
    }
}

fn scalar_mult(k: &U256, pt: &Point) -> Point {
    let mut acc = INF;
    for i in (0..256).rev() {
        acc = pt_double(&acc);
        if (k[i / 64] >> (i % 64)) & 1 == 1 {
            acc = pt_add(&acc, pt);
        }
    }
    acc
}

/// affine x of a finite Jacobian point (X / Z²)
fn affine_x(pt: &Point) -> Option<U256> {
    if pt.z == ZERO {
        return None;
    }
    let z2 = mul_mod(&pt.z, &pt.z, &P);
    let inv = inv_mod(&z2, &P);
    Some(mul_mod(&pt.x, &inv, &P))
}

fn be32_to_u256(bytes: &[u8; 32]) -> U256 {
    let mut out = ZERO;
    for i in 0..4 {
        let mut b = [0u8; 8];
        b.copy_from_slice(&bytes[i * 8..i * 8 + 8]);
        out[3 - i] = u64::from_be_bytes(b);
    }
    out
}

/// DER ECDSA-Sig-Value (ASN.1 SEQUENCE of two INTEGERs) → (r, s).
fn der_parse_ecdsa_sig(der: &[u8]) -> Option<(U256, U256)> {
    if der.first() != Some(&0x30) {
        return None;
    }
    // short-form length (sig < 128 bytes) with long-form tolerated
    let mut pos = 1usize;
    let seq_len = if der.get(pos).is_some_and(|l| *l < 0x80) {
        let l = der[pos] as usize;
        pos += 1;
        l
    } else if der.get(pos) == Some(&0x81) {
        let l = *der.get(pos + 1)? as usize;
        pos += 2;
        l
    } else {
        return None;
    };
    if pos + seq_len > der.len() {
        return None;
    }
    let read_int = |pos: &mut usize| -> Option<U256> {
        if der.get(*pos) != Some(&0x02) {
            return None;
        }
        let len = *der.get(*pos + 1)? as usize;
        let start = *pos + 2;
        if start + len > der.len() || len == 0 || len > 33 {
            return None;
        }
        let int_bytes = &der[start..start + len];
        // strip leading zero pad; reject a value that needs it after the pad
        let mut sig = int_bytes;
        if sig.first() == Some(&0x00) {
            sig = &sig[1..];
        }
        if sig.is_empty() || sig.first().is_some_and(|b| *b & 0x80 == 0x80) || sig.len() > 32 {
            return None;
        }
        *pos = start + len;
        let mut b = [0u8; 32];
        b[32 - sig.len()..].copy_from_slice(sig);
        Some(be32_to_u256(&b))
    };
    let r = read_int(&mut pos)?;
    let s = read_int(&mut pos)?;
    Some((r, s))
}

/// WebCrypto `subtle.verify({ name: 'ECDSA', hash: 'SHA-256' })` over
/// secp256r1 — `der_sig` is the WebAuthn ASN.1-wrapped (r, s), `msg` is the
/// raw signature base (authenticatorData || sha256(clientDataJSON)).
pub fn ecdsa_p256_sha256_verify(
    msg: &[u8],
    pub_x: &[u8; 32],
    pub_y: &[u8; 32],
    der_sig: &[u8],
) -> bool {
    let Some((r, s)) = der_parse_ecdsa_sig(der_sig) else {
        return false;
    };
    // 1 ≤ r, s < n (subtle rejects out-of-range components)
    if cmp256(&r, &ZERO) != std::cmp::Ordering::Greater
        || cmp256(&s, &ZERO) != std::cmp::Ordering::Greater
        || cmp256(&r, &N_P256) != std::cmp::Ordering::Less
        || cmp256(&s, &N_P256) != std::cmp::Ordering::Less
    {
        return false;
    }
    // e = SHA256(msg) interpreted big-endian; a 256-bit digest needs at most
    // one subtract mod n (n > 2^255)
    let hash = sha256(msg);
    let mut e = be32_to_u256(&hash);
    if cmp256(&e, &N_P256) != std::cmp::Ordering::Less {
        e = sub256(&e, &N_P256).0;
    }
    let q = Point {
        x: be32_to_u256(pub_x),
        y: be32_to_u256(pub_y),
        z: ONE,
    };
    let g = Point {
        x: GX_P256,
        y: GY_P256,
        z: ONE,
    };
    // w = s^-1 mod n; u1 = e*w, u2 = r*w; R = u1*G + u2*Q
    let w = inv_mod(&s, &N_P256);
    if w == ZERO {
        return false;
    }
    let u1 = mul_mod(&e, &w, &N_P256);
    let u2 = mul_mod(&r, &w, &N_P256);
    let acc = pt_add(&scalar_mult(&u1, &g), &scalar_mult(&u2, &q));
    let Some(x_aff) = affine_x(&acc) else {
        return false;
    };
    let v = if cmp256(&x_aff, &N_P256) != std::cmp::Ordering::Less {
        sub256(&x_aff, &N_P256).0
    } else {
        x_aff
    };
    v == r
}

// ==== user_security_key db paths (0001 baseline; pinned MiUserSecurityKey) ====

/// The verify lane's key shape (verifyAuthentication findOneBy({id, userId})).
#[derive(Debug, sqlx::FromRow)]
pub struct SecurityKeyRow {
    pub id: String,
    #[sqlx(rename = "userId")]
    pub user_id: String,
    pub name: String,
    #[sqlx(rename = "publicKey")]
    pub public_key: String,
    pub counter: i64,
}

/// The secureKeysList/[i] pack shape (select id, name, lastUsed).
#[derive(Debug, sqlx::FromRow)]
pub struct SecurityKeyBrief {
    pub id: String,
    pub name: String,
    /// `to_char(... 'Z')` ISO-8601 mirroring JSON.stringify(Date) shape.
    #[sqlx(rename = "lastUsed")]
    pub last_used: String,
}

/// key-done.ts L101-110 insert (upstream leaves `lastUsed` at its now()
/// column default — the e2e's `lastUsed != undefined` rides that default).
pub async fn insert_key(
    pool: &PgPool,
    id: &str,
    user_id: &str,
    name: &str,
    public_key_b64url: &str,
    counter: i64,
    device_type: Option<&str>,
    backed_up: Option<bool>,
    transports: Option<Vec<String>>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO "user_security_key"
           ("id", "userId", "name", "publicKey", "counter", "credentialDeviceType",
            "credentialBackedUp", "transports")
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"#,
    )
    .bind(id)
    .bind(user_id)
    .bind(name)
    .bind(public_key_b64url)
    .bind(counter)
    .bind(device_type)
    .bind(backed_up)
    .bind(transports)
    .execute(pool)
    .await?;
    Ok(())
}

/// SigninApiService L145 `countBy({ userId }).then(result => result >= 1)`.
pub async fn count_by_user(pool: &PgPool, user_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(r#"SELECT count(*) FROM "user_security_key" WHERE "userId" = $1"#)
        .bind(user_id)
        .fetch_one(pool)
        .await
}

pub async fn list_briefs(
    pool: &PgPool,
    user_id: &str,
) -> Result<Vec<SecurityKeyBrief>, sqlx::Error> {
    sqlx::query_as::<_, SecurityKeyBrief>(
        r#"SELECT "id", "name",
                  to_char("lastUsed" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS "lastUsed"
           FROM "user_security_key" WHERE "userId" = $1
           ORDER BY "id""#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
}

pub async fn find_by_id(pool: &PgPool, id: &str) -> Result<Option<SecurityKeyRow>, sqlx::Error> {
    sqlx::query_as::<_, SecurityKeyRow>(
        r#"SELECT "id", "userId", "name", "publicKey", "counter"
           FROM "user_security_key" WHERE "id" = $1"#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// update-key.ts L66 `update(key.id, { name })`.
pub async fn set_key_name(pool: &PgPool, id: &str, name: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "user_security_key" SET "name" = $2 WHERE "id" = $1"#)
        .bind(id)
        .bind(name)
        .execute(pool)
        .await?;
    Ok(())
}

/// remove-key.ts L75 `delete({ userId, id: ps.credentialId })` — returns the
/// affected row count.
pub async fn delete_own(pool: &PgPool, user_id: &str, id: &str) -> Result<u64, sqlx::Error> {
    let r = sqlx::query(r#"DELETE FROM "user_security_key" WHERE "userId" = $1 AND "id" = $2"#)
        .bind(user_id)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(r.rows_affected())
}

/// verifyAuthentication's success tail (L231-238) and the remove-key
/// `keyCount === 0 → usePasswordLessLogin: false` flip (L92-96).
pub async fn touch_key(
    pool: &PgPool,
    id: &str,
    counter: i64,
    device_type: Option<&str>,
    backed_up: Option<bool>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"UPDATE "user_security_key"
           SET "lastUsed" = now(), "counter" = $2,
               "credentialDeviceType" = $3, "credentialBackedUp" = $4
           WHERE "id" = $1"#,
    )
    .bind(id)
    .bind(counter)
    .bind(device_type)
    .bind(backed_up)
    .execute(pool)
    .await?;
    Ok(())
}

/// user_profile.usePasswordLessLogin write (password-less.ts L63-73 and the
/// remove-key flip).
pub async fn set_use_password_less_login(
    pool: &PgPool,
    user_id: &str,
    value: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "user_profile" SET "usePasswordLessLogin" = $2 WHERE "userId" = $1"#)
        .bind(user_id)
        .bind(value)
        .execute(pool)
        .await?;
    Ok(())
}

// ==== verify lanes (WebAuthnService.verifyRegistration/verifyAuthentication) ====

/// Registration verify output — the fields key-done.ts persists plus the
/// response-transports passthrough.
#[derive(Debug, Clone)]
pub struct VerifiedRegistration {
    /// base64url credential id (registrationInfo.credential.id)
    pub credential_id: String,
    /// base64url COSE key bytes (registrationInfo.credential.publicKey)
    pub public_key_b64url: String,
    pub counter: i64,
    pub user_verified: bool,
    pub device_type: &'static str,
    pub backed_up: bool,
    pub transports: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VerifyFail {
    /// IdentifiableError upstream → INTERNAL_ERROR 500 on the handler.
    Error,
}

struct ClientData {
    typ: String,
    challenge: String,
    origin: String,
}

fn parse_client_data_json(raw_b64url: &str) -> Result<ClientData, VerifyFail> {
    let raw = b64url_decode(raw_b64url).ok_or(VerifyFail::Error)?;
    let v: serde_json::Value = serde_json::from_slice(&raw).map_err(|_| VerifyFail::Error)?;
    Ok(ClientData {
        typ: v
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or(VerifyFail::Error)?
            .to_string(),
        challenge: v
            .get("challenge")
            .and_then(serde_json::Value::as_str)
            .ok_or(VerifyFail::Error)?
            .to_string(),
        origin: v
            .get("origin")
            .and_then(serde_json::Value::as_str)
            .ok_or(VerifyFail::Error)?
            .to_string(),
    })
}

/// parseAuthenticatorData + matchExpectedRPID — returns None when the
/// authenticator bytes are malformed or the rpIdHash does not match.
fn parse_auth_data(auth_data: &[u8], rp_id: &str) -> Option<AuthData> {
    if auth_data.len() < 37 {
        return None;
    }
    let rp_id_hash = &auth_data[..32];
    if rp_id_hash != sha256(rp_id.as_bytes()).as_slice() {
        return None;
    }
    let flags = auth_data[32];
    let counter = u32::from_be_bytes([auth_data[33], auth_data[34], auth_data[35], auth_data[36]]);
    let mut out = AuthData {
        flags,
        counter: counter as i64,
        credential_id: None,
        cose_key: None,
    };
    if flags & 0x40 != 0 {
        // attested credential data: AAGUID(16) credIdLen(2) credId(L) COSE key
        if auth_data.len() < 37 + 16 + 2 {
            return None;
        }
        let cred_len = u16::from_be_bytes([auth_data[53], auth_data[54]]) as usize;
        let cred_start = 55usize;
        let cred_end = cred_start.checked_add(cred_len)?;
        if auth_data.len() < cred_end {
            return None;
        }
        let cred_id = auth_data[cred_start..cred_end].to_vec();
        let (_, cose_end) = cbor_scan(auth_data, cred_end)?;
        let cose_bytes = auth_data[cred_end..cose_end].to_vec();
        out.credential_id = Some(cred_id);
        out.cose_key = Some(cose_bytes);
    }
    Some(out)
}

struct AuthData {
    flags: u8,
    counter: i64,
    credential_id: Option<Vec<u8>>,
    cose_key: Option<Vec<u8>>,
}

/// credential.id must equal rawId and both be the base64url of the response
/// id (verifyRegistrationResponse L106-112 / verifyAuthentication L71-76).
fn credential_id_strings(credential: &serde_json::Value) -> Result<(String, String), VerifyFail> {
    let get_str = |k: &str| -> Result<String, VerifyFail> {
        credential
            .get(k)
            .and_then(serde_json::Value::as_str)
            .map(|s| s.to_string())
            .ok_or(VerifyFail::Error)
    };
    Ok((get_str("id")?, get_str("rawId")?))
}

/// WebAuthnService.verifyRegistration (L84-138): stored-challenge fetch+del,
/// clientDataJSON (type/challenge/origin), attestationObject parse,
/// rpIdHash/flags/AAGUID/COSE-key validation for the fmt=none family.
pub fn verify_registration(
    user_id: &str,
    rp_id: &str,
    rp_origin: &str,
    credential: &serde_json::Value,
) -> Result<VerifiedRegistration, VerifyFail> {
    // redis challenge getdel first (L95-101)
    let challenge = take_registration_challenge(user_id).ok_or(VerifyFail::Error)?;
    // Ensure ID is base64url-encoded (id === rawId)
    let (id, raw_id) = credential_id_strings(credential)?;
    if id != raw_id {
        return Err(VerifyFail::Error);
    }
    if credential.get("type").and_then(serde_json::Value::as_str) != Some("public-key") {
        return Err(VerifyFail::Error);
    }
    let response = credential
        .get("response")
        .and_then(|r| r.as_object())
        .ok_or(VerifyFail::Error)?;
    let cdj = response
        .get("clientDataJSON")
        .and_then(serde_json::Value::as_str)
        .ok_or(VerifyFail::Error)?;
    let cd = parse_client_data_json(cdj)?;
    // type check (v14 default expectedType)
    if cd.typ != "webauthn.create" {
        return Err(VerifyFail::Error);
    }
    // challenge (string form, L139-142)
    if cd.challenge != challenge {
        return Err(VerifyFail::Error);
    }
    // origin
    if cd.origin != rp_origin {
        return Err(VerifyFail::Error);
    }
    let attestation_b64 = response
        .get("attestationObject")
        .and_then(serde_json::Value::as_str)
        .ok_or(VerifyFail::Error)?;
    let attestation = b64url_decode(attestation_b64).ok_or(VerifyFail::Error)?;
    let (decoded, _) = cbor_scan(&attestation, 0).ok_or(VerifyFail::Error)?;
    let fmt = cbor_map_text(&decoded, "fmt")
        .and_then(|f| match f {
            Cbor::Text(t) => Some(t.clone()),
            _ => None,
        })
        .ok_or(VerifyFail::Error)?;
    let auth_data = cbor_map_text(&decoded, "authData")
        .and_then(|f| match f {
            Cbor::Bytes(b) => Some(b.clone()),
            _ => None,
        })
        .ok_or(VerifyFail::Error)?;
    let parsed = parse_auth_data(&auth_data, rp_id).ok_or(VerifyFail::Error)?;
    // requireUserPresence (default true) + requireUserVerification (L206-214)
    if parsed.flags & 0x01 == 0 {
        return Err(VerifyFail::Error);
    }
    if parsed.flags & 0x04 == 0 {
        return Err(VerifyFail::Error);
    }
    let cred_id = parsed.credential_id.clone().ok_or(VerifyFail::Error)?;
    let cose = parsed.cose_key.clone().ok_or(VerifyFail::Error)?;
    // supportedAlgorithmIDs default [-8, -7, -257] (L232-241)
    let (cose_decoded, _) = cbor_scan(&cose, 0).ok_or(VerifyFail::Error)?;
    let alg = cbor_map_int(&cose_decoded, 3)
        .and_then(|v| match v {
            Cbor::Neg(n) => Some(*n as i64),
            Cbor::UInt(u) => Some(*u as i64),
            _ => None,
        })
        .ok_or(VerifyFail::Error)?;
    if ![-8, -7, -257].contains(&alg) {
        return Err(VerifyFail::Error);
    }
    // fmt === 'none' → attStmt must be empty, verified = true (L341-351); any
    // other format needs the attestation-signature verifiers the pinned 2fa.ts
    // never sends — treated as unsupported (same throw surface).
    if fmt != "none" {
        return Err(VerifyFail::Error);
    }
    let att_stmt_nonempty = cbor_map_text(&decoded, "attStmt")
        .and_then(|v| match v {
            Cbor::Map(items) => Some(!items.is_empty()),
            _ => None,
        })
        .unwrap_or(true);
    if att_stmt_nonempty {
        return Err(VerifyFail::Error);
    }
    let device_type = if parsed.flags & 0x08 != 0 {
        "multiDevice"
    } else {
        "singleDevice"
    };
    Ok(VerifiedRegistration {
        credential_id: b64url_encode(&cred_id),
        public_key_b64url: b64url_encode(&cose),
        counter: parsed.counter,
        user_verified: parsed.flags & 0x04 != 0,
        device_type,
        backed_up: parsed.flags & 0x10 != 0,
        transports: response
            .get("transports")
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(|s| s.to_string())
                    .collect()
            }),
    })
}

/// WebAuthnService.verifyAuthentication (L244-323): challenge getdel →
/// findOneBy({ id, userId }) (unknown → Err) → clientDataJSON/origin/rpId
/// checks → ECDSA-256 verify over (authenticatorData || sha256(clientDataJSON))
/// → lastUsed/counter touch. Returns Ok(false) ONLY for a signature the
/// authenticator failed to prove (upstream `verified: false` → the
/// signin-flow fail(403, 93b86c4b) lane).
pub async fn verify_authentication(
    pool: &PgPool,
    user_id: &str,
    rp_id: &str,
    rp_origin: &str,
    credential: &serde_json::Value,
) -> Result<bool, VerifyFail> {
    let challenge = take_authentication_challenge(user_id).ok_or(VerifyFail::Error)?;
    let (id, raw_id) = credential_id_strings(credential)?;
    if id != raw_id {
        return Err(VerifyFail::Error);
    }
    if credential.get("type").and_then(serde_json::Value::as_str) != Some("public-key") {
        return Err(VerifyFail::Error);
    }
    // findOneBy({ id, userId }) — a key owned by someone else is unknown
    let Some(key) = find_by_id(pool, &id).await.map_err(|_| VerifyFail::Error)? else {
        return Err(VerifyFail::Error);
    };
    if key.user_id != user_id {
        return Err(VerifyFail::Error);
    }
    let response = credential
        .get("response")
        .and_then(|r| r.as_object())
        .ok_or(VerifyFail::Error)?;
    let cdj = response
        .get("clientDataJSON")
        .and_then(serde_json::Value::as_str)
        .ok_or(VerifyFail::Error)?;
    let cd = parse_client_data_json(cdj)?;
    if cd.typ != "webauthn.get" {
        return Err(VerifyFail::Error);
    }
    if cd.challenge != challenge {
        return Err(VerifyFail::Error);
    }
    if cd.origin != rp_origin {
        return Err(VerifyFail::Error);
    }
    let authenticator_data_b64 = response
        .get("authenticatorData")
        .and_then(serde_json::Value::as_str)
        .ok_or(VerifyFail::Error)?;
    let signature_b64 = response
        .get("signature")
        .and_then(serde_json::Value::as_str)
        .ok_or(VerifyFail::Error)?;
    let authenticator_data = b64url_decode(authenticator_data_b64).ok_or(VerifyFail::Error)?;
    let signature = b64url_decode(signature_b64).ok_or(VerifyFail::Error)?;
    let parsed = parse_auth_data(&authenticator_data, rp_id).ok_or(VerifyFail::Error)?;
    // WebAuthn spec rule: flags.up required; requireUserVerification → uv
    if parsed.flags & 0x01 == 0 {
        return Err(VerifyFail::Error);
    }
    if parsed.flags & 0x04 == 0 {
        return Err(VerifyFail::Error);
    }
    let cose = b64url_decode(&key.public_key).ok_or(VerifyFail::Error)?;
    let (cose_decoded, _) = cbor_scan(&cose, 0).ok_or(VerifyFail::Error)?;
    // crv must be P-256 (verifyEC2 throws on any other curve)
    let crv = cbor_map_int(&cose_decoded, -1)
        .and_then(|v| match v {
            Cbor::UInt(u) => Some(*u as i64),
            _ => None,
        })
        .ok_or(VerifyFail::Error)?;
    if crv != 1 {
        return Err(VerifyFail::Error);
    }
    let get_hex32 = |key: i64| -> Result<[u8; 32], VerifyFail> {
        let v = cbor_map_int(&cose_decoded, key).ok_or(VerifyFail::Error)?;
        match v {
            Cbor::Bytes(b) if b.len() == 32 => {
                let mut out = [0u8; 32];
                out.copy_from_slice(b);
                Ok(out)
            }
            _ => Err(VerifyFail::Error),
        }
    };
    let pub_x = get_hex32(-2)?;
    let pub_y = get_hex32(-3)?;
    // signatureBase = authenticatorData || sha256(clientDataJSON) (L272-275)
    let mut sig_base = Vec::with_capacity(authenticator_data.len() + 32);
    sig_base.extend_from_slice(&authenticator_data);
    sig_base.extend_from_slice(&sha256(cdj.as_bytes()));
    let verified = ecdsa_p256_sha256_verify(&sig_base, &pub_x, &pub_y, &signature);
    if verified {
        touch_key(
            pool,
            &key.id,
            parsed.counter,
            if parsed.flags & 0x08 != 0 {
                Some("multiDevice")
            } else {
                Some("singleDevice")
            },
            Some(parsed.flags & 0x10 != 0),
        )
        .await
        .map_err(|_| VerifyFail::Error)?;
    }
    Ok(verified)
}

// ==== the initiate fns (WebAuthnService.initiateRegistration/Authentication) ====

/// initiateRegistration options body (generateRegistrationOptions v14 shape):
/// rp/user/challenge/pubKeyCredParams/excludeCredentials/authenticatorSelection.
pub async fn registration_options(
    pool: &PgPool,
    rp_id: &str,
    rp_name: &str,
    user_id: &str,
    user_name: &str,
    user_display_name: Option<&str>,
) -> Result<serde_json::Value, sqlx::Error> {
    let challenge = new_challenge_b64url();
    put_registration_challenge(user_id, &challenge);
    let mut user = serde_json::json!({
        "id": b64url_encode(user_id.as_bytes()),
        "name": user_name,
    });
    if let Some(dn) = user_display_name {
        user["displayName"] = serde_json::Value::String(dn.to_string());
    }
    let existing = list_briefs(pool, user_id).await?;
    let exclude_credentials: Vec<serde_json::Value> = existing
        .iter()
        .map(|k| serde_json::json!({ "id": k.id, "type": "public-key" }))
        .collect();
    Ok(serde_json::json!({
        "rp": { "id": rp_id, "name": rp_name },
        "user": user,
        "challenge": challenge,
        "pubKeyCredParams": [
            { "type": "public-key", "alg": -7 },
            { "type": "public-key", "alg": -8 },
            { "type": "public-key", "alg": -257 },
        ],
        "timeout": 60_000,
        "excludeCredentials": exclude_credentials,
        "authenticatorSelection": {
            "residentKey": "required",
            "userVerification": "preferred",
        },
        "attestation": "none",
    }))
}

/// initiateAuthentication options (generateAuthenticationOptions v14 default
/// timeout 60000, userVerification preferred) — challenge stored for the
/// assertion verify.
pub async fn authentication_options(
    pool: &PgPool,
    rp_id: &str,
    user_id: &str,
) -> Result<serde_json::Value, sqlx::Error> {
    let briefs = list_briefs(pool, user_id).await?;
    if briefs.is_empty() {
        // IdentifiableError 'no keys found' → the caller 500s
        return Err(sqlx::Error::RowNotFound);
    }
    let challenge = new_challenge_b64url();
    put_authentication_challenge(user_id, &challenge);
    let allow_credentials: Vec<serde_json::Value> = briefs
        .iter()
        .map(|k| serde_json::json!({ "id": k.id, "type": "public-key" }))
        .collect();
    Ok(serde_json::json!({
        "challenge": challenge,
        "rpId": rp_id,
        "timeout": 60_000,
        "userVerification": "preferred",
        "allowCredentials": allow_credentials,
    }))
}
