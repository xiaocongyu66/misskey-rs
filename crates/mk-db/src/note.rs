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
    /// `cw` param (upstream note.cw; null when unset).
    pub cw: Option<&'a str>,
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
    /// hashtags extracted from text (insertNote L661 `tags` — merged into
    /// user.tags by the caller's credit pass; the hashtag streaming channel
    /// and users/search read the note-row column).
    pub hashtags: &'a [String],
    /// `fileIds` (md-order preserved; upstream also denormalizes
    /// `attachedFileTypes` from the fetched drive rows).
    pub file_ids: &'a [String],
    pub attached_file_types: &'a [String],
    /// `visibleUserIds` for visibility='specified' rows only (upstream inserts
    /// an empty array otherwise).
    pub visible_user_ids: &'a [String],
    /// `hasPoll` flag (+ the poll row itself lives in mk_db::poll).
    pub has_poll: bool,
    /// `localOnly` param (reply/renote of a localOnly row inherits it).
    pub local_only: bool,
    /// chan-note2: channelId — the insert lane of the channel legs
    /// (NoteCreateService insertNote `channelId: data.channel
    /// ? data.channel.id : null`, L651); the channel TL + the home/hybrid
    /// followed-channel folds key off this column.
    pub channel_id: Option<&'a str>,
}

