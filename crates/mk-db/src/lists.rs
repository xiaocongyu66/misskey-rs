//! User lists domain (upstream `endpoints/users/lists/*` + `notes/
//! user-list-timeline.ts`).
//!
//! Mirrors the pinned upstream notes/user-list-timeline.ts `getFromDb`
//! (enableFanoutTimeline: false path) — `QueryService.generateVisibilityQuery`
//! / `generateBaseNoteFilteringQuery` / `generateMutedUserRenotesQueryForNotes`
//! fused per the mk-db timelines.rs convention — plus the list-membership
//! plane joins that path inner-joins on (`user_list_membership` rows carry the
//! per-member `withReplies` reply gate; UserListService.addMember inserts
//! `withReplies: options.withReplies ?? false`).
//!
//! Tables ship in the 0001 baseline: `user_list`, `user_list_membership`
//! (+ `user_list_favorite`, unused here). Aid ids sort monotonically as plain
//! strings, so sinceId/untilId paging is a string compare; bounds are
//! EXCLUSIVE (`makePaginationQuery`), sinceId-only → oldest-first ASC,
//! otherwise newest-first DESC.

use sqlx::PgPool;

use crate::note::{NOTE_COLS, NoteRow};

/// `UserListsRepository.findOneBy({ id, userId })` — every users/lists/*
/// endpoint + notes/user-list-timeline scope the list to its OWNER (a
/// non-owner's listId → NO_SUCH_LIST, not 403).
#[derive(Debug, sqlx::FromRow)]
pub struct UserListRow {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub is_public: bool,
}

