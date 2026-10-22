//! Page domain db paths — upstream `endpoints/pages/*.ts` +
//! `core/entities/PageEntityService.ts`.
//!
//! Tables ship in the 0001 baseline (`page`, `page_like`); no migration.
//! `page.updatedAt` comes back as epoch-ms (`(EXTRACT(EPOCH ...) * 1000)`),
//! the flash/announcement convention. Aid ids sort monotonically as strings,
//! so sinceId/untilId paging is a plain string compare; the pager bounds are
//! EXCLUSIVE (`makePaginationQuery`), sinceId-only → oldest-first ASC,
//! otherwise newest-first DESC.

use sqlx::PgPool;

/// A `page` row as `PageEntityService.pack` sees it.
#[derive(Debug, sqlx::FromRow)]
pub struct PageRow {
    pub id: String,
    #[sqlx(rename = "updatedAtMs")]
    pub updated_at_ms: i64,
    pub user_id: String,
    pub name: String,
    pub title: String,
    pub summary: Option<String>,
    pub content: serde_json::Value,
    pub variables: serde_json::Value,
    pub eye_catching_image_id: Option<String>,
    pub font: String,
    pub align_center: bool,
    pub hide_title_when_pinned: bool,
    pub liked_count: i32,
    pub visibility: String,
}

const PAGE_COLS: &str = r#""id",
    (EXTRACT(EPOCH FROM "updatedAt") * 1000)::bigint AS "updatedAtMs",
    "userId" AS "user_id", "name", "title", "summary", "content", "variables",
    "eyeCatchingImageId" AS "eye_catching_image_id", "font",
    "alignCenter" AS "align_center",
    "hideTitleWhenPinned" AS "hide_title_when_pinned",
    "likedCount" AS "liked_count", "visibility"::text AS "visibility""#;

/// Same projection with the `p.` join alias (`pages/liked`).
const PAGE_COLS_P: &str = r#"p."id",
    (EXTRACT(EPOCH FROM p."updatedAt") * 1000)::bigint AS "updatedAtMs",
    p."userId" AS "user_id", p."name", p."title", p."summary", p."content",
    p."variables", p."eyeCatchingImageId" AS "eye_catching_image_id",
    p."font", p."alignCenter" AS "align_center",
    p."hideTitleWhenPinned" AS "hide_title_when_pinned",
    p."likedCount" AS "liked_count", p."visibility"::text AS "visibility""#;

pub async fn find_page(pool: &PgPool, id: &str) -> Result<Option<PageRow>, sqlx::Error> {
    sqlx::query_as::<_, PageRow>(&format!(
        r#"SELECT {PAGE_COLS} FROM "page" WHERE "id" = $1"#
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// `endpoints/pages/create.ts` — `pagesRepository.insertOne`.
pub struct NewPage<'a> {
    pub id: &'a str,
    pub user_id: &'a str,
    pub name: &'a str,
    pub title: &'a str,
    pub summary: Option<&'a str>,
    pub content: &'a serde_json::Value,
    pub variables: &'a serde_json::Value,
    pub eye_catching_image_id: Option<&'a str>,
    pub font: &'a str,
    pub align_center: bool,
    pub hide_title_when_pinned: bool,
    pub script: &'a str,
}

pub async fn insert_page(pool: &PgPool, n: &NewPage<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO "page"
          ("id", "updatedAt", "userId", "name", "title", "summary", "content",
           "variables", "eyeCatchingImageId", "font", "alignCenter",
           "hideTitleWhenPinned", "script", "visibility", "visibleUserIds")
        VALUES ($1, now(), $2, $3, $4, $5, $6::jsonb, $7::jsonb, $8, $9, $10,
                $11, $12, 'public', '{}')
        "#,
    )
    .bind(n.id)
    .bind(n.user_id)
    .bind(n.name)
    .bind(n.title)
    .bind(n.summary)
    .bind(n.content.to_string())
    .bind(n.variables.to_string())
    .bind(n.eye_catching_image_id)
    .bind(n.font)
    .bind(n.align_center)
    .bind(n.hide_title_when_pinned)
    .bind(n.script)
    .execute(pool)
    .await?;
    Ok(())
}

/// `pagesRepository.update(id, { updatedAt: new Date(), ...provided })`.
#[derive(Debug, Default)]
pub struct UpdatePage<'a> {
    pub name: Option<&'a str>,
    pub title: Option<&'a str>,
    pub summary: Option<Option<&'a str>>,
    pub content: Option<&'a serde_json::Value>,
    pub variables: Option<&'a serde_json::Value>,
    pub eye_catching_image_id: Option<Option<&'a str>>,
    pub font: Option<&'a str>,
    pub align_center: Option<bool>,
    pub hide_title_when_pinned: Option<bool>,
    pub script: Option<&'a str>,
}

