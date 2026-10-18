//! Note SQL (M1 create/show + M2 notes-slice queries).
//!
//! Timeline WHERE-clauses mirror the db-path queries in the pinned upstream:
//! `QueryService.generateVisibilityQuery` + `generateBaseNoteFilteringQuery`
//! plus each endpoint's own `getFromDb` filters (the Redis fanout path is not
//! ported). Aid ids sort monotonically as plain strings (`misc/id.ts`), so
//! sinceId/untilId paging is a string compare; both bounds are EXCLUSIVE
//! (`makePaginationQuery`), sinceId-only → oldest-first ASC, otherwise
//! newest-first DESC.

use sqlx::PgPool;

pub struct NewNote<'a> {
    pub id: &'a str,
    pub text: Option<&'a str>,
    pub user_id: &'a str,
    pub visibility: &'a str,
    pub reply_id: Option<&'a str>,
    /// reply target author denormals (upstream `replyUserId`/`replyUserHost`;
    /// the visibility clause and the reply filters read these).
    pub reply_user_id: Option<&'a str>,
    pub reply_user_host: Option<&'a str>,
    /// upstream `threadId` = reply.threadId ?? reply.id (reply chains keep the
    /// root id; note_thread_muting keys on it).
    pub thread_id: Option<&'a str>,
    /// resolved mention user ids (upstream `mentionedUsers.map(u => u.id)`).
    pub mentions: &'a [String],
    /// hashtags extracted from text (merged into user.tags)
    pub hashtags: &'a [String],
}

/// Bumps user counters after a note insert (`NoteCreateService`
/// incrementUserNotesCount + tag merge). Hashtag arrays are normalized
/// (lowercase) at the caller.
pub async fn credit_note_to_user(
    pool: &PgPool,
    user_id: &str,
    hashtags: &[String],
) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "user" SET "notesCount" = "notesCount" + 1 WHERE "id" = $1"#)
        .bind(user_id)
        .execute(pool)
        .await?;
    for tag in hashtags {
        sqlx::query(
            r#"UPDATE "user" SET "tags" = array_append("tags", $2)
               WHERE "id" = $1 AND NOT ($2 = ANY("tags"))"#,
        )
        .bind(user_id)
        .bind(tag)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// notes/delete slice spec: the author's `notesCount` is decremented on
/// delete. (Divergence note: upstream `NoteDeleteService.delete` in this
/// pinned tree hard-deletes the row and leaves `user.notesCount` untouched
/// — only the instance counter is decremented; the coordinator's slice pins
/// the user-side decrement.)
pub async fn dec_note_count(pool: &PgPool, user_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "user" SET "notesCount" = "notesCount" - 1 WHERE "id" = $1"#)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn insert_note(pool: &PgPool, n: &NewNote<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO "note"
          ("id", "text", "userId", "replyId", "replyUserId", "replyUserHost", "threadId",
           "localOnly", "renoteCount", "repliesCount", "reactions", "visibility",
           "fileIds", "attachedFileTypes", "visibleUserIds", "mentions", "emojis",
           "tags", "hasPoll", "pageCount")
        VALUES ($1, $2, $3, $4, $5, $6, $7, FALSE, 0, 0, '{}'::jsonb,
                $8::note_visibility_enum, '{}'::varchar[], '{}'::varchar[],
                '{}'::varchar[], $9, '{}'::varchar[], '{}'::varchar[], FALSE, 0)
        "#,
    )
    .bind(n.id)
    .bind(n.text)
    .bind(n.user_id)
    .bind(n.reply_id)
    .bind(n.reply_user_id)
    .bind(n.reply_user_host)
    .bind(n.thread_id)
    .bind(n.visibility)
    .bind(n.mentions)
    .execute(pool)
    .await?;
    Ok(())
}