pub async fn find_list(
    pool: &PgPool,
    list_id: &str,
    owner_id: &str,
) -> Result<Option<UserListRow>, sqlx::Error> {
    sqlx::query_as::<_, UserListRow>(
        r#"SELECT "id", "userId" AS "user_id", "name", "isPublic" AS "is_public"
           FROM "user_list" WHERE "id" = $1 AND "userId" = $2"#,
    )
    .bind(list_id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await
}

/// `UserListsRepository.countBy({ userId: me.id })` — the
/// `roleService.getUserPolicies(me.id).userListLimit` gate input.
pub async fn count_lists_owned(pool: &PgPool, owner_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(r#"SELECT count(*) FROM "user_list" WHERE "userId" = $1"#)
        .bind(owner_id)
        .fetch_one(pool)
        .await
}

/// `userListsRepository.insertOne({ id, userId, name })` — isPublic defaults
/// false (column default).
pub async fn insert_list(
    pool: &PgPool,
    id: &str,
    owner_id: &str,
    name: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(r#"INSERT INTO "user_list" ("id", "userId", "name") VALUES ($1, $2, $3)"#)
        .bind(id)
        .bind(owner_id)
        .bind(name)
        .execute(pool)
        .await?;
    Ok(())
}

/// `userListMembershipsRepository.findBy({ userListId })` — the UserList pack
/// `userIds` source.
pub async fn list_member_ids(pool: &PgPool, list_id: &str) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(r#"SELECT "userId" FROM "user_list_membership" WHERE "userListId" = $1"#)
        .bind(list_id)
        .fetch_all(pool)
        .await
}

/// `userListMembershipsRepository.countBy({ userListId })` — the
/// `userEachUserListsLimit` per-list cap input (UserListService.addMember).
pub async fn count_memberships(pool: &PgPool, list_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(r#"SELECT count(*) FROM "user_list_membership" WHERE "userListId" = $1"#)
        .bind(list_id)
        .fetch_one(pool)
        .await
}

/// `userListMembershipsRepository.exists({ userListId, userId })` — push.ts
/// ALREADY_ADDED gate.
pub async fn membership_exists(
    pool: &PgPool,
    list_id: &str,
    user_id: &str,
) -> Result<bool, sqlx::Error> {
    let n: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "user_list_membership"
           WHERE "userListId" = $1 AND "userId" = $2"#,
    )
    .bind(list_id)
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

/// `UserListService.addMember` — `withReplies: options.withReplies ?? false`.
/// `userListUserId` = the list owner id (NOT NULL column).
pub async fn insert_membership(
    pool: &PgPool,
    id: &str,
    user_id: &str,
    list_id: &str,
    list_owner_id: &str,
    with_replies: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO "user_list_membership"
            ("id", "userId", "userListId", "userListUserId", "withReplies")
        VALUES ($1, $2, $3, $4, $5)"#,
    )
    .bind(id)
    .bind(user_id)
    .bind(list_id)
    .bind(list_owner_id)
    .bind(with_replies)
    .execute(pool)
    .await?;
    Ok(())
}

/// `UserListService.updateMembership` — `update({ id: membership.id },
/// { withReplies })`; `false` here = no membership row (upstream throws
/// `Error('User is not a member of the list')` → 500).
pub async fn update_membership_with_replies(
    pool: &PgPool,
    list_id: &str,
    user_id: &str,
    with_replies: bool,
) -> Result<bool, sqlx::Error> {
    let res = sqlx::query(
        r#"UPDATE "user_list_membership" SET "withReplies" = $3
           WHERE "userListId" = $1 AND "userId" = $2"#,
    )
    .bind(list_id)
    .bind(user_id)
    .bind(with_replies)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() > 0)
}

/// `ChannelMutingService.mute` — channel mute row (expiresAt NULL =
/// indefinite); the notes/user-list-timeline `mutedChannelIds` renote gate
/// reads the same table.
pub async fn insert_channel_muting(
    pool: &PgPool,
    id: &str,
    user_id: &str,
    channel_id: &str,
    expires_at: Option<i64>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO "channel_muting" ("id", "userId", "channelId", "expiresAt")
        VALUES ($1, $2, $3, to_timestamp($4::double precision / 1000))"#,
    )
    .bind(id)
    .bind(user_id)
    .bind(channel_id)
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// notes/user-list-timeline getFromDb
// ---------------------------------------------------------------------------

/// The db-path reply plane: non-replies always pass; a reply passes when it is
/// a self-reply, addressed to ME (`replyUserId = :meId`), or its membership
/// row opened `withReplies` (user-list-timeline.ts L168-186).
const REPLY_GATE: &str = r#" AND (
          n."replyId" IS NULL
          OR n."replyUserId" = n."userId"
          OR n."replyUserId" = $2
          OR m."withReplies" = TRUE
        )"#;

/// generateVisibilityQuery(me) — public/home, self, specified-to-me,
/// mentioned-me, followers-of-author (author among my followees) or a
/// followers reply addressed to me (QueryService.ts L270-315).
const VIS_ME: &str = r#" AND (
          (n."visibility" IN ('public','home'))
          OR n."userId" = $2
          OR ($3::varchar[] <@ n."visibleUserIds")
          OR ($3::varchar[] <@ n."mentions")
          OR (
            n."visibility" = 'followers'
            AND (n."userId" = ANY($4) OR n."replyUserId" = $2)
          )
        )"#;

const MUTE_SEL: &str = r#"(SELECT "muteeId" FROM "muting" WHERE "muterId" = $2
              AND ("expiresAt" IS NULL OR "expiresAt" > now()))"#;

const BLOCK_SEL: &str = r#"(SELECT "blockerId" FROM "blocking" WHERE "blockeeId" = $2)"#;

