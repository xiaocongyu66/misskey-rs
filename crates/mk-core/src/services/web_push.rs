//! VAPID (Web Push authentication) assembly — the `WebPushService` lane's
//! JWT half (upstream `core/WebPushService.ts` + the `push` notification
//! transport's `Authorization: vapid t=…,k=…` header).
//!
//! ES256 over the `{header}.{payload}` bytes, P-256/SHA-256 via the
//! hand-rolled primitives in [`mk_db::webauthn`]; base64url rides the same
//! unpadded alphabet those challenges use.

use mk_db::webauthn::{
    b64url_encode, ecdsa_p256_sha256_sign, p256_ecdh, p256_pub_from_priv, sha256,
};

/// `{typ: "JWT", alg: "ES256"}.{aud, exp (+12h), sub}` signed ES256 — the
/// exact token the `t=` field carries. `key` is the 32-byte secret scalar,
/// `now_ms` the clock (injected so tests pin `exp` deterministically).
pub fn vapid_jwt(
    audience_origin: &str,
    sub_mailto: &str,
    key: &[u8; 32],
    now_ms: i64,
) -> Option<String> {
    let header = b64url_encode(br#"{"typ":"JWT","alg":"ES256"}"#);
    let payload = format!(
        r#"{{"aud":"{}","exp":{},"sub":"{}"}}"#,
        audience_origin,
        now_ms / 1000 + 12 * 3600,
        sub_mailto
    );
    let payload = b64url_encode(payload.as_bytes());
    let signing_input = format!("{header}.{payload}");
    let k = fresh_nonce();
    let der = ecdsa_p256_sha256_sign(signing_input.as_bytes(), key, &k)?;
    // raw JWS: r || s, each padded to 32 bytes (strip the DER wrapper)
    let (r, s) = der_to_raw(&der)?;
    let sig = b64url_encode(&[r.as_slice(), s.as_slice()].concat());
    Some(format!("{signing_input}.{sig}"))
}

/// The `k=` field: the uncompressed public key point (`0x04 || x || y`),
/// base64url — the VAPID application server key.
pub fn vapid_public_key(key: &[u8; 32]) -> Option<String> {
    let (x, y) = p256_pub_from_priv(key)?;
    let mut point = Vec::with_capacity(65);
    point.push(0x04);
    point.extend_from_slice(&x);
    point.extend_from_slice(&y);
    Some(b64url_encode(&point))
}

/// Split a DER `SEQUENCE { INTEGER r, INTEGER s }` back into two 32-byte
/// big-endian halves (the reverse of mk-db's minimal encoder).
fn der_to_raw(der: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    if der.first() != Some(&0x30) {
        return None;
    }
    let mut i = 2usize; // skip SEQUENCE + length (short form is all we emit)
    let int = |der: &[u8], i: &mut usize| -> Option<Vec<u8>> {
        if der.get(*i) != Some(&0x02) {
            return None;
        }
        let len = *der.get(*i + 1)? as usize;
        let mut v = der.get(*i + 2..*i + 2 + len)?.to_vec();
        while v.len() > 32 {
            if v.first() != Some(&0) {
                return None;
            }
            v.remove(0);
        }
        while v.len() < 32 {
            v.insert(0, 0);
        }
        *i += 2 + len;
        Some(v)
    };
    let r = int(der, &mut i)?;
    let s = int(der, &mut i)?;
    Some((r, s))
}

/// Fresh per-signature nonce — OS randomness via the workspace `rand`.
fn fresh_nonce() -> [u8; 32] {
    use rand::RngCore;
    let mut k = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut k);
    // clamp into (0, n): rejection by masking the top bit keeps it < 2^255 <
    // n, and the sign fn rejects zero (astronomically improbable, still
    // caller-visible as None)
    k[0] &= 0x7f;
    k
}

// ==== RFC 5869 HKDF-SHA256 (the RFC 8291 key-schedule half) ====

/// HMAC-SHA256 (RFC 2104) over the vendored SHA-256: ipad/opad blocks.
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&sha256(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = ipad.to_vec();
    inner.extend_from_slice(msg);
    let ih = sha256(&inner);
    let mut outer = opad.to_vec();
    outer.extend_from_slice(&ih);
    sha256(&outer)
}

