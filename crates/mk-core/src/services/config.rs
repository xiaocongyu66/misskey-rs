//! Instance-config reads shared by the delivery seams (upstream
//! `MetaService` / `config`): the VAPID private key and the SMTP envelope
//! sender both live in the `meta` sentinel row.
//!
//! `0001_init.sql` carries `meta."swPrivateKey"` / `meta."swPublicKey"` and
//! `meta.email`, but no `meta.url` (the server URL rides the YAML config), so
//! the sender falls back to the historical literal rather than a host.

use mk_db::sqlx::PgPool;

/// Historical `MAIL FROM` / VAPID-fallback identity — kept when `meta.email`
/// is unset (there is no `meta.url` to derive a host from).
pub const DEFAULT_EMAIL_FROM: &str = "no-reply@mk-rs";

/// `meta."swPrivateKey"` decoded to the 32-byte VAPID secret scalar.
///
/// `None` when the row/column is unset or the value is not base64url-encoded
/// 32 bytes; callers keep their random fallback and log.
pub async fn push_vapid_key(pool: &PgPool) -> Option<[u8; 32]> {
    let raw: Option<String> = mk_db::sqlx::query_scalar(
        r#"SELECT (SELECT "swPrivateKey" FROM "meta" ORDER BY "id" DESC LIMIT 1)"#,
    )
    .fetch_one(pool)
    .await
    .ok()
    .flatten();
    let raw = raw?;
    mk_db::webauthn::b64url_decode(raw.trim())?.try_into().ok()
}

/// Generate-and-persist the VAPID identity (upstream `MetaService.init`):
/// `meta."swPrivateKey"` / `meta."swPublicKey"` are filled on first use and
/// reused verbatim afterwards.
///
/// Returns the 32-byte secret scalar and the base64url uncompressed public
/// point (`0x04 || x || y`). Idempotent: the sentinel `meta` row (`id = 'x'`,
/// the one `MetaService.fetch` upserts) is locked, so concurrent first calls
/// converge on one pair. A set-but-undecodable private key is replaced.
pub async fn ensure_vapid_keys(pool: &PgPool) -> Result<([u8; 32], String), String> {
    use rand::RngCore;

    let mut tx = pool.begin().await.map_err(|e| format!("begin: {e}"))?;
    mk_db::sqlx::query(r#"INSERT INTO "meta" ("id") VALUES ('x') ON CONFLICT ("id") DO NOTHING"#)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("ensure meta row: {e}"))?;
    let (priv_raw, pub_raw): (Option<String>, Option<String>) = mk_db::sqlx::query_as(
        r#"SELECT "swPrivateKey", "swPublicKey" FROM "meta" WHERE "id" = 'x' FOR UPDATE"#,
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| format!("read meta: {e}"))?;

    // Existing scalar → reuse (and repair the public half if it is missing).
    if let Some(priv_bytes) = priv_raw
        .as_deref()
        .and_then(|raw| mk_db::webauthn::b64url_decode(raw.trim()))
        .and_then(|v| <[u8; 32]>::try_from(v).ok())
    {
        if let Some((x, y)) = mk_db::webauthn::p256_pub_from_priv(&priv_bytes) {
            let derived = b64url_point(&x, &y);
            let pub_str = match pub_raw.as_deref().map(str::trim) {
                Some(p) if !p.is_empty() => p.to_string(),
                _ => {
                    mk_db::sqlx::query(r#"UPDATE "meta" SET "swPublicKey" = $1 WHERE "id" = 'x'"#)
                        .bind(&derived)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| format!("repair swPublicKey: {e}"))?;
                    derived
                }
            };
            tx.commit().await.map_err(|e| format!("commit: {e}"))?;
            return Ok((priv_bytes, pub_str));
        }
    }

    // Fresh scalar (p256_pub_from_priv also rejects 0 and >= n).
    let (priv_bytes, pub_str) = loop {
        let mut key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut key);
        if let Some((x, y)) = mk_db::webauthn::p256_pub_from_priv(&key) {
            break (key, b64url_point(&x, &y));
        }
    };
    mk_db::sqlx::query(
        r#"UPDATE "meta" SET "swPrivateKey" = $1, "swPublicKey" = $2 WHERE "id" = 'x'"#,
    )
    .bind(mk_db::webauthn::b64url_encode(&priv_bytes))
    .bind(&pub_str)
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("persist vapid keys: {e}"))?;
    tx.commit().await.map_err(|e| format!("commit: {e}"))?;
    Ok((priv_bytes, pub_str))
}