/// generateBaseNoteFilteringQuery(me) + generateMutedUserRenotesQueryForNotes
/// (muting/blocking over author / reply target / renote target, renote_muting
/// for pure renotes, suspended authors + suspended reply/renote parent
/// authors; the blocked-host slice is out of scope for the whole mk-db
/// timeline family, matching timelines.rs).
const BASE_ME: &str = r#" AND u."isSuspended" = FALSE
        AND (n."userId" IS NULL OR n."userId" NOT IN (MUTE_SEL))
        AND (n."replyUserId" IS NULL OR n."replyUserId" NOT IN (MUTE_SEL))
        AND (n."renoteUserId" IS NULL OR n."renoteUserId" NOT IN (MUTE_SEL))
        AND (n."userId" IS NULL OR n."userId" NOT IN (BLOCK_SEL))
        AND (n."replyUserId" IS NULL OR n."replyUserId" NOT IN (BLOCK_SEL))
        AND (n."renoteUserId" IS NULL OR n."renoteUserId" NOT IN (BLOCK_SEL))
        AND (n."renoteId" IS NULL OR n."text" IS NOT NULL
            OR n."userId" NOT IN (SELECT "muteeId" FROM "renote_muting" WHERE "muterId" = $2))
        AND (n."renoteId" IS NULL OR NOT EXISTS (
              SELECT 1 FROM "note" p WHERE p."id" = n."renoteId" AND (
                   (p."userId" IS NOT NULL
                        AND (p."userId" IN (MUTE_SEL) OR p."userId" IN (BLOCK_SEL)))
                OR (p."replyUserId" IS NOT NULL AND p."replyUserId" != p."userId"
                        AND (p."replyUserId" IN (MUTE_SEL) OR p."replyUserId" IN (BLOCK_SEL)))
                OR (p."renoteUserId" IS NOT NULL AND p."renoteUserId" != p."userId"
                        AND (p."renoteUserId" IN (MUTE_SEL) OR p."renoteUserId" IN (BLOCK_SEL)))
              )))
        AND (n."replyId" IS NULL OR NOT EXISTS (
              SELECT 1 FROM "note" rp JOIN "user" ru ON ru."id" = rp."userId"
              WHERE rp."id" = n."replyId" AND ru."isSuspended" = TRUE))
        AND (n."renoteId" IS NULL OR NOT EXISTS (
              SELECT 1 FROM "note" rp JOIN "user" ru ON ru."id" = rp."userId"
              WHERE rp."id" = n."renoteId" AND ru."isSuspended" = TRUE))"#;

/// includeMyRenotes === false (user-list-timeline.ts L203-211): my own PURE
/// renotes drop out (quotes / file notes / polls stay).
const EXCLUDE_MY_RENOTES: &str = r#" AND (n."userId" != $2 OR n."renoteId" IS NULL
            OR n."text" IS NOT NULL OR n."fileIds" != '{}'
            OR EXISTS (SELECT 1 FROM "poll" WHERE "poll"."noteId" = n."id")) "#;

/// includeRenotedMyNotes === false (L213-221): renotes OF my notes drop out.
const EXCLUDE_RENOTED_MY_NOTES: &str = r#" AND (n."renoteUserId" != $2 OR n."renoteId" IS NULL
            OR n."text" IS NOT NULL OR n."fileIds" != '{}'
            OR EXISTS (SELECT 1 FROM "poll" WHERE "poll"."noteId" = n."id")) "#;

/// includeLocalRenotes === false (L223-231): renotes of LOCAL authors drop
/// out (remote renote targets stay).
const EXCLUDE_LOCAL_RENOTES: &str = r#" AND (n."renoteUserHost" IS NOT NULL OR n."renoteId" IS NULL
            OR n."text" IS NOT NULL OR n."fileIds" != '{}'
            OR (SELECT COUNT(*) FROM "poll" WHERE "poll"."noteId" = n."id") > 0) "#;

/// withRenotes === false → FanoutTimelineEndpointService excludePureRenotes
/// (L233-242): keep non-renotes and quotes only.
const EXCLUDE_PURE_RENOTES: &str = r#" AND (n."renoteId" IS NULL OR n."text" IS NOT NULL
            OR n."fileIds" != '{}' OR EXISTS (SELECT 1 FROM "poll" WHERE "poll"."noteId" = n."id")) "#;

const WITH_FILES: &str = r#" AND n."fileIds" != '{}' "#;

/// channelMuting renote-side gate (L192-201, ChannelMutingService.list keeps
/// only live mutes): a renote/quote of a MUTED channel's note drops out
/// (channel notes themselves are already excluded by the `channelId IS NULL`
/// plane). The renote PARENT's channel is read via the renoteId join — the
/// timelines.rs RENOTE_CHANNEL_MUTE precedent: upstream stamps
/// `renoteChannelId = renote.channelId` at insert time and channelId rows are
/// never re-scoped afterwards, so the two forms are equivalent without
/// depending on the sibling lane's denormalization write.
const RENOTE_CHANNEL_MUTE: &str = r#" AND (n."renoteId" IS NULL OR NOT EXISTS (
              SELECT 1 FROM "note" rc WHERE rc."id" = n."renoteId"
                AND rc."channelId" IS NOT NULL
                AND rc."channelId" IN (
                    SELECT "channelId" FROM "channel_muting" WHERE "userId" = $2
                      AND ("expiresAt" IS NULL OR "expiresAt" > now()))))"#;