/// `NoteCreateService.saveReply`: the parent note's `repliesCount` bump.
pub async fn bump_replies_count(
    pool: &PgPool,
    reply_id: &str,
    delta: i16,
) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "note" SET "repliesCount" = "repliesCount" + $2 WHERE "id" = $1"#)
        .bind(reply_id)
        .bind(delta)
        .execute(pool)
        .await?;
    Ok(())
}

/// Columns carried by the detail-with-reactions note pack.
pub const NOTE_COLS: &str = r#""id", ("replyId") AS "reply_id", ("renoteId") AS "renote_id", "text", "cw", ("userId") AS "user_id", ("localOnly") AS "local_only", ("visibility"::text) AS "visibility", "mentions", "emojis", "tags", ("hasPoll") AS "has_poll", ("reactionAcceptance"::text) AS "reaction_acceptance", ("reactions"::text) AS "reactions", "renoteCount" AS "renote_count", "repliesCount" AS "replies_count", "fileIds" AS "file_ids", "visibleUserIds" AS "visible_user_ids", "replyUserId" AS "reply_user_id", "clippedCount" AS "clipped_count", "channelId" AS "channel_id", "uri", "url""#;

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
    pub mentions: Vec<String>,
    pub emojis: Vec<String>,
    pub tags: Vec<String>,
    pub has_poll: bool,
    pub reaction_acceptance: Option<String>,
    pub reactions: String,
    #[sqlx(rename = "renote_count")]
    pub renote_count: i16,
    #[sqlx(rename = "replies_count")]
    pub replies_count: i16,
    #[sqlx(rename = "file_ids")]
    pub file_ids: Vec<String>,
    #[sqlx(rename = "visible_user_ids")]
    pub visible_user_ids: Vec<String>,
    #[sqlx(rename = "reply_user_id")]
    pub reply_user_id: Option<String>,
    #[sqlx(rename = "clipped_count")]
    pub clipped_count: i16,
    #[sqlx(rename = "channel_id")]
    pub channel_id: Option<String>,
    pub uri: Option<String>,
    pub url: Option<String>,
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

