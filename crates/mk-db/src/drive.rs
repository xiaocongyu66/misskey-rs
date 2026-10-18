//! Drive file rows (`drive_file`): insert (the `drive/files/create` upload
//! path) and lookup for the `i/update` avatar/banner swap. Upstream logic:
//! `DriveService.addFile` + `endpoints/i/update.ts` L377-411 (avatar == file
//! must exist, belong to the caller, and be `image/*`).

use sqlx::PgPool;

/// Columns consumed by the api layer (drive pack + avatar/banner swap).
#[derive(Debug, sqlx::FromRow)]
pub struct DriveFileRow {
    pub id: String,
    #[sqlx(rename = "userId")]
    pub user_id: Option<String>,
    /// MD5 hex of the stored bytes (varchar(32)); informational here.
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
             "accessKey", "isSensitive", "isLink", "blurhash", "maybeSensitive", "maybePorn")
        VALUES ($1, $2, NULL, $3, $4, $5, $6, NULL, $7::jsonb, FALSE, $8, NULL, NULL,
                $9, FALSE, FALSE, $10, FALSE, FALSE)
        "#,
    )
    .bind(&row.id)
    .bind(&row.user_id)
    .bind(&row.md5)
    .bind(&row.name)
    .bind(&row.file_type)
    .bind(row.size)
    .bind(serde_json::to_string(&row.properties).unwrap_or_else(|_| "{}".to_string()))
    .bind(&row.url)
    .bind(access_key)
    .bind(&row.blurhash)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn find_drive_file(pool: &PgPool, id: &str) -> Result<Option<DriveFileRow>, sqlx::Error> {
    sqlx::query_as::<_, DriveFileRow>(
        r#"SELECT "id", "userId", "md5", "name", ("type"::text) AS "type", "size",
                  "blurhash", "url", "webpublicUrl", ("properties"::text)::jsonb AS "properties"
           FROM "drive_file" WHERE "id" = $1"#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}
