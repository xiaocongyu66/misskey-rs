//! Drive-file SQL for the note.ts acceptance slice, deliberately separate
//! from `drive.rs` (the drive-arena agent's file): the ordered by-ids fetch
//! notes/create uses, a `drive/files/update` write and the note-pack row
//! (`fileIds` → `files`). Upstream: `NoteCreateService.fetchAndCreate`
//! (`file.userId = :userId AND file.id IN (:...fileIds)` with
//! `array_position` ordering + the count-mismatch NO_SUCH_FILE check) and
//! `DriveService.updateFile` (name/isSensitive/comment columns only).

use sqlx::PgPool;

/// Row for the note-pack `files` array + the update/read endpoints. No
/// `createdAt` column exists in the baseline schema — the pack derives it
/// from the aid like every other row.
#[derive(Debug, sqlx::FromRow)]
pub struct NoteDriveFileRow {
    pub id: String,
    #[sqlx(rename = "userId")]
    pub user_id: Option<String>,
    pub name: String,
    #[sqlx(rename = "type")]
    pub file_type: String,
    pub md5: String,
    pub size: i32,
    pub comment: Option<String>,
    #[sqlx(rename = "isSensitive")]
    pub is_sensitive: bool,
    pub blurhash: Option<String>,
    pub url: String,
    #[sqlx(rename = "thumbnailUrl")]
    pub thumbnail_url: Option<String>,
    #[sqlx(rename = "webpublicUrl")]
    pub webpublic_url: Option<String>,
    pub properties: serde_json::Value,
}

const FILE_COLS: &str = r#""id", ("userId") AS "userId", "name", ("type"::text) AS "type", "md5", "size", "comment",
    "isSensitive", "blurhash", "url", "thumbnailUrl", "webpublicUrl",
    ("properties"::text)::jsonb AS "properties""#;

/// Owned fileIds fetch: `userId` + `id IN (...)`; order is applied on the
/// caller side (mapping `Vec` → id) so the upstream `array_position` ORDER is
/// mirrored without SQL-side index gymnastics.
pub async fn fetch_owned_files(
    pool: &PgPool,
    user_id: &str,
    ids: &[String],
) -> Result<Vec<NoteDriveFileRow>, sqlx::Error> {
    sqlx::query_as::<_, NoteDriveFileRow>(&format!(
        r#"SELECT {FILE_COLS} FROM "drive_file" AS "note_drive_file"
           WHERE "userId" = $1 AND "id" = ANY($2)"#
    ))
    .bind(user_id)
    .bind(ids.to_vec())
    .fetch_all(pool)
    .await
}

/// Any-file-id fetch (single file endpoints: drive/files/update read-back).
pub async fn find_file_by_id(
    pool: &PgPool,
    id: &str,
) -> Result<Option<NoteDriveFileRow>, sqlx::Error> {
    sqlx::query_as::<_, NoteDriveFileRow>(&format!(
        r#"SELECT {FILE_COLS} FROM "drive_file" AS "note_drive_file" WHERE "id" = $1"#
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// `DriveService.updateFile` columns applied by drive/files/update's paramDef
/// (`folderId` routing is upstream-only; the note.ts arena asserts
/// isSensitive/name/comment). `set_comment = false` leaves `comment` alone;
/// `true` writes `comment` (nullable).
pub async fn update_file(
    pool: &PgPool,
    id: &str,
    name: Option<&str>,
    is_sensitive: Option<bool>,
    set_comment: bool,
    comment: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE "drive_file" SET
            "name" = COALESCE($2, "name"),
            "isSensitive" = COALESCE($3, "isSensitive"),
            "comment" = CASE WHEN $4 THEN $5 ELSE "comment" END
        WHERE "id" = $1
        "#,
    )
    .bind(id)
    .bind(name)
    .bind(is_sensitive)
    .bind(set_comment)
    .bind(comment)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn owned_fetch_and_update() {
        if std::env::var("DATABASE_URL").is_err() {
            return;
        }
        // fresh_db only provisions the database — migrations ride explicitly
        // (the api-side fresh_pool contract); without them the drive_file
        // table is missing (42P01).
        let pool = crate::test_support::fresh_db("mkdb_drive_note").await;
        crate::run_migrations(&pool).await.unwrap();
        sqlx::query(
            r#"INSERT INTO "drive_file" ("id","userId","md5","name","type","size","properties",
                                        "storedInternal","url","isSensitive")
               VALUES ('f1','u1','0123456789abcdef0123456789abcdef','a.jpg','image/jpeg',3,'{}'::jsonb,
                       FALSE,'http://x/a.jpg',FALSE)"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let got = fetch_owned_files(&pool, "u1", &["f1".into()])
            .await
            .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "f1");
        assert!(!got[0].is_sensitive);
        assert_eq!(got[0].file_type, "image/jpeg");

        // not the owner
        assert!(
            fetch_owned_files(&pool, "u2", &["f1".into()])
                .await
                .unwrap()
                .is_empty()
        );
        // missing id just absent
        assert!(
            fetch_owned_files(&pool, "u1", &["zz".into()])
                .await
                .unwrap()
                .is_empty()
        );

        update_file(&pool, "f1", Some("b.jpg"), Some(true), true, Some("hi"))
            .await
            .unwrap();
        let f = find_file_by_id(&pool, "f1").await.unwrap().unwrap();
        assert_eq!(f.name, "b.jpg");
        assert!(f.is_sensitive);
        assert_eq!(f.comment.as_deref(), Some("hi"));

        update_file(&pool, "f1", None, None, true, None)
            .await
            .unwrap();
        let f = find_file_by_id(&pool, "f1").await.unwrap().unwrap();
        assert_eq!(f.comment, None);
        // untouched columns
        assert_eq!(f.name, "b.jpg");
        assert!(f.is_sensitive);
    }
}
