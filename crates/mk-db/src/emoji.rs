//! `emoji` table paths for the `admin/emoji/*` family (ports of
//! `endpoints/admin/emoji/{add,delete,list,update,copy,set-category-bulk,
//! set-license-bulk,add-aliases-bulk,remove-aliases-bulk,set-aliases-bulk,
//! delete-bulk,list-remote}.ts`). The table ships in the 0001 baseline; no
//! migration.
//!
//! Timestamps bridge timestamptz → epoch-ms like the rest of mk-db (the
//! workspace sqlx has no chrono feature); the pack layer never needs
//! `updatedAt`, but writes stamp it (`CustomEmojiService.add/update`).
//!
//! Pagination mirrors `QueryService.makePaginationQuery`: ids are aid
//! (chronological), bounds are exclusive, a lone `since` flips the scan to
//! ASC and every other shape is DESC. sinceDate/untilDate are normalized to
//! id boundaries at the api layer (mk-db does not depend on mk-types).
//!
//! `import-zip` is intentionally not ported: it needs the drive/zip storage
//! lane (ExtractImagesService) which this server does not carry yet. The
//! remote ingestion lane is also absent, so `list-remote` reads whatever
//! `emoji` rows carry a host (normally none) and is otherwise a pure query.

use sqlx::PgPool;

/// Columns consumed by the api pack (`EmojiEntityService.packDetailed`) plus
/// the copy source fields.
#[derive(Debug, Default, sqlx::FromRow)]
pub struct EmojiRow {
    pub id: String,
    pub name: String,
    pub host: Option<String>,
    #[sqlx(rename = "originalUrl")]
    pub original_url: String,
    pub uri: Option<String>,
    #[sqlx(rename = "type")]
    pub file_type: Option<String>,
    pub aliases: Vec<String>,
    pub category: Option<String>,
    #[sqlx(rename = "publicUrl")]
    pub public_url: String,
    pub license: Option<String>,
    #[sqlx(rename = "localOnly")]
    pub local_only: bool,
    #[sqlx(rename = "isSensitive")]
    pub is_sensitive: bool,
    #[sqlx(rename = "roleIdsThatCanBeUsedThisEmojiAsReaction")]
    pub role_ids: Vec<String>,
}

const EMOJI_COLS: &str = r#""id", "name", "host", "originalUrl", "uri", ("type"::text) AS "type",
    "aliases", "category", "publicUrl", "license", "localOnly", "isSensitive",
    "roleIdsThatCanBeUsedThisEmojiAsReaction""#;

