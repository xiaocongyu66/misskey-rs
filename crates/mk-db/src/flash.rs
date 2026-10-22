//! Flash (Misskey Play) domain db paths — upstream
//! `endpoints/flash/*.ts` + `core/FlashService.ts` +
//! `core/entities/FlashEntityService.ts`.
//!
//! Tables ship in the 0001 baseline (`flash`, `flash_like`); no migration.
//! `flash.updatedAt` comes back as epoch-ms (`(EXTRACT(EPOCH ...) * 1000)`),
//! the announcement.rs convention. Aid ids sort monotonically as strings, so
//! sinceId/untilId paging is a plain string compare; the pager bounds are
//! EXCLUSIVE (`makePaginationQuery`), sinceId-only → oldest-first ASC,
//! otherwise newest-first DESC.

use sqlx::PgPool;

/// A `flash` row as `FlashsRepository.findOneBy` sees it.
#[derive(Debug, sqlx::FromRow)]
pub struct FlashRow {
    pub id: String,
    #[sqlx(rename = "updatedAtMs")]
    pub updated_at_ms: i64,
    pub title: String,
    pub summary: String,
    pub user_id: String,
    pub script: String,
    pub permissions: Vec<String>,
    pub liked_count: i32,
    pub visibility: String,
}

const FLASH_COLS: &str = r#""id",
    (EXTRACT(EPOCH FROM "updatedAt") * 1000)::bigint AS "updatedAtMs",
    "title", "summary", "userId" AS "user_id", "script", "permissions",
    "likedCount" AS "liked_count", "visibility""#;

