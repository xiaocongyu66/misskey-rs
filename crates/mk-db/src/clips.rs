//! Clips domain (upstream `endpoints/clips/*` + `endpoints/users/clips.ts`,
//! `core/ClipService.ts` + `core/entities/ClipEntityService.ts`).
//!
//! Tables ship in the 0001 baseline: `clip`, `clip_note`, `clip_favorite`
//! (clip_note carries the `(noteId, clipId)` UNIQUE index
//! `IDX_6fc0ec357d55a18646262fdfff` and CASCADE FKs to note/clip — the
//! add-note path classifies insert errors into ALREADY_CLIPPED (23505) /
//! NO_SUCH_NOTE (23503) exactly like upstream `isDuplicateKeyValueError` +
//! the `is not present in table "note"` detail probe). Aid ids sort
//! monotonically as plain strings, so sinceId/untilId paging is a string
//! compare; the pager bounds are EXCLUSIVE (`makePaginationQuery`),
//! sinceId-only → oldest-first ASC, otherwise newest-first DESC. timestamptz
//! columns come back as epoch-ms (`(EXTRACT(EPOCH ...) * 1000)::bigint`) per
//! the announcement.rs convention.

use sqlx::PgPool;

use crate::note::{NOTE_COLS, NoteRow};

/// The `clip` row as `ClipsRepository.findOneBy` sees it (`lastClippedAt`
/// pre-converted to epoch ms).
#[derive(Debug, sqlx::FromRow)]
pub struct ClipRow {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub description: Option<String>,
    pub is_public: bool,
    #[sqlx(rename = "last_clipped_at_ms")]
    pub last_clipped_at_ms: Option<i64>,
}

const CLIP_COLS: &str = r#""id", "userId" AS "user_id", "name", "description",
               "isPublic" AS "is_public",
               CASE WHEN "lastClippedAt" IS NULL THEN NULL
                    ELSE (EXTRACT(EPOCH FROM "lastClippedAt") * 1000)::bigint
               END AS "last_clipped_at_ms""#;

/// `ClipsRepository.findOneBy({ id })` — clips/show, clips/favorite,
/// clips/unfavorite and clips/notes look the clip up WITHOUT an owner scope
/// (non-owner visibility is decided afterwards from `isPublic`/me).
pub async fn find_clip(pool: &PgPool, clip_id: &str) -> Result<Option<ClipRow>, sqlx::Error> {
    sqlx::query_as::<_, ClipRow>(&format!(
        r#"SELECT {CLIP_COLS} FROM "clip" WHERE "id" = $1"#
    ))
    .bind(clip_id)
    .fetch_optional(pool)
    .await
}