/// HKDF-SHA256 (RFC 5869): extract (HMAC-SHA256 keyed by the salt; all-zero
/// 32-byte salt when `None`) then expand (`T(i) = HMAC(PRK, T(i-1) || info ||
/// i)`), first `len` bytes. `len` ≤ 255·32 (the RFC's output cap).
///
/// This is the RFC 8291 §4.2/§4.3 schedule's PRK→cek/nonce machinery — the
/// push payload encryption half lands against it next.
pub fn hkdf_sha256(ikm: &[u8], salt: Option<&[u8]>, info: &[u8], len: usize) -> Vec<u8> {
    const HASH_LEN: usize = 32;
    assert!(len <= 255 * HASH_LEN, "hkdf output over the RFC 5869 cap");
    let prk = hmac_sha256(salt.unwrap_or(&[0u8; HASH_LEN]), ikm);
    let mut okm = Vec::with_capacity(len);
    let mut t: Vec<u8> = Vec::new();
    let mut counter = 1u8;
    while okm.len() < len {
        let mut input = t.clone();
        input.extend_from_slice(info);
        input.push(counter);
        t = hmac_sha256(&prk, &input).to_vec();
        okm.extend_from_slice(&t);
        counter += 1;
    }
    okm.truncate(len);
    okm
}

// ==== RFC 8291 `aes128gcm` payload seal (the wire half) ====

