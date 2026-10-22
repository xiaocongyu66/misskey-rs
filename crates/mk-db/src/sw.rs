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

/// One `sw_subscription` row as the push-delivery leg reads it (upstream
/// `MiSwSubscription` via PushNotificationService's per-user cache fetch,
/// `findBy({ userId })`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Row {
    pub endpoint: String,
    pub auth: String,
    pub publickey: String,
    pub send_read_message: bool,
}

/// `subscriptionsCache.fetcher: findBy({ userId })` — every subscription of
/// one user, in PK order (stable for tests; upstream relies on cache order).
pub async fn list_by_user(pool: &PgPool, user_id: &str) -> Result<Vec<Row>, sqlx::Error> {
    sqlx::query_as::<_, Row>(
        r#"SELECT "endpoint", "auth", "publickey", "sendReadMessage" AS send_read_message FROM "sw_subscription"
           WHERE "userId" = $1 ORDER BY "id""#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
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

/// One registration as `sw/show-registration` + `sw/update-registration`
/// read it (`findOneBy({ userId, endpoint })`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RegistrationRow {
    pub id: String,
    pub endpoint: String,
    #[sqlx(rename = "sendReadMessage")]
    pub send_read_message: bool,
}

/// `swSubscriptionsRepository.findOneBy({ userId, endpoint })`.
pub async fn find_registration(
    pool: &PgPool,
    user_id: &str,
    endpoint: &str,
) -> Result<Option<RegistrationRow>, sqlx::Error> {
    sqlx::query_as::<_, RegistrationRow>(
        r#"SELECT "id", "endpoint", "sendReadMessage" FROM "sw_subscription"
           WHERE "userId" = $1 AND "endpoint" = $2
           ORDER BY "id" LIMIT 1"#,
    )
    .bind(user_id)
    .bind(endpoint)
    .fetch_optional(pool)
    .await
}

/// `sw/update-registration` write (`update(id, { sendReadMessage })`).
pub async fn update_send_read_message(
    pool: &PgPool,
    id: &str,
    send_read_message: bool,
) -> Result<u64, sqlx::Error> {
    let res = sqlx::query(r#"UPDATE "sw_subscription" SET "sendReadMessage" = $2 WHERE "id" = $1"#)
        .bind(id)
        .bind(send_read_message)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}
