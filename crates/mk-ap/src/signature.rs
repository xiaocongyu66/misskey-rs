//! HTTP-Signature verification skeleton (draft-cavage-11, rsa-sha256).
//!
//! Upstream anchors:
//! - signing-string construction:
//!   `packages/backend/src/core/activitypub/ApRequestService.ts`
//!   `ApRequestCreator.#genSigningString` — `createSignedPost` signs
//!   `['(request-target)', 'date', 'host', 'digest']`; each include header is
//!   lowercased, `(request-target)` expands to `<method> <path>?<query>` with
//!   the fragment stripped and an empty query delimiter preserved, and the
//!   lines are joined with `\n`.
//! - the verify step:
//!   `packages/backend/src/queue/processors/InboxProcessorService.ts`
//!   (`httpSignature.verifySignature(signature, authUser.key.keyPem)` —
//!   RSA-SHA256 over the signing string; the signer must also equal
//!   `activity.actor`, a check that stays with the caller).
//!
//! Skeleton boundary: remote-key lookup by keyId (upstream
//! `ApDbResolverService.getAuthUserFromKeyId`) is a DB surface and stays with
//! the caller — `verify_http_signature` takes the PEM. The five-argument form
//! named by the fed-min slice covers parse + `(request-target)`/host build;
//! the extended form carries the `date`/`digest` header values and the key
//! PEM the RSA verify mathematically requires.

use base64::Engine;
use rsa::pkcs1v15::{Signature, VerifyingKey};
use rsa::sha2::{Digest, Sha256};
use rsa::signature::Verifier;
use rsa::{RsaPublicKey, pkcs1::DecodeRsaPublicKey, pkcs8::DecodePublicKey};

/// The pieces a caller needs to finish the trust chain upstream performs in
/// InboxProcessorService: fetch the key by `key_id`, then check that the
/// signer equals `activity.actor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedKey {
    pub key_id: String,
    /// Normalized lower-case (`rsa-sha256`); absent algorithm implies the
    /// draft-cavage default `rsa-sha256`.
    pub algorithm: String,
    /// The signing string the RSA-SHA256 digest was taken over.
    pub signing_string: String,
    /// The base64 signature exactly as sent (for audit/logging).
    pub signature: String,
}

/// Draft-cavage `Signature:` header parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedSignature {
    key_id: String,
    algorithm: Option<String>,
    /// Signed header names, lowercased, in signed order.
    headers: Vec<String>,
    /// Base64 signature bytes as sent.
    signature: String,
}

/// Parse `keyId="…",algorithm="…",headers="…",signature="…"`.
///
/// `headers` defaults to `["date"]` per draft-cavage when the parameter is
/// absent; unquoted scalar values (the draft allows them) are accepted.
pub fn parse_signature_header(header: &str) -> Option<ParsedSignature> {
    let mut key_id = None;
    let mut algorithm = None;
    let mut headers = None;
    let mut signature = None;

    for part in header.split(',') {
        let part = part.trim();
        // split_once (first '='): the base64 signature value may carry '='
        // padding, so the value is everything after the first separator.
        let (name, value) = part.split_once('=')?;
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value);
        match name.trim().to_ascii_lowercase().as_str() {
            "keyid" => key_id = Some(value.to_string()),
            "algorithm" => algorithm = Some(value.to_ascii_lowercase()),
            "headers" => {
                headers = Some(
                    value
                        .split_whitespace()
                        .map(|h| h.to_ascii_lowercase())
                        .collect::<Vec<String>>(),
                )
            }
            "signature" => signature = Some(value.to_string()),
            // Unknown params are ignored (the draft permits extensions).
            _ => {}
        }
    }

    Some(ParsedSignature {
        key_id: key_id?,
        algorithm,
        headers: headers.unwrap_or_else(|| vec!["date".to_string()]),
        signature: signature?,
    })
}

/// `ApRequestCreator.#genSigningString`: join the signed-header lines with
/// `\n`. `(request-target)` expands to `<method lowercased> <path>` where
/// `path` is the request target with the fragment stripped — query strings
/// are kept verbatim, including an empty `?` delimiter (upstream
/// `test/unit/ap-request.ts` 'request-target omits hash and preserves empty
/// query delimiter'). Only the upstream `createSignedPost` include set
/// (`(request-target)`, `date`, `host`, `digest`) is supported; any other
/// signed header is a hard failure (the caller must extend the value map,
/// not silently skip a line that was covered by the signature).
pub fn gen_signing_string(
    method: &str,
    path: &str,
    host: &str,
    date: Option<&str>,
    digest: Option<&str>,
    include_headers: &[String],
) -> Result<String, ()> {
    let target = match path.split_once('#') {
        Some((before, _)) => before,
        None => path,
    };
    let mut lines: Vec<String> = Vec::with_capacity(include_headers.len());
    for header in include_headers {
        match header.as_str() {
            "(request-target)" => {
                lines.push(format!(
                    "(request-target): {} {}",
                    method.to_ascii_lowercase(),
                    target
                ));
            }
            "host" => lines.push(format!("host: {host}")),
            "date" => lines.push(format!("date: {}", date.ok_or(())?)),
            "digest" => lines.push(format!("digest: {}", digest.ok_or(())?)),
            _ => return Err(()),
        }
    }
    Ok(lines.join("\n"))
}

