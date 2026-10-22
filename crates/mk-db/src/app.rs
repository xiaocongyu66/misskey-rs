//! `app` / `auth_session` / app-scoped `access_token` db paths (upstream
//! `endpoints/app/*.ts`, `endpoints/auth/session/*.ts`, `endpoints/auth/
//! accept.ts`, `endpoints/i/{apps,regenerate-token}.ts`).
//!
//! All three tables ship in the 0001 baseline: `app` (id, userId?, secret,
//! name, description, permission[], callbackUrl?), `auth_session` (id, token,
//! userId?, appId) and `access_token` (id, token, hash, userId, appId?,
//! lastUsedAt?, session?, name?, description?, iconUrl?, permission[],
//! fetched). `description` is NOT NULL on `app`; `callbackUrl` is nullable.

use crate::sqlx::PgPool;

// ---------------------------------------------------------------------------
// app
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AppRow {
    pub id: String,
    pub user_id: Option<String>,
    pub secret: String,
    pub name: String,
    pub description: String,
    pub permission: Vec<String>,
    pub callback_url: Option<String>,
}

const APP_COLS: &str = r#""id", "userId" AS user_id, "secret", "name",
    "description", "permission", "callbackUrl" AS callback_url"#;

pub async fn insert_app(
    pool: &PgPool,
    id: &str,
    user_id: Option<&str>,
    secret: &str,
    name: &str,
    description: &str,
    permission: &[String],
    callback_url: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO "app"
           ("id", "userId", "secret", "name", "description", "permission", "callbackUrl")
           VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
    )
    .bind(id)
    .bind(user_id)
    .bind(secret)
    .bind(name)
    .bind(description)
    .bind(permission)
    .bind(callback_url)
    .execute(pool)
    .await
    .map(|_| ())
}

