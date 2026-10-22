//! `auth/*` db paths (upstream `endpoints/auth/session/show.ts` +
//! `core/entities/AuthSessionEntityService.ts` / `AppEntityService.ts`).
//!
//! `auth_session` (id, token, userId?, appId) and `app` (id, userId?, secret,
//! name, description, permission[], callbackUrl?) ship in the 0001 baseline.
//! The session pack nests `AppEntityService.pack` (id, name, callbackUrl,
//! permission; `secret` and `description` are not exposed) plus the session
//! id/token.
//!
//! `auth/session/userkey` (app-secret handshake + access-token rotation +
//! `UserDetailedNotMe` pack) and `auth/accept` (secure-random token mint +
//! sha256 hash + AID + access-token insert) are not wired in this slice; see
//! `api/auth_more.rs`.

use crate::sqlx::PgPool;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SessionRow {
    pub id: String,
    pub token: String,
    pub app_id: String,
    pub app_name: String,
    pub app_callback_url: Option<String>,
    pub app_permission: Vec<String>,
}

/// `authSessionsRepository.findOneBy({ token })` joined with the app the
/// `AuthSessionEntityService.pack` embed needs.
pub async fn find_session_by_token(
    pool: &PgPool,
    token: &str,
) -> Result<Option<SessionRow>, sqlx::Error> {
    sqlx::query_as::<_, SessionRow>(
        r#"SELECT s."id", s."token",
                  a."id" AS app_id, a."name" AS app_name,
                  a."callbackUrl" AS app_callback_url,
                  a."permission" AS app_permission
           FROM "auth_session" s
           JOIN "app" a ON a."id" = s."appId"
           WHERE s."token" = $1"#,
    )
    .bind(token)
    .fetch_optional(pool)
    .await
}
