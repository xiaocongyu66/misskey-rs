//! Channels domain db paths (upstream `endpoints/channels/*.ts`):
//! `channel` / `channel_following` / `channel_favorite` rows plus the
//! `channels/timeline` note listing (db path of
//! `channels/timeline.ts getFromDb` — channelId filter + suspended-author
//! gate + the logged-in mute/block gates; guest UGC restriction is the
//! `meta.ugcVisibilityForVisitor` setting, `all` by default → no extra
//! clause). Notes-in-channel carry the row's `channelId` column — there is
//! no `channel_note` table (only `channel_note_pining` for pins).
//!
//! Self-contained: the note column list mirrors `note.rs NOTE_COLS` (kept
//! local so co-agent files stay untouched; `NoteRow` and the note pack
//! pipeline itself are shared).

use sqlx::PgPool;

/// channel row projection (`ChannelEntityService.pack` inputs; `createdAt`
/// is derived from the aid `id`, so `lastNotedAt` rides as epoch-ms).
#[derive(Debug, sqlx::FromRow)]
pub struct ChannelRow {
    pub id: String,
    #[sqlx(rename = "lastNotedAtMs")]
    pub last_noted_at_ms: Option<i64>,
    #[sqlx(rename = "userId")]
    pub user_id: Option<String>,
    pub name: String,
    pub description: Option<String>,
    #[sqlx(rename = "bannerId")]
    pub banner_id: Option<String>,
    #[sqlx(rename = "notesCount")]
    pub notes_count: i32,
    #[sqlx(rename = "usersCount")]
    pub users_count: i32,
    #[sqlx(rename = "pinnedNoteIds")]
    pub pinned_note_ids: Vec<String>,
    pub color: String,
    #[sqlx(rename = "isArchived")]
    pub is_archived: bool,
    #[sqlx(rename = "isSensitive")]
    pub is_sensitive: bool,
    #[sqlx(rename = "allowRenoteToExternal")]
    pub allow_renote_to_external: bool,
}

pub const CHANNEL_COLS: &str = r#""id", (CASE WHEN "lastNotedAt" IS NULL THEN NULL ELSE (EXTRACT(EPOCH FROM "lastNotedAt") * 1000)::bigint END) AS "lastNotedAtMs", "userId", "name", "description", "bannerId", "notesCount", "usersCount", "pinnedNoteIds", "color", "isArchived", "isSensitive", "allowRenoteToExternal""#;

/// CHANNEL_COLS projected off a join alias (followed / my-favorites).
const JOIN_CHANNEL_COLS: &str = r#"c."id", (CASE WHEN c."lastNotedAt" IS NULL THEN NULL ELSE (EXTRACT(EPOCH FROM c."lastNotedAt") * 1000)::bigint END) AS "lastNotedAtMs", c."userId", c."name", c."description", c."bannerId", c."notesCount", c."usersCount", c."pinnedNoteIds", c."color", c."isArchived", c."isSensitive", c."allowRenoteToExternal""#;