/// SHA-256 digest header for a body: `createDigest` =
/// `SHA-256=${base64(sha256(body))}`.
pub fn create_digest(body: &[u8]) -> String {
    format!(
        "SHA-256={}",
        base64::engine::general_purpose::STANDARD.encode(Sha256::digest(body))
    )
}

/// Parse + build + verify, in one call.
///
/// - `signature_header`: the raw `Signature:` header value.
/// - `method`/`path`: request line pieces (`path` carries query and possibly
///   fragment; the fragment is stripped).
/// - `host`/`date`/`digest`: the corresponding request header values as sent.
/// - `public_key_pem`: the actor's public key (PKCS#8 `BEGIN PUBLIC KEY` or
///   PKCS#1 `BEGIN RSA PUBLIC KEY`), the upstream `authUser.key.keyPem`.
///
/// Verify = algorithm must be rsa-sha256, every signed header must be in the
/// supported set, a signed `digest` must equal `SHA-256=base64(sha256(body))`
/// of the body received, and the RSA-SHA256 (PKCS#1 v1.5) signature must
/// verify over the signing string. Any failure collapses to `Err(())` (the
/// upstream worker logs the reason and drops the job; the HTTP surface
/// already answered 202).
pub fn verify_http_signature(
    signature_header: &str,
    method: &str,
    path: &str,
    host: &str,
    body: &[u8],
    date: &str,
    digest: &str,
    public_key_pem: &str,
) -> Result<VerifiedKey, ()> {
    let parsed = parse_signature_header(signature_header).ok_or(())?;
    match parsed.algorithm.as_deref() {
        None | Some("rsa-sha256") => {}
        _ => return Err(()),
    }

    let date_value = if parsed.headers.iter().any(|h| h == "date") {
        Some(date).filter(|d| !d.is_empty())
    } else {
        None
    };
    let digest_value = if parsed.headers.iter().any(|h| h == "digest") {
        Some(digest).filter(|d| !d.is_empty())
    } else {
        None
    };
    let signing_string = gen_signing_string(
        method,
        path,
        host,
        date_value,
        digest_value,
        &parsed.headers,
    )?;

    // draft-cavage digest check: when `digest` is covered by the signature
    // the body must hash to it (both halves of the sender's claim — the
    // signed digest line and the received bytes — must agree).
    if let Some(sent) = digest_value {
        if !sent.eq_ignore_ascii_case(&create_digest(body)) {
            return Err(());
        }
    }

    rsa_sha256_verify(&signing_string, &parsed.signature, public_key_pem)?;

    Ok(VerifiedKey {
        key_id: parsed.key_id,
        algorithm: parsed.algorithm.unwrap_or_else(|| "rsa-sha256".to_string()),
        signing_string,
        signature: parsed.signature,
    })
}

