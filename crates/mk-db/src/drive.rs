//! Drive SQL (drive.ts arena slice): drive_file rows (the `drive/files/create`
//! upload path + `uploadFromUrl`), the `drive/files/attached-notes` reverse
//! lookup and the notes/create fileIds persistence UPDATE.
//!
//! Upstream logic: `DriveService.addFile` / `downloadUrl` (name from the URL
//! basename, type sniffed from bytes), `endpoints/drive/files/attached-notes.ts`
//! (`:file <@ note.fileIds` array containment + `makePaginationQuery` aid
//! paging) and `NoteCreateService` (owned fileIds fetched via
//! `file.userId = :userId AND file.id IN (:...fileIds)` ordered by
//! array_position, stored on the note row). NOTE: the note-table WRITE lives
//! here (not note.rs) so the note-agent's file stays untouched — read-only
//! note queries remain theirs.

use sqlx::PgPool;

// ==== endpoints-arena: drive_folder rows (upstream endpoints/drive/folders/*
// + DriveFolderEntityService) — the table ships in the 0001 baseline. ========

/// Columns the api layer packs (id supplies the aid createdAt).
#[derive(Debug, sqlx::FromRow)]
pub struct DriveFolderRow {
    pub id: String,
    #[sqlx(rename = "userId")]
    pub user_id: String,
    pub name: String,
    #[sqlx(rename = "parentId")]
    pub parent_id: Option<String>,
}