pub async fn find_channel(pool: &PgPool, id: &str) -> Result<Option<ChannelRow>, sqlx::Error> {
    sqlx::query_as::<_, ChannelRow>(&format!(
        r#"SELECT {CHANNEL_COLS} FROM "channel" WHERE "id" = $1"#
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// `endpoints/channels/create.ts` — `channelsRepository.insertOne`.
pub struct NewChannel<'a> {
    pub id: &'a str,
    pub user_id: &'a str,
    pub name: &'a str,
    pub description: Option<&'a str>,
    pub banner_id: Option<&'a str>,
    pub color: &'a str,
    pub is_sensitive: bool,
    pub allow_renote_to_external: bool,
}

pub async fn insert_channel(pool: &PgPool, n: &NewChannel<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO "channel"
          ("id", "userId", "name", "description", "bannerId", "notesCount",
           "usersCount", "pinnedNoteIds", "color", "isArchived", "isSensitive",
           "allowRenoteToExternal")
        VALUES ($1, $2, $3, $4, $5, 0, 0, '{}'::varchar[], $6, FALSE, $7, $8)
        "#,
    )
    .bind(n.id)
    .bind(n.user_id)
    .bind(n.name)
    .bind(n.description)
    .bind(n.banner_id)
    .bind(n.color)
    .bind(n.is_sensitive)
    .bind(n.allow_renote_to_external)
    .execute(pool)
    .await?;
    Ok(())
}

/// `endpoints/channels/update.ts` — only the provided params are written
/// (tri-state `description`/`bannerId`: `Some(None)` writes NULL).
pub struct UpdateChannel<'a> {
    pub name: Option<&'a str>,
    pub description: Option<Option<&'a str>>,
    pub banner_id: Option<Option<&'a str>>,
    pub pinned_note_ids: Option<&'a [String]>,
    pub color: Option<&'a str>,
    pub is_archived: Option<bool>,
    pub is_sensitive: Option<bool>,
    pub allow_renote_to_external: Option<bool>,
}

