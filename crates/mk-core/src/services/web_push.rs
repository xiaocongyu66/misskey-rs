//! VAPID (Web Push authentication) assembly — the `WebPushService` lane's
//! JWT half (upstream `core/WebPushService.ts` + the `push` notification
//! transport's `Authorization: vapid t=…,k=…` header).
//!
//! ES256 over the `{header}.{payload}` bytes, P-256/SHA-256 via the
//! hand-rolled primitives in [`mk_db::webauthn`]; base64url rides the same
//! unpadded alphabet those challenges use.

use mk_db::webauthn::{b64url_encode, ecdsa_p256_sha256_sign, p256_pub_from_priv};

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
}
