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
    #[sqlx(rename = "webpublicType")]
    pub webpublic_type: Option<String>,
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
             "webpublicType",
             "accessKey", "isSensitive", "isLink", "blurhash", "maybeSensitive", "maybePorn",
             "folderId")
        VALUES ($1, $2, NULL, $3, $4, $5, $6, $7, $8::jsonb, FALSE, $9, NULL, NULL,
                $14, $10, $11, FALSE, $12, FALSE, FALSE, $13)
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
    .bind(&row.webpublic_type)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn find_drive_file(pool: &PgPool, id: &str) -> Result<Option<DriveFileRow>, sqlx::Error> {
    sqlx::query_as::<_, DriveFileRow>(
        r#"SELECT "id", "userId", "md5", "name", ("type"::text) AS "type", "size",
                  "blurhash", "url", "webpublicUrl", "webpublicType",
                  ("properties"::text)::jsonb AS "properties",
                  "comment", "isSensitive", "folderId"
           FROM "drive_file" WHERE "id" = $1"#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

// ==== arena-files: drive/files {list,find,find-by-hash,check-existence,
// show,delete} (upstream endpoints/drive/files/*) — the read/lookup surface
// e2e-harness/files.ts drives. ==========================================

/// SELECT list shared by the arena-files lookups (identical shape to
/// `find_drive_file`).
const FILE_COLS: &str = r#""id", "userId", "md5", "name", ("type"::text) AS "type", "size", "blurhash", "url", "webpublicUrl", "webpublicType", ("properties"::text)::jsonb AS "properties", "comment", "isSensitive", "folderId""#;

/// `drive/files` (endpoints/drive/files.ts): owner-scoped list.
/// `makePaginationQuery` bounds are exclusive (id = the raw varchar compare);
/// sinceId-only → oldest-first ASC, otherwise newest-first DESC. folderId:
/// `if (ps.folderId)` → equal, else IS NULL. type: exact match, or the
/// `x/*` wildcard → `LIKE 'x/%'` (files.ts type arm). The default sort
/// (`ps.sort` switch aside) is the pagination id DESC.
#[allow(clippy::too_many_arguments)]
pub async fn files_list(
    pool: &PgPool,
    user_id: &str,
    since_id: Option<&str>,
    until_id: Option<&str>,
    folder_id: Option<&str>,
    type_exact: Option<&str>,
    type_prefix: Option<&str>,
    limit: i64,
) -> Result<Vec<DriveFileRow>, sqlx::Error> {
    let mut q = sqlx::QueryBuilder::new("SELECT ");
    q.push(FILE_COLS)
        .push(r#" FROM "drive_file" WHERE "userId" = "#);
    q.push_bind(user_id);
    q.push(r#" AND "folderId""#);
    match folder_id {
        Some(fid) => {
            q.push(" = ").push_bind(fid);
        }
        None => {
            q.push(" IS NULL");
        }
    }
    if let Some(t) = type_exact {
        q.push(r#" AND "type" = "#).push_bind(t);
    }
    if let Some(p) = type_prefix {
        let pattern = format!("{p}%");
        q.push(r#" AND "type" LIKE "#).push_bind(pattern);
    }
    match (since_id, until_id) {
        (Some(s), Some(u)) => {
            q.push(r#" AND "id" > "#).push_bind(s);
            q.push(r#" AND "id" < "#).push_bind(u);
        }
        (Some(s), None) => {
            q.push(r#" AND "id" > "#).push_bind(s);
        }
        (None, Some(u)) => {
            q.push(r#" AND "id" < "#).push_bind(u);
        }
        (None, None) => {}
    }
    // makePaginationQuery order: sinceId+untilId → DESC, sinceId-only →
    // ASC (oldest-first), untilId-only / neither → DESC.
    if since_id.is_some() && until_id.is_none() {
        q.push(r#" ORDER BY "id" ASC"#);
    } else {
        q.push(r#" ORDER BY "id" DESC"#);
    }
    q.push(" LIMIT ").push_bind(limit);
    q.build_query_as::<DriveFileRow>().fetch_all(pool).await
}

/// `drive/files/find` (find.ts): name + userId, folderId ?? IsNull. No
/// explicit order upstream (findBy) — keep the pagination DESC shape for
/// determinism.
pub async fn files_find_by_name(
    pool: &PgPool,
    user_id: &str,
    name: &str,
    folder_id: Option<&str>,
) -> Result<Vec<DriveFileRow>, sqlx::Error> {
    let mut q = sqlx::QueryBuilder::new("SELECT ");
    q.push(FILE_COLS)
        .push(r#" FROM "drive_file" WHERE "name" = "#)
        .push_bind(name)
        .push(r#" AND "userId" = "#)
        .push_bind(user_id)
        .push(r#" AND "folderId""#);
    match folder_id {
        Some(fid) => {
            q.push(" = ").push_bind(fid);
        }
        None => {
            q.push(" IS NULL");
        }
    }
    q.build_query_as::<DriveFileRow>().fetch_all(pool).await
}

/// `drive/files/find-by-hash.ts`: md5 + userId.
pub async fn files_find_by_md5(
    pool: &PgPool,
    user_id: &str,
    md5: &str,
) -> Result<Vec<DriveFileRow>, sqlx::Error> {
    sqlx::query_as::<_, DriveFileRow>(&format!(
        r#"SELECT {FILE_COLS} FROM "drive_file" WHERE "md5" = $1 AND "userId" = $2 ORDER BY "id" DESC"#
    ))
    .bind(md5)
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// `drive/files/check-existence.ts`: EXISTS(md5 + userId). `SELECT EXISTS`
/// always yields exactly one row, so `fetch_one` cannot miss (the defensive
/// Option here does not typecheck — a query_as row type must impl FromRow).
pub async fn md5_exists(pool: &PgPool, user_id: &str, md5: &str) -> Result<bool, sqlx::Error> {
    let hit: (bool,) = sqlx::query_as(
        r#"SELECT EXISTS(SELECT 1 FROM "drive_file" WHERE "md5" = $1 AND "userId" = $2)"#,
    )
    .bind(md5)
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    Ok(hit.0)
}

/// `drive/files/show.ts` `findOneBy([{ url }, { webpublicUrl },
/// { thumbnailUrl }])` — TypeORM array → OR, first match wins (stable order
/// by id).
pub async fn find_file_by_any_url(
    pool: &PgPool,
    url: &str,
) -> Result<Option<DriveFileRow>, sqlx::Error> {
    sqlx::query_as::<_, DriveFileRow>(&format!(
        r#"SELECT {FILE_COLS} FROM "drive_file"
           WHERE "url" = $1 OR "webpublicUrl" = $1 OR "thumbnailUrl" = $1
           ORDER BY "id" LIMIT 1"#
    ))
    .bind(url)
    .fetch_optional(pool)
    .await
}

/// `DriveService.deleteFile` → `deletePostProcess`: the row itself (charts
/// and the object-storage sweep have no server-side counterpart; notes keep
/// their fileIds exactly like upstream — the packer merely drops the missing
/// file).
pub async fn delete_drive_file(pool: &PgPool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "drive_file" WHERE "id" = $1"#)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
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
                  f."blurhash", f."url", f."webpublicUrl", f."webpublicType", ("f"."properties"::text)::jsonb AS "properties",
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