/// RSA-SHA256 (RSASSA-PKCS1-v1_5 with SHA-256) over the signing string — the
/// `httpSignature.verifySignature(signature, keyPem)` core. The key PEM may
/// be PKCS#8 (what `UserKeypairService` / signup store) or PKCS#1 (what some
/// remote actors serve in their `publicKey.publicKeyPem`).
pub fn rsa_sha256_verify(
    signing_string: &str,
    signature_b64: &str,
    public_key_pem: &str,
) -> Result<(), ()> {
    let key = RsaPublicKey::from_public_key_pem(public_key_pem)
        .or_else(|_| RsaPublicKey::from_pkcs1_pem(public_key_pem))
        .map_err(|_| ())?;
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(signature_b64)
        .map_err(|_| ())?;
    let signature = Signature::try_from(sig_bytes.as_slice()).map_err(|_| ())?;
    VerifyingKey::<Sha256>::new(key)
        .verify(signing_string.as_bytes(), &signature)
        .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    // fed-min: the upstream `test/unit/ap-request.ts` 'createSignedPost with
    // verify' pin, mirrored — build a signing string, sign it with the
    // generated key, and round-trip through verify_http_signature.
    use super::*;
    use rsa::RsaPrivateKey;
    use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
    use rsa::signature::{SignatureEncoding, Signer};

    #[test]
    fn create_signed_post_round_trips_through_verify() {
        let mut rng = rand::thread_rng();
        let key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let public_pem = key
            .to_public_key()
            .to_public_key_pem(LineEnding::LF)
            .unwrap();

        let body = br#"{"a":1}"#;
        let include: Vec<String> = ["(request-target)", "date", "host", "digest"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // ApRequestCreator.createSignedPost: signing string over
        // (request-target)/date/host/digest, then sign rsa-sha256.
        let signing_string = gen_signing_string(
            "POST",
            "/inbox",
            "example.com",
            Some("Tue, 20 Oct 2026 00:00:00 GMT"),
            Some(&create_digest(body)),
            &include,
        )
        .unwrap();
        let signing_key = rsa::pkcs1v15::SigningKey::<Sha256>::new(key);
        let signature_b64 = base64::engine::general_purpose::STANDARD
            .encode(signing_key.sign(signing_string.as_bytes()).to_bytes());
        let signature_header = format!(
            "keyId=\"https://example.com/users/x#main-key\",algorithm=\"rsa-sha256\",\
             headers=\"(request-target) date host digest\",signature=\"{signature_b64}\""
        );

        // happy path: the signed digest matches the received body and the
        // RSA verify passes → the caller gets the keyId back
        let verified = verify_http_signature(
            &signature_header,
            "POST",
            "/inbox",
            "example.com",
            body,
            "Tue, 20 Oct 2026 00:00:00 GMT",
            &create_digest(body),
            &public_pem,
        )
        .unwrap();
        assert_eq!(verified.key_id, "https://example.com/users/x#main-key");
        assert_eq!(verified.algorithm, "rsa-sha256");
        assert_eq!(verified.signing_string, signing_string);

        // tampered body: the digest-vs-body half fails
        assert!(
            verify_http_signature(
                &signature_header,
                "POST",
                "/inbox",
                "example.com",
                br#"{"a":2}"#,
                "Tue, 20 Oct 2026 00:00:00 GMT",
                &create_digest(body),
                &public_pem,
            )
            .is_err()
        );

        // tampered signature (digest + body still agree): the RSA half fails
        let corrupted = signature_header.replace(&signature_b64[..8], "AAAAAAAA");
        assert_ne!(corrupted, signature_header);
        assert!(
            verify_http_signature(
                &corrupted,
                "POST",
                "/inbox",
                "example.com",
                body,
                "Tue, 20 Oct 2026 00:00:00 GMT",
                &create_digest(body),
                &public_pem,
            )
            .is_err()
        );

        // unsupported algorithm
        assert!(
            verify_http_signature(
                "keyId=\"x\",algorithm=\"hs2019\",headers=\"(request-target)\",\
             signature=\"AAA\"",
                "POST",
                "/inbox",
                "example.com",
                body,
                "",
                "",
                &public_pem,
            )
            .is_err()
        );
    }

    #[test]
    fn signing_string_shape_pins_upstream_request_target_rules() {
        let include: Vec<String> = ["(request-target)"].iter().map(|s| s.to_string()).collect();
        // upstream test 'createSignedPost includes query string in
        // request-target': /inbox?token=abc
        let with_query =
            gen_signing_string("POST", "/inbox?token=abc#ignored", "", None, None, &include)
                .unwrap();
        assert_eq!(with_query, "(request-target): post /inbox?token=abc");
        // empty query delimiter preserved: /outbox?
        let empty_query =
            gen_signing_string("GET", "/outbox?#ignored", "", None, None, &include).unwrap();
        assert_eq!(empty_query, "(request-target): get /outbox?");
        // no query: no delimiter
        let no_query = gen_signing_string("GET", "/outbox", "", None, None, &include).unwrap();
        assert_eq!(no_query, "(request-target): get /outbox");
        // header lines join with \n, lowercase method, host verbatim
        let all: Vec<String> = ["(request-target)", "host", "date", "digest"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let full = gen_signing_string(
            "Post",
            "/inbox",
            "example.com",
            Some("d"),
            Some("SHA-256=x"),
            &all,
        )
        .unwrap();
        assert_eq!(
            full,
            "(request-target): post /inbox\nhost: example.com\ndate: d\ndigest: SHA-256=x"
        );
        // an unsupported signed header is a hard failure, never a skipped line
        let with_ua: Vec<String> = ["host", "user-agent"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(gen_signing_string("POST", "/inbox", "h", None, None, &with_ua).is_err());
    }
}
