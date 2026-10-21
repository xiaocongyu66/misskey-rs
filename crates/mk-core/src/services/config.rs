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
    let raw: Option<String> = sqlx::query_scalar(
        r#"SELECT (SELECT "swPrivateKey" FROM "meta" ORDER BY "id" DESC LIMIT 1)"#,
    )
    .fetch_one(pool)
    .await
    .ok()
    .flatten();
    let raw = raw?;
    mk_db::webauthn::b64url_decode(raw.trim())?.try_into().ok()
}

/// `meta.email` (the configured noreply address), trimmed; `None` when the
/// row/column is absent or blank.
async fn meta_email(pool: &PgPool) -> Option<String> {
    let raw: Option<String> =
        sqlx::query_scalar(r#"SELECT (SELECT "email" FROM "meta" ORDER BY "id" DESC LIMIT 1)"#)
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
}
