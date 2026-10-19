//! arena-exports: the export-job db plane (upstream
//! `queue/processors/ExportClipsProcessorService.ts` +
//! `endpoints/drive/{files.ts,files/show.ts,files/delete.ts}`).
//!
//! - `mk_export_blobs` (0006): the internal-storage byte lane the
//!   `/files/{accessKey}/{name}` route serves — `drive_file` is metadata-only
//!   in this rewrite (the upload path persists no bytes, TODO(M4)); the
//!   export processor needs its JSON fetchable, so the bytes ride the same
//!   `accessKey` the pack url embeds.
//! - `export_clips_asc` + `export_clip_notes`: the processor's single-shot
//!   equivalents of the upstream per-100 paging queries (the e2e corpora are
//!   single-order-of-100; the clip/clipNote ORDER BYs + the visibility plane
//!   the api layer filters on are what the harness observes).
//! - `drive_files_page` / `find_file_by_any_url` / `delete_drive_file` /
//!   `drive_file_access_key`: the drive/files list + show + delete supports.
//!   Kept OUT of the drive / drive_note agents' files — read-only reuse of
//!   their shared `DriveFileRow` projection.

use sqlx::PgPool;

use crate::drive::DriveFileRow;

// ==== mk_export_blobs (0006) ================================================

/// Store the export bytes (upstream: the temp file DriveService.addFile
/// uploads). Same accessKey the drive_file row + pack url carry.
///
/// arena-exports: the table is (re)ensured first — the e2e harness's TypeORM
/// `initTestDb` (dropSchema: true, setup.e2e.ts beforeAll) rebuilds the
/// schema after this server's migrations and wipes non-entity tables.
/// `IF NOT EXISTS` keeps it idempotent (0006 remains the canonical DDL).
pub async fn put_blob(pool: &PgPool, access_key: &str, content: &[u8]) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS "mk_export_blobs" (
               "access_key" character varying(256) NOT NULL,
               "content" bytea NOT NULL,
               "created_at" timestamp with time zone DEFAULT now() NOT NULL,
               CONSTRAINT mk_export_blobs_pkey PRIMARY KEY (access_key)
           )"#,
    )
    .execute(pool)
    .await?;
    sqlx::query(r#"INSERT INTO "mk_export_blobs" ("access_key", "content") VALUES ($1, $2)"#)
        .bind(access_key)
        .bind(content)
        .execute(pool)
        .await?;
    Ok(())
}

/// `/files/{key}/{name}` route read.
pub async fn get_blob(pool: &PgPool, access_key: &str) -> Result<Option<Vec<u8>>, sqlx::Error> {
    sqlx::query_scalar(r#"SELECT "content" FROM "mk_export_blobs" WHERE "access_key" = $1"#)
        .bind(access_key)
        .fetch_optional(pool)
        .await
}

/// `DriveService.deleteFile` storage half (files/delete).
pub async fn delete_blob(pool: &PgPool, access_key: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "mk_export_blobs" WHERE "access_key" = $1"#)
        .bind(access_key)
        .execute(pool)
        .await?;
    Ok(())
}

/// The drive_file row's `accessKey` (the blob key + url segment) — the
/// delete path needs it to drop the bytes with the row.
pub async fn drive_file_access_key(
    pool: &PgPool,
    file_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(r#"SELECT "accessKey" FROM "drive_file" WHERE "id" = $1"#)
        .bind(file_id)
        .fetch_optional(pool)
        .await
}

/// blob lookup + the MIME the drive row pinned (the route's content-type);
/// LEFT JOIN so a blob whose row is gone still serves (upstream's file
/// server streams whatever the storage has, metadata is the api's concern).
pub async fn blob_and_type(
    pool: &PgPool,
    access_key: &str,
) -> Result<Option<(Vec<u8>, String)>, sqlx::Error> {
    let row: Option<(Vec<u8>, Option<String>)> = sqlx::query_as(
        r#"SELECT b."content", f."type"
           FROM "mk_export_blobs" b
           LEFT JOIN "drive_file" f ON f."accessKey" = b."access_key"
           WHERE b."access_key" = $1"#,
    )
    .bind(access_key)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(content, ty)| {
        (
            content,
            ty.unwrap_or_else(|| "application/octet-stream".into()),
        )
    }))
}

// ==== export-processor reads (ExportClipsProcessorService) ==================