/// notes/user-list-timeline — membership-join timeline over the list's
/// members (the list TL shows member notes with visible*Ids independent of
/// owner/mutual follows; visibility + base filtering are per-viewer `me`).
/// Bind map: $1 listId, $2 me, $3 [me], $4 my followees, $5 untilId,
/// $6 sinceId; limit + direction are format-inlined.
#[allow(clippy::too_many_arguments)]
pub async fn user_list_timeline_notes(
    pool: &PgPool,
    list_id: &str,
    me_id: &str,
    followee_ids: &[String],
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
    with_files: bool,
    with_renotes: bool,
    include_my_renotes: bool,
    include_renoted_my_notes: bool,
    include_local_renotes: bool,
) -> Result<Vec<NoteRow>, sqlx::Error> {
    let me_list = vec![me_id.to_string()];
    // makePaginationQuery (QueryService.ts L46): both bounds → BETWEEN with
    // DESC order; sinceId-only is the only oldest-first ASC shape.
    let dir = if since_id.is_some() && until_id.is_none() {
        "ASC"
    } else {
        "DESC"
    };

    let mut inner = String::from(
        r#"SELECT n."id" FROM "note" n
           INNER JOIN "user" u ON u."id" = n."userId"
           INNER JOIN "user_list_membership" m
                ON m."userId" = n."userId" AND m."userListId" = $1
           WHERE n."channelId" IS NULL"#,
    );
    inner.push_str(REPLY_GATE);
    inner.push_str(VIS_ME);
    inner.push_str(BASE_ME);
    inner.push_str(RENOTE_CHANNEL_MUTE);
    if !include_my_renotes {
        inner.push_str(EXCLUDE_MY_RENOTES);
    }
    if !include_renoted_my_notes {
        inner.push_str(EXCLUDE_RENOTED_MY_NOTES);
    }
    if !include_local_renotes {
        inner.push_str(EXCLUDE_LOCAL_RENOTES);
    }
    if !with_renotes {
        inner.push_str(EXCLUDE_PURE_RENOTES);
    }
    if with_files {
        inner.push_str(WITH_FILES);
    }
    inner.push_str(&format!(
        r#" AND ($5::varchar IS NULL OR n."id" < $5::varchar)
            AND ($6::varchar IS NULL OR n."id" > $6::varchar)
            ORDER BY n."id" {dir} LIMIT {limit}"#
    ));

    let sql = inner
        .replace("MUTE_SEL", MUTE_SEL)
        .replace("BLOCK_SEL", BLOCK_SEL);
    let outer =
        format!(r#"SELECT {NOTE_COLS} FROM "note" WHERE "id" IN ({sql}) ORDER BY "id" {dir}"#);

    sqlx::query_as::<_, NoteRow>(&outer)
        .bind(list_id)
        .bind(me_id)
        .bind(me_list)
        .bind(followee_ids)
        .bind(until_id)
        .bind(since_id)
        .fetch_all(pool)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixed bind layout must stay hole-free: every fragment binds only
    /// $1..$4 and the pager half binds $5/$6 with the cast idiom intact.
    #[test]
    fn fragments_reference_only_fixed_binds() {
        for frag in [
            REPLY_GATE,
            VIS_ME,
            BASE_ME,
            RENOTE_CHANNEL_MUTE,
            EXCLUDE_MY_RENOTES,
            EXCLUDE_RENOTED_MY_NOTES,
            EXCLUDE_LOCAL_RENOTES,
        ] {
            let mut max = 0;
            let mut rest = frag;
            while let Some(i) = rest.find('$') {
                let digit = rest[i + 1..].chars().next().unwrap();
                assert!(digit.is_ascii_digit(), "malformed bind in {frag}");
                max = max.max(digit as u8 - b'0');
                rest = &rest[i + 1..];
            }
            assert!(max <= 4, "bind ${max} out of range: {frag}");
        }
        assert!(REPLY_GATE.contains("m.\"withReplies\""));
        assert!(EXCLUDE_MY_RENOTES.contains("n.\"userId\" != $2"));
    }
}
