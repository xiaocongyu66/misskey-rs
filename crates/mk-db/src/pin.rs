//! Note pinings (`user_note_pining`). Upstream `NotePiningService.addPinned`/
//! `removePinned` + the `UserEntityService.pack` pins block (pins are ordered
//! `pin.id DESC`, i.e. newest pin first).

use sqlx::PgPool;

/// `NotePiningService.addPinned`'s insert half (existence/limit/dup gates live
/// at the endpoint layer). `id` is a fresh aid.
pub async fn insert_pining(
    pool: &PgPool,
    id: &str,
    user_id: &str,
    note_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(r#"INSERT INTO "user_note_pining" ("id", "userId", "noteId") VALUES ($1, $2, $3)"#)
        .bind(id)
        .bind(user_id)
        .bind(note_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// `NotePiningService.removePinned`'s delete half.
pub async fn delete_pining(pool: &PgPool, user_id: &str, note_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "user_note_pining" WHERE "userId" = $1 AND "noteId" = $2"#)
        .bind(user_id)
        .bind(note_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Current pinings count (addPinned's `pinings.length >= pinLimit` gate input).
pub async fn count_pinings(pool: &PgPool, user_id: &str) -> Result<i64, sqlx::Error> {
    let n: i64 =
        sqlx::query_scalar(r#"SELECT count(*) FROM "user_note_pining" WHERE "userId" = $1"#)
            .bind(user_id)
            .fetch_one(pool)
            .await?;
    Ok(n)
}

pub async fn pining_exists(
    pool: &PgPool,
    user_id: &str,
    note_id: &str,
) -> Result<bool, sqlx::Error> {
    let exists: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(SELECT 1 FROM "user_note_pining" WHERE "userId" = $1 AND "noteId" = $2)"#,
    )
    .bind(user_id)
    .bind(note_id)
    .fetch_one(pool)
    .await?;
    Ok(exists)
}

/// Pinned note ids for the user pack (`pinnedNoteIds`), pin.id DESC order.
pub async fn pinned_note_ids(pool: &PgPool, user_id: &str) -> Result<Vec<String>, sqlx::Error> {
    let rows: Vec<String> = sqlx::query_scalar(
        r#"SELECT "noteId" FROM "user_note_pining" WHERE "userId" = $1 ORDER BY "id" DESC"#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}
