//! avatar decorations: db paths behind the avatar-decoration domain
//! (upstream `AvatarDecorationService.ts` + `endpoints/get-avatar-decorations.ts`
//! + `endpoints/admin/avatar-decorations/*.ts`). The `avatar_decoration`
//! table ships in the 0001 baseline, so no migration is needed.
//!
//! Timestamps: `createdAt` is derived from the AID (upstream
//! `IdService.parse(id).date`), `updatedAt` is written with `now()` and read
//! back as epoch milliseconds (the api layer formats it as ISO 8601).
//! `roleIdsThatCanBeUsedThisDecoration` is a varchar[] mapped to `Vec<String>`.

use crate::sqlx::PgPool;

/// `avatar_decoration` columns in entity order; `updatedAt` maps to epoch ms.
const COLS: &str = r#""id",
(EXTRACT(EPOCH FROM "updatedAt") * 1000)::bigint AS "updated_at_ms",
"url", "name", "description",
"roleIdsThatCanBeUsedThisDecoration" AS "role_ids",
"category""#;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AvatarDecorationRow {
    pub id: String,
    pub updated_at_ms: Option<i64>,
    pub url: String,
    pub name: String,
    pub description: String,
    pub role_ids: Vec<String>,
    pub category: Option<String>,
}

/// `avatarDecorationsRepository.find()` (upstream `getAll`): every row, no
/// pagination (the admin list endpoint ignores its query params upstream).
pub async fn list_all(pool: &PgPool) -> Result<Vec<AvatarDecorationRow>, sqlx::Error> {
    let sql = format!(r#"SELECT {COLS} FROM "avatar_decoration""#);
    sqlx::query_as::<_, AvatarDecorationRow>(&sql)
        .fetch_all(pool)
        .await
}

pub async fn find_by_id(
    pool: &PgPool,
    id: &str,
) -> Result<Option<AvatarDecorationRow>, sqlx::Error> {
    let sql = format!(r#"SELECT {COLS} FROM "avatar_decoration" WHERE "id" = $1"#);
    sqlx::query_as::<_, AvatarDecorationRow>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// `avatarDecorationsRepository.insertOne({ id, ...options })` — `updatedAt`
/// starts NULL (the column default).
pub async fn insert(
    pool: &PgPool,
    id: &str,
    url: &str,
    name: &str,
    description: &str,
    role_ids: &Vec<String>,
    category: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO "avatar_decoration"
             ("id", "url", "name", "description",
              "roleIdsThatCanBeUsedThisDecoration", "category")
           VALUES ($1, $2, $3, $4, $5, $6)"#,
    )
    .bind(id)
    .bind(url)
    .bind(name)
    .bind(description)
    .bind(role_ids)
    .bind(category)
    .execute(pool)
    .await?;
    Ok(())
}

/// Partial `avatarDecorationsRepository.update(id, params)` patch. `None`
/// skips the column (TypeORM `undefined`); `Some(None)` writes NULL (the
/// `category` reset arm).
#[derive(Debug, Default)]
pub struct AvatarDecorationPatch {
    pub name: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
    pub role_ids: Option<Vec<String>>,
    pub category: Option<Option<String>>,
}

/// Always bumps `"updatedAt" = now()` like the service.
pub async fn update(
    pool: &PgPool,
    id: &str,
    patch: &AvatarDecorationPatch,
) -> Result<(), sqlx::Error> {
    let mut qb = sqlx::QueryBuilder::new(r#"UPDATE "avatar_decoration" SET "updatedAt" = now()"#);
    if let Some(v) = &patch.name {
        qb.push(r#", "name" = "#).push_bind(v);
    }
    if let Some(v) = &patch.description {
        qb.push(r#", "description" = "#).push_bind(v);
    }
    if let Some(v) = &patch.url {
        qb.push(r#", "url" = "#).push_bind(v);
    }
    if let Some(v) = &patch.role_ids {
        qb.push(r#", "roleIdsThatCanBeUsedThisDecoration" = "#)
            .push_bind(v);
    }
    if let Some(v) = &patch.category {
        qb.push(r#", "category" = "#).push_bind(v.as_deref());
    }
    qb.push(r#" WHERE "id" = "#).push_bind(id);
    qb.build().execute(pool).await?;
    Ok(())
}

pub async fn delete(pool: &PgPool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "avatar_decoration" WHERE "id" = $1"#)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Role ids the decoration's role list is filtered against: every role for a
/// signed-in caller, public roles only for a guest (`get-avatar-decorations`
/// visibleRoles).
pub async fn visible_role_ids(
    pool: &PgPool,
    public_only: bool,
) -> Result<Vec<String>, sqlx::Error> {
    let sql = if public_only {
        r#"SELECT "id" FROM "role" WHERE "isPublic" = TRUE"#
    } else {
        r#"SELECT "id" FROM "role""#
    };
    sqlx::query_scalar::<_, String>(sql).fetch_all(pool).await
}
