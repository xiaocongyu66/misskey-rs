//! `i/signin-history` SQL (upstream `endpoints/i/signin-history.ts`). The
//! `signin` table ships in the 0001 baseline (id/userId/ip/headers/success;
//! no createdAt column — upstream derives the response createdAt from the
//! AID id, and so does the handler). Rows page by id DESC on the standard
//! misskey:id cursors (`id < untilId`, `id > sinceId`).

use serde_json::Value;
use sqlx::PgPool;

#[derive(Debug, sqlx::FromRow)]
pub struct SigninRow {
    pub id: String,
    pub ip: String,
    pub headers: Value,
    pub success: bool,
}

pub async fn signin_history(
    pool: &PgPool,
    user_id: &str,
    limit: i64,
    since_id: Option<&str>,
    until_id: Option<&str>,
) -> Result<Vec<SigninRow>, sqlx::Error> {
    sqlx::query_as::<_, SigninRow>(
        r#"SELECT "id", "ip", "headers", "success" FROM "signin"
           WHERE "userId" = $1
             AND ($2::text IS NULL OR "id" > $2)
             AND ($3::text IS NULL OR "id" < $3)
           ORDER BY "id" DESC LIMIT $4"#,
    )
    .bind(user_id)
    .bind(since_id)
    .bind(until_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}