/// `ClipsRepository.findOneBy({ id, userId })` — clips/update/delete and
/// ClipService.addNote/removeNote scope ONLY to the owner (a non-owner's
/// clipId → NO_SUCH_CLIP, not 403).
pub async fn find_clip_owned(
    pool: &PgPool,
    clip_id: &str,
    owner_id: &str,
) -> Result<Option<ClipRow>, sqlx::Error> {
    sqlx::query_as::<_, ClipRow>(&format!(
        r#"SELECT {CLIP_COLS} FROM "clip" WHERE "id" = $1 AND "userId" = $2"#
    ))
    .bind(clip_id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await
}

/// `ClipsRepository.countBy({ userId })` — the `clipLimit` policy gate input.
pub async fn count_clips_owned(pool: &PgPool, owner_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(r#"SELECT count(*) FROM "clip" WHERE "userId" = $1"#)
        .bind(owner_id)
        .fetch_one(pool)
        .await
}

/// `ClipsRepository.insertOne({ id, userId, name, isPublic, description })`.
pub async fn insert_clip(
    pool: &PgPool,
    id: &str,
    user_id: &str,
    name: &str,
    is_public: bool,
    description: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO "clip" ("id", "userId", "name", "isPublic", "description")
           VALUES ($1, $2, $3, $4, $5)"#,
    )
    .bind(id)
    .bind(user_id)
    .bind(name)
    .bind(is_public)
    .bind(description)
    .execute(pool)
    .await?;
    Ok(())
}

/// TypeORM `update({ id }, { name, description, isPublic })` — `undefined`
/// fields are skipped, an all-undefined delta is a no-op. `description:
/// Some(None)` writes NULL (upstream normalizes `ps.description || null`).
#[derive(Debug, Default)]
pub struct ClipUpdate {
    pub name: Option<String>,
    pub is_public: Option<bool>,
    pub description: Option<Option<String>>,
}

enum Bind {
    Text(Option<String>),
    Flag(bool),
}

pub async fn update_clip(
    pool: &PgPool,
    clip_id: &str,
    delta: &ClipUpdate,
) -> Result<(), sqlx::Error> {
    // Dynamic SET (TypeORM update() semantics): absent params touch nothing.
    // Placeholders are numbered in field order; the boolean rides a CAST so
    // the typed bind goes through, the texts infer varchar.
    let mut binds: Vec<Bind> = Vec::new();
    let mut set: Vec<String> = Vec::new();
    if let Some(name) = &delta.name {
        binds.push(Bind::Text(Some(name.clone())));
        set.push(format!(r#""name" = ${}"#, binds.len()));
    }
    if let Some(is_public) = delta.is_public {
        binds.push(Bind::Flag(is_public));
        set.push(format!(r#""isPublic" = CAST(${} AS boolean)"#, binds.len()));
    }
    if let Some(desc) = &delta.description {
        binds.push(Bind::Text(desc.clone()));
        set.push(format!(r#""description" = ${}"#, binds.len()));
    }
    if set.is_empty() {
        return Ok(());
    }
    let sql = format!(
        r#"UPDATE "clip" SET {} WHERE "id" = ${}"#,
        set.join(", "),
        binds.len() + 1
    );
    let mut q = sqlx::query(&sql);
    for b in binds {
        // by-value binds (the announcement.rs update) — sqlx Encode covers
        // Option<String> / bool / String directly, refs wobble on inference
        q = match b {
            Bind::Text(t) => q.bind(t),
            Bind::Flag(f) => q.bind(f),
        };
    }
    q = q.bind(clip_id);
    q.execute(pool).await?;
    Ok(())
}

/// `ClipsRepository.delete(clip.id)` — clip_note / clip_favorite rows fall
/// away on the CASCADE FKs.
pub async fn delete_clip(pool: &PgPool, clip_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "clip" WHERE "id" = $1"#)
        .bind(clip_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// `makePaginationQuery(clips, sinceId, untilId[, sinceDate, untilDate])
/// .andWhere('clip.userId = :userId')` (+ `clip.isPublic = true` for
/// users/clips). sinceId-only is the only ASC shape; bounds are exclusive.
/// Bind map: $1 owner, $2 untilId, $3 sinceId.
pub async fn list_clips(
    pool: &PgPool,
    owner_id: &str,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
    public_only: bool,
) -> Result<Vec<ClipRow>, sqlx::Error> {
    let dir = if since_id.is_some() && until_id.is_none() {
        "ASC"
    } else {
        "DESC"
    };
    let sql = format!(
        r#"SELECT {CLIP_COLS} FROM "clip"
           WHERE "userId" = $1
             {public_gate}
             AND ($2::varchar IS NULL OR "id" < $2::varchar)
             AND ($3::varchar IS NULL OR "id" > $3::varchar)
           ORDER BY "id" {dir} LIMIT {limit}"#,
        public_gate = if public_only {
            r#"AND "isPublic" = TRUE"#
        } else {
            ""
        },
    );
    sqlx::query_as::<_, ClipRow>(&sql)
        .bind(owner_id)
        .bind(until_id)
        .bind(since_id)
        .fetch_all(pool)
        .await
}

/// clips/my-favorites — the ClipFavorites query (userId = me, joined clip)
/// with no pagination ("pagenationはない。全部一気にとれる。"). Ordered by the
/// favorite row id (upstream leaves the order undefined; the census sorts or
/// asserts singletons).
pub async fn list_favorited_clips(pool: &PgPool, me_id: &str) -> Result<Vec<ClipRow>, sqlx::Error> {
    let sql = format!(
        r#"SELECT {CLIP_COLS} FROM "clip_favorite" f
           INNER JOIN "clip" ON "clip"."id" = f."clipId"
           WHERE f."userId" = $1
           ORDER BY f."id""#
    );
    sqlx::query_as::<_, ClipRow>(&sql)
        .bind(me_id)
        .fetch_all(pool)
        .await
}

/// `clipFavoritesRepository.countBy({ clipId })` — the pack `favoritedCount`.
pub async fn favorite_count(pool: &PgPool, clip_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(r#"SELECT count(*) FROM "clip_favorite" WHERE "clipId" = $1"#)
        .bind(clip_id)
        .fetch_one(pool)
        .await
}

/// `clipFavoritesRepository.findOneBy({ clipId, userId })` → the row id (the
/// unfavorite path deletes by row id); None → NOT_FAVORITED.
pub async fn find_favorite_id(
    pool: &PgPool,
    clip_id: &str,
    user_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(r#"SELECT "id" FROM "clip_favorite" WHERE "clipId" = $1 AND "userId" = $2"#)
        .bind(clip_id)
        .bind(user_id)
        .fetch_optional(pool)
        .await
}

/// `clipFavoritesRepository.exists({ clipId, userId })` — the pack
/// `isFavorited` me-gate.
pub async fn is_favorited(
    pool: &PgPool,
    clip_id: &str,
    user_id: &str,
) -> Result<bool, sqlx::Error> {
    let n: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "clip_favorite" WHERE "clipId" = $1 AND "userId" = $2"#,
    )
    .bind(clip_id)
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

/// `clipFavoritesRepository.insert({ id, clipId, userId })`.
pub async fn insert_favorite(
    pool: &PgPool,
    id: &str,
    clip_id: &str,
    user_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(r#"INSERT INTO "clip_favorite" ("id", "clipId", "userId") VALUES ($1, $2, $3)"#)
        .bind(id)
        .bind(clip_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// `clipFavoritesRepository.delete(exist.id)`.
pub async fn delete_favorite(pool: &PgPool, favorite_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "clip_favorite" WHERE "id" = $1"#)
        .bind(favorite_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// `clipNotesRepository.countBy({ clipId })` — the `noteEachClipsLimit` cap
/// input (ClipService.addNote).
pub async fn count_clip_notes(pool: &PgPool, clip_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(r#"SELECT count(*) FROM "clip_note" WHERE "clipId" = $1"#)
        .bind(clip_id)
        .fetch_one(pool)
        .await
}

/// The insert error classes ClipService.addNote maps (duplicate key →
/// AlreadyAddedError; FK to note → NoSuchNoteError; rest rethrown).
pub enum InsertClipNoteError {
    Duplicate,
    NoSuchNote,
    Db(sqlx::Error),
}

/// `clipNotesRepository.insert({ id, noteId, clipId })` — the unique
/// `(noteId, clipId)` index raises 23505 → Duplicate; the noteId FK raises
/// 23503 → NoSuchNote (no separate exists-probe, mirroring upstream's
/// insert-then-classify).
pub async fn insert_clip_note(
    pool: &PgPool,
    id: &str,
    clip_id: &str,
    note_id: &str,
) -> Result<(), InsertClipNoteError> {
    let res =
        sqlx::query(r#"INSERT INTO "clip_note" ("id", "noteId", "clipId") VALUES ($1, $2, $3)"#)
            .bind(id)
            .bind(note_id)
            .bind(clip_id)
            .execute(pool)
            .await;
    match res {
        Ok(_) => Ok(()),
        Err(e) => {
            let code = e
                .as_database_error()
                .and_then(|d| d.code().map(|c| c.to_string()));
            match code.as_deref() {
                Some("23505") => Err(InsertClipNoteError::Duplicate),
                // also covers the clipId FK — unreachable: the caller verified
                // the clip row first, keeping upstream's error priority
                Some("23503") => Err(InsertClipNoteError::NoSuchNote),
                _ => Err(InsertClipNoteError::Db(e)),
            }
        }
    }
}

/// `clipNotesRepository.delete({ noteId, clipId })` → affected === 0 →
/// NoSuchNoteError.
pub async fn delete_clip_note(
    pool: &PgPool,
    clip_id: &str,
    note_id: &str,
) -> Result<bool, sqlx::Error> {
    let res = sqlx::query(r#"DELETE FROM "clip_note" WHERE "clipId" = $1 AND "noteId" = $2"#)
        .bind(clip_id)
        .bind(note_id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}

/// ClipService.addNote tail: `update(clip.id, { lastClippedAt: new Date() })`.
pub async fn set_last_clipped_at_now(pool: &PgPool, clip_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "clip" SET "lastClippedAt" = now() WHERE "id" = $1"#)
        .bind(clip_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// `notesRepository.increment/decrement({ id }, 'clippedCount', 1)` —
/// `delta` is +1 (addNote) or -1 (removeNote); the smallint column carries
/// the notes/show pack counter this maintenance feeds.
pub async fn bump_note_clipped_count(
    pool: &PgPool,
    note_id: &str,
    delta: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "note" SET "clippedCount" = "clippedCount" + $2 WHERE "id" = $1"#)
        .bind(note_id)
        .bind(delta)
        .execute(pool)
        .await?;
    Ok(())
}

/// generateVisibilityQuery(me) — public/home, self, specified-to-me,
/// mentioned-me, followers-of-author / followers reply addressed to me
/// (QueryService.ts L270-315), rebound for the clips pager (me $4, [me] $5,
/// my followees $6 — the pager owns $1..$3).
const CLIP_VIS_ME: &str = r#" AND (
          (n."visibility" IN ('public','home'))
          OR n."userId" = $4
          OR ($5::varchar[] <@ n."visibleUserIds")
          OR ($5::varchar[] <@ n."mentions")
          OR (
            n."visibility" = 'followers'
            AND (n."userId" = ANY($6) OR n."replyUserId" = $4)
          )
        )"#;

/// generateVisibilityQuery(null) — the guest plane is public/home only.
const CLIP_VIS_GUEST: &str = r#" AND n."visibility" IN ('public','home')"#;

/// clips/notes — makePaginationQuery over note ⨝ clip_note (the visibility
/// plane rides per-viewer `me`; the mute/block note queries + blocked-host
/// slice are out of scope for the whole mk-db note-list family, matching
/// lists.rs/timelines.rs). Bind map: $1 clipId, $2 untilId, $3 sinceId, then
/// the visibility trio. The SQL string, builder and binds share this scope
/// (the E0716 wrap_outer trap from the mute lane).
async fn notes_query(
    pool: &PgPool,
    clip_id: &str,
    vis: &str,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
    me: Option<(&str, &[String])>,
) -> Result<Vec<NoteRow>, sqlx::Error> {
    // makePaginationQuery (QueryService.ts L46): both bounds → BETWEEN with
    // DESC order; sinceId-only is the only oldest-first ASC shape.
    let dir = if since_id.is_some() && until_id.is_none() {
        "ASC"
    } else {
        "DESC"
    };
    let inner = format!(
        r#"SELECT n."id" FROM "note" n
           INNER JOIN "clip_note" cn ON cn."noteId" = n."id" AND cn."clipId" = $1
           INNER JOIN "user" u ON u."id" = n."userId"
           WHERE u."isSuspended" = FALSE{vis}
             AND ($2::varchar IS NULL OR n."id" < $2::varchar)
             AND ($3::varchar IS NULL OR n."id" > $3::varchar)
           ORDER BY n."id" {dir} LIMIT {limit}"#
    );
    let outer =
        format!(r#"SELECT {NOTE_COLS} FROM "note" WHERE "id" IN ({inner}) ORDER BY "id" {dir}"#);
    let mut q = sqlx::query_as::<_, NoteRow>(&outer)
        .bind(clip_id)
        .bind(until_id)
        .bind(since_id);
    if let Some((me_id, followees)) = me {
        q = q.bind(me_id).bind(vec![me_id.to_string()]).bind(followees);
    }
    q.fetch_all(pool).await
}

/// The signed-in view (viewer visibility plane).
pub async fn clip_notes_for_me(
    pool: &PgPool,
    clip_id: &str,
    me_id: &str,
    followee_ids: &[String],
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<NoteRow>, sqlx::Error> {
    notes_query(
        pool,
        clip_id,
        CLIP_VIS_ME,
        since_id,
        until_id,
        limit,
        Some((me_id, followee_ids)),
    )
    .await
}

/// The guest view (public/home only).
pub async fn clip_notes_for_guest(
    pool: &PgPool,
    clip_id: &str,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<NoteRow>, sqlx::Error> {
    notes_query(
        pool,
        clip_id,
        CLIP_VIS_GUEST,
        since_id,
        until_id,
        limit,
        None,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixed bind layout must stay hole-free: the visibility fragments
    /// bind only $4..$6 ($1 is the clipId, $2/$3 the pager bounds).
    #[test]
    fn fragments_reference_only_fixed_binds() {
        for frag in [CLIP_VIS_ME, CLIP_VIS_GUEST] {
            let mut max = 3;
            let mut rest = frag;
            while let Some(i) = rest.find('$') {
                let digit = rest[i + 1..].chars().next().unwrap();
                assert!(digit.is_ascii_digit(), "malformed bind in {frag}");
                max = max.max(digit as u8 - b'0');
                rest = &rest[i + 1..];
            }
            assert!(max <= 6, "bind ${max} out of range: {frag}");
        }
        assert!(CLIP_VIS_ME.contains(r#"n."userId" = $4"#));
        assert!(!CLIP_VIS_GUEST.contains('$'));
    }
}
