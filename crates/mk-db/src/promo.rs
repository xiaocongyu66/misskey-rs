//! promo: db paths behind upstream `endpoints/promo/read.ts` +
//! `endpoints/admin/promo/create.ts`, plus the repo `promo/show` picker.
//!
//! `promo_note` (PK noteId, indexed userId) and `promo_read` (PK id, unique
//! (userId, noteId)) ship in the 0001 baseline, so no migration is needed.
//! Timestamps follow the workspace convention (timestamptz ↔ epoch ms).

use crate::sqlx::PgPool;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PromoNoteRow {
    pub note_id: String,
    pub expires_at_ms: i64,
    pub user_id: String,
}

const NOTE_COLS: &str = r#""noteId" AS "note_id",
(EXTRACT(EPOCH FROM "expiresAt") * 1000)::bigint AS "expires_at_ms",
"userId" AS "user_id""#;

/// `promoNotesRepository.exists({ where: { noteId } })`.
pub async fn note_exists(pool: &PgPool, note_id: &str) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(SELECT 1 FROM "promo_note" WHERE "noteId" = $1)"#,
    )
    .bind(note_id)
    .fetch_one(pool)
    .await
}

pub async fn insert_note(
    pool: &PgPool,
    note_id: &str,
    expires_at_ms: i64,
    user_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO "promo_note" ("noteId", "expiresAt", "userId")
           VALUES ($1, TIMESTAMP 'epoch' + $2::bigint * interval '1 millisecond', $3)"#,
    )
    .bind(note_id)
    .bind(expires_at_ms)
    .bind(user_id)
    .execute(pool)
    .await
    .map(|_| ())
}

pub async fn list_notes(pool: &PgPool) -> Result<Vec<PromoNoteRow>, sqlx::Error> {
    sqlx::query_as::<_, PromoNoteRow>(&format!(
        r#"SELECT {NOTE_COLS} FROM "promo_note" ORDER BY "expiresAt" DESC"#
    ))
    .fetch_all(pool)
    .await
}

/// promo/show: a random unexpired promo note the caller has not read yet.
pub async fn random_unread(
    pool: &PgPool,
    user_id: &str,
    now_ms: i64,
) -> Result<Option<PromoNoteRow>, sqlx::Error> {
    sqlx::query_as::<_, PromoNoteRow>(&format!(
        r#"SELECT {NOTE_COLS} FROM "promo_note" pn
           WHERE pn."expiresAt" > TIMESTAMP 'epoch' + $2::bigint * interval '1 millisecond'
             AND NOT EXISTS (
               SELECT 1 FROM "promo_read" pr
               WHERE pr."noteId" = pn."noteId" AND pr."userId" = $1
             )
           ORDER BY random()
           LIMIT 1"#
    ))
    .bind(user_id)
    .bind(now_ms)
    .fetch_optional(pool)
    .await
}

/// `promoReadsRepository.exists({ where: { noteId, userId } })`.
pub async fn is_read(pool: &PgPool, user_id: &str, note_id: &str) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(SELECT 1 FROM "promo_read" WHERE "userId" = $1 AND "noteId" = $2)"#,
    )
    .bind(user_id)
    .bind(note_id)
    .fetch_one(pool)
    .await
}

/// Insert a promo_read row; the unique (userId, noteId) index makes it
/// idempotent (upstream checks `exists` first and returns early).
pub async fn mark_read(
    pool: &PgPool,
    id: &str,
    user_id: &str,
    note_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO "promo_read" ("id", "userId", "noteId") VALUES ($1, $2, $3)
           ON CONFLICT ("userId", "noteId") DO NOTHING"#,
    )
    .bind(id)
    .bind(user_id)
    .bind(note_id)
    .execute(pool)
    .await
    .map(|_| ())
}