pub async fn update_page(pool: &PgPool, id: &str, ps: &UpdatePage<'_>) -> Result<(), sqlx::Error> {
    let mut q = sqlx::QueryBuilder::new(r#"UPDATE "page" SET "updatedAt" = now()"#);
    if let Some(v) = ps.name {
        q.push(r#", "name" = "#);
        q.push_bind(v);
    }
    if let Some(v) = ps.title {
        q.push(r#", "title" = "#);
        q.push_bind(v);
    }
    if let Some(v) = ps.summary {
        q.push(r#", "summary" = "#);
        q.push_bind(v);
    }
    if let Some(v) = ps.content {
        q.push(r#", "content" = "#);
        q.push_bind(v.to_string()).push("::jsonb");
    }
    if let Some(v) = ps.variables {
        q.push(r#", "variables" = "#);
        q.push_bind(v.to_string()).push("::jsonb");
    }
    if let Some(v) = ps.eye_catching_image_id {
        q.push(r#", "eyeCatchingImageId" = "#);
        q.push_bind(v);
    }
    if let Some(v) = ps.font {
        q.push(r#", "font" = "#);
        q.push_bind(v);
    }
    if let Some(v) = ps.align_center {
        q.push(r#", "alignCenter" = "#);
        q.push_bind(v);
    }
    if let Some(v) = ps.hide_title_when_pinned {
        q.push(r#", "hideTitleWhenPinned" = "#);
        q.push_bind(v);
    }
    if let Some(v) = ps.script {
        q.push(r#", "script" = "#);
        q.push_bind(v);
    }
    q.push(r#" WHERE "id" = "#);
    q.push_bind(id);
    q.build().execute(pool).await?;
    Ok(())
}

pub async fn delete_page(pool: &PgPool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "page" WHERE "id" = $1"#)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// likes (page_like; the 0001 baseline carries a unique (userId, pageId) index,
// so the insert's ON CONFLICT mirrors the service-level exists check)
// ---------------------------------------------------------------------------

pub async fn is_liked(pool: &PgPool, page_id: &str, user_id: &str) -> Result<bool, sqlx::Error> {
    let n: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "page_like" WHERE "pageId" = $1 AND "userId" = $2"#,
    )
    .bind(page_id)
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

/// `true` means the like already existed (the service-level ALREADY_LIKED).
pub async fn insert_like(
    pool: &PgPool,
    id: &str,
    page_id: &str,
    user_id: &str,
) -> Result<bool, sqlx::Error> {
    let res = sqlx::query(
        r#"
        INSERT INTO "page_like" ("id", "userId", "pageId")
        VALUES ($1, $2, $3)
        ON CONFLICT ("userId", "pageId") DO NOTHING
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(page_id)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() == 0)
}

/// `false` means there was no like to delete (the service-level NOT_LIKED).
pub async fn delete_like(pool: &PgPool, page_id: &str, user_id: &str) -> Result<bool, sqlx::Error> {
    let res = sqlx::query(r#"DELETE FROM "page_like" WHERE "pageId" = $1 AND "userId" = $2"#)
        .bind(page_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}

pub async fn increment_liked_count(pool: &PgPool, page_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "page" SET "likedCount" = "likedCount" + 1 WHERE "id" = $1"#)
        .bind(page_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn decrement_liked_count(pool: &PgPool, page_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "page" SET "likedCount" = "likedCount" - 1 WHERE "id" = $1"#)
        .bind(page_id)
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

/// `endpoints/pages/my.ts` — the caller's pages, paged on page id.
pub async fn list_my(
    pool: &PgPool,
    me_id: &str,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<PageRow>, sqlx::Error> {
    let sql = format!(
        r#"SELECT {PAGE_COLS} FROM "page"
           WHERE "userId" = $1
             AND ($2::text IS NULL OR "id" > $2)
             AND ($3::text IS NULL OR "id" < $3)
           ORDER BY "id" {}
           LIMIT $4"#,
        order_dir(since_id, until_id)
    );
    sqlx::query_as::<_, PageRow>(&sql)
        .bind(me_id)
        .bind(since_id)
        .bind(until_id)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// `endpoints/pages/liked.ts` — pages the caller liked, paged on like id.
pub async fn list_liked(
    pool: &PgPool,
    me_id: &str,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<PageRow>, sqlx::Error> {
    let sql = format!(
        r#"SELECT {PAGE_COLS_P} FROM "page" p
           INNER JOIN "page_like" l ON l."pageId" = p."id"
           WHERE l."userId" = $1
             AND ($2::text IS NULL OR l."id" > $2)
             AND ($3::text IS NULL OR l."id" < $3)
           ORDER BY l."id" {}
           LIMIT $4"#,
        order_dir(since_id, until_id)
    );
    sqlx::query_as::<_, PageRow>(&sql)
        .bind(me_id)
        .bind(since_id)
        .bind(until_id)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// `endpoints/pages/featured.ts` — likedCount DESC, updatedAt DESC, id DESC.
pub async fn list_featured(
    pool: &PgPool,
    offset: i64,
    limit: i64,
) -> Result<Vec<PageRow>, sqlx::Error> {
    let sql = format!(
        r#"SELECT {PAGE_COLS} FROM "page"
           WHERE "likedCount" > 0
           ORDER BY "likedCount" DESC, "updatedAt" DESC, "id" DESC
           OFFSET $1 LIMIT $2"#
    );
    sqlx::query_as::<_, PageRow>(&sql)
        .bind(offset)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// `endpoints/users/pages.ts` — public pages created by `user_id`, paged on
/// id with the makePaginationQuery direction rule.
pub async fn list_by_user(
    pool: &PgPool,
    user_id: &str,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<PageRow>, sqlx::Error> {
    let sql = format!(
        r#"SELECT {PAGE_COLS} FROM "page"
           WHERE "userId" = $1
             AND "visibility" = 'public'
             AND ($2::text IS NULL OR "id" > $2)
             AND ($3::text IS NULL OR "id" < $3)
           ORDER BY "id" {}
           LIMIT $4"#,
        order_dir(since_id, until_id)
    );
    sqlx::query_as::<_, PageRow>(&sql)
        .bind(user_id)
        .bind(since_id)
        .bind(until_id)
        .bind(limit)
        .fetch_all(pool)
        .await
}