/// RFC 8291 `aes128gcm` content-coding: seal `plaintext` to the client key
/// (`p256dh_b64` = base64url `0x04 || x || y`, `auth_b64` = the auth secret)
/// under the server EC scalar `keypair`. `None` on any bad input.
///
/// Derivation structure (RFC 8291 §4.2/§4.3 — every primitive below is
/// already present in-tree):
/// 1. `client = b64url_decode(p256dh_b64)` → `0x04 || x[..32] || y[..32]`
/// 2. `auth = b64url_decode(auth_b64)`
/// 3. fresh ephemeral scalar (OsRng, masked) + `p256_pub_from_priv`
/// 4. `shared = p256_ecdh(eph_priv, client_x, client_y)`
/// 5. `ikm = hkdf_sha256(&shared, Some(&auth), b"Content-Encoding: auth\0", 32)`
/// 6. random 16-byte `salt`
/// 7. `cek = hkdf_sha256(&ikm, Some(&salt),
///    b"Content-Encoding: aes128gcm\0" || u32be(16), 16)`
/// 8. `nonce = hkdf_sha256(&ikm, Some(&salt),
///    b"Content-Encoding: nonce\0" || u32be(12), 12)`
/// 9. AES-128-GCM seal (no AAD) + framing header
///    `salt(16) || eph_pub(65) || rs=u32be(4096) || idlen=0`.
///
/// **Dead-end:** step 9 needs AES-128-GCM, and the `aes-gcm` crate is NOT in
/// the workspace lock (`Cargo.lock` carries no `aes`/`ghash` — hand-rolling
/// the block cipher + GHASH is out of scope), so no ciphertext can be
/// produced yet. This seam returns `None` unconditionally until the crate
/// lands; [`crate::services::push_deliver`] keeps the empty body + fallback
/// log meanwhile. Steps 1-8 above are exactly what will run once it does.
pub fn encrypt_payload(
    plaintext: &[u8],
    keypair: &[u8; 32],
    p256dh_b64: &str,
    auth_b64: &str,
) -> Option<Vec<u8>> {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes128Gcm, Nonce};

    // p256dh = base64url(0x04 || x || y) of the subscription's client key
    let p256dh = mk_db::webauthn::b64url_decode(p256dh_b64).ok()?;
    if p256dh.len() != 65 || p256dh[0] != 0x04 {
        return None;
    }
    let mut client_x = [0u8; 32];
    client_x.copy_from_slice(&p256dh[1..33]);
    let mut client_y = [0u8; 32];
    client_y.copy_from_slice(&p256dh[33..65]);
    let auth = mk_db::webauthn::b64url_decode(auth_b64).ok()?;

    let (eph_x, eph_y) = mk_db::webauthn::p256_pub_from_priv(keypair)?;
    let shared = mk_db::webauthn::p256_ecdh(keypair, &client_x, &client_y)?;

    // RFC 8291 §3.3: ikm = HKDF(shared, auth_secret, "Content-Encoding: auth\0" || 0x00, 32)
    let mut ikm_info = b"Content-Encoding: auth\0".to_vec();
    ikm_info.push(0);
    let ikm = hkdf_sha256(&shared, Some(&auth), &ikm_info, 32);

    let mut salt = [0u8; 16];
    {
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut salt);
    }

    // cek = HKDF(ikm, salt, "Content-Encoding: aes128gcm\0" || 0x01 || u16be(16), 16)
    let mut cek_info = b"Content-Encoding: aes128gcm\0".to_vec();
    cek_info.push(1);
    cek_info.extend_from_slice(&16u16.to_be_bytes());
    let cek = hkdf_sha256(&ikm, Some(&salt), &cek_info, 16);

    // nonce = HKDF(ikm, salt, "Content-Encoding: nonce\0" || 0x01 || u16be(12), 12)
    let mut nonce_info = b"Content-Encoding: nonce\0".to_vec();
    nonce_info.push(1);
    nonce_info.extend_from_slice(&12u16.to_be_bytes());
    let nonce = hkdf_sha256(&ikm, Some(&salt), &nonce_info, 12);

    let cipher = Aes128Gcm::new_from_slice(&cek).ok()?;
    let ct = cipher.encrypt(Nonce::from_slice(&nonce), plaintext).ok()?;

    // aes128gcm framing header: salt(16) || eph pub(65) || rs u32be || idlen(0)
    let mut out = Vec::with_capacity(16 + 65 + 4 + 1 + ct.len());
    out.extend_from_slice(&salt);
    out.push(0x04);
    out.extend_from_slice(&eph_x);
    out.extend_from_slice(&eph_y);
    out.extend_from_slice(&4096u32.to_be_bytes());
    out.push(0);
    out.extend_from_slice(&ct);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mk_db::webauthn::ecdsa_p256_sha256_verify;

    #[test]
    fn vapid_token_round_trips_through_p256_verify() {
        // fixed key; exp pins deterministically at now + 12h
        let key = hex32("c9afa9d845ba75166b5c215767b1d6934e50c3db36e89b127b8a622b120f6721");
        let now = 1_700_000_000_000i64;
        let token = vapid_jwt("https://example.org", "mailto:ops@example.org", &key, now)
            .expect("vapid_jwt");
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        // payload carries aud/exp/sub with exp = now/1000 + 12h
        let payload =
            String::from_utf8(mk_db::webauthn::b64url_decode(parts[1]).expect("payload b64url"))
                .unwrap();
        assert_eq!(
            payload,
            r#"{"aud":"https://example.org","exp":1700043200,"sub":"mailto:ops@example.org"}"#
        );
        // the signature verifies against the derived public key
        let (x, y) = p256_pub_from_priv(&key).expect("pubkey");
        let sig = mk_db::webauthn::b64url_decode(parts[2]).expect("sig b64url");
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        // re-encode the raw r||s into DER for the verify fn
        let der = raw_to_der(&sig);
        assert!(ecdsa_p256_sha256_verify(
            signing_input.as_bytes(),
            &x,
            &y,
            &der
        ));
    }

    #[test]
    fn vapid_public_key_is_the_uncompressed_point() {
        let key = hex32("c9afa9d845ba75166b5c215767b1d6934e50c3db36e89b127b8a622b120f6721");
        let k = vapid_public_key(&key).expect("k");
        let point = mk_db::webauthn::b64url_decode(&k).expect("point");
        assert_eq!(point[0], 0x04);
        assert_eq!(point.len(), 65);
        // RFC 6979 A.2.5's public key for this scalar (SHA-256, "sample")
        assert_eq!(
            &point[1..33],
            &hex32("60FED4BA255A9D31C961EB74C6356D68C049B8923B61FA6CE669622E60F29FB6")
        );
    }

    #[test]
    fn hkdf_sha256_matches_rfc5869_a1() {
        let okm = hkdf_sha256(
            &[0x0b; 22],
            Some(&[
                0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
            ]),
            &[0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9],
            42,
        );
        assert_eq!(
            okm,
            hex(concat!(
                "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf",
                "34007208d5b887185865"
            ))
        );
    }

    #[test]
    fn p256_ecdh_is_symmetric_between_two_scalars() {
        // two distinct in-range scalars (RFC 6979 A.2.5's + one arbitrary)
        let a = hex32("c9afa9d845ba75166b5c215767b1d6934e50c3db36e89b127b8a622b120f6721");
        let b = hex32("2f2e1d3c9e8f7a6b5c4d3e2f1a0b9c8d7e6f5a4b3c2d1e0f9a8b7c6d5e4f3a2b");
        let (ax, ay) = p256_pub_from_priv(&a).expect("A");
        let (bx, by) = p256_pub_from_priv(&b).expect("B");
        let ab = p256_ecdh(&a, &bx, &by).expect("a·B");
        let ba = p256_ecdh(&b, &ax, &ay).expect("b·A");
        assert_eq!(ab, ba);
        // out-of-range scalars (0 and n) are rejected
        assert_eq!(p256_ecdh(&[0u8; 32], &bx, &by), None);
        let n = hex32("ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551");
        assert_eq!(p256_ecdh(&n, &bx, &by), None);
    }

    #[test]
    fn encrypt_payload_rejects_garbage_keys() {
        // non-base64 or non-0x04 p256dh cannot produce a body
        assert_eq!(
            encrypt_payload(b"{}", &[7u8; 32], "not-base64!!", "also-not"),
            None
        );
        let not_uncompressed = mk_db::webauthn::b64url_encode(&[1u8; 65]);
        assert_eq!(
            encrypt_payload(b"{}", &[7u8; 32], &not_uncompressed, "YXV0aA"),
            None
        );
    }

    #[test]
    fn encrypt_payload_round_trips_with_rfc8291_derivation() {
        use aes_gcm::aead::{Aead, KeyInit};
        use aes_gcm::{Aes128Gcm, Nonce};

        // client keypair (the subscription's p256dh) + sender ephemeral
        let client_priv = [0x11u8; 32];
        let (cx, cy) = mk_db::webauthn::p256_pub_from_priv(&client_priv).expect("client");
        let mut p256dh = vec![0x04u8];
        p256dh.extend_from_slice(&cx);
        p256dh.extend_from_slice(&cy);
        let auth = [0x22u8; 16];
        let sender_priv = [0x33u8; 32];

        let out = encrypt_payload(
            b"hello",
            &sender_priv,
            &mk_db::webauthn::b64url_encode(&p256dh),
            &mk_db::webauthn::b64url_encode(&auth),
        )
        .expect("encrypt");

        // header: salt(16) | 0x04 x(32) y(32) | rs(4) | idlen(0) = 86 bytes
        assert_eq!(out[16], 0x04);
        assert_eq!(&out[81..85], &4096u32.to_be_bytes());
        assert_eq!(out[85], 0);
        let salt = &out[..16];
        let mut eph_x = [0u8; 32];
        eph_x.copy_from_slice(&out[17..49]);
        let mut eph_y = [0u8; 32];
        eph_y.copy_from_slice(&out[49..81]);

        // receiver side: shared = client_priv × eph_pub
        let shared = mk_db::webauthn::p256_ecdh(&client_priv, &eph_x, &eph_y).expect("shared");
        let mut ikm_info = b"Content-Encoding: auth\0".to_vec();
        ikm_info.push(0);
        let ikm = hkdf_sha256(&shared, Some(&auth), &ikm_info, 32);
        let mut cek_info = b"Content-Encoding: aes128gcm\0".to_vec();
        cek_info.push(1);
        cek_info.extend_from_slice(&16u16.to_be_bytes());
        let cek = hkdf_sha256(&ikm, Some(salt), &cek_info, 16);
        let mut nonce_info = b"Content-Encoding: nonce\0".to_vec();
        nonce_info.push(1);
        nonce_info.extend_from_slice(&12u16.to_be_bytes());
        let nonce = hkdf_sha256(&ikm, Some(salt), &nonce_info, 12);

        let cipher = Aes128Gcm::new_from_slice(&cek).expect("cipher");
        let plain = cipher
            .decrypt(Nonce::from_slice(&nonce), &out[86..])
            .expect("decrypt");
        assert_eq!(plain, b"hello");
    }

    fn raw_to_der(sig: &[u8]) -> Vec<u8> {
        let mut der = vec![0x30, 0x44, 0x02, 0x20];
        der.extend_from_slice(&sig[..32]);
        der.extend_from_slice(&[0x02, 0x20]);
        der.extend_from_slice(&sig[32..]);
        der
    }

    fn hex32(s: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for i in 0..32 {
            out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
        }
        out
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }
}