/// `CustomEmojiService.add` insert (`host` is NULL for local emoji).
pub async fn insert(pool: &PgPool, row: &EmojiRow) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO "emoji"
            ("id", "updatedAt", "name", "host", "originalUrl", "uri", "type",
             "aliases", "category", "publicUrl", "license", "localOnly",
             "isSensitive", "roleIdsThatCanBeUsedThisEmojiAsReaction")
        VALUES ($1, now(), $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
        "#,
    )
    .bind(&row.id)
    .bind(&row.name)
    .bind(&row.host)
    .bind(&row.original_url)
    .bind(&row.uri)
    .bind(&row.file_type)
    .bind(&row.aliases)
    .bind(&row.category)
    .bind(&row.public_url)
    .bind(&row.license)
    .bind(row.local_only)
    .bind(row.is_sensitive)
    .bind(&row.role_ids)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn find_by_id(pool: &PgPool, id: &str) -> Result<Option<EmojiRow>, sqlx::Error> {
    sqlx::query_as::<_, EmojiRow>(&format!(
        r#"SELECT {EMOJI_COLS} FROM "emoji" WHERE "id" = $1"#
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// `CustomEmojiService.getEmojiByName` — local rows only (`host IS NULL`).
pub async fn find_local_by_name(
    pool: &PgPool,
    name: &str,
) -> Result<Option<EmojiRow>, sqlx::Error> {
    sqlx::query_as::<_, EmojiRow>(&format!(
        r#"SELECT {EMOJI_COLS} FROM "emoji" WHERE "name" = $1 AND "host" IS NULL LIMIT 1"#
    ))
    .bind(name)
    .fetch_optional(pool)
    .await
}

/// `CustomEmojiService.checkDuplicate` — `exists({ name, host: IsNull() })`.
pub async fn local_name_exists(pool: &PgPool, name: &str) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(SELECT 1 FROM "emoji" WHERE "name" = $1 AND "host" IS NULL)"#,
    )
    .bind(name)
    .fetch_one(pool)
    .await
}

pub async fn find_by_ids(pool: &PgPool, ids: &[String]) -> Result<Vec<EmojiRow>, sqlx::Error> {
    sqlx::query_as::<_, EmojiRow>(&format!(
        r#"SELECT {EMOJI_COLS} FROM "emoji" WHERE "id" = ANY($1)"#
    ))
    .bind(ids)
    .fetch_all(pool)
    .await
}

pub async fn delete(pool: &PgPool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "emoji" WHERE "id" = $1"#)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_bulk(pool: &PgPool, ids: &[String]) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "emoji" WHERE "id" = ANY($1)"#)
        .bind(ids)
        .execute(pool)
        .await?;
    Ok(())
}

/// Partial `emojisRepository.update` patch. `None` skips the column
/// (TypeORM `undefined`); `Some(None)` writes NULL (the `category`/
/// `license` reset arms).
#[derive(Debug, Default)]
pub struct EmojiPatch {
    pub name: Option<String>,
    pub category: Option<Option<String>>,
    pub aliases: Option<Vec<String>>,
    pub license: Option<Option<String>>,
    pub is_sensitive: Option<bool>,
    pub local_only: Option<bool>,
    pub role_ids: Option<Vec<String>>,
    pub original_url: Option<String>,
    pub public_url: Option<String>,
    pub file_type: Option<String>,
}

pub async fn update(pool: &PgPool, id: &str, patch: &EmojiPatch) -> Result<(), sqlx::Error> {
    let mut qb = sqlx::QueryBuilder::new(r#"UPDATE "emoji" SET "updatedAt" = now()"#);
    if let Some(v) = &patch.name {
        qb.push(r#", "name" = "#).push_bind(v);
    }
    if let Some(v) = &patch.category {
        qb.push(r#", "category" = "#).push_bind(v.as_deref());
    }
    if let Some(v) = &patch.aliases {
        qb.push(r#", "aliases" = "#).push_bind(v);
    }
    if let Some(v) = &patch.license {
        qb.push(r#", "license" = "#).push_bind(v.as_deref());
    }
    if let Some(v) = patch.is_sensitive {
        qb.push(r#", "isSensitive" = "#).push_bind(v);
    }
    if let Some(v) = patch.local_only {
        qb.push(r#", "localOnly" = "#).push_bind(v);
    }
    if let Some(v) = &patch.role_ids {
        qb.push(r#", "roleIdsThatCanBeUsedThisEmojiAsReaction" = "#)
            .push_bind(v);
    }
    if let Some(v) = &patch.original_url {
        qb.push(r#", "originalUrl" = "#).push_bind(v);
    }
    if let Some(v) = &patch.public_url {
        qb.push(r#", "publicUrl" = "#).push_bind(v);
    }
    if let Some(v) = &patch.file_type {
        qb.push(r#", "type" = "#).push_bind(v);
    }
    qb.push(r#" WHERE "id" = "#).push_bind(id);
    qb.build().execute(pool).await?;
    Ok(())
}

/// `endpoints/admin/emoji/list.ts`: `host IS NULL` + makePaginationQuery.
///
/// With a query the upstream fetches every page-matched row (`q.getMany()`,
/// no LIMIT) and filters in memory — `:name:` token matches when the query
/// carries colon tokens, otherwise name/alias/category substring — then
/// `splice(limit + 1)` (an upstream off-by-one that keeps up to limit+1
/// rows). Mirrored exactly.
pub async fn list_local(
    pool: &PgPool,
    query: Option<&str>,
    limit: i64,
    since_id: Option<&str>,
    until_id: Option<&str>,
    asc: bool,
) -> Result<Vec<EmojiRow>, sqlx::Error> {
    let dir = if asc { "ASC" } else { "DESC" };
    let limit_clause = if query.is_some() {
        String::new()
    } else {
        "LIMIT $3".to_string()
    };
    let sql = format!(
        r#"SELECT {EMOJI_COLS} FROM "emoji"
           WHERE "host" IS NULL
             AND ($1::varchar IS NULL OR "id" < $1::varchar)
             AND ($2::varchar IS NULL OR "id" > $2::varchar)
           ORDER BY "id" {dir} {limit_clause}"#
    );
    let mut q = sqlx::query_as::<_, EmojiRow>(&sql)
        .bind(until_id)
        .bind(since_id);
    if query.is_none() {
        q = q.bind(limit);
    }
    let mut rows = q.fetch_all(pool).await?;
    if let Some(query) = query {
        let tokens = colon_tokens(query);
        rows.retain(|e| {
            if !tokens.is_empty() {
                tokens.iter().any(|t| *t == format!(":{}:", e.name))
            } else {
                e.name.contains(query)
                    || e.aliases.iter().any(|a| a.contains(query))
                    || e.category.as_deref().is_some_and(|c| c.contains(query))
            }
        });
        rows.truncate((limit + 1) as usize);
    }
    Ok(rows)
}

/// `endpoints/admin/emoji/list-remote.ts`: `host IS NOT NULL` (or the exact
/// host when given), `name LIKE %query%` (escaped), always id DESC (the
/// upstream `.orderBy('emoji.id','DESC')` overrides the pagination order).
pub async fn list_remote(
    pool: &PgPool,
    query: Option<&str>,
    host: Option<&str>,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<EmojiRow>, sqlx::Error> {
    let mut qb = sqlx::QueryBuilder::new(format!(
        r#"SELECT {EMOJI_COLS} FROM "emoji" WHERE "host" IS NOT NULL"#
    ));
    if let Some(host) = host {
        qb.push(r#" AND "host" = "#).push_bind(host.to_string());
    }
    if let Some(query) = query {
        qb.push(r#" AND "name" LIKE "#)
            .push_bind(format!("%{}%", sql_like_escape(query)));
    }
    if let Some(until_id) = until_id {
        qb.push(r#" AND "id" < "#).push_bind(until_id.to_string());
    }
    if let Some(since_id) = since_id {
        qb.push(r#" AND "id" > "#).push_bind(since_id.to_string());
    }
    qb.push(r#" ORDER BY "id" DESC LIMIT "#).push_bind(limit);
    qb.build_query_as::<EmojiRow>().fetch_all(pool).await
}

/// `setCategoryBulk`: `update({ id: In(ids) }, { updatedAt, category })`.
pub async fn set_category_bulk(
    pool: &PgPool,
    ids: &[String],
    category: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "emoji" SET "updatedAt" = now(), "category" = $1 WHERE "id" = ANY($2)"#)
        .bind(category)
        .bind(ids)
        .execute(pool)
        .await?;
    Ok(())
}

/// `setLicenseBulk`.
pub async fn set_license_bulk(
    pool: &PgPool,
    ids: &[String],
    license: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "emoji" SET "updatedAt" = now(), "license" = $1 WHERE "id" = ANY($2)"#)
        .bind(license)
        .bind(ids)
        .execute(pool)
        .await?;
    Ok(())
}

/// `setAliasesBulk`.
pub async fn set_aliases_bulk(
    pool: &PgPool,
    ids: &[String],
    aliases: &[String],
) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "emoji" SET "updatedAt" = now(), "aliases" = $1 WHERE "id" = ANY($2)"#)
        .bind(aliases)
        .bind(ids)
        .execute(pool)
        .await?;
    Ok(())
}

/// `addAliasesBulk`: per row `[...new Set(existing.concat(aliases))]` (order
/// preserving, exactly like the upstream JS).
pub async fn add_aliases_bulk(
    pool: &PgPool,
    ids: &[String],
    aliases: &[String],
) -> Result<(), sqlx::Error> {
    let rows = find_by_ids(pool, ids).await?;
    for row in rows {
        let mut merged = row.aliases.clone();
        for alias in aliases {
            if !merged.contains(alias) {
                merged.push(alias.clone());
            }
        }
        sqlx::query(r#"UPDATE "emoji" SET "updatedAt" = now(), "aliases" = $1 WHERE "id" = $2"#)
            .bind(&merged)
            .bind(&row.id)
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// `removeAliasesBulk`: `aliases.filter(x => !aliases.includes(x))`.
pub async fn remove_aliases_bulk(
    pool: &PgPool,
    ids: &[String],
    aliases: &[String],
) -> Result<(), sqlx::Error> {
    let rows = find_by_ids(pool, ids).await?;
    for row in rows {
        let kept: Vec<String> = row
            .aliases
            .iter()
            .filter(|a| !aliases.contains(a))
            .cloned()
            .collect();
        sqlx::query(r#"UPDATE "emoji" SET "updatedAt" = now(), "aliases" = $1 WHERE "id" = $2"#)
            .bind(&kept)
            .bind(&row.id)
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// `/:([a-z0-9_]*):/g` matches as substrings (the admin/emoji/list.ts query
/// token mode).
fn colon_tokens(query: &str) -> Vec<String> {
    let bytes = query.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' {
            let mut j = i + 1;
            while j < bytes.len()
                && (bytes[j].is_ascii_lowercase() || bytes[j].is_ascii_digit() || bytes[j] == b'_')
            {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b':' {
                out.push(query[i..=j].to_string());
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// `misc/sql-like-escape.ts` (Postgres backslash escape).
fn sql_like_escape(q: &str) -> String {
    q.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

// ==== v2/admin/emoji/list (upstream endpoints/v2/admin/emoji/list.ts) ====

/// Query filter subset of CustomEmojiService.fetchEmojis (the fields the v2
/// list endpoint forwards).
#[derive(Debug, Default)]
pub struct AdminEmojiFilter<'a> {
    pub name: Option<&'a str>,
    pub host: Option<&'a str>,
    pub uri: Option<&'a str>,
    pub type_: Option<&'a str>,
    pub aliases: Option<&'a str>,
    pub category: Option<&'a str>,
    pub license: Option<&'a str>,
    pub is_sensitive: Option<bool>,
    pub local_only: Option<bool>,
    pub host_type: Option<&'a str>,
}

/// One page plus the filtered total (upstream `{emojis, count, allCount}`).
pub async fn list_admin(
    pool: &PgPool,
    f: &AdminEmojiFilter<'_>,
    limit: i64,
    offset: i64,
    sort_col: &str,
    asc: bool,
) -> Result<(Vec<EmojiRow>, i64), sqlx::Error> {
    // sort column comes from a closed whitelist in the api layer
    let dir = if asc { "ASC" } else { "DESC" };
    let where_clause = r#"
        WHERE ($1::text IS NULL OR "name" ILIKE '%' || $1 || '%')
          AND ($2::text IS NULL OR "host" = $2)
          AND ($3::text IS NULL OR "uri" ILIKE '%' || $3 || '%')
          AND ($4::text IS NULL OR "type" = $4)
          AND ($5::text IS NULL OR array_to_string("aliases", ',') ILIKE '%' || $5 || '%')
          AND ($6::text IS NULL OR "category" = $6)
          AND ($7::text IS NULL OR "license" = $7)
          AND ($8::bool IS NULL OR "isSensitive" = $8)
          AND ($9::bool IS NULL OR "localOnly" = $9)
          AND ($10::text IS NULL OR $10 = 'all'
               OR ($10 = 'local' AND "host" IS NULL)
               OR ($10 = 'remote' AND "host" IS NOT NULL))"#;
    let sql = format!(
        r#"SELECT {EMOJI_COLS} FROM "emoji"{where_clause}
           ORDER BY "{sort_col}" {dir} LIMIT $11 OFFSET $12"#
    );
    let mut q = sqlx::query_as::<_, EmojiRow>(&sql)
        .bind(f.name)
        .bind(f.host)
        .bind(f.uri)
        .bind(f.type_)
        .bind(f.aliases)
        .bind(f.category)
        .bind(f.license)
        .bind(f.is_sensitive)
        .bind(f.local_only)
        .bind(f.host_type)
        .bind(limit)
        .bind(offset);
    let rows = q.fetch_all(pool).await?;
    let count_sql = format!(r#"SELECT count(*) FROM "emoji"{where_clause}"#);
    let total: i64 = sqlx::query_scalar(&count_sql)
        .bind(f.name)
        .bind(f.host)
        .bind(f.uri)
        .bind(f.type_)
        .bind(f.aliases)
        .bind(f.category)
        .bind(f.license)
        .bind(f.is_sensitive)
        .bind(f.local_only)
        .bind(f.host_type)
        .fetch_one(pool)
        .await?;
    Ok((rows, total))
}