/// The clip projection serializeClip emits (id/name/description/
/// lastClippedAt), all of the exporter's clips, `clip.id` ASC (the upstream
/// export loop's order).
#[derive(Debug, sqlx::FromRow)]
pub struct ExportClipRow {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    #[sqlx(rename = "last_clipped_at_ms")]
    pub last_clipped_at_ms: Option<i64>,
}

pub async fn export_clips_asc(
    pool: &PgPool,
    user_id: &str,
) -> Result<Vec<ExportClipRow>, sqlx::Error> {
    sqlx::query_as::<_, ExportClipRow>(
        r#"SELECT "id", "name", "description",
                  CASE WHEN "lastClippedAt" IS NULL THEN NULL
                       ELSE (EXTRACT(EPOCH FROM "lastClippedAt") * 1000)::bigint
                  END AS "last_clipped_at_ms"
           FROM "clip" WHERE "userId" = $1 ORDER BY "id" ASC"#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// One clip_note ⨝ note ⨝ (note author) row — everything
/// `serializeClipNote` emits plus the visibility-plane inputs (author
/// `makeNotesHiddenBefore` for shouldHideNoteByTime, and the id arrays the
/// mentioned/specified arms probe).
#[derive(Debug, sqlx::FromRow)]
pub struct ExportClipNoteRow {
    #[sqlx(rename = "clip_note_id")]
    pub clip_note_id: String,
    #[sqlx(rename = "note_id")]
    pub note_id: String,
    pub text: Option<String>,
    pub cw: Option<String>,
    pub visibility: String,
    #[sqlx(rename = "file_ids")]
    pub file_ids: Vec<String>,
    #[sqlx(rename = "reply_id")]
    pub reply_id: Option<String>,
    #[sqlx(rename = "renote_id")]
    pub renote_id: Option<String>,
    #[sqlx(rename = "reply_user_id")]
    pub reply_user_id: Option<String>,
    #[sqlx(rename = "visible_user_ids")]
    pub visible_user_ids: Vec<String>,
    pub mentions: Vec<String>,
    #[sqlx(rename = "has_poll")]
    pub has_poll: bool,
    #[sqlx(rename = "local_only")]
    pub local_only: bool,
    #[sqlx(rename = "reaction_acceptance")]
    pub reaction_acceptance: Option<String>,
    #[sqlx(rename = "note_uri")]
    pub note_uri: Option<String>,
    #[sqlx(rename = "note_url")]
    pub note_url: Option<String>,
    #[sqlx(rename = "author_id")]
    pub author_id: String,
    pub username: String,
    #[sqlx(rename = "author_name")]
    pub author_name: Option<String>,
    pub host: Option<String>,
    #[sqlx(rename = "author_uri")]
    pub author_uri: Option<String>,
    #[sqlx(rename = "make_notes_hidden_before")]
    pub make_notes_hidden_before: Option<i32>,
}

/// All clip Notes of one clip, `clipNote.id` ASC (the upstream cursor loop).
pub async fn export_clip_notes(
    pool: &PgPool,
    clip_id: &str,
) -> Result<Vec<ExportClipNoteRow>, sqlx::Error> {
    sqlx::query_as::<_, ExportClipNoteRow>(
        r#"SELECT cn."id" AS "clip_note_id",
                  n."id" AS "note_id", n."text", n."cw",
                  ("n"."visibility"::text) AS "visibility",
                  n."fileIds" AS "file_ids", n."replyId" AS "reply_id",
                  n."renoteId" AS "renote_id", n."replyUserId" AS "reply_user_id",
                  n."visibleUserIds" AS "visible_user_ids", n."mentions",
                  n."hasPoll" AS "has_poll", n."localOnly" AS "local_only",
                  n."reactionAcceptance" AS "reaction_acceptance",
                  n."uri" AS "note_uri", n."url" AS "note_url",
                  u."id" AS "author_id", u."username", u."name" AS "author_name",
                  u."host", u."uri" AS "author_uri",
                  u."makeNotesHiddenBefore" AS "make_notes_hidden_before"
           FROM "clip_note" cn
           INNER JOIN "note" n ON n."id" = cn."noteId"
           INNER JOIN "user" u ON u."id" = n."userId"
           WHERE cn."clipId" = $1
           ORDER BY cn."id" ASC"#,
    )
    .bind(clip_id)
    .fetch_all(pool)
    .await
}

// ==== drive/files {list,show,delete} reads ==================================

/// The `mk_db::drive::find_drive_file` projection — files/{list,show} return
/// exactly the shape the pack consumes.
const FILE_COLS: &str = r#""id", "userId" AS "user_id", "md5", "name", ("type"::text) AS "type", "size",
                  "blurhash", "url", "webpublicUrl" AS "webpublic_url",
                  "webpublicType" AS "webpublic_type",
                  ("properties"::text)::jsonb AS "properties",
                  "comment", "isSensitive" AS "is_sensitive", "folderId" AS "folder_id""#;

/// `drive/files`: `makePaginationQuery(...).andWhere('file.userId = :me')` +
/// folderId gate (paramDef default null → `folderId IS NULL`) + the type
/// prefix/eq filter + the sort switch. Binds are numbered explicitly in
/// field order ($1 owner, $2 untilId, $3 sinceId, then the optional gates
/// only when present — no parameter-number gaps).
pub async fn drive_files_page(
    pool: &PgPool,
    user_id: &str,
    folder_id: Option<&str>,
    file_type: Option<&str>,
    since_id: Option<&str>,
    until_id: Option<&str>,
    sort: Option<&str>,
    limit: i64,
) -> Result<Vec<DriveFileRow>, sqlx::Error> {
    let mut n = 3usize; // $1 owner, $2 untilId, $3 sinceId
    let mut binds: Vec<String> = Vec::new();

    let folder_gate = match folder_id {
        Some(f) => {
            n += 1;
            binds.push(f.to_string());
            format!(r#"AND ("folderId" = ${n})"#)
        }
        None => r#"AND ("folderId" IS NULL)"#.to_string(),
    };
    let type_gate = match file_type {
        Some(t) if t.ends_with("/*") => {
            n += 1;
            binds.push(format!("{}%", t.trim_end_matches('*')));
            format!(r#"AND ("type" LIKE ${n})"#)
        }
        Some(t) => {
            n += 1;
            binds.push(t.to_string());
            format!(r#"AND ("type" = ${n})"#)
        }
        None => String::new(),
    };
    // makePaginationQuery ASC only for the sinceId-only shape; the sort
    // switch REPLACES the id orderBy entirely (TypeORM orderBy overwrite).
    let dir = if since_id.is_some() && until_id.is_none() {
        "ASC"
    } else {
        "DESC"
    };
    let order_by = match sort {
        None => format!(r#"ORDER BY "id" {dir}"#),
        Some("+createdAt") => r#"ORDER BY "id" DESC"#.to_string(),
        Some("-createdAt") => r#"ORDER BY "id" ASC"#.to_string(),
        Some("+name") => r#"ORDER BY "name" DESC"#.to_string(),
        Some("-name") => r#"ORDER BY "name" ASC"#.to_string(),
        Some("+size") => r#"ORDER BY "size" DESC"#.to_string(),
        Some("-size") => r#"ORDER BY "size" ASC"#.to_string(),
        _ => format!(r#"ORDER BY "id" {dir}"#),
    };
    let sql = format!(
        r#"SELECT {FILE_COLS} FROM "drive_file"
           WHERE "userId" = $1
             AND ($2::varchar IS NULL OR "id" < $2::varchar)
             AND ($3::varchar IS NULL OR "id" > $3::varchar)
             {folder_gate}
             {type_gate}
             {order_by}
           LIMIT {limit}"#
    );
    let mut q = sqlx::query_as::<_, DriveFileRow>(&sql)
        .bind(user_id)
        .bind(until_id)
        .bind(since_id);
    for b in binds {
        q = q.bind(b);
    }
    q.fetch_all(pool).await
}

/// `drive/files/show` url branch: `findOneBy([{url}, {webpublicUrl},
/// {thumbnailUrl}])` — the object-array findOneBy picks the first matching
/// condition in order, so url outranks webpublicUrl.
pub async fn find_file_by_any_url(
    pool: &PgPool,
    url: &str,
) -> Result<Option<DriveFileRow>, sqlx::Error> {
    sqlx::query_as::<_, DriveFileRow>(&format!(
        r#"SELECT {FILE_COLS} FROM "drive_file"
           WHERE "url" = $1 OR "webpublicUrl" = $1 OR "thumbnailUrl" = $1
           ORDER BY CASE WHEN "url" = $1 THEN 0 ELSE 1 END
           LIMIT 1"#
    ))
    .bind(url)
    .fetch_optional(pool)
    .await
}

/// `drive/files/delete` data half: drop the metadata row (the caller deletes
/// the blob through drive_file_access_key first).
pub async fn delete_drive_file(pool: &PgPool, file_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "drive_file" WHERE "id" = $1"#)
        .bind(file_id)
        .execute(pool)
        .await?;
    Ok(())
}
