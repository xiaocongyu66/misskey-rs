//! Social-graph writes: following / follow_request / muting / blocking rows
//! plus the note renote row (M2 truthful slice, local users only).
//!
//! Semantics mirrored from upstream services:
//! - `core/UserFollowingService.ts` (insertFollowingDoc / unfollow /
//!   createFollowRequest / acceptFollowRequest) — including the
//!   denormalized host/inbox columns (all NULL for local-only M2 targets)
//!   and the `pending follow_request` delete inside insertFollowingDoc.
//! - `core/UserMutingService.ts` (mute → insert, unmute → delete).
//! - `core/UserBlockingService.ts` (block cancels both follow requests,
//!   unfollows both directions, then inserts; unblock deletes).
//! - `core/NoteCreateService.ts` incRenoteCount for the renote slice.
//!
//! Count bumps match upstream `usersRepository.increment/decrement`:
//! follow → follower.followingCount + followee.followersCount; unfollow the
//! reverse; renote → target.renoteCount.

use sqlx::PgPool;

// ---------------------------------------------------------------------------
// following
// ---------------------------------------------------------------------------

/// `followingsRepository.exists({ followerId, followeeId })`.
pub async fn following_exists(
    pool: &PgPool,
    follower_id: &str,
    followee_id: &str,
) -> Result<bool, sqlx::Error> {
    let n: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "following" WHERE "followerId" = $1 AND "followeeId" = $2"#,
    )
    .bind(follower_id)
    .bind(followee_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

/// `blockingsRepository.exists({ blockerId, blockeeId })` (checkBlocked).
pub async fn blocking_exists(
    pool: &PgPool,
    blocker_id: &str,
    blockee_id: &str,
) -> Result<bool, sqlx::Error> {
    let n: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "blocking" WHERE "blockerId" = $1 AND "blockeeId" = $2"#,
    )
    .bind(blocker_id)
    .bind(blockee_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

/// `UserFollowingService.getFollowees(me)` — ids only, for the timeline union.
pub async fn followee_ids(pool: &PgPool, follower_id: &str) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(r#"SELECT "followeeId" FROM "following" WHERE "followerId" = $1"#)
        .bind(follower_id)
        .fetch_all(pool)
        .await
}

/// One `following` row of the follower's own list (the endpoint layer packs
/// the edge together with the followee the query joins upstream).
#[derive(Debug, sqlx::FromRow)]
pub struct FollowingListEdge {
    pub id: String,
    #[sqlx(rename = "followeeId")]
    pub followee_id: String,
    #[sqlx(rename = "followerId")]
    pub follower_id: String,
}

/// `following/list` (endpoints/following/list.ts) — the follower's own edges
/// (`following.followerId = :userId`, L54). `notification: true` adds
/// `following.notify IS NOT NULL` (L56-58 — with insert_following's NULL
/// default only explicit 'normal' rows surface). Bounds are exclusive aid
/// cursors (QueryService.makePaginationQuery), order DESC unless sinceId.
pub async fn list_following_edges(
    pool: &PgPool,
    follower_id: &str,
    notify_is_not_null: bool,
    from_id: Option<&str>,
    until_id: Option<&str>,
    desc: bool,
    limit: u32,
) -> Result<Vec<FollowingListEdge>, sqlx::Error> {
    let order = if desc { "DESC" } else { "ASC" };
    sqlx::query_as::<_, FollowingListEdge>(&format!(
        r#"
        SELECT "id", "followeeId", "followerId" FROM "following"
        WHERE "followerId" = $1
          AND ($2 = FALSE OR "notify" IS NOT NULL)
          AND ($3::varchar IS NULL OR "id" > $3)
          AND ($4::varchar IS NULL OR "id" < $4)
        ORDER BY "id" {order}
        LIMIT $5
        "#
    ))
    .bind(follower_id)
    .bind(notify_is_not_null)
    .bind(from_id)
    .bind(until_id)
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await
}

/// NoteCreateService.create L790-813 notify fanout — the
/// `followingsRepository.findBy({ followeeId: user.id, notify: 'normal' })`
/// set: followers of the author whose follow edge carries notify = 'normal'.
pub async fn notify_follower_ids(
    pool: &PgPool,
    followee_id: &str,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        r#"SELECT "followerId" FROM "following" WHERE "followeeId" = $1 AND "notify" = 'normal'"#,
    )
    .bind(followee_id)
    .fetch_all(pool)
    .await
}

/// `user.isBot` scalar (LocalUserRow does not carry it; reading a plain
/// column, no join needed).
pub async fn user_is_bot(pool: &PgPool, user_id: &str) -> Result<bool, sqlx::Error> {
    let b: bool = sqlx::query_scalar(r#"SELECT "isBot" FROM "user" WHERE "id" = $1"#)
        .bind(user_id)
        .fetch_one(pool)
        .await?;
    Ok(b)
}

/// Upstream `insertFollowingDoc` (UserFollowingService.ts L251-263): insert
/// the follow edge — `notify` is NOT set (column default NULL; the flag
/// flips later through following/update, following/update.ts L99-104) — and
/// the host/inbox denormalized columns stay NULL for local-only M2. Then a
/// matching pending follow_request row is deleted (requestExist branch).
/// `id` is generated by the caller (mk-db has no id-gen dependency). Returns
/// whether a pending request was swept (upstream binds the local
/// 'followRequestAccepted' notification leg to that row existing).
pub async fn insert_following(
    pool: &PgPool,
    id: &str,
    follower_id: &str,
    followee_id: &str,
    with_replies: bool,
) -> Result<bool, sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO "following"
          ("id", "followeeId", "followerId", "followerHost", "followerInbox",
           "followerSharedInbox", "followeeHost", "followeeInbox",
           "followeeSharedInbox", "notify", "withReplies", "isFollowerHibernated")
        VALUES ($1, $2, $3, NULL, NULL, NULL, NULL, NULL, NULL, NULL, $4, FALSE)
        "#,
    )
    .bind(id)
    .bind(followee_id)
    .bind(follower_id)
    .bind(with_replies)
    .execute(pool)
    .await?;
    delete_follow_request(pool, follower_id, followee_id).await
}