pub async fn update_channel(
    pool: &PgPool,
    id: &str,
    ps: &UpdateChannel<'_>,
) -> Result<(), sqlx::Error> {
    let mut q = sqlx::QueryBuilder::new(r#"UPDATE "channel" SET "#);
    let mut wrote = false;
    macro_rules! set_col {
        ($col:expr, $value:expr) => {{
            if wrote {
                q.push(", ");
            }
            wrote = true;
            q.push($col);
            q.push(" = ");
            q.push_bind($value);
        }};
    }
    if let Some(v) = ps.name {
        set_col!(r#""name""#, v);
    }
    if let Some(v) = ps.description {
        set_col!(r#""description""#, v);
    }
    if let Some(v) = ps.banner_id {
        set_col!(r#""bannerId""#, v);
    }
    if let Some(v) = ps.pinned_note_ids {
        set_col!(r#""pinnedNoteIds""#, v.to_vec());
    }
    if let Some(v) = ps.color {
        set_col!(r#""color""#, v);
    }
    if let Some(v) = ps.is_archived {
        set_col!(r#""isArchived""#, v);
    }
    if let Some(v) = ps.is_sensitive {
        set_col!(r#""isSensitive""#, v);
    }
    if let Some(v) = ps.allow_renote_to_external {
        set_col!(r#""allowRenoteToExternal""#, v);
    }
    if !wrote {
        return Ok(()); // nothing requested — upstream `update({},)` is a no-op too
    }
    q.push(r#" WHERE "id" = "#);
    q.push_bind(id);
    q.build().execute(pool).await?;
    Ok(())
}

/// endpoints/channels/owned.ts — non-archived channels owned by me,
/// paged on `id` (since-only → ASC).
pub async fn list_owned(
    pool: &PgPool,
    me_id: &str,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<ChannelRow>, sqlx::Error> {
    let dir = if since_id.is_some() { "ASC" } else { "DESC" };
    sqlx::query_as::<_, ChannelRow>(&format!(
        r#"SELECT {CHANNEL_COLS} FROM "channel"
           WHERE "isArchived" = FALSE AND "userId" = $1
             AND ($2::varchar IS NULL OR "id" < $2)
             AND ($3::varchar IS NULL OR "id" > $3)
           ORDER BY "id" {dir} LIMIT $4"#
    ))
    .bind(me_id)
    .bind(until_id)
    .bind(since_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// endpoints/channels/search.ts — non-archived, ILIKE name (± description),
/// paged on `id`. `sqlLikeEscape` tames backslash / % / _ first.
pub async fn search_channels(
    pool: &PgPool,
    query: &str,
    name_only: bool,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<ChannelRow>, sqlx::Error> {
    let escaped = query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let pat = format!("%{escaped}%");
    let clause = if query.is_empty() {
        String::new()
    } else if name_only {
        r#" AND "name" ILIKE $4"#.to_string()
    } else {
        r#" AND ("name" ILIKE $4 OR "description" ILIKE $4)"#.to_string()
    };
    let dir = if since_id.is_some() { "ASC" } else { "DESC" };
    sqlx::query_as::<_, ChannelRow>(&format!(
        r#"SELECT {CHANNEL_COLS} FROM "channel"
           WHERE "isArchived" = FALSE{clause}
             AND ($2::varchar IS NULL OR "id" < $2)
             AND ($3::varchar IS NULL OR "id" > $3)
           ORDER BY "id" {dir} LIMIT $5"#
    ))
    .bind(until_id)
    .bind(since_id)
    .bind(&pat)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// endpoints/channels/featured.ts — lastNotedAt DESC, limit 10.
pub async fn list_featured(pool: &PgPool) -> Result<Vec<ChannelRow>, sqlx::Error> {
    sqlx::query_as::<_, ChannelRow>(&format!(
        r#"SELECT {CHANNEL_COLS} FROM "channel"
           WHERE "lastNotedAt" IS NOT NULL AND "isArchived" = FALSE
           ORDER BY "lastNotedAt" DESC LIMIT 10"#
    ))
    .fetch_all(pool)
    .await
}

/// endpoints/channels/followed.ts — channel_following rows paged on
/// `followeeId`, packed via the joined channel rows.
pub async fn list_followed(
    pool: &PgPool,
    me_id: &str,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<ChannelRow>, sqlx::Error> {
    let dir = if since_id.is_some() { "ASC" } else { "DESC" };
    sqlx::query_as::<_, ChannelRow>(&format!(
        r#"SELECT {JOIN_CHANNEL_COLS} FROM "channel_following" cf
           INNER JOIN "channel" c ON c."id" = cf."followeeId"
           WHERE cf."followerId" = $1
             AND ($2::varchar IS NULL OR cf."followeeId" < $2)
             AND ($3::varchar IS NULL OR cf."followeeId" > $3)
           ORDER BY cf."followeeId" {dir} LIMIT $4"#
    ))
    .bind(me_id)
    .bind(until_id)
    .bind(since_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// endpoints/channels/my-favorites.ts — user's channel favorites (joined rows;
/// upstream has no ORDER BY, favorite id orders deterministically here).
pub async fn list_favorites(pool: &PgPool, me_id: &str) -> Result<Vec<ChannelRow>, sqlx::Error> {
    sqlx::query_as::<_, ChannelRow>(&format!(
        r#"SELECT {JOIN_CHANNEL_COLS} FROM "channel_favorite" fav
           INNER JOIN "channel" c ON c."id" = fav."channelId"
           WHERE fav."userId" = $1
           ORDER BY fav."id" ASC"#
    ))
    .bind(me_id)
    .fetch_all(pool)
    .await
}

// ---------------------------------------------------------------------------
// follow / unfollow (ChannelFollowingService)
// ---------------------------------------------------------------------------

/// `ChannelEntityService.pack` me flags.
pub async fn is_following(
    pool: &PgPool,
    me_id: &str,
    channel_id: &str,
) -> Result<bool, sqlx::Error> {
    let n: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "channel_following" WHERE "followerId" = $1 AND "followeeId" = $2"#,
    )
    .bind(me_id)
    .bind(channel_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

pub async fn is_favorited(
    pool: &PgPool,
    me_id: &str,
    channel_id: &str,
) -> Result<bool, sqlx::Error> {
    let n: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "channel_favorite" WHERE "userId" = $1 AND "channelId" = $2"#,
    )
    .bind(me_id)
    .bind(channel_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

/// `channelMutingRepository.exists({ userId, channelId })` — pack flag only
/// (the endpoints are the channels/mute/* slice, not this agent's).
pub async fn is_muting(pool: &PgPool, me_id: &str, channel_id: &str) -> Result<bool, sqlx::Error> {
    let n: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "channel_muting" WHERE "userId" = $1 AND "channelId" = $2"#,
    )
    .bind(me_id)
    .bind(channel_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

/// `ChannelFollowingService.follow` — duplicate → downstream raises
/// `IdentifiableError('6e335e39-…')`; `true` here means already-following.
pub async fn follow(
    pool: &PgPool,
    id: &str,
    me_id: &str,
    channel_id: &str,
) -> Result<bool, sqlx::Error> {
    let res = sqlx::query(
        r#"
        INSERT INTO "channel_following" ("id", "followeeId", "followerId")
        SELECT $1, $2, $3
        WHERE NOT EXISTS (
            SELECT 1 FROM "channel_following" WHERE "followerId" = $3 AND "followeeId" = $2
        )
        "#,
    )
    .bind(id)
    .bind(channel_id)
    .bind(me_id)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() == 0)
}

/// `ChannelFollowingService.unfollow` — no-op when not following.
pub async fn unfollow(pool: &PgPool, me_id: &str, channel_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "channel_following" WHERE "followerId" = $1 AND "followeeId" = $2"#)
        .bind(me_id)
        .bind(channel_id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// favorite / unfavorite (channel_favorite; UNIQUE (userId, channelId) index)
// ---------------------------------------------------------------------------

/// `channels/favorite` — `true` means a duplicate insert (upstream lets the
/// PG unique violation surface as 500 ISE; mirrored by the caller as well).
pub async fn favorite(
    pool: &PgPool,
    id: &str,
    me_id: &str,
    channel_id: &str,
) -> Result<bool, sqlx::Error> {
    let res = sqlx::query(
        r#"
        INSERT INTO "channel_favorite" ("id", "channelId", "userId")
        SELECT $1, $2, $3
        WHERE NOT EXISTS (
            SELECT 1 FROM "channel_favorite" WHERE "userId" = $3 AND "channelId" = $2
        )
        "#,
    )
    .bind(id)
    .bind(channel_id)
    .bind(me_id)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() == 0)
}

pub async fn unfavorite(pool: &PgPool, me_id: &str, channel_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "channel_favorite" WHERE "userId" = $1 AND "channelId" = $2"#)
        .bind(me_id)
        .bind(channel_id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// channels/timeline — db path of timeline.ts getFromDb: channelId filter,
// base note filtering (suspended author always; the my mute/block gates only
// bind when $me is non-null — a NULL muter matches nothing so NOT IN passes,
// same single-statement pattern as note.rs timelines).
// ---------------------------------------------------------------------------

/// Column list mirrors `note.rs NOTE_COLS` (kept local: note.rs is not this
/// agent's file; `NoteRow`/`packers::note::note_pack_min` are still shared).
const NOTE_COLS: &str = r#""id", ("replyId") AS "reply_id", ("renoteId") AS "renote_id", "text", "cw", ("userId") AS "user_id", ("localOnly") AS "local_only", ("visibility"::text) AS "visibility", "mentions", "emojis", "tags", ("hasPoll") AS "has_poll", ("reactionAcceptance"::text) AS "reaction_acceptance", ("reactions"::text) AS "reactions", "renoteCount" AS "renote_count", "repliesCount" AS "replies_count", "fileIds" AS "file_ids", "visibleUserIds" AS "visible_user_ids", "replyUserId" AS "reply_user_id", "clippedCount" AS "clipped_count", "channelId" AS "channel_id", "uri", "url""#;

#[allow(clippy::too_many_arguments)]
pub async fn timeline_notes(
    pool: &PgPool,
    channel_id: &str,
    me_id: Option<&str>,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<crate::note::NoteRow>, sqlx::Error> {
    let dir = if since_id.is_some() { "ASC" } else { "DESC" };
    let sql = format!(
        r#"
        SELECT {NOTE_COLS} FROM "note" WHERE "id" IN (
            SELECT n."id" FROM "note" n
              INNER JOIN "user" u ON u."id" = n."userId"
            WHERE n."channelId" = $1
              AND u."isSuspended" = FALSE
              AND n."userId" NOT IN (SELECT "muteeId" FROM "muting"
                    WHERE "muterId" = $2 AND ("expiresAt" IS NULL OR "expiresAt" > now()))
              AND n."userId" NOT IN (SELECT "blockerId" FROM "blocking" WHERE "blockeeId" = $2)
              AND (n."replyUserId" IS NULL OR n."replyUserId" NOT IN (
                    SELECT "blockerId" FROM "blocking" WHERE "blockeeId" = $2))
              AND (n."renoteUserId" IS NULL OR n."renoteUserId" NOT IN (
                    SELECT "blockerId" FROM "blocking" WHERE "blockeeId" = $2))
              AND ($3::varchar IS NULL OR n."id" < $3)
              AND ($4::varchar IS NULL OR n."id" > $4)
            ORDER BY n."id" {dir} LIMIT $5
        ) ORDER BY "id" {dir}
        "#
    );
    sqlx::query_as::<_, crate::note::NoteRow>(&sql)
        .bind(channel_id)
        .bind(me_id)
        .bind(until_id)
        .bind(since_id)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// `ChannelEntityService.pack`'s pinnedNotes bucket: the pinned note rows in
/// `channel.pinnedNoteIds` order (missing ids dropped, as the
/// `findBy({ id: In(...) })` upstream path does).
pub async fn pinned_notes(
    pool: &PgPool,
    pinned_note_ids: &[String],
) -> Result<Vec<crate::note::NoteRow>, sqlx::Error> {
    let mut rows = sqlx::query_as::<_, crate::note::NoteRow>(&format!(
        r#"SELECT {NOTE_COLS} FROM "note" WHERE "id" = ANY($1)"#
    ))
    .bind(pinned_note_ids.to_vec())
    .fetch_all(pool)
    .await?;
    rows.sort_by_key(|r| {
        pinned_note_ids
            .iter()
            .position(|id| id == &r.id)
            .unwrap_or(usize::MAX)
    });
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_migrations;
    use crate::test_support::fresh_db;

    async fn setup() -> PgPool {
        let pool = fresh_db("mkdb_channels").await;
        run_migrations(&pool).await.expect("migrations");
        pool
    }

    async fn insert_user(pool: &PgPool, id: &str) {
        sqlx::query(
            r#"INSERT INTO "user" ("id", "username", "usernameLower") VALUES ($1, $2, $2)"#,
        )
        .bind(id)
        .bind(format!("u{id}"))
        .execute(pool)
        .await
        .expect("insert user");
    }

    fn ch(id: &str, owner: &str) -> NewChannel<'_> {
        NewChannel {
            id,
            user_id: owner,
            name: "ch",
            description: None,
            banner_id: None,
            color: "#86b300",
            is_sensitive: false,
            allow_renote_to_external: true,
        }
    }

    #[tokio::test]
    async fn follow_cycle_and_duplicate_flag() {
        if std::env::var("DATABASE_URL").is_err() {
            return;
        }
        let p = setup().await;
        let me = "u".repeat(32);
        insert_user(&p, &me).await;
        let cid = "a".repeat(32);
        insert_channel(&p, &ch(&cid, &me)).await.unwrap();

        assert!(
            !follow(&p, &"b".repeat(32), &me, &cid).await.unwrap(),
            "first follow must insert"
        );
        assert!(
            follow(&p, &"c".repeat(32), &me, &cid).await.unwrap(),
            "second follow must hit the duplicate branch"
        );
        assert!(is_following(&p, &me, &cid).await.unwrap());
        assert!(!is_favorited(&p, &me, &cid).await.unwrap());

        let followed = list_followed(&p, &me, None, None, 5).await.unwrap();
        assert_eq!(followed.len(), 1);
        assert_eq!(followed[0].id, cid);

        unfollow(&p, &me, &cid).await.unwrap();
        assert!(!is_following(&p, &me, &cid).await.unwrap());
    }
}
