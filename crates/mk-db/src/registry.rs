//! `i/registry/*` SQL (upstream `core/RegistryApiService.ts`). The
//! `registry_item` table ships in the 0001 baseline:
//!
//! ```text
//! id varchar(32), updatedAt timestamptz, userId varchar(32),
//! key varchar(1024), scope varchar(1024)[], domain varchar(512), value jsonb
//! ```
//!
//! Upstream stores the caller's `key: string[]` joined with `/`
//! (`key.join('/')`), and `scope` as the array column verbatim (exact array
//! equality). `remove` deletes the exact key plus its subtree
//! (`key LIKE '<joined>/%'`). No unique constraint on
//! (userId, key, scope) exists, so `set` follows upstream's
//! find-then-update-or-insert flow.

use serde_json::Value;
use sqlx::PgPool;

/// `RegistryApiService.get` — exact (userId, scope, key) lookup.
pub async fn get(
    pool: &PgPool,
    user_id: &str,
    scope: &[String],
    key: &str,
) -> Result<Option<Value>, sqlx::Error> {
    sqlx::query_scalar::<_, Value>(
        r#"SELECT "value" FROM "registry_item"
           WHERE "userId" = $1 AND "scope" = $2::varchar[] AND "key" = $3"#,
    )
    .bind(user_id)
    .bind(scope)
    .bind(key)
    .fetch_optional(pool)
    .await
}

/// `RegistryApiService.set` — update in place when the (userId, scope, key)
/// row exists, otherwise insert with the caller-supplied AID.
pub async fn set(
    pool: &PgPool,
    id: &str,
    user_id: &str,
    scope: &[String],
    key: &str,
    value: &Value,
) -> Result<(), sqlx::Error> {
    let updated = sqlx::query(
        r#"UPDATE "registry_item" SET "value" = $4, "updatedAt" = now()
           WHERE "userId" = $1 AND "scope" = $2::varchar[] AND "key" = $3"#,
    )
    .bind(user_id)
    .bind(scope)
    .bind(key)
    .bind(value)
    .execute(pool)
    .await?;

    if updated.rows_affected() == 0 {
        sqlx::query(
            r#"INSERT INTO "registry_item" ("id", "updatedAt", "userId", "key", "scope", "value")
               VALUES ($1, now(), $2, $3, $4::varchar[], $5)"#,
        )
        .bind(id)
        .bind(user_id)
        .bind(key)
        .bind(scope)
        .bind(value)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// `RegistryApiService.remove` — exact key + subtree (`key/%`).
pub async fn remove(
    pool: &PgPool,
    user_id: &str,
    scope: &[String],
    key: &str,
) -> Result<(), sqlx::Error> {
    if key.is_empty() {
        // upstream guards `if (key.length > 0)` — an empty key removes the
        // whole scope for the caller
        sqlx::query(
            r#"DELETE FROM "registry_item" WHERE "userId" = $1 AND "scope" = $2::varchar[]"#,
        )
        .bind(user_id)
        .bind(scope)
        .execute(pool)
        .await?;
        return Ok(());
    }
    let like = format!("{key}/%");
    sqlx::query(
        r#"DELETE FROM "registry_item"
           WHERE "userId" = $1 AND "scope" = $2::varchar[]
             AND ("key" = $3 OR "key" LIKE $4)"#,
    )
    .bind(user_id)
    .bind(scope)
    .bind(key)
    .bind(like)
    .execute(pool)
    .await?;
    Ok(())
}

/// `RegistryApiService.getAll` — every (key, value) row in the scope; the
/// caller folds the `/`-joined keys back into a nested object.
pub async fn get_all(
    pool: &PgPool,
    user_id: &str,
    scope: &[String],
) -> Result<Vec<(String, Value)>, sqlx::Error> {
    sqlx::query_as::<_, (String, Value)>(
        r#"SELECT "key", "value" FROM "registry_item"
           WHERE "userId" = $1 AND "scope" = $2::varchar[] ORDER BY "key" ASC"#,
    )
    .bind(user_id)
    .bind(scope)
    .fetch_all(pool)
    .await
}

/// `i/registry/keys` — the caller's stored keys in the scope (joined form;
/// the handler splits them back into arrays).
pub async fn keys(
    pool: &PgPool,
    user_id: &str,
    scope: &[String],
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        r#"SELECT "key" FROM "registry_item"
           WHERE "userId" = $1 AND "scope" = $2::varchar[] ORDER BY "key" ASC"#,
    )
    .bind(user_id)
    .bind(scope)
    .fetch_all(pool)
    .await
}

/// `i/registry/get-detail` — every scope row for the key (upstream filters by
/// userId + key only, so the response can report all scopes). `to_jsonb`
/// yields the entity columns under their schema (camelCase) names.
pub async fn get_detail(
    pool: &PgPool,
    user_id: &str,
    key: &str,
) -> Result<Vec<Value>, sqlx::Error> {
    sqlx::query_scalar::<_, Value>(
        r#"SELECT to_jsonb(r) FROM "registry_item" r
           WHERE r."userId" = $1 AND r."key" = $2 ORDER BY r."updatedAt" DESC"#,
    )
    .bind(user_id)
    .bind(key)
    .fetch_all(pool)
    .await
}