pub async fn insert_folder(
    pool: &PgPool,
    id: &str,
    user_id: &str,
    name: &str,
    parent_id: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO "drive_folder" ("id", "userId", "name", "parentId") VALUES ($1, $2, $3, $4)"#,
    )
    .bind(id)
    .bind(user_id)
    .bind(name)
    .bind(parent_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Owner-scoped lookup (folders/{create,update}.ts findOneBy({id, userId})).
pub async fn find_folder_owned(
    pool: &PgPool,
    id: &str,
    user_id: &str,
) -> Result<Option<DriveFolderRow>, sqlx::Error> {
    sqlx::query_as::<_, DriveFolderRow>(
        r#"SELECT "id", "userId", "name", "parentId" FROM "drive_folder"
           WHERE "id" = $1 AND "userId" = $2"#,
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

/// UNscoped lookup — the folders/update.ts checkCircle walk uses
/// findOneByOrFail without the owner filter.
pub async fn find_folder_by_id(
    pool: &PgPool,
    id: &str,
) -> Result<Option<DriveFolderRow>, sqlx::Error> {
    sqlx::query_as::<_, DriveFolderRow>(
        r#"SELECT "id", "userId", "name", "parentId" FROM "drive_folder" WHERE "id" = $1"#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub async fn update_folder(
    pool: &PgPool,
    id: &str,
    name: Option<&str>,
    parent_id: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"UPDATE "drive_folder" SET
            "name" = COALESCE($2, "name"),
            "parentId" = $3
        WHERE "id" = $1"#,
    )
    .bind(id)
    .bind(name)
    .bind(parent_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Columns consumed by the api layer (drive pack + avatar/banner swap).
#[derive(Debug, Default, sqlx::FromRow)]
pub struct DriveFileRow {
    pub id: String,
    #[sqlx(rename = "userId")]
    pub user_id: Option<String>,
    /// MD5 hex of the stored bytes (varchar(32)).
    pub md5: String,
    pub name: String,
    /// MIME type (`drive_file.type`).
    #[sqlx(rename = "type")]
    pub file_type: String,
    pub size: i32,
    /// Bars/blur data placeholder (`drive_file.blurhash`); real blurhash
    /// computation needs image decoding — TODO(M4).
    pub blurhash: Option<String>,
    pub url: String,
    #[sqlx(rename = "webpublicUrl")]
    pub webpublic_url: Option<String>,
    pub properties: serde_json::Value,
    /// upload-from-url / files/create `comment` param (max 512).
    pub comment: Option<String>,
    #[sqlx(rename = "isSensitive")]
    pub is_sensitive: bool,
    #[sqlx(rename = "folderId")]
    pub folder_id: Option<String>,
}

/// Insert one upload row. Mirrors `addFile`'s essential columns; bytes are
/// not persisted (`storedInternal = false`) because no `/files` route exists
/// in this server yet — TODO(M4) internal storage.
pub async fn insert_drive_file(
    pool: &PgPool,
    row: &DriveFileRow,
    access_key: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO "drive_file"
            ("id", "userId", "userHost", "md5", "name", "type", "size", "comment",
             "properties", "storedInternal", "url", "thumbnailUrl", "webpublicUrl",
             "accessKey", "isSensitive", "isLink", "blurhash", "maybeSensitive", "maybePorn",
             "folderId")
        VALUES ($1, $2, NULL, $3, $4, $5, $6, $7, $8::jsonb, FALSE, $9, NULL, NULL,
                $10, $11, FALSE, $12, FALSE, FALSE, $13)
        "#,
    )
    .bind(&row.id)
    .bind(&row.user_id)
    .bind(&row.md5)
    .bind(&row.name)
    .bind(&row.file_type)
    .bind(row.size)
    .bind(&row.comment)
    .bind(serde_json::to_string(&row.properties).unwrap_or_else(|_| "{}".to_string()))
    .bind(&row.url)
    .bind(access_key)
    .bind(row.is_sensitive)
    .bind(&row.blurhash)
    .bind(&row.folder_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn find_drive_file(pool: &PgPool, id: &str) -> Result<Option<DriveFileRow>, sqlx::Error> {
    sqlx::query_as::<_, DriveFileRow>(
        r#"SELECT "id", "userId", "md5", "name", ("type"::text) AS "type", "size",
                  "blurhash", "url", "webpublicUrl", ("properties"::text)::jsonb AS "properties",
                  "comment", "isSensitive", "folderId"
           FROM "drive_file" WHERE "id" = $1"#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// `NoteCreateService.create`: the note's files = drive files owned by the
/// author with the requested ids, ordered by array_position so the pack's
/// fileIds/files arrays match the request order (upstream L305-315).
pub async fn files_owned_ordered(
    pool: &PgPool,
    user_id: &str,
    file_ids: &[String],
) -> Result<Vec<DriveFileRow>, sqlx::Error> {
    sqlx::query_as::<_, DriveFileRow>(
        r#"SELECT f."id", f."userId", f."md5", f."name", ("f"."type"::text) AS "type", f."size",
                  f."blurhash", f."url", f."webpublicUrl", ("f"."properties"::text)::jsonb AS "properties",
                  f."comment", f."isSensitive", f."folderId"
           FROM "drive_file" f
           WHERE f."userId" = $1 AND f."id" = ANY($2::varchar[])
           ORDER BY array_position($2::varchar[], f."id")::integer"#,
    )
    .bind(user_id)
    .bind(file_ids)
    .fetch_all(pool)
    .await
}

/// Notes-creation file attachment (drive-graph slice). The note-agent owns
/// mk-db/src/note.rs, whose insert hardcodes empty `fileIds`/`attachedFileTypes`
/// for now, so the drive arena persists them via this UPDATE keyed on the new
/// note's id (upstream `NoteCreateService` sets both on insert).
pub async fn set_note_files(
    pool: &PgPool,
    note_id: &str,
    file_ids: &[String],
    file_types: &[String],
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"UPDATE "note"
           SET "fileIds" = $2::varchar[], "attachedFileTypes" = $3::varchar[]
           WHERE "id" = $1"#,
    )
    .bind(note_id)
    .bind(file_ids)
    .bind(file_types)
    .execute(pool)
    .await?;
    Ok(())
}

/// `endpoints/drive/files/attached-notes.ts`: ids of notes whose
/// `note.fileIds` array CONTAINS the file (upstream `:file <@ note.fileIds`
/// with param `[file.id]` — for a single element equivalent to
/// `$1 = ANY("fileIds")`, which is the shape used here: a scalar bind. The
/// array literal binds as `text[]` while note.fileIds is `varchar[]`, and
/// `varying[] @> text[]` has no Postgres operator — e2e-verified).
/// `makePaginationQuery` paging: both bounds exclusive, sinceId-only →
/// oldest-first ASC, otherwise newest-first DESC.
pub async fn note_ids_attaching_file(
    pool: &PgPool,
    file_id: &str,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<String>, sqlx::Error> {
    let mut q = sqlx::QueryBuilder::new(r#"SELECT "id" FROM "note" WHERE "#);
    q.push_bind(file_id);
    q.push(r#" = ANY("fileIds")"#);
    if let Some(since) = since_id {
        q.push(r#" AND "id" > "#).push_bind(since);
    }
    if let Some(until) = until_id {
        q.push(r#" AND "id" < "#).push_bind(until);
    }
    if since_id.is_some() {
        q.push(" ORDER BY \"id\" ASC");
    } else {
        q.push(" ORDER BY \"id\" DESC");
    }
    q.push(" LIMIT ").push_bind(limit);
    let rows: Vec<(String,)> = q.build_query_as().fetch_all(pool).await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}
