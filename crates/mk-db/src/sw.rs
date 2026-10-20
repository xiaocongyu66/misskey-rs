//! Web-push subscriptions (upstream `sw/register.ts` + `sw/unregister.ts`
//! on the 0001 `sw_subscription` baseline; the push-delivery leg lives in
//! mk-core::services::web_push).

use sqlx::PgPool;

/// meta."swPublicKey" — the `serverSettings.swPublicKey` the register res
/// echoes as `key` (0001_init.sql:2645, nullable varchar(1024)); absent row
/// or column → None (the res field renders null).
pub async fn meta_sw_public_key(pool: &PgPool) -> Result<Option<String>, sqlx::Error> {
    let v: Option<Option<String>> =
        sqlx::query_scalar(r#"SELECT "swPublicKey" FROM "meta" ORDER BY "id" DESC LIMIT 1"#)
            .fetch_optional(pool)
            .await?;
    Ok(v.flatten())
}

/// `swSubscriptionsRepository.insert` — an aid id rides from the caller.
pub async fn insert_subscription(
    pool: &PgPool,
    id: &str,
    user_id: &str,
    endpoint: &str,
    auth: &str,
    publickey: &str,
    send_read_message: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO "sw_subscription"
               ("id", "userId", "endpoint", "auth", "publickey", "sendReadMessage")
           VALUES ($1, $2, $3, $4, $5, $6)"#,
    )
    .bind(id)
    .bind(user_id)
    .bind(endpoint)
    .bind(auth)
    .bind(publickey)
    .bind(send_read_message)
    .execute(pool)
    .await?;
    Ok(())
}

/// `findOneBy({ userId, endpoint, auth, publickey })` — the
/// already-subscribed probe.
pub async fn find_subscription(
    pool: &PgPool,
    user_id: &str,
    endpoint: &str,
    auth: &str,
    publickey: &str,
) -> Result<Option<(String, bool)>, sqlx::Error> {
    let row = sqlx::query_as::<_, (String, bool)>(
        r#"SELECT "id", "sendReadMessage" FROM "sw_subscription"
           WHERE "userId" = $1 AND "endpoint" = $2 AND "auth" = $3
             AND "publickey" = $4"#,
    )
    .bind(user_id)
    .bind(endpoint)
    .bind(auth)
    .bind(publickey)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// `delete(subscriptions.map(s => s.id))` — unregister removes every row
/// matching the endpoint/auth/publickey triple for this user.
pub async fn delete_subscription(
    pool: &PgPool,
    user_id: &str,
    endpoint: &str,
    auth: &str,
    publickey: &str,
) -> Result<u64, sqlx::Error> {
    let res = sqlx::query(
        r#"DELETE FROM "sw_subscription"
           WHERE "userId" = $1 AND "endpoint" = $2 AND "auth" = $3
             AND "publickey" = $4"#,
    )
    .bind(user_id)
    .bind(endpoint)
    .bind(auth)
    .bind(publickey)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}