/// base64url of the uncompressed P-256 point `0x04 || x || y` (the VAPID
/// application server key shape).
fn b64url_point(x: &[u8; 32], y: &[u8; 32]) -> String {
    let mut point = Vec::with_capacity(65);
    point.push(0x04);
    point.extend_from_slice(x);
    point.extend_from_slice(y);
    mk_db::webauthn::b64url_encode(&point)
}

/// `meta.email` (the configured noreply address), trimmed; `None` when the
/// row/column is absent or blank.
async fn meta_email(pool: &PgPool) -> Option<String> {
    let raw: Option<String> = mk_db::sqlx::query_scalar(
        r#"SELECT (SELECT "email" FROM "meta" ORDER BY "id" DESC LIMIT 1)"#,
    )
    .fetch_one(pool)
    .await
    .ok()
    .flatten();
    raw.map(|e| e.trim().to_string()).filter(|e| !e.is_empty())
}

/// SMTP envelope sender (`MAIL FROM`) — `meta.email` or the historical
/// literal when unset.
pub async fn email_from(pool: &PgPool) -> String {
    meta_email(pool)
        .await
        .unwrap_or_else(|| DEFAULT_EMAIL_FROM.to_string())
}

/// VAPID `sub` claim — `mailto:` + `meta.email` when configured; `None` lets
/// the caller keep its placeholder.
pub async fn push_mailto(pool: &PgPool) -> Option<String> {
    meta_email(pool).await.map(|e| format!("mailto:{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sw_private_key_round_trips_and_unset_is_none() {
        if std::env::var("DATABASE_URL").is_err() {
            return;
        }
        let pool = mk_db::test_support::fresh_db("mkcore_config_vapid").await;
        mk_db::run_migrations(&pool).await.expect("migrations");

        // Unset meta row → None (the random-fallback pin).
        assert_eq!(push_vapid_key(&pool).await, None);
        assert_eq!(email_from(&pool).await, DEFAULT_EMAIL_FROM);
        assert_eq!(push_mailto(&pool).await, None);

        // Seed the sentinel row (same shape as the update-meta lane).
        let bytes = [0x11u8; 32];
        let encoded = mk_db::webauthn::b64url_encode(&bytes);
        mk_db::sqlx::query(r#"INSERT INTO "meta" ("id", "swPrivateKey") VALUES ('x', $1)"#)
            .bind(&encoded)
            .execute(&pool)
            .await
            .expect("seed meta");
        assert_eq!(push_vapid_key(&pool).await, Some(bytes));

        // Configured email feeds both the SMTP sender and the VAPID sub.
        mk_db::sqlx::query(r#"UPDATE "meta" SET "email" = $1"#)
            .bind("noreply@example.test")
            .execute(&pool)
            .await
            .expect("seed email");
        assert_eq!(email_from(&pool).await, "noreply@example.test");
        assert_eq!(
            push_mailto(&pool).await.as_deref(),
            Some("mailto:noreply@example.test")
        );
    }

    #[tokio::test]
    async fn ensure_vapid_keys_generates_once_and_persists() {
        if std::env::var("DATABASE_URL").is_err() {
            return;
        }
        let pool = mk_db::test_support::fresh_db("mkcore_config_vapid_gen").await;
        mk_db::run_migrations(&pool).await.expect("migrations");

        // Fresh meta (no sentinel row yet) → generate.
        let (priv1, pub1) = ensure_vapid_keys(&pool).await.expect("generate");
        // Second call reuses the persisted pair (idempotent).
        let (priv2, pub2) = ensure_vapid_keys(&pool).await.expect("reuse");
        assert_eq!(priv1, priv2, "private key stable across calls");
        assert_eq!(pub1, pub2, "public key stable across calls");

        // The meta row holds the base64url forms.
        let (stored_priv, stored_pub): (String, String) = mk_db::sqlx::query_as(
            r#"SELECT "swPrivateKey", "swPublicKey" FROM "meta" WHERE "id" = 'x'"#,
        )
        .fetch_one(&pool)
        .await
        .expect("meta row");
        assert_eq!(stored_priv, mk_db::webauthn::b64url_encode(&priv1));
        assert_eq!(
            mk_db::webauthn::b64url_decode(&stored_priv),
            Some(priv1.to_vec()),
            "stored private key is base64url of the 32 scalar bytes"
        );

        // Public key = base64url(0x04 || x || y) derived from the private scalar.
        let (x, y) = mk_db::webauthn::p256_pub_from_priv(&priv1).expect("pub point");
        let mut point = vec![0x04u8];
        point.extend_from_slice(&x);
        point.extend_from_slice(&y);
        assert_eq!(stored_pub, mk_db::webauthn::b64url_encode(&point));
        assert_eq!(pub1, stored_pub);

        // The legacy read path sees the same scalar.
        assert_eq!(push_vapid_key(&pool).await, Some(priv1));
    }
}
