//! Gallery domain db paths — upstream `endpoints/gallery/*.ts` +
//! `core/entities/GalleryPostEntityService.ts`.
//!
//! Tables ship in the 0001 baseline (`gallery_post`, `gallery_like`); no
//! migration. `gallery_post.updatedAt` comes back as epoch-ms
//! (`(EXTRACT(EPOCH ...) * 1000)`), the flash.rs convention. Aid ids sort
//! monotonically as strings, so sinceId/untilId paging is a plain string
//! compare; the pager bounds are EXCLUSIVE (`makePaginationQuery`),
//! sinceId-only → oldest-first ASC, otherwise newest-first DESC.

use sqlx::PgPool;

/// A `gallery_post` row as `GalleryPostsRepository.findOneBy` sees it.
#[derive(Debug, sqlx::FromRow)]
pub struct GalleryPostRow {
    pub id: String,
    pub updated_at_ms: i64,
    pub title: String,
    pub description: Option<String>,
    pub user_id: String,
    pub file_ids: Vec<String>,
    pub is_sensitive: bool,
    pub liked_count: i32,
    pub tags: Vec<String>,
}

const POST_COLS: &str = r#""id",
    (EXTRACT(EPOCH FROM "updatedAt") * 1000)::bigint AS updated_at_ms,
    "title", "description", "userId" AS user_id, "fileIds" AS file_ids,
    "isSensitive" AS is_sensitive, "likedCount" AS liked_count, "tags""#;

pub async fn find_post(pool: &PgPool, id: &str) -> Result<Option<GalleryPostRow>, sqlx::Error> {
    sqlx::query_as::<_, GalleryPostRow>(&format!(
        r#"SELECT {POST_COLS} FROM "gallery_post" WHERE "id" = $1"#
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// `endpoints/gallery/posts/create.ts` — `galleryPostsRepository.insertOne`.
pub struct NewGalleryPost<'a> {
    pub id: &'a str,
    pub user_id: &'a str,
    pub title: &'a str,
    pub description: Option<&'a str>,
    pub file_ids: &'a [String],
    pub is_sensitive: bool,
}

pub async fn insert_post(pool: &PgPool, n: &NewGalleryPost<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO "gallery_post"
          ("id", "updatedAt", "title", "description", "userId", "fileIds",
           "isSensitive", "likedCount", "tags")
        VALUES ($1, now(), $2, $3, $4, $5, $6, 0, '{}')
        "#,
    )
    .bind(n.id)
    .bind(n.title)
    .bind(n.description)
    .bind(n.user_id)
    .bind(n.file_ids)
    .bind(n.is_sensitive)
    .execute(pool)
    .await?;
    Ok(())
}

/// `galleryPostsRepository.update({ id, userId }, { updatedAt: new Date(), ...
/// })` — `isSensitive` always rides (paramDef default false), the other
/// fields only when provided.
#[derive(Debug, Default)]
pub struct UpdateGalleryPost<'a> {
    pub title: Option<&'a str>,
    /// Outer None = omitted; Some(None) = explicit null.
    pub description: Option<Option<&'a str>>,
    pub file_ids: Option<&'a [String]>,
    pub is_sensitive: bool,
}