/// `followRequestsRepository.find({ where: { followeeId } })` — requester ids
/// of every pending request to `followee_id` (UserFollowingService
/// acceptAllFollowRequests's request list).
pub async fn follow_request_follower_ids(
    pool: &PgPool,
    followee_id: &str,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        r#"SELECT "followerId" FROM "follow_request" WHERE "followeeId" = $1 ORDER BY "id""#,
    )
    .bind(followee_id)
    .fetch_all(pool)
    .await
}

/// Increment/decrement the denormalized counters the way upstream
/// `usersRepository.increment('followingCount')` /
/// `increment('followersCount')` do (delta is +1 on follow, -1 on unfollow).
pub async fn bump_follow_counts(
    pool: &PgPool,
    follower_id: &str,
    followee_id: &str,
    delta: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "user" SET "followingCount" = "followingCount" + $2 WHERE "id" = $1"#)
        .bind(follower_id)
        .bind(delta)
        .execute(pool)
        .await?;
    sqlx::query(r#"UPDATE "user" SET "followersCount" = "followersCount" + $2 WHERE "id" = $1"#)
        .bind(followee_id)
        .bind(delta)
        .execute(pool)
        .await?;
    Ok(())
}

/// `followingsRepository.delete(...)`; returns whether a row was removed.
pub async fn delete_following(
    pool: &PgPool,
    follower_id: &str,
    followee_id: &str,
) -> Result<bool, sqlx::Error> {
    let n = sqlx::query(r#"DELETE FROM "following" WHERE "followerId" = $1 AND "followeeId" = $2"#)
        .bind(follower_id)
        .bind(followee_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(n > 0)
}

// ---------------------------------------------------------------------------
// follow_request
// ---------------------------------------------------------------------------

/// `followRequestsRepository.exists({ followeeId, followerId })`.
pub async fn follow_request_exists(
    pool: &PgPool,
    follower_id: &str,
    followee_id: &str,
) -> Result<bool, sqlx::Error> {
    let n: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "follow_request" WHERE "followerId" = $1 AND "followeeId" = $2"#,
    )
    .bind(follower_id)
    .bind(followee_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

/// `acceptFollowRequest`'s findOneBy: returns the request's `withReplies`
/// flag when a pending request from `follower_id` to `followee_id` exists.
pub async fn follow_request_with_replies(
    pool: &PgPool,
    followee_id: &str,
    follower_id: &str,
) -> Result<Option<bool>, sqlx::Error> {
    let r: Option<bool> = sqlx::query_scalar(
        r#"SELECT "withReplies" FROM "follow_request"
           WHERE "followeeId" = $1 AND "followerId" = $2"#,
    )
    .bind(followee_id)
    .bind(follower_id)
    .fetch_optional(pool)
    .await?;
    Ok(r)
}

/// Upstream `createFollowRequest`: remove any previous request row first,
/// then insert (requestId/host cols are AP-flow only → NULL locally). `id`
/// is generated by the caller.
pub async fn insert_follow_request(
    pool: &PgPool,
    id: &str,
    follower_id: &str,
    followee_id: &str,
    with_replies: bool,
) -> Result<(), sqlx::Error> {
    delete_follow_request(pool, follower_id, followee_id).await?;
    sqlx::query(
        r#"
        INSERT INTO "follow_request"
          ("id", "followeeId", "followerId", "requestId", "followerHost",
           "followerInbox", "followerSharedInbox", "followeeHost", "followeeInbox",
           "followeeSharedInbox", "withReplies")
        VALUES ($1, $2, $3, NULL, NULL, NULL, NULL, NULL, NULL, NULL, $4)
        "#,
    )
    .bind(id)
    .bind(followee_id)
    .bind(follower_id)
    .bind(with_replies)
    .execute(pool)
    .await?;
    Ok(())
}

/// `followRequestsRepository.delete({ followeeId, followerId })`.
pub async fn delete_follow_request(
    pool: &PgPool,
    follower_id: &str,
    followee_id: &str,
) -> Result<bool, sqlx::Error> {
    let n = sqlx::query(
        r#"DELETE FROM "follow_request" WHERE "followerId" = $1 AND "followeeId" = $2"#,
    )
    .bind(follower_id)
    .bind(followee_id)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(n > 0)
}

// ---------------------------------------------------------------------------
// muting
// ---------------------------------------------------------------------------

/// `mutingsRepository.exists({ muterId, muteeId })`.
pub async fn muting_exists(
    pool: &PgPool,
    muter_id: &str,
    mutee_id: &str,
) -> Result<bool, sqlx::Error> {
    let n: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "muting" WHERE "muterId" = $1 AND "muteeId" = $2"#,
    )
    .bind(muter_id)
    .bind(mutee_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

/// `UserMutingService.mute`: expiresAt as Unix-epoch ms (None → indefinite).
/// `id` is generated by the caller.
pub async fn insert_muting(
    pool: &PgPool,
    id: &str,
    muter_id: &str,
    mutee_id: &str,
    expires_at_ms: Option<i64>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO "muting" ("id", "muteeId", "muterId", "expiresAt")
        VALUES ($1, $2, $3, CASE WHEN $4 IS NULL THEN NULL
                                 ELSE to_timestamp($4::double precision / 1000.0) END)
        "#,
    )
    .bind(id)
    .bind(mutee_id)
    .bind(muter_id)
    .bind(expires_at_ms)
    .execute(pool)
    .await?;
    Ok(())
}

/// `UserMutingService.unmute([row])`; returns whether a row was removed.
pub async fn delete_muting(
    pool: &PgPool,
    muter_id: &str,
    mutee_id: &str,
) -> Result<bool, sqlx::Error> {
    let n = sqlx::query(r#"DELETE FROM "muting" WHERE "muterId" = $1 AND "muteeId" = $2"#)
        .bind(muter_id)
        .bind(mutee_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(n > 0)
}

// ---------------------------------------------------------------------------
// blocking
// ---------------------------------------------------------------------------

/// `UserBlockingService.block` (local M2 slice): cancel both pending follow
/// requests, unfollow both directions (each decrementing counters), then
/// insert the blocking row. List-membership cleanup is M2-later (no lists).
/// `id` is generated by the caller.
pub async fn block(
    pool: &PgPool,
    id: &str,
    blocker_id: &str,
    blockee_id: &str,
) -> Result<(), sqlx::Error> {
    // cancelRequest(blocker, blockee) + cancelRequest(blockee, blocker)
    delete_follow_request(pool, blocker_id, blockee_id).await?;
    delete_follow_request(pool, blockee_id, blocker_id).await?;
    // unfollow both directions (unfollow decrements only when a row existed)
    if delete_following(pool, blocker_id, blockee_id).await? {
        bump_follow_counts(pool, blocker_id, blockee_id, -1).await?;
    }
    if delete_following(pool, blockee_id, blocker_id).await? {
        bump_follow_counts(pool, blockee_id, blocker_id, -1).await?;
    }
    sqlx::query(r#"INSERT INTO "blocking" ("id", "blockeeId", "blockerId") VALUES ($1, $2, $3)"#)
        .bind(id)
        .bind(blockee_id)
        .bind(blocker_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// `UserBlockingService.unblock`; returns whether a row was removed.
pub async fn delete_blocking(
    pool: &PgPool,
    blocker_id: &str,
    blockee_id: &str,
) -> Result<bool, sqlx::Error> {
    let n = sqlx::query(r#"DELETE FROM "blocking" WHERE "blockerId" = $1 AND "blockeeId" = $2"#)
        .bind(blocker_id)
        .bind(blockee_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(n > 0)
}

// ---------------------------------------------------------------------------
// renote
// ---------------------------------------------------------------------------

/// Minimal renote-target read for the NoteCreateService renote rules
/// (existence, pure-renote detection, visibility, localOnly, author).
#[derive(Debug, sqlx::FromRow)]
pub struct RenoteTarget {
    pub id: String,
    #[sqlx(rename = "userId")]
    pub user_id: String,
    #[sqlx(rename = "userHost")]
    pub user_host: Option<String>,
    #[sqlx(rename = "renoteId")]
    pub renote_id: Option<String>,
    pub text: Option<String>,
    pub cw: Option<String>,
    #[sqlx(rename = "replyId")]
    pub reply_id: Option<String>,
    #[sqlx(rename = "hasPoll")]
    pub has_poll: bool,
    #[sqlx(rename = "fileIds")]
    pub file_ids: Vec<String>,
    pub visibility: String,
    #[sqlx(rename = "localOnly")]
    pub local_only: bool,
    // utl-misc (users/notes [チャンネル外リノート] チャンネルミュート leg):
    // the parent's channel feeds the renote-row denormalization
    // (`renoteChannelId: data.renote.channelId`, NoteCreateService.ts L680).
    #[sqlx(rename = "channelId")]
    pub channel_id: Option<String>,
}

pub async fn find_renote_target(
    pool: &PgPool,
    note_id: &str,
) -> Result<Option<RenoteTarget>, sqlx::Error> {
    sqlx::query_as::<_, RenoteTarget>(
        r#"
        SELECT "id", ("userId") AS "userId", ("userHost") AS "userHost",
               ("renoteId") AS "renoteId", "text", "cw", ("replyId") AS "replyId",
               "hasPoll", "fileIds", ("visibility"::text) AS "visibility",
               "localOnly", ("channelId") AS "channelId"
        FROM "note" WHERE "id" = $1
        "#,
    )
    .bind(note_id)
    .fetch_optional(pool)
    .await
}

/// Renote/quote row (visibility rules are applied by the caller; a quote
/// passes `text` alongside the renoteId, a pure renote passes NULL). A
/// poll-carrying renote is a quote for the withRenotes stream lanes
/// (misc/is-renote.ts isQuotePacked `note.poll != null`), so the fresh row
/// carries `hasPoll` + the poll row via mk_db::poll (upstream rides the one
/// create() paramDef — notes/create { renoteId, poll }).
pub async fn insert_renote_note(
    pool: &PgPool,
    id: &str,
    user_id: &str,
    renote: &RenoteTarget,
    text: Option<&str>,
    visibility: &str,
    local_only: bool,
    tags: &[String],
    has_poll: bool,
) -> Result<(), sqlx::Error> {
    // tl-remote: author host denormalization on the renote row too
    // (insertNote `userHost: user.host`) — a remote author's renote must not
    // ride the local planes' `userHost IS NULL` gates.
    sqlx::query(
        r#"
        INSERT INTO "note"
          ("id", "renoteId", "text", "userId", "renoteUserId", "renoteUserHost", "localOnly",
           "renoteCount", "repliesCount", "reactions", "visibility", "fileIds",
           "attachedFileTypes", "visibleUserIds", "mentions", "emojis", "tags", "hasPoll",
           "pageCount", "userHost")
        VALUES ($1, $2, $3, $4, $5, $6, $7, 0, 0, '{}'::jsonb, $8::note_visibility_enum,
                '{}'::varchar[], '{}'::varchar[], '{}'::varchar[], '{}'::varchar[],
                '{}'::varchar[], $9, $10, 0,
                (SELECT "host" FROM "user" WHERE "id" = $4))
        "#,
    )
    .bind(id)
    .bind(renote.id.as_str())
    .bind(text)
    .bind(user_id)
    .bind(renote.user_id.as_str())
    .bind(renote.user_host.as_deref())
    .bind(local_only)
    .bind(visibility)
    .bind(tags)
    .bind(has_poll)
    .execute(pool)
    .await?;
    Ok(())
}

/// NoteCreateService.incRenoteCount (skipped for own notes / bot users by the
/// caller, mirroring `data.renote.userId !== user.id && !user.isBot`).
pub async fn bump_renote_count(pool: &PgPool, note_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "note" SET "renoteCount" = "renoteCount" + 1 WHERE "id" = $1"#)
        .bind(note_id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// renote-mute (renote-mute/create + renote-mute/delete; the users.ts census
// flips isRenoteMuted through these rows)
// ---------------------------------------------------------------------------

/// `RenoteMutingEntityService` existence probe.
pub async fn renote_muting_exists(
    pool: &PgPool,
    muter_id: &str,
    mutee_id: &str,
) -> Result<bool, sqlx::Error> {
    let n: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "renote_muting" WHERE "muterId" = $1 AND "muteeId" = $2"#,
    )
    .bind(muter_id)
    .bind(mutee_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

/// `RenoteMutingService.mute`: indefinite row (`id` generated by the caller).
pub async fn insert_renote_muting(
    pool: &PgPool,
    id: &str,
    muter_id: &str,
    mutee_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(r#"INSERT INTO "renote_muting" ("id", "muteeId", "muterId") VALUES ($1, $2, $3)"#)
        .bind(id)
        .bind(mutee_id)
        .bind(muter_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// `RenoteMutingService.unmute`; returns whether a row was removed.
pub async fn delete_renote_muting(
    pool: &PgPool,
    muter_id: &str,
    mutee_id: &str,
) -> Result<bool, sqlx::Error> {
    let n = sqlx::query(r#"DELETE FROM "renote_muting" WHERE "muterId" = $1 AND "muteeId" = $2"#)
        .bind(muter_id)
        .bind(mutee_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(n > 0)
}

// ---------------------------------------------------------------------------
// following/update (following-relationship flag flip; the timelines matrix
// legs toggle withReplies through it — upstream following/update.ts
// `followingsRepository.update({ id: exist.id }, { notify, withReplies })`)
// ---------------------------------------------------------------------------

/// Partial flip of the notify / withReplies flags on an existing edge;
/// `None` args leave the column untouched (upstream `undefined` semantics).
/// Returns whether the edge row existed.
pub async fn update_following_flags(
    pool: &PgPool,
    follower_id: &str,
    followee_id: &str,
    notify: Option<Option<String>>,
    with_replies: Option<bool>,
) -> Result<bool, sqlx::Error> {
    // Build each SET with its own numbered bind; every supplied bind must be
    // referenced by the statement, so the placeholders are assigned in order.
    let has_notify = notify.is_some();
    let mut sets: Vec<String> = Vec::with_capacity(2);
    let mut binds_notify: Option<Option<String>> = None;
    let mut binds_replies: Option<bool> = None;
    if has_notify {
        sets.push(r#""notify" = $3"#.to_string());
        binds_notify = notify;
    }
    if with_replies.is_some() {
        sets.push(format!(
            r#""withReplies" = ${}"#,
            if has_notify { 4 } else { 3 }
        ));
        binds_replies = with_replies;
    }
    if sets.is_empty() {
        let n: i64 = sqlx::query_scalar(
            r#"SELECT count(*) FROM "following" WHERE "followerId" = $1 AND "followeeId" = $2"#,
        )
        .bind(follower_id)
        .bind(followee_id)
        .fetch_one(pool)
        .await?;
        return Ok(n > 0);
    }
    let sql = format!(
        r#"UPDATE "following" SET {} WHERE "followerId" = $1 AND "followeeId" = $2"#,
        sets.join(", ")
    );
    let mut q = sqlx::query(&sql).bind(follower_id).bind(followee_id);
    if binds_notify.is_some() {
        q = q.bind(binds_notify);
    }
    if binds_replies.is_some() {
        q = q.bind(binds_replies);
    }
    let n = q.execute(pool).await?.rows_affected();
    Ok(n > 0)
}