pub async fn find_app_by_id(pool: &PgPool, id: &str) -> Result<Option<AppRow>, sqlx::Error> {
    sqlx::query_as::<_, AppRow>(&format!(
        r#"SELECT {} FROM "app" WHERE "id" = $1"#,
        APP_COLS
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub async fn find_app_by_secret(
    pool: &PgPool,
    secret: &str,
) -> Result<Option<AppRow>, sqlx::Error> {
    sqlx::query_as::<_, AppRow>(&format!(
        r#"SELECT {} FROM "app" WHERE "secret" = $1"#,
        APP_COLS
    ))
    .bind(secret)
    .fetch_optional(pool)
    .await
}

/// `my/apps`: `appsRepository.find({ where: { userId }, take, skip })`.
pub async fn list_apps_by_user(
    pool: &PgPool,
    user_id: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<AppRow>, sqlx::Error> {
    sqlx::query_as::<_, AppRow>(&format!(
        r#"SELECT {} FROM "app" WHERE "userId" = $1 LIMIT $2 OFFSET $3"#,
        APP_COLS
    ))
    .bind(user_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// `accessTokensRepository.countBy({ appId, userId })` — the App pack's
/// `isAuthorized` field.
pub async fn count_app_tokens(
    pool: &PgPool,
    app_id: &str,
    user_id: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        r#"SELECT COUNT(*) FROM "access_token" WHERE "appId" = $1 AND "userId" = $2"#,
    )
    .bind(app_id)
    .bind(user_id)
    .fetch_one(pool)
    .await
}

// ---------------------------------------------------------------------------
// auth_session
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AuthSessionRow {
    pub id: String,
    pub token: String,
    pub user_id: Option<String>,
    pub app_id: String,
}

const SESSION_COLS: &str = r#""id", "token", "userId" AS user_id, "appId" AS app_id"#;

pub async fn insert_session(
    pool: &PgPool,
    id: &str,
    token: &str,
    app_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(r#"INSERT INTO "auth_session" ("id", "token", "appId") VALUES ($1, $2, $3)"#)
        .bind(id)
        .bind(token)
        .bind(app_id)
        .execute(pool)
        .await
        .map(|_| ())
}

pub async fn find_session_by_token(
    pool: &PgPool,
    token: &str,
) -> Result<Option<AuthSessionRow>, sqlx::Error> {
    sqlx::query_as::<_, AuthSessionRow>(&format!(
        r#"SELECT {} FROM "auth_session" WHERE "token" = $1"#,
        SESSION_COLS
    ))
    .bind(token)
    .fetch_optional(pool)
    .await
}

pub async fn find_session_by_token_app(
    pool: &PgPool,
    token: &str,
    app_id: &str,
) -> Result<Option<AuthSessionRow>, sqlx::Error> {
    sqlx::query_as::<_, AuthSessionRow>(&format!(
        r#"SELECT {} FROM "auth_session" WHERE "token" = $1 AND "appId" = $2"#,
        SESSION_COLS
    ))
    .bind(token)
    .bind(app_id)
    .fetch_optional(pool)
    .await
}

pub async fn update_session_user(
    pool: &PgPool,
    session_id: &str,
    user_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "auth_session" SET "userId" = $2 WHERE "id" = $1"#)
        .bind(session_id)
        .bind(user_id)
        .execute(pool)
        .await
        .map(|_| ())
}

pub async fn delete_session(pool: &PgPool, session_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "auth_session" WHERE "id" = $1"#)
        .bind(session_id)
        .execute(pool)
        .await
        .map(|_| ())
}

// ---------------------------------------------------------------------------
// app-scoped access tokens
// ---------------------------------------------------------------------------

pub async fn app_access_token_exists(
    pool: &PgPool,
    app_id: &str,
    user_id: &str,
) -> Result<bool, sqlx::Error> {
    let hit: Option<i32> = sqlx::query_scalar(
        r#"SELECT 1 FROM "access_token"
           WHERE "appId" = $1 AND "userId" = $2 LIMIT 1"#,
    )
    .bind(app_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(hit.is_some())
}

/// `accessTokensRepository.findOneByOrFail({ appId, userId })` — the session
/// handshake's token row.
pub async fn find_app_access_token(
    pool: &PgPool,
    app_id: &str,
    user_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        r#"SELECT "token" FROM "access_token"
           WHERE "appId" = $1 AND "userId" = $2 LIMIT 1"#,
    )
    .bind(app_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

/// `auth/accept.ts`: insert the sha256(token + app.secret) hash row with
/// `lastUsedAt = now()`; `permission` keeps the table default `{}`.
pub async fn insert_app_access_token(
    pool: &PgPool,
    id: &str,
    token: &str,
    hash: &str,
    app_id: &str,
    user_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO "access_token"
           ("id", "token", "hash", "userId", "appId", "lastUsedAt")
           VALUES ($1, $2, $3, $4, $5, now())"#,
    )
    .bind(id)
    .bind(token)
    .bind(hash)
    .bind(user_id)
    .bind(app_id)
    .execute(pool)
    .await
    .map(|_| ())
}

// ---------------------------------------------------------------------------
// i/apps (access-token list)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AppTokenRow {
    pub id: String,
    pub name: Option<String>,
    /// ISO-8601 text (the timestamp is formatted in SQL; mk-db has no chrono).
    pub last_used_at: Option<String>,
    pub permission: Vec<String>,
    pub icon_url: Option<String>,
    pub description: Option<String>,
    pub app_name: Option<String>,
    pub app_permission: Option<Vec<String>>,
    pub app_description: Option<String>,
}

/// `i/apps.ts`: access tokens joined with their app, ordered per the `sort`
/// enum (`+createdAt` = id DESC — upstream's inverted mapping).
pub async fn list_app_tokens_by_user(
    pool: &PgPool,
    user_id: &str,
    sort: Option<&str>,
) -> Result<Vec<AppTokenRow>, sqlx::Error> {
    let order = match sort {
        Some("+createdAt") => r#"ORDER BY t."id" DESC"#,
        Some("-createdAt") => r#"ORDER BY t."id" ASC"#,
        Some("+lastUsedAt") => r#"ORDER BY t."lastUsedAt" DESC"#,
        Some("-lastUsedAt") => r#"ORDER BY t."lastUsedAt" ASC"#,
        _ => r#"ORDER BY t."id" ASC"#,
    };
    let sql = format!(
        r#"SELECT t."id",
                  t."name",
                  to_char(t."lastUsedAt" AT TIME ZONE 'UTC',
                          'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS last_used_at,
                  t."permission",
                  t."iconUrl" AS icon_url,
                  t."description",
                  a."name" AS app_name,
                  a."permission" AS app_permission,
                  a."description" AS app_description
           FROM "access_token" t
           LEFT JOIN "app" a ON a."id" = t."appId"
           WHERE t."userId" = $1
           {order}"#
    );
    sqlx::query_as::<_, AppTokenRow>(&sql)
        .bind(user_id)
        .fetch_all(pool)
        .await
}

// ---------------------------------------------------------------------------
// i/regenerate-token
// ---------------------------------------------------------------------------

pub async fn set_user_token(pool: &PgPool, user_id: &str, token: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "user" SET "token" = $2 WHERE "id" = $1"#)
        .bind(user_id)
        .bind(token)
        .execute(pool)
        .await
        .map(|_| ())
}