pub async fn update_post(
    pool: &PgPool,
    id: &str,
    user_id: &str,
    ps: &UpdateGalleryPost<'_>,
) -> Result<(), sqlx::Error> {
    let mut q = sqlx::QueryBuilder::new(
        r#"UPDATE "gallery_post" SET "updatedAt" = now(), "isSensitive" = "#,
    );
    q.push_bind(ps.is_sensitive);
    if let Some(v) = ps.title {
        q.push(r#", "title" = "#);
        q.push_bind(v);
    }
    if let Some(v) = ps.description {
        q.push(r#", "description" = "#);
        q.push_bind(v);
    }
    if let Some(v) = ps.file_ids {
        q.push(r#", "fileIds" = "#);
        q.push_bind(v.to_vec());
    }
    q.push(r#" WHERE "id" = "#);
    q.push_bind(id);
    q.push(r#" AND "userId" = "#);
    q.push_bind(user_id);
    q.build().execute(pool).await?;
    Ok(())
}

pub async fn delete_post(pool: &PgPool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "gallery_post" WHERE "id" = $1"#)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// likes (gallery_like carries a UNIQUE (userId, postId) btree)
// ---------------------------------------------------------------------------

pub async fn is_liked(pool: &PgPool, post_id: &str, user_id: &str) -> Result<bool, sqlx::Error> {
    let n: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "gallery_like" WHERE "postId" = $1 AND "userId" = $2"#,
    )
    .bind(post_id)
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

/// `true` means the like already existed (the service-level ALREADY_LIKED).
pub async fn insert_like(
    pool: &PgPool,
    id: &str,
    post_id: &str,
    user_id: &str,
) -> Result<bool, sqlx::Error> {
    let res = sqlx::query(
        r#"
        INSERT INTO "gallery_like" ("id", "userId", "postId")
        VALUES ($1, $2, $3)
        ON CONFLICT ("userId", "postId") DO NOTHING
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(post_id)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() == 0)
}

/// `false` means there was no like to delete (the service-level NOT_LIKED).
pub async fn delete_like(pool: &PgPool, post_id: &str, user_id: &str) -> Result<bool, sqlx::Error> {
    let res = sqlx::query(r#"DELETE FROM "gallery_like" WHERE "postId" = $1 AND "userId" = $2"#)
        .bind(post_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}

pub async fn increment_liked_count(pool: &PgPool, post_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "gallery_post" SET "likedCount" = "likedCount" + 1 WHERE "id" = $1"#)
        .bind(post_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn decrement_liked_count(pool: &PgPool, post_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "gallery_post" SET "likedCount" = "likedCount" - 1 WHERE "id" = $1"#)
        .bind(post_id)
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

/// `endpoints/gallery/posts.ts` — the global gallery list, paged on id.
pub async fn list_posts(
    pool: &PgPool,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<GalleryPostRow>, sqlx::Error> {
    let sql = format!(
        r#"SELECT {POST_COLS} FROM "gallery_post"
           WHERE ($1::text IS NULL OR "id" > $1)
             AND ($2::text IS NULL OR "id" < $2)
           ORDER BY "id" {}
           LIMIT $3"#,
        order_dir(since_id, until_id)
    );
    sqlx::query_as::<_, GalleryPostRow>(&sql)
        .bind(since_id)
        .bind(until_id)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// `endpoints/gallery/popular.ts` — likedCount > 0 DESC, fixed limit 10.
pub async fn list_popular(pool: &PgPool) -> Result<Vec<GalleryPostRow>, sqlx::Error> {
    let sql = format!(
        r#"SELECT {POST_COLS} FROM "gallery_post"
           WHERE "likedCount" > 0
           ORDER BY "likedCount" DESC, "id" DESC
           LIMIT 10"#
    );
    sqlx::query_as::<_, GalleryPostRow>(&sql)
        .fetch_all(pool)
        .await
}

/// `endpoints/gallery/featured.ts` — FeaturedService ranking without the
/// Redis zset: the top-100 liked posts inside the current+previous 3-day
/// window (`floor_id` = gen(now - 6d)), then the newest ids first (upstream
/// sorts the ranking ids descending before slicing), untilId-exclusive.
pub async fn list_featured(
    pool: &PgPool,
    floor_id: &str,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<GalleryPostRow>, sqlx::Error> {
    let sql = format!(
        r#"SELECT {POST_COLS} FROM "gallery_post"
           WHERE "id" IN (
               SELECT "id" FROM "gallery_post"
               WHERE "likedCount" > 0 AND "id" > $1
                 AND ($2::text IS NULL OR "id" < $2)
               ORDER BY "likedCount" DESC
               LIMIT 100
           )
           ORDER BY "id" DESC
           LIMIT $3"#
    );
    sqlx::query_as::<_, GalleryPostRow>(&sql)
        .bind(floor_id)
        .bind(until_id)
        .bind(limit)
        .fetch_all(pool)
        .await
}