/// `noteThreadMutingsRepository.exists({ userId, threadId })` — gates mention
/// and reply notifications upstream (NoteCreateService).
pub async fn thread_muted(
    pool: &PgPool,
    user_id: &str,
    thread_id: &str,
) -> Result<bool, sqlx::Error> {
    let n: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "note_thread_muting" WHERE "userId" = $1 AND "threadId" = $2"#,
    )
    .bind(user_id)
    .bind(thread_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

// ---------------------------------------------------------------------------
// notes/delete — upstream NoteDeleteService.delete hard-DELETEs the row (the
// pinned schema has NO `note.deletedAt` column at all, so there is no
// soft-delete state to set; "note vanish" is a physical DELETE), after
// decrementing the parent's repliesCount for replies.
// ---------------------------------------------------------------------------

/// `notesRepository.delete({ id, userId })` — author-scoped: 0 rows affected
/// when the id exists but the caller is not the author.
pub async fn delete_note(
    pool: &PgPool,
    note_id: &str,
    author_id: &str,
) -> Result<u64, sqlx::Error> {
    let res = sqlx::query(r#"DELETE FROM "note" WHERE "id" = $1 AND "userId" = $2"#)
        .bind(note_id)
        .bind(author_id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}

// ---------------------------------------------------------------------------
// notes/reactions — note_reaction row + the packed `note.reactions` jsonb
// counter (`ReactionService.create/delete` plain-SQL path; the reactions-
// buffering Redis mode stays off by default upstream).
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
pub struct NoteReactionRow {
    pub id: String,
    #[sqlx(rename = "user_id")]
    pub user_id: String,
    pub reaction: String,
}

pub async fn find_note_reaction(
    pool: &PgPool,
    note_id: &str,
    user_id: &str,
) -> Result<Option<NoteReactionRow>, sqlx::Error> {
    sqlx::query_as::<_, NoteReactionRow>(
        r#"SELECT "id", ("userId") AS "user_id", "reaction" FROM "note_reaction"
           WHERE "noteId" = $1 AND "userId" = $2"#,
    )
    .bind(note_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

pub async fn insert_note_reaction(
    pool: &PgPool,
    id: &str,
    user_id: &str,
    note_id: &str,
    reaction: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO "note_reaction" ("id", "userId", "noteId", "reaction") VALUES ($1, $2, $3, $4)"#,
    )
    .bind(id)
    .bind(user_id)
    .bind(note_id)
    .bind(reaction)
    .execute(pool)
    .await?;
    Ok(())
}

/// `ReactionService.delete` repo part; true when exactly one row was removed.
pub async fn delete_note_reaction(
    pool: &PgPool,
    note_id: &str,
    user_id: &str,
) -> Result<bool, sqlx::Error> {
    let res = sqlx::query(r#"DELETE FROM "note_reaction" WHERE "noteId" = $1 AND "userId" = $2"#)
        .bind(note_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() == 1)
}

/// jsonb counter bump mirroring the upstream update-builder expressions
/// `jsonb_set("reactions", ARRAY[reaction], (COALESCE("reactions"->>reaction,'0')::int ± 1)::text::jsonb)`.
/// Zero entries stay in the map (upstream leaves them; pack filters count<=0).
pub async fn bump_reactions_map(
    pool: &PgPool,
    note_id: &str,
    reaction: &str,
    delta: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE "note" SET "reactions" =
            jsonb_set("reactions", ARRAY[$2],
                (COALESCE("reactions"->>$2, '0')::int + $3)::text::jsonb)
        WHERE "id" = $1
        "#,
    )
    .bind(note_id)
    .bind(reaction)
    .bind(delta)
    .execute(pool)
    .await?;
    Ok(())
}

/// notes/reactions list rows. Exclusive aid paging; since-only → ASC.
#[allow(clippy::too_many_arguments)]
pub async fn list_note_reactions(
    pool: &PgPool,
    note_id: &str,
    reaction_type: Option<&str>,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<NoteReactionRow>, sqlx::Error> {
    let dir = if since_id.is_some() { "ASC" } else { "DESC" };
    sqlx::query_as::<_, NoteReactionRow>(&format!(
        r#"
        SELECT "id", ("userId") AS "user_id", "reaction" FROM "note_reaction"
        WHERE "noteId" = $1
          AND ($2::varchar IS NULL OR "reaction" = $2)
          AND ($3::varchar IS NULL OR "id" < $3)
          AND ($4::varchar IS NULL OR "id" > $4)
        ORDER BY "id" {dir} LIMIT $5
        "#,
    ))
    .bind(note_id)
    .bind(reaction_type)
    .bind(until_id)
    .bind(since_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

// ---------------------------------------------------------------------------
// timelines / replies / renotes — db-path queries. Each inner select joins
// "user" (needed for the suspension filter; also the pack-lite user) and the
// outer single-table select applies NOTE_COLS.
//
// Parameter maps (all fixed):
//   timeline:  $1 meOrFolloweeIds $2 me $3 [me] $4 withFiles $5 excludePureRenotes
//              $6 untilId $7 sinceId $8 limit
//   local:     $1 untilId $2 sinceId $3 limit $4 withFiles $5 withReplies $6 me(Null)
//   hybrid:    $1 meOrFolloweeIds $2 me $3 [me] $4 withFiles $5 withReplies
//              $6 untilId $7 sinceId $8 limit
//   replies/renotes: $1 targetId $2 me(Null) $3 [me] $4 myFollowees $5 untilId
//              $6 sinceId $7 limit
// ---------------------------------------------------------------------------

const INNER_JOIN_USER: &str =
    r#"SELECT n."id" FROM "note" n INNER JOIN "user" u ON u."id" = n."userId" WHERE "#;

/// 返信ではない、または投稿者自身への返信 (applied unless withReplies).
const NOT_FOREIGN_REPLY: &str = r#" AND (n."replyId" IS NULL OR n."replyUserId" = n."userId") "#;

const WITH_FILES: &str = r#" AND n."fileIds" != '{}' "#;

/// excludePureRenotes: keep non-renotes and quotes (text/files/poll present).
const EXCLUDE_PURE_RENOTES: &str = r#" AND (n."renoteId" IS NULL OR n."text" IS NOT NULL
            OR n."fileIds" != '{}' OR EXISTS (SELECT 1 FROM "poll" WHERE "poll"."noteId" = n."id")) "#;

/// `QueryService.generateVisibilityQuery` for a logged-in viewer ($ME id,
/// $MELIST [me], $FOL myFollowees).
const VISIBILITY_ME: &str = r#"
        AND (
          (n."visibility" IN ('public','home'))
          OR n."userId" = $ME
          OR ($MELIST::varchar[] <@ n."visibleUserIds")
          OR ($MELIST::varchar[] <@ n."mentions")
          OR (
            n."visibility" = 'followers'
            AND (n."userId" = ANY($FOL) OR n."replyUserId" = $ME)
          )
        )"#;

/// generateVisibilityQuery guest side (home visibility is public upstream).
const VISIBILITY_GUEST: &str = r#" AND n."visibility" IN ('public','home') "#;

/// generateBaseNoteFilteringQuery: suspended-author gate always; my muted /
/// blocked authors (plus reply/renote author blocks) for logged-in viewers.
const BASE_ME: &str = r#"
        AND u."isSuspended" = FALSE
        AND n."userId" NOT IN (SELECT "muteeId" FROM "muting" WHERE "muterId" = $ME
              AND ("expiresAt" IS NULL OR "expiresAt" > now()))
        AND n."userId" NOT IN (SELECT "blockerId" FROM "blocking" WHERE "blockeeId" = $ME)
        AND (n."replyUserId" IS NULL OR n."replyUserId" NOT IN (
            SELECT "blockerId" FROM "blocking" WHERE "blockeeId" = $ME))
        AND (n."renoteUserId" IS NULL OR n."renoteUserId" NOT IN (
            SELECT "blockerId" FROM "blocking" WHERE "blockeeId" = $ME))"#;

const BASE_GUEST: &str = r#" AND u."isSuspended" = FALSE"#;

const PAGER: &str = r#" AND ($UNTIL::varchar IS NULL OR n."id" < $UNTIL)
            AND ($SINCE::varchar IS NULL OR n."id" > $SINCE)
            ORDER BY n."id" {DIR} LIMIT $LIMIT"#;

/// Token substitution for the numbered placeholders of the enclosing query.
fn expand(
    sql: &str,
    p_me: u8,
    p_me_list: u8,
    p_followees: u8,
    p_until: u8,
    p_since: u8,
    p_limit: u8,
    dir: &str,
) -> String {
    sql.replace("$ME", &format!("${p_me}"))
        .replace("$MELIST", &format!("${p_me_list}"))
        .replace("$FOL", &format!("${p_followees}"))
        .replace("$UNTIL", &format!("${p_until}"))
        .replace("$SINCE", &format!("${p_since}"))
        .replace("$LIMIT", &format!("${p_limit}"))
        .replace("{DIR}", dir)
}

fn dir_asc(since_id: Option<&str>) -> &'static str {
    if since_id.is_some() {
        "ASC"
    } else {
        "DESC"
    }
}

fn wrap_outer(inner: &str, asc: bool) -> String {
    let dir = if asc { "ASC" } else { "DESC" };
    format!(
        r#"SELECT {} FROM "note" WHERE "id" IN ({}) ORDER BY "id" {dir}"#,
        NOTE_COLS, inner
    )
}

/// notes/timeline db path: own + followees (channel rows excluded), visibility
/// filtered per the viewer, suspended/muted/blocked authors gated.
#[allow(clippy::too_many_arguments)]
pub async fn timeline_notes(
    pool: &PgPool,
    me_id: &str,
    followee_ids: &[String],
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
    with_files: bool,
    with_renotes: bool,
) -> Result<Vec<NoteRow>, sqlx::Error> {
    let mut me_or_followees: Vec<String> = Vec::with_capacity(followee_ids.len() + 1);
    me_or_followees.push(me_id.to_string());
    me_or_followees.extend(followee_ids.iter().cloned());
    let me_list = vec![me_id.to_string()];
    let asc = since_id.is_some();

    let mut inner = String::from(INNER_JOIN_USER);
    inner.push_str(r#"n."channelId" IS NULL AND n."userId" = ANY($1)"#);
    inner.push_str(NOT_FOREIGN_REPLY);
    inner.push_str(VISIBILITY_ME);
    inner.push_str(BASE_ME);
    if with_files {
        inner.push_str(WITH_FILES);
    }
    if !with_renotes {
        inner.push_str(EXCLUDE_PURE_RENOTES);
    }
    inner.push_str(PAGER);

    // $1 meOrFolloweeIds $2 me $3 [me] $4 withFiles $5 excludePureRenotes
    // $6 untilId $7 sinceId $8 limit
    let inner = expand(&inner, 2, 3, 1, 6, 7, 8, dir_asc(since_id));

    sqlx::query_as::<_, NoteRow>(&wrap_outer(&inner, asc))
        .bind(me_or_followees)
        .bind(me_id)
        .bind(me_list)
        .bind(with_files)
        .bind(!with_renotes)
        .bind(until_id)
        .bind(since_id)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// notes/local-timeline db path: `(visibility='public') AND (userHost IS NULL)
/// AND (channelId IS NULL)`; guest-visible (no requireCredential in the pinned
/// upstream endpoint meta). Muted / blocked gates apply only for logged-in
/// viewers. Upstream's dbFallback does NOT apply withRenotes here (only the
/// fanout path excludes pure renotes) — mirrored as-is.
#[allow(clippy::too_many_arguments)]
pub async fn local_timeline_notes(
    pool: &PgPool,
    me_id: Option<&str>,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
    with_files: bool,
    with_replies: bool,
) -> Result<Vec<NoteRow>, sqlx::Error> {
    let asc = since_id.is_some();

    let mut inner = String::from(INNER_JOIN_USER);
    inner.push_str(
        r#"(n."visibility" = 'public') AND (n."userHost" IS NULL) AND (n."channelId" IS NULL)"#,
    );
    // generateBaseNoteFilteringQuery is called unconditionally in this
    // endpoint. The mute/block sub-selects degrade to no-ops when $me binds
    // NULL (subquery matches nothing → NOT IN passes), so a single SQL covers
    // guests and logged-in viewers.
    inner.push_str(BASE_ME);
    inner.push_str(BASE_GUEST);
    if with_files {
        inner.push_str(WITH_FILES);
    }
    if !with_replies {
        inner.push_str(NOT_FOREIGN_REPLY);
    }
    inner.push_str(PAGER);

    // $1 untilId $2 sinceId $3 limit $4 withFiles $5 withReplies $6 me
    let inner = expand(&inner, 6, 6, 1, 1, 2, 3, dir_asc(since_id));

    sqlx::query_as::<_, NoteRow>(&wrap_outer(&inner, asc))
        .bind(until_id)
        .bind(since_id)
        .bind(limit)
        .bind(with_files)
        .bind(with_replies)
        .bind(me_id)
        .fetch_all(pool)
        .await
}

/// notes/hybrid-timeline db path: own + followees ∪ local public, channel rows
/// excluded. Visibility/mute/block gates per the viewer.
#[allow(clippy::too_many_arguments)]
pub async fn hybrid_timeline_notes(
    pool: &PgPool,
    me_id: &str,
    followee_ids: &[String],
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
    with_files: bool,
    with_replies: bool,
) -> Result<Vec<NoteRow>, sqlx::Error> {
    let mut me_or_followees: Vec<String> = Vec::with_capacity(followee_ids.len() + 1);
    me_or_followees.push(me_id.to_string());
    me_or_followees.extend(followee_ids.iter().cloned());
    let me_list = vec![me_id.to_string()];
    let asc = since_id.is_some();

    let mut inner = String::from(INNER_JOIN_USER);
    inner.push_str(
        r#"( ( n."userId" = ANY($1) OR ((n."visibility" = 'public') AND (n."userHost" IS NULL)) )
             AND n."channelId" IS NULL )"#,
    );
    inner.push_str(VISIBILITY_ME);
    inner.push_str(BASE_ME);
    if with_files {
        inner.push_str(WITH_FILES);
    }
    if !with_replies {
        inner.push_str(NOT_FOREIGN_REPLY);
    }
    inner.push_str(PAGER);

    // $1 meOrFolloweeIds $2 me $3 [me] $4 withFiles $5 withReplies
    // $6 untilId $7 sinceId $8 limit
    let inner = expand(&inner, 2, 3, 1, 6, 7, 8, dir_asc(since_id));

    sqlx::query_as::<_, NoteRow>(&wrap_outer(&inner, asc))
        .bind(me_or_followees)
        .bind(me_id)
        .bind(me_list)
        .bind(with_files)
        .bind(with_replies)
        .bind(until_id)
        .bind(since_id)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// notes/replies db path: children of the given note; visibility + base
/// filtering per viewer (guests see public/home only).
#[allow(clippy::too_many_arguments)]
pub async fn replies_of_note(
    pool: &PgPool,
    reply_id: &str,
    me_id: Option<&str>,
    my_followees: &[String],
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<NoteRow>, sqlx::Error> {
    let me_list = me_id.map(|m| vec![m.to_string()]).unwrap_or_default();
    let asc = since_id.is_some();

    let mut inner = String::from(INNER_JOIN_USER);
    inner.push_str(r#"n."replyId" = $1"#);
    if me_id.is_some() {
        inner.push_str(VISIBILITY_ME);
        inner.push_str(BASE_ME);
    } else {
        inner.push_str(VISIBILITY_GUEST);
        inner.push_str(BASE_GUEST);
    }
    inner.push_str(PAGER);

    // $1 replyId $2 me $3 [me] $4 myFollowees $5 untilId $6 sinceId $7 limit
    let inner = expand(&inner, 2, 3, 4, 5, 6, 7, dir_asc(since_id));

    sqlx::query_as::<_, NoteRow>(&wrap_outer(&inner, asc))
        .bind(reply_id)
        .bind(me_id)
        .bind(me_list)
        .bind(my_followees)
        .bind(until_id)
        .bind(since_id)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// notes/renotes db path: rows whose renoteId is the given note (the endpoint
/// raises NO_SUCH_NOTE on the target before the query).
#[allow(clippy::too_many_arguments)]
pub async fn renotes_of_note(
    pool: &PgPool,
    renote_id: &str,
    me_id: Option<&str>,
    my_followees: &[String],
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<NoteRow>, sqlx::Error> {
    let me_list = me_id.map(|m| vec![m.to_string()]).unwrap_or_default();
    let asc = since_id.is_some();

    let mut inner = String::from(INNER_JOIN_USER);
    inner.push_str(r#"n."renoteId" = $1"#);
    if me_id.is_some() {
        inner.push_str(VISIBILITY_ME);
        inner.push_str(BASE_ME);
    } else {
        inner.push_str(VISIBILITY_GUEST);
        inner.push_str(BASE_GUEST);
    }
    inner.push_str(PAGER);

    // $1 renoteId $2 me $3 [me] $4 myFollowees $5 untilId $6 sinceId $7 limit
    let inner = expand(&inner, 2, 3, 4, 5, 6, 7, dir_asc(since_id));

    sqlx::query_as::<_, NoteRow>(&wrap_outer(&inner, asc))
        .bind(renote_id)
        .bind(me_id)
        .bind(me_list)
        .bind(my_followees)
        .bind(until_id)
        .bind(since_id)
        .bind(limit)
        .fetch_all(pool)
        .await
}