/// Bumps user counters after a note insert (`NoteCreateService`
/// incNotesCountOfUser: `updatedAt: new Date()` rides the same update —
/// the users/search activeness legs key off `user.updatedAt` — + tag
/// merge). Hashtag arrays are normalized (lowercase) at the caller.
pub async fn credit_note_to_user(
    pool: &PgPool,
    user_id: &str,
    hashtags: &[String],
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"UPDATE "user" SET "notesCount" = "notesCount" + 1, "updatedAt" = now() WHERE "id" = $1"#,
    )
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
    // tl-remote: the denormalized `note.userHost` rides the insert the way
    // NoteCreateService.ts insertNote writes `userHost: user.host` — the
    // local/social timeline planes' `(visibility='public') AND (userHost
    // IS NULL)` gates (timelines.ts L1172/L1747 リモートユーザーのノートが
    // 含まれない + the Social TL 凍結(リモート) L2204 leg) read this column.
    // Sub-select keeps the author host verbatim from the user row (same
    // value the signup test-mode host legs store).
    // tags: NOTE_TAGS — insertNote L645-661 writes `tags: tags.map(tag =>
    // normalizeForSearch(tag))` on the row (the hashtag channel lane and
    // users/search read it; it previously never left the user-tags merge).
    sqlx::query(
        r#"
        INSERT INTO "note"
          ("id", "text", "userId", "replyId", "replyUserId", "replyUserHost", "threadId",
           "localOnly", "renoteCount", "repliesCount", "reactions", "visibility",
           "fileIds", "attachedFileTypes", "visibleUserIds", "mentions", "emojis",
           "tags", "hasPoll", "pageCount", "cw", "channelId", "userHost")
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 0, 0, '{}'::jsonb,
                $9::note_visibility_enum, $10, $11, $12, $13, '{}'::varchar[],
                $14, $15, 0, $16, $17,
                (SELECT "host" FROM "user" WHERE "id" = $3))
        "#,
    )
    .bind(n.id)
    .bind(n.text)
    .bind(n.user_id)
    .bind(n.reply_id)
    .bind(n.reply_user_id)
    .bind(n.reply_user_host)
    .bind(n.thread_id)
    .bind(n.local_only)
    .bind(n.visibility)
    .bind(n.file_ids)
    .bind(n.attached_file_types)
    .bind(n.visible_user_ids)
    .bind(n.mentions)
    .bind(n.hashtags) // NOTE_TAGS ($14)
    .bind(n.has_poll)
    .bind(n.cw)
    .bind(n.channel_id) // chan-note2
    .execute(pool)
    .await?;
    // upstream NoteCreateService's per-user chart bump (PerUserNotesChart):
    // hour + day twins, best-effort (a chart failure must not fail the note)
    let now = crate::charts::now_ms();
    let deltas: [(&str, i32); 5] = [
        ("total", 1),
        ("inc", 1),
        ("dec", 0),
        ("diffs_normal", if n.reply_id.is_none() { 1 } else { 0 }),
        ("diffs_reply", if n.reply_id.is_some() { 1 } else { 0 }),
    ];
    for span in ["hour", "day"] {
        if let Err(e) = crate::charts::update_per_user(
            pool,
            "notes",
            span,
            crate::charts::bucket(span, now),
            n.user_id,
            &deltas,
        )
        .await
        {
            tracing::warn!(target: "charts", error = %e, "per-user notes chart bump failed");
        }
    }
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
/// notes/replies.ts: direct replies of one note, id paging (sinceId → ASC
/// else DESC), dense $1..$4 in bind order (reply, until, since, limit) —
/// the same positional-fill trap search_channels hit.
pub async fn find_replies(
    pool: &PgPool,
    reply_id: &str,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<NoteRow>, sqlx::Error> {
    let dir = if since_id.is_some() { "ASC" } else { "DESC" };
    sqlx::query_as::<_, NoteRow>(&format!(
        r#"SELECT {NOTE_COLS} FROM "note"
           WHERE "replyId" = $1
             AND ($2::varchar IS NULL OR "id" < $2)
             AND ($3::varchar IS NULL OR "id" > $3)
           ORDER BY "id" {dir} LIMIT $4"#
    ))
    .bind(reply_id)
    .bind(until_id)
    .bind(since_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

pub const NOTE_COLS: &str = r#""id", ("replyId") AS "reply_id", ("renoteId") AS "renote_id", "text", "cw", ("userId") AS "user_id", "userHost" AS "user_host", ("localOnly") AS "local_only", ("visibility"::text) AS "visibility", "mentions", "emojis", "tags", ("hasPoll") AS "has_poll", ("reactionAcceptance"::text) AS "reaction_acceptance", ("reactions"::text) AS "reactions", "renoteCount" AS "renote_count", "repliesCount" AS "replies_count", "fileIds" AS "file_ids", "visibleUserIds" AS "visible_user_ids", "replyUserId" AS "reply_user_id", "threadId" AS "thread_id", "clippedCount" AS "clipped_count", "channelId" AS "channel_id", "uri", "url""#;

#[derive(Debug, sqlx::FromRow)]
pub struct NoteRow {
    pub id: String,
    pub reply_id: Option<String>,
    pub renote_id: Option<String>,
    pub text: Option<String>,
    pub cw: Option<String>,
    pub user_id: String,
    /// Denormalized author host (None → local); drives the pack `emojis`
    /// presence rule and the remote-word checks.
    #[sqlx(rename = "user_host")]
    pub user_host: Option<String>,
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
    /// reply-chain root id (upstream `threadId` = reply.threadId ?? reply.id;
    /// the thread-mute gates and the notification legs key on it).
    #[sqlx(rename = "thread_id")]
    pub thread_id: Option<String>,
    #[sqlx(rename = "clipped_count")]
    pub clipped_count: i16,
    #[sqlx(rename = "channel_id")]
    pub channel_id: Option<String>,
    pub uri: Option<String>,
    pub url: Option<String>,
}

/// Author-host scalar (mention fallback `m.host ?? user.host`).
/// Local users carry `host = NULL` — the scalar MUST decode as an Option
/// (query_scalar + fetch_optional otherwise type-params the scalar as plain
/// `String` and 500s every local-note creation; e2e-verified on drive.ts).
pub async fn user_host_of(pool: &PgPool, user_id: &str) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, Option<String>>(r#"SELECT "host" FROM "user" WHERE "id" = $1"#)
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .map(|rows| rows.flatten())
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

/// Upstream `note.threadId ?? note.id` scalar (NotesCreateService.compute
/// thread id; the thread-mute insert key and the NotificationService gate
/// key both derive from the REPLY target's threadId ?? id).
pub async fn thread_key(pool: &PgPool, note_id: &str) -> Result<Option<String>, sqlx::Error> {
    // thread-cap: "threadId" is nullable — the untyped query_scalar type-params
    // column 0 as plain String, so every ROOT note (threadId IS NULL) died with
    // `unexpected null` and thread-muting/create 500'd before any mute row
    // landed (census run 39199419722: both thread-mute.ts cases asserted
    // absence against a gate that never armed). user_host_of below documents
    // the same trap. Explicit Option scalar + flatten: None = no row (caller
    // falls back to note.id), flattened None = NULL threadId (same fallback).
    sqlx::query_scalar::<_, Option<String>>(r#"SELECT "threadId" FROM "note" WHERE "id" = $1"#)
        .bind(note_id)
        .fetch_optional(pool)
        .await
        .map(|row| row.flatten())
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

/// `notes/thread-muting/create` repo part
/// (`noteThreadMutingsRepository.insert({ id, threadId, userId })` — upstream
/// gen()s the id; the table carries no createdAt).
pub async fn insert_thread_muting(
    pool: &PgPool,
    id: &str,
    user_id: &str,
    thread_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO "note_thread_muting" ("id", "userId", "threadId") VALUES ($1, $2, $3)"#,
    )
    .bind(id)
    .bind(user_id)
    .bind(thread_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// `notes/thread-muting/delete` repo part
/// (`noteThreadMutingsRepository.delete({ threadId, userId })`); returns rows
/// affected (upstream ignores the count).
pub async fn delete_thread_muting(
    pool: &PgPool,
    user_id: &str,
    thread_id: &str,
) -> Result<u64, sqlx::Error> {
    let r =
        sqlx::query(r#"DELETE FROM "note_thread_muting" WHERE "userId" = $1 AND "threadId" = $2"#)
            .bind(user_id)
            .bind(thread_id)
            .execute(pool)
            .await?;
    Ok(r.rows_affected())
}

// ---------------------------------------------------------------------------
// notes/delete — upstream NoteDeleteService.delete hard-DELETEs the row (the
// pinned schema has NO `note.deletedAt` column at all, so there is no
// soft-delete state to set; "note vanish" is a physical DELETE), after
// decrementing the parent's repliesCount for replies.
// ---------------------------------------------------------------------------

/// `NoteDeleteService.delete` repo part: hard-DELETE the author's row, then
/// decrement the parent note's `repliesCount` when the deleted row was a
/// reply (upstream `notesRepository.decrement({ id: note.replyId },
/// 'repliesCount', 1)`). Returns rows affected.
pub async fn delete_note(
    pool: &PgPool,
    note_id: &str,
    author_id: &str,
) -> Result<u64, sqlx::Error> {
    let reply_id: Option<Option<String>> = sqlx::query_scalar(
        r#"DELETE FROM "note" WHERE "id" = $1 AND "userId" = $2 RETURNING "replyId""#,
    )
    .bind(note_id)
    .bind(author_id)
    .fetch_optional(pool)
    .await?;
    let Some(affected) = reply_id else {
        return Ok(0);
    };
    if let Some(parent) = affected {
        let _ = bump_replies_count(pool, &parent, -1).await;
    }
    Ok(1)
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
/// $MELIST must be rewritten BEFORE $ME — `$ME` is a prefix of `$MELIST`, so
/// the reverse order mangles `$MELIST` into `$2LIST` (postgres: "trailing
/// junk after parameter").
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
    sql.replace("$MELIST", &format!("${p_me_list}"))
        .replace("$ME", &format!("${p_me}"))
        .replace("$FOL", &format!("${p_followees}"))
        .replace("$UNTIL", &format!("${p_until}"))
        .replace("$SINCE", &format!("${p_since}"))
        .replace("$LIMIT", &format!("${p_limit}"))
        .replace("{DIR}", dir)
}

fn dir_asc(since_id: Option<&str>) -> &'static str {
    if since_id.is_some() { "ASC" } else { "DESC" }
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

/// `endpoints/notes.ts` (the global list): db fallback for the
/// note.ts census cases — `visibility='public'` + `localOnly=FALSE` rows with
/// the reply/renote/withFiles/poll toggles and the optional `local`
/// userHost IS NULL filter. Aid paging: EXCLUSIVE bounds, sinceId-only → ASC.
#[allow(clippy::too_many_arguments)]
pub async fn global_note_ids(
    pool: &PgPool,
    local: bool,
    reply: Option<bool>,
    renote: Option<bool>,
    with_files: Option<bool>,
    poll: Option<bool>,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<String>, sqlx::Error> {
    let dir = if since_id.is_some() { "ASC" } else { "DESC" };
    let mut sql = String::from(
        r#"SELECT n."id" FROM "note" n WHERE n."visibility" = 'public' AND n."localOnly" = FALSE"#,
    );
    if local {
        sql.push_str(r#" AND n."userHost" IS NULL"#);
    }
    if let Some(r) = reply {
        sql.push_str(if r {
            r#" AND n."replyId" IS NOT NULL"#
        } else {
            r#" AND n."replyId" IS NULL"#
        });
    }
    if let Some(r) = renote {
        sql.push_str(if r {
            r#" AND n."renoteId" IS NOT NULL"#
        } else {
            r#" AND n."renoteId" IS NULL"#
        });
    }
    if let Some(w) = with_files {
        sql.push_str(if w {
            r#" AND n."fileIds" != '{}'"#
        } else {
            r#" AND n."fileIds" = '{}'"#
        });
    }
    if let Some(p) = poll {
        sql.push_str(if p {
            r#" AND n."hasPoll" = TRUE"#
        } else {
            r#" AND n."hasPoll" = FALSE"#
        });
    }
    sql.push_str(
        r#" AND ($1::varchar IS NULL OR n."id" > $1)
            AND ($2::varchar IS NULL OR n."id" < $2)
            ORDER BY n."id" "#,
    );
    sql.push_str(dir);
    sql.push_str(r#" LIMIT $3"#);
    sqlx::query_scalar(&sql)
        .bind(since_id)
        .bind(until_id)
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

// ---------------------------------------------------------------------------
// meili-min: notes/search + notes/search-by-tag SQL fallback lanes. The SQL
// branch IS upstream's no-Meili default (`fulltextSearch.provider: 'sqlLike'`
// → SearchService.searchNoteByLike) for notes/search, and the direct TypeORM
// query of endpoints/notes/search-by-tag.ts for the tag arm.
// ---------------------------------------------------------------------------

/// `misc/sql-like-escape.ts` — tames backslash / % / _ before the caller's
/// fragment is wrapped with `%..%` (search_channels keeps the same inline
/// trio; shared here so both search lanes escape from one source).
pub fn escape_like(q: &str) -> String {
    q.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// notes/search db path: `"text" ILIKE '%q%'` over public/home rows with the
/// optional host (`'.'` = local → userHost IS NULL) / userId / channelId
/// filters and the aid paging tail + OFFSET/LIMIT. Upstream orders via
/// makePaginationQuery (EXCLUSIVE bounds, sinceId-only → ASC else DESC) and
/// searches note.text only in the sqlLike provider.
/// userId/channelId mirror the upstream else-if (`if userId … else if
/// channelId`): a set $3 (userId) masks the $4 arm entirely.
/// Deviation pinned by the slice: viewer visibility degrades to the guest
/// lane (`IN ('public','home')`) and only the always-on leg of
/// generateBaseNoteFilteringQuery (suspended authors) is carried — the
/// me-aware mute/block sub-selects are TODO with the full visibility walk.
#[allow(clippy::too_many_arguments)]
pub async fn search_notes(
    pool: &PgPool,
    q_escaped: &str,
    host: Option<&str>,
    user_id: Option<&str>,
    channel_id: Option<&str>,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<NoteRow>, sqlx::Error> {
    let pat = format!("%{q_escaped}%");
    let dir = if since_id.is_some() { "ASC" } else { "DESC" };
    // $1 pattern $2 host ('.' sentinel folds the IS NULL arm) $3 userId
    // $4 channelId $5 offset $6 limit — dense placeholders (the
    // search_channels positional-fill trap).
    sqlx::query_as::<_, NoteRow>(&format!(
        r#"SELECT {NOTE_COLS} FROM "note"
           WHERE "text" ILIKE $1
             AND "visibility" IN ('public','home')
             AND "userId" NOT IN (SELECT "id" FROM "user" WHERE "isSuspended" = TRUE)
             AND ($2::varchar IS NULL OR ($2 = '.' AND "userHost" IS NULL) OR "userHost" = $2)
             AND ($3::varchar IS NULL OR "userId" = $3)
             AND ($4::varchar IS NULL OR $3::varchar IS NOT NULL OR "channelId" = $4)
           ORDER BY "id" {dir} OFFSET $5 LIMIT $6"#
    ))
    .bind(&pat)
    .bind(host)
    .bind(user_id)
    .bind(channel_id)
    .bind(offset)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// notes/search-by-tag db path (the single-`tag` branch): exact tag
/// membership `$1 = ANY("tags")` over public/home rows. Upstream matches
/// `:tag <@ note.tags` after normalizeForSearch — lowercasing here (the
/// insert lane stores lowercased tags, users_by_tag keeps the same fold).
#[allow(clippy::too_many_arguments)]
pub async fn search_notes_by_tag(
    pool: &PgPool,
    tag: &str,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<NoteRow>, sqlx::Error> {
    let dir = if since_id.is_some() { "ASC" } else { "DESC" };
    // $1 tag $2 untilId $3 sinceId $4 offset $5 limit — dense placeholders.
    sqlx::query_as::<_, NoteRow>(&format!(
        r#"SELECT {NOTE_COLS} FROM "note"
           WHERE $1 = ANY("tags")
             AND "visibility" IN ('public','home')
             AND "userId" NOT IN (SELECT "id" FROM "user" WHERE "isSuspended" = TRUE)
             AND ($2::varchar IS NULL OR "id" < $2)
             AND ($3::varchar IS NULL OR "id" > $3)
           ORDER BY "id" {dir} OFFSET $4 LIMIT $5"#
    ))
    .bind(tag.to_lowercase())
    .bind(until_id)
    .bind(since_id)
    .bind(offset)
    .bind(limit)
    .fetch_all(pool)
    .await
}

// ---------------------------------------------------------------------------
// notes-more batch (endpoints/notes/*): favorites create/delete, children,
// state and unrenote. `note_favorite` ships in the 0001 baseline.
// ---------------------------------------------------------------------------

/// `noteFavoritesRepository.exists({ noteId, userId })`.
pub async fn favorite_exists(
    pool: &PgPool,
    user_id: &str,
    note_id: &str,
) -> Result<bool, sqlx::Error> {
    let n: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "note_favorite" WHERE "userId" = $1 AND "noteId" = $2"#,
    )
    .bind(user_id)
    .bind(note_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

/// `noteFavoritesRepository.insert({ id, noteId, userId })` (no createdAt —
/// the timestamp is recovered from the aid).
pub async fn insert_note_favorite(
    pool: &PgPool,
    id: &str,
    user_id: &str,
    note_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO "note_favorite" ("id", "userId", "noteId") VALUES ($1, $2, $3)"#,
    )
    .bind(id)
    .bind(user_id)
    .bind(note_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// `findOneBy({ noteId, userId })` — the favorite row id (delete key).
pub async fn find_note_favorite_id(
    pool: &PgPool,
    user_id: &str,
    note_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        r#"SELECT "id" FROM "note_favorite" WHERE "userId" = $1 AND "noteId" = $2 LIMIT 1"#,
    )
    .bind(user_id)
    .bind(note_id)
    .fetch_optional(pool)
    .await
}

/// `noteFavoritesRepository.delete(exist.id)` — affected-row count.
pub async fn delete_note_favorite(pool: &PgPool, id: &str) -> Result<u64, sqlx::Error> {
    let res = sqlx::query(r#"DELETE FROM "note_favorite" WHERE "id" = $1"#)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}

/// notes/children db path (endpoints/notes/children.ts): direct replies plus
/// quote/renotes (`renoteId = target AND (text IS NOT NULL OR files/poll)`),
/// visibility + base filtering per viewer, aid paging.
#[allow(clippy::too_many_arguments)]
pub async fn children_of_note(
    pool: &PgPool,
    note_id: &str,
    me_id: Option<&str>,
    my_followees: &[String],
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<NoteRow>, sqlx::Error> {
    let me_list = me_id.map(|m| vec![m.to_string()]).unwrap_or_default();
    let asc = since_id.is_some();

    let mut inner = String::from(INNER_JOIN_USER);
    inner.push_str(
        r#"(n."replyId" = $1 OR (n."renoteId" = $1
              AND (n."text" IS NOT NULL OR n."fileIds" != '{}' OR n."hasPoll" = TRUE)))"#,
    );
    if me_id.is_some() {
        inner.push_str(VISIBILITY_ME);
        inner.push_str(BASE_ME);
    } else {
        inner.push_str(VISIBILITY_GUEST);
        inner.push_str(BASE_GUEST);
    }
    inner.push_str(PAGER);

    // $1 noteId $2 me $3 [me] $4 myFollowees $5 untilId $6 sinceId $7 limit
    let inner = expand(&inner, 2, 3, 4, 5, 6, 7, dir_asc(since_id));

    sqlx::query_as::<_, NoteRow>(&wrap_outer(&inner, asc))
        .bind(note_id)
        .bind(me_id)
        .bind(me_list)
        .bind(my_followees)
        .bind(until_id)
        .bind(since_id)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// notes/unrenote db path: every renote row authored by `user_id` targeting
/// `renote_id` (upstream `findBy({ userId: me.id, renoteId: note.id })`).
pub async fn find_renotes_by_user(
    pool: &PgPool,
    user_id: &str,
    renote_id: &str,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        r#"SELECT "id" FROM "note" WHERE "userId" = $1 AND "renoteId" = $2"#,
    )
    .bind(user_id)
    .bind(renote_id)
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

// ---------------------------------------------------------------------------
// users/reactions — the reactions a user made, joined onto their notes and
// filtered by the viewer's note visibility (endpoints/users/reactions.ts).
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
pub struct UserReactionRow {
    pub id: String,
    pub user_id: String,
    pub reaction: String,
    pub note_id: String,
}

/// `noteReactionsRepository` page scoped to `reaction.userId`, inner-joined
/// onto the note + author for `generateVisibilityQuery` /
/// `generateBlockedHostQueryForNote` / `generateSuspendedUserQueryForNote`
/// (blocked-host has no storage in this port; suspended authors are gated).
/// Exclusive id bounds; `desc` follows the makePaginationQuery branch order.
#[allow(clippy::too_many_arguments)]
pub async fn user_reaction_rows(
    pool: &PgPool,
    user_id: &str,
    me_id: Option<&str>,
    my_followees: &[String],
    since_id: Option<&str>,
    until_id: Option<&str>,
    desc: bool,
    limit: i64,
) -> Result<Vec<UserReactionRow>, sqlx::Error> {
    let me_list = me_id.map(|m| vec![m.to_string()]).unwrap_or_default();

    let mut sql = String::from(
        r#"SELECT r."id", r."userId" AS "user_id", r."reaction", r."noteId" AS "note_id"
           FROM "note_reaction" r
           INNER JOIN "note" n ON n."id" = r."noteId"
           INNER JOIN "user" u ON u."id" = n."userId"
           WHERE r."userId" = $1"#,
    );
    if me_id.is_some() {
        sql.push_str(VISIBILITY_ME);
        sql.push_str(BASE_ME);
    } else {
        sql.push_str(VISIBILITY_GUEST);
        sql.push_str(BASE_GUEST);
    }
    sql.push_str(
        r#" AND ($UNTIL::varchar IS NULL OR r."id" < $UNTIL)
            AND ($SINCE::varchar IS NULL OR r."id" > $SINCE)
            ORDER BY r."id" {DIR} LIMIT $LIMIT"#,
    );

    // $1 userId $2 me $3 [me] $4 followees $5 untilId $6 sinceId $7 limit
    let dir = if desc { "DESC" } else { "ASC" };
    let sql = expand(&sql, 2, 3, 4, 5, 6, 7, dir);

    sqlx::query_as::<_, UserReactionRow>(&sql)
        .bind(user_id)
        .bind(me_id)
        .bind(me_list)
        .bind(my_followees)
        .bind(until_id)
        .bind(since_id)
        .bind(limit)
        .fetch_all(pool)
        .await
}