pub async fn find_flash(pool: &PgPool, id: &str) -> Result<Option<FlashRow>, sqlx::Error> {
    sqlx::query_as::<_, FlashRow>(&format!(
        r#"SELECT {FLASH_COLS} FROM "flash" WHERE "id" = $1"#
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// `endpoints/flash/create.ts` — `flashsRepository.insertOne`.
pub struct NewFlash<'a> {
    pub id: &'a str,
    pub user_id: &'a str,
    pub title: &'a str,
    pub summary: &'a str,
    pub script: &'a str,
    pub permissions: &'a [String],
    pub visibility: &'a str,
}

pub async fn insert_flash(pool: &PgPool, n: &NewFlash<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO "flash"
          ("id", "updatedAt", "title", "summary", "userId", "script",
           "permissions", "visibility")
        VALUES ($1, now(), $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(n.id)
    .bind(n.title)
    .bind(n.summary)
    .bind(n.user_id)
    .bind(n.script)
    .bind(n.permissions)
    .bind(n.visibility)
    .execute(pool)
    .await?;
    Ok(())
}

/// `flashsRepository.update(id, { updatedAt: new Date(), ...provided })`.
#[derive(Debug, Default)]
pub struct UpdateFlash<'a> {
    pub title: Option<&'a str>,
    pub summary: Option<&'a str>,
    pub script: Option<&'a str>,
    pub permissions: Option<&'a [String]>,
    pub visibility: Option<&'a str>,
}

pub async fn update_flash(
    pool: &PgPool,
    id: &str,
    ps: &UpdateFlash<'_>,
) -> Result<(), sqlx::Error> {
    let mut q = sqlx::QueryBuilder::new(r#"UPDATE "flash" SET "updatedAt" = now()"#);
    if let Some(v) = ps.title {
        q.push(r#", "title" = "#);
        q.push_bind(v);
    }
    if let Some(v) = ps.summary {
        q.push(r#", "summary" = "#);
        q.push_bind(v);
    }
    if let Some(v) = ps.script {
        q.push(r#", "script" = "#);
        q.push_bind(v);
    }
    if let Some(v) = ps.permissions {
        q.push(r#", "permissions" = "#);
        q.push_bind(v.to_vec());
    }
    if let Some(v) = ps.visibility {
        q.push(r#", "visibility" = "#);
        q.push_bind(v);
    }
    q.push(r#" WHERE "id" = "#);
    q.push_bind(id);
    q.build().execute(pool).await?;
    Ok(())
}

pub async fn delete_flash(pool: &PgPool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "flash" WHERE "id" = $1"#)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// likes (flash_like; no unique index in 0001 — the service does the exists
// check, mirrored here by the insert's NOT EXISTS guard)
// ---------------------------------------------------------------------------

pub async fn is_liked(pool: &PgPool, flash_id: &str, user_id: &str) -> Result<bool, sqlx::Error> {
    let n: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "flash_like" WHERE "flashId" = $1 AND "userId" = $2"#,
    )
    .bind(flash_id)
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

/// `true` means the like already existed (the service-level ALREADY_LIKED).
pub async fn insert_like(
    pool: &PgPool,
    id: &str,
    flash_id: &str,
    user_id: &str,
) -> Result<bool, sqlx::Error> {
    let res = sqlx::query(
        r#"
        INSERT INTO "flash_like" ("id", "userId", "flashId")
        SELECT $1, $2, $3
        WHERE NOT EXISTS (
            SELECT 1 FROM "flash_like" WHERE "userId" = $2 AND "flashId" = $3
        )
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(flash_id)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() == 0)
}

/// `false` means there was no like to delete (the service-level NOT_LIKED).
pub async fn delete_like(
    pool: &PgPool,
    flash_id: &str,
    user_id: &str,
) -> Result<bool, sqlx::Error> {
    let res = sqlx::query(r#"DELETE FROM "flash_like" WHERE "flashId" = $1 AND "userId" = $2"#)
        .bind(flash_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}

pub async fn increment_liked_count(pool: &PgPool, flash_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "flash" SET "likedCount" = "likedCount" + 1 WHERE "id" = $1"#)
        .bind(flash_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn decrement_liked_count(pool: &PgPool, flash_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "flash" SET "likedCount" = "likedCount" - 1 WHERE "id" = $1"#)
        .bind(flash_id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// listings
// ---------------------------------------------------------------------------

/// sinceId-only pages oldest-first, every other cursor shape newest-first
/// (QueryService.makePaginationQuery).
fn order_dir(since_id: Option<&str>, until_id: Option<&str>) -> &'static str {
    if since_id.is_some() && until_id.is_none() {
        "ASC"
    } else {
        "DESC"
    }
}

/// `endpoints/flash/my.ts` — my flashes, paged on id.
pub async fn list_my(
    pool: &PgPool,
    me_id: &str,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<FlashRow>, sqlx::Error> {
    let sql = format!(
        r#"SELECT {FLASH_COLS} FROM "flash"
           WHERE "userId" = $1
             AND ($2::text IS NULL OR "id" > $2)
             AND ($3::text IS NULL OR "id" < $3)
           ORDER BY "id" {}
           LIMIT $4"#,
        order_dir(since_id, until_id)
    );
    sqlx::query_as::<_, FlashRow>(&sql)
        .bind(me_id)
        .bind(since_id)
        .bind(until_id)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// `FlashService.featured` — likedCount DESC, updatedAt DESC, id DESC.
pub async fn list_featured(
    pool: &PgPool,
    offset: i64,
    limit: i64,
) -> Result<Vec<FlashRow>, sqlx::Error> {
    let sql = format!(
        r#"SELECT {FLASH_COLS} FROM "flash"
           WHERE "likedCount" > 0 AND "visibility" = 'public'
           ORDER BY "likedCount" DESC, "updatedAt" DESC, "id" DESC
           OFFSET $1 LIMIT $2"#
    );
    sqlx::query_as::<_, FlashRow>(&sql)
        .bind(offset)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// Escape `%`, `_` and `\` for an ILIKE pattern (upstream sqlLikeEscape).
fn like_pattern(word: &str) -> String {
    let mut out = String::with_capacity(word.len() + 2);
    out.push('%');
    for ch in word.chars() {
        match ch {
            '\\' => out.push_str(r"\\"),
            '%' => out.push_str(r"\%"),
            '_' => out.push_str(r"\_"),
            _ => out.push(ch),
        }
    }
    out.push('%');
    out
}

fn word_patterns(words: &[String]) -> Vec<String> {
    words.iter().map(|w| like_pattern(w)).collect()
}

/// `FlashService.search` — public flashes whose title OR summary matches
/// every whitespace-separated word (Brackets + ILIKE per word).
pub async fn search(
    pool: &PgPool,
    words: &[String],
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<FlashRow>, sqlx::Error> {
    let patterns = word_patterns(words);
    let sql = format!(
        r#"SELECT {FLASH_COLS} FROM "flash"
           WHERE "visibility" = 'public'
             AND ($1::text IS NULL OR "id" > $1)
             AND ($2::text IS NULL OR "id" < $2)
             AND NOT EXISTS (
                 SELECT 1 FROM unnest($3::text[]) AS w
                 WHERE NOT ("title" ILIKE w OR "summary" ILIKE w)
             )
           ORDER BY "id" {}
           LIMIT $4"#,
        order_dir(since_id, until_id)
    );
    sqlx::query_as::<_, FlashRow>(&sql)
        .bind(since_id)
        .bind(until_id)
        .bind(patterns)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// One `flash_like` row for `flash/my-likes` (`FlashLikeEntityService.pack`).
#[derive(Debug, sqlx::FromRow)]
pub struct FlashLikeRow {
    pub id: String,
    #[sqlx(rename = "flashId")]
    pub flash_id: String,
}

/// `FlashService.myLikes` — likes by me, paged on like.id, joined to the
/// flash for the optional search filter.
pub async fn list_my_likes(
    pool: &PgPool,
    me_id: &str,
    words: &[String],
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<FlashLikeRow>, sqlx::Error> {
    let patterns = word_patterns(words);
    let sql = format!(
        r#"SELECT l."id", l."flashId" AS "flashId"
           FROM "flash_like" l
           INNER JOIN "flash" f ON f."id" = l."flashId"
           WHERE l."userId" = $1
             AND ($2::text IS NULL OR l."id" > $2)
             AND ($3::text IS NULL OR l."id" < $3)
             AND NOT EXISTS (
                 SELECT 1 FROM unnest($4::text[]) AS w
                 WHERE NOT (f."title" ILIKE w OR f."summary" ILIKE w)
             )
           ORDER BY l."id" {}
           LIMIT $5"#,
        order_dir(since_id, until_id)
    );
    sqlx::query_as::<_, FlashLikeRow>(&sql)
        .bind(me_id)
        .bind(since_id)
        .bind(until_id)
        .bind(patterns)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// `endpoints/users/flashs.ts` — public flashes created by `user_id`, paged
/// on id with the makePaginationQuery direction rule.
pub async fn list_by_user(
    pool: &PgPool,
    user_id: &str,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<FlashRow>, sqlx::Error> {
    let sql = format!(
        r#"SELECT {FLASH_COLS} FROM "flash"
           WHERE "userId" = $1
             AND "visibility" = 'public'
             AND ($2::text IS NULL OR "id" > $2)
             AND ($3::text IS NULL OR "id" < $3)
           ORDER BY "id" {}
           LIMIT $4"#,
        order_dir(since_id, until_id)
    );
    sqlx::query_as::<_, FlashRow>(&sql)
        .bind(user_id)
        .bind(since_id)
        .bind(until_id)
        .bind(limit)
        .fetch_all(pool)
        .await
}
