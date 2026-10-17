//! Note SQL (M1 minimal create/show path; full query surface arrives with M2).

use sqlx::PgPool;

pub struct NewNote<'a> {
    pub id: &'a str,
    pub text: Option<&'a str>,
    pub user_id: &'a str,
    pub visibility: &'a str,
    pub reply_id: Option<&'a str>,
}

pub async fn insert_note(pool: &PgPool, n: &NewNote<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO "note" ("id", "text", "userId", "localOnly", "renoteCount", "repliesCount", "reactions", "visibility", "fileIds", "attachedFileTypes", "visibleUserIds", "mentions", "emojis", "tags", "hasPoll", "pageCount")
        VALUES ($1, $2, $3, FALSE, 0, 0, '{}'::jsonb, $4::note_visibility_enum, '{}'::varchar[], '{}'::varchar[], '{}'::varchar[], '{}'::varchar[], '{}'::varchar[], '{}'::varchar[], FALSE, 0)
        "#,
    )
    .bind(n.id)
    .bind(n.text)
    .bind(n.user_id)
    .bind(n.visibility)
    .execute(pool)
    .await?;
    Ok(())
}

/// Columns carried by the M1 note pack.
pub const NOTE_COLS: &str = r#""id", ("replyId") AS "reply_id", ("renoteId") AS "renote_id", "text", "cw", ("userId") AS "user_id", ("localOnly") AS "local_only", "visibility", "mentions", "emojis", "tags", ("hasPoll") AS "has_poll", ("reactionAcceptance"::text) AS "reaction_acceptance""#;

#[derive(Debug, sqlx::FromRow)]
pub struct NoteRow {
    pub id: String,
    pub reply_id: Option<String>,
    pub renote_id: Option<String>,
    pub text: Option<String>,
    pub cw: Option<String>,
    pub user_id: String,
    pub local_only: bool,
    pub visibility: String,
    pub mentions: serde_json::Value,
    pub emojis: serde_json::Value,
    pub tags: serde_json::Value,
    pub has_poll: bool,
    pub reaction_acceptance: Option<String>,
}

pub async fn find_note_by_id(pool: &PgPool, id: &str) -> Result<Option<NoteRow>, sqlx::Error> {
    sqlx::query_as::<_, NoteRow>(&format!(
        r#"SELECT {} FROM "note" WHERE "id" = $1"#,
        NOTE_COLS
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
}
