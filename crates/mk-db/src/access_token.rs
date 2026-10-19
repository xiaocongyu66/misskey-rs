//! access_token domain — `MiAccessToken` rows (upstream
//! `src/models/AccessToken.ts`, table ships in the 0001 baseline). Two
//! consumers: the api.ts arena's miauth token mint (`miauth/gen-token`)
//! and the app-token scope gate (`ApiCallService.call` L465 — the
//! `ep.meta.kind` permission check that bites access tokens only,
//! native user tokens ride the `token == null` path upstream).

/// A resolved access token: the owning user + the permission scope list.
pub struct AccessTokenRow {
    pub user_id: String,
    pub permission: Vec<String>,
}

/// `AuthenticateService.authenticate` app branch: access tokens are looked
/// up by `hash` (upstream lowercases the presented token before the lookup
/// — minted tokens are lowercase alnum by construction, so plain equality
/// matches). `lastUsedAt` is bumped upstream on every hit; skipped here
/// (no assert reads it).
pub async fn find_by_hash(
    pool: &sqlx::PgPool,
    hash: &str,
) -> Result<Option<AccessTokenRow>, sqlx::Error> {
    let row: Option<(String, Vec<String>)> =
        sqlx::query_as(r#"SELECT "userId", "permission" FROM "access_token" WHERE "hash" = $1"#)
            .bind(hash)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(user_id, permission)| AccessTokenRow {
        user_id,
        permission,
    }))
}

/// `miauth/gen-token.ts` handler insert: fresh token/hash pair (the token
/// resolves the row; the hash column is the lookup key there), session +
/// scope list from the request. appId/name/description/iconUrl stay NULL
/// and `fetched` false (the misskey-js assert-free mint path never sets
/// them).
pub async fn insert(
    pool: &sqlx::PgPool,
    id: &str,
    token: &str,
    user_id: &str,
    session: Option<&str>,
    permission: &[String],
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO "access_token"
           ("id", "token", "hash", "userId", "lastUsedAt", "session", "permission")
           VALUES ($1, $2, $3, $4, now(), $5, $6)"#,
    )
    .bind(id)
    .bind(token)
    .bind(token) // hash — upstream stores the unhashed secureRndstr in both
    .bind(user_id)
    .bind(session)
    .bind(permission)
    .execute(pool)
    .await
    .map(|_| ())
}
