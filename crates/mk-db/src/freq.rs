//! freq-agent: db paths behind users/get-frequently-replied-users,
//! pinned-users, users/notes and the pinnedUsers slice of
//! admin/update-meta.
//!
//! Upstream refs (read-only): endpoints/users/get-frequently-replied-users.ts,
//! endpoints/pinned-users.ts, endpoints/users/notes.ts,
//! core/QueryService.ts, core/MetaService.ts.
//!
//! The frequently-replied projection mirrors the emergent upstream state:
//! NoteCreateService rejects a reply when the target note's author blocks the
//! replying user (`b0df6025-f2e8-44b4-a26a-17ad99104612`), so a target user's
//! reply history in upstream data never contains users who block them. The
//! same shape is produced here with a blocking NOT EXISTS keyed on the
//! requested user (upstream only ever gates at creation time, so the read
//! query itself carries no such clause).

use crate::sqlx::PgPool;

/// One replied-to author with its reply count (pre-normalization).
pub struct ReplyUserCount {
    pub user_id: String,
    pub count: i64,
}

/// generateVisibilityQuery: logged-in viewer (public/home, own note,
/// mentioned/visible, followers-only for followees or replies to me).
/// `$2` is the viewer id; the note alias is `{alias}`.
const VIS_ME: &str = r#" AND (
          ({alias}."visibility" IN ('public','home'))
          OR {alias}."userId" = $2
          OR ($2 = ANY({alias}."visibleUserIds"))
          OR ($2 = ANY({alias}."mentions"))
          OR (
            {alias}."visibility" = 'followers'
            AND (
              {alias}."userId" IN (
                SELECT f."followeeId" FROM "following" f WHERE f."followerId" = $2
              )
              OR {alias}."replyUserId" = $2
            )
          )
        )"#;

/// generateVisibilityQuery: guest side (home is public upstream).
const VIS_GUEST: &str = r#" AND {alias}."visibility" IN ('public','home') "#;

/// Emergent NoteCreateService block gate keyed on the requested user: users
/// who block them never appear in their frequency table.
const TARGET_BLOCKER_GATE: &str = r#" AND {alias}."userId" NOT IN (
          SELECT "blockerId" FROM "blocking" WHERE "blockeeId" = $K)"#;

/// users/get-frequently-replied-users projection: reply-target author counts
/// over the target user's recent replies, sorted by count DESC with stable
/// ties (first-appearance order, like the upstream object-key sort).
///
/// Counting unit: DISTINCT reply-target notes per author — upstream scans
/// `note.id IN (:replyIds)` rows (one row per existing target note), so two
/// replies hitting one note count once. step 2 below returns each target
/// note once, preserving that semantics.
pub async fn frequent_replied_counts(
    pool: &PgPool,
    target: &str,
    me: Option<&str>,
) -> Result<Vec<ReplyUserCount>, sqlx::Error> {
    // step 1: recent (latest 1000, id DESC) reply destinations of the target
    // user; the visibility clause is skipped when the target requests their
    // own list (it is always true for `note.userId = meId` rows upstream).
    let base = r#"SELECT n."replyId" FROM "note" n
        WHERE n."userId" = $1 AND n."replyId" IS NOT NULL"#;
    let tail = r#" ORDER BY n."id" DESC LIMIT 1000"#;
    let (sql, step1_viewer): (String, Option<&str>) = match me {
        None => (
            format!("{base}{}{tail}", VIS_GUEST.replace("{alias}", "n")),
            None,
        ),
        Some(m) if m == target => (format!("{base}{tail}"), None),
        Some(m) => (
            format!("{base}{}{tail}", VIS_ME.replace("{alias}", "n")),
            Some(m),
        ),
    };
    let mut q1 = sqlx::query_scalar::<_, String>(&sql).bind(target);
    if let Some(m) = step1_viewer {
        q1 = q1.bind(m);
    }
    let reply_ids = q1.fetch_all(pool).await?;
    if reply_ids.is_empty() {
        return Ok(vec![]);
    }

    // step 2: count per author of the viewer-visible reply targets. The
    // visibility clause applies unconditionally here (upstream never skips it
    // on this query, even for self). Binds: $1 = reply ids; then guest →
    // $2 = gate target; logged-in → $2 = viewer, $3 = gate target.
    let base2 = r#"SELECT r."userId" FROM "note" r WHERE r."id" = ANY($1::varchar[])"#;
    let target_gate = TARGET_BLOCKER_GATE.replace("{alias}", "r");
    let (sql2, step2_viewer) = match me {
        None => (
            format!(
                "{base2}{}{}",
                VIS_GUEST.replace("{alias}", "r"),
                target_gate.replace("$K", "$2")
            ),
            None,
        ),
        Some(m) => (
            format!(
                "{base2}{}{}",
                VIS_ME.replace("{alias}", "r"),
                target_gate.replace("$K", "$3")
            ),
            Some(m),
        ),
    };
    let mut q2 = sqlx::query_scalar::<_, String>(&sql2).bind(reply_ids);
    if let Some(m) = step2_viewer {
        q2 = q2.bind(m);
    }
    q2 = q2.bind(target);
    let authors = q2.fetch_all(pool).await?;

    let mut order: Vec<String> = Vec::new();
    let mut counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for uid in authors {
        if !counts.contains_key(&uid) {
            order.push(uid.clone());
        }
        *counts.entry(uid).or_insert(0) += 1;
    }
    let mut out: Vec<ReplyUserCount> = order
        .into_iter()
        .map(|user_id| ReplyUserCount {
            count: counts[&user_id],
            user_id,
        })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count)); // Vec::sort_by is stable
    Ok(out)
}

// -- utl-misc (users/notes channel + mute-chain legs, upstream
// endpoints/users/notes.ts getFromDb + core/QueryService.ts) ---------------
/// users/notes db path: the requested user's notes. Channel plane rides
/// withChannelNotes (default plane keeps `channelId IS NULL`), the mute
/// chain covers author/reply/renote + the renote-side pass with the
/// requested user exempted (`excludeUserFromMute`), and sinceId-only paging
/// runs ASC (makePaginationQuery). withRenotes=false drops pure renotes
/// authored by the requested user. Binds: $1 = requested user; $2 = viewer
/// when logged in; the pager occupies the trailing binds (guest: $2/$3/$4;
/// logged-in: $3/$4/$5).
#[allow(clippy::too_many_arguments)]
pub async fn user_note_ids(
    pool: &PgPool,
    user_id: &str,
    me: Option<&str>,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
    with_files: bool,
    with_renotes: bool,
    with_replies: bool,
    with_channel_notes: bool,
) -> Result<Vec<String>, sqlx::Error> {
    let mut sql = String::from(
        r#"SELECT n."id" FROM "note" n
        WHERE n."userId" = $1
        AND NOT EXISTS (
            SELECT 1 FROM "user" u WHERE u."id" = n."userId" AND u."isSuspended")"#,
    );
    // isSelf gates the sensitive-channel pass (users/notes.ts L174-199):
    // only the owner's own fetch keeps sensitive-channel notes; the guest
    // face sees the same gate (me == null ⇒ isSelf false upstream).
    let is_self = me == Some(user_id);
    if with_channel_notes {
        if me.is_some() {
            // mutingChannelIds gate: (channelId IS NULL OR NOT IN muted);
            // the empty-set subquery degrades to TRUE, so the clause rides
            // unconditionally like upstream's length check would
            // (users/notes.ts L185-192).
            sql.push_str(
                r#" AND (n."channelId" IS NULL OR n."channelId" NOT IN (
                SELECT "channelId" FROM "channel_muting" WHERE "userId" = $2
                  AND ("expiresAt" IS NULL OR "expiresAt" > now())))"#,
            );
        }
        if !is_self {
            sql.push_str(
                r#" AND (n."channelId" IS NULL OR NOT EXISTS (
                SELECT 1 FROM "channel" c WHERE c."id" = n."channelId" AND c."isSensitive"))"#,
            );
        }
    } else {
        sql.push_str(r#" AND n."channelId" IS NULL"#);
    }
    if me.is_some() {
        // ミュートされたチャンネルのリノート対策 (users/notes.ts L205-211):
        // a renote of a muted channel's note drops out of every plane.
        sql.push_str(
            r#" AND (n."renoteChannelId" IS NULL OR n."renoteChannelId" NOT IN (
            SELECT "channelId" FROM "channel_muting" WHERE "userId" = $2
              AND ("expiresAt" IS NULL OR "expiresAt" > now())))"#,
        );
        sql.push_str(&VIS_ME.replace("{alias}", "n"));
        // generateMutedUserQueryForNotes with excludeUserFromMute = the
        // requested user (QueryService.ts L206-212): the shared subquery is
        // reused for the author / reply / renote columns and the renote-side
        // pass (noteColumn='renote', L109) — a renote of a quote/reply of a
        // muted user vanishes (users/notes.ts L215-218).
        let mute_sel = r#"(SELECT "muteeId" FROM "muting" WHERE "muterId" = $2
                  AND ("expiresAt" IS NULL OR "expiresAt" > now())
                  AND "muteeId" != $1)"#;
        sql.push_str(&format!(
            r#" AND (n."replyUserId" IS NULL OR n."replyUserId" NOT IN {mute_sel})
            AND (n."renoteUserId" IS NULL OR n."renoteUserId" NOT IN {mute_sel})
            AND (n."renoteId" IS NULL OR NOT EXISTS (
            SELECT 1 FROM "note" rn WHERE rn."id" = n."renoteId" AND (
                  rn."userId" IN {mute_sel}
               OR rn."replyUserId" IN {mute_sel}
               OR rn."renoteUserId" IN {mute_sel})))"#,
        ));
        sql.push_str(
            r#" AND n."userId" NOT IN (
            SELECT "blockerId" FROM "blocking" WHERE "blockeeId" = $2)"#,
        );
        sql.push_str(
            r#" AND (n."replyUserId" IS NULL OR n."replyUserId" NOT IN (
            SELECT "blockerId" FROM "blocking" WHERE "blockeeId" = $2))"#,
        );
        sql.push_str(
            r#" AND (n."renoteUserId" IS NULL OR n."renoteUserId" NOT IN (
            SELECT "blockerId" FROM "blocking" WHERE "blockeeId" = $2))"#,
        );
    } else {
        sql.push_str(&VIS_GUEST.replace("{alias}", "n"));
    }
    if !with_replies {
        if with_channel_notes {
            // excludeReplies: userTimelineWithChannel may include replies
            // (users/notes.ts L135): all replies dropped on that plane
            // unless withReplies opened the gate.
            sql.push_str(r#" AND n."replyId" IS NULL"#);
        } else {
            // NoteCreateService.pushToTl (L1157-1180, misc/is-reply.ts):
            // the base userTimeline holds non-replies + self-replies;
            // replies to others ride only userTimelineWithReplies.
            sql.push_str(r#" AND (n."replyId" IS NULL OR n."replyUserId" = n."userId")"#);
        }
    }
    if with_files {
        sql.push_str(r#" AND n."fileIds" != '{}'"#);
    }
    if !with_renotes {
        sql.push_str(
            r#" AND (n."renoteId" IS NULL OR n."userId" != $1 OR n."text" IS NOT NULL
              OR n."fileIds" != '{}' OR n."hasPoll")"#,
        );
    }
    // makePaginationQuery (QueryService.ts L54-77): sinceId without untilId
    // paginates oldest-first ASC — the FTT sinceId legs assert ascending;
    // both bounds (or untilId alone) stay newest-first DESC.
    let asc = since_id.is_some() && until_id.is_none();
    let dir = if asc { "ASC" } else { "DESC" };
    let paged = if me.is_some() {
        format!(
            "{sql} AND ($3::varchar IS NULL OR n.\"id\" < $3)
         AND ($4::varchar IS NULL OR n.\"id\" > $4)
         ORDER BY n.\"id\" {dir} LIMIT $5"
        )
    } else {
        format!(
            "{sql} AND ($2::varchar IS NULL OR n.\"id\" < $2)
         AND ($3::varchar IS NULL OR n.\"id\" > $3)
         ORDER BY n.\"id\" {dir} LIMIT $4"
        )
    };

    let q = sqlx::query_scalar::<_, String>(&paged).bind(user_id);
    match me {
        None => {
            q.bind(until_id)
                .bind(since_id)
                .bind(limit)
                .fetch_all(pool)
                .await
        }
        Some(m) => {
            q.bind(m)
                .bind(until_id)
                .bind(since_id)
                .bind(limit)
                .fetch_all(pool)
                .await
        }
    }
}

/// meta.pinnedUsers of the newest meta row ('{}' when no row exists).
pub async fn meta_pinned_user_accts(pool: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar::<_, Vec<String>>(
        r#"SELECT COALESCE(
            (SELECT "pinnedUsers" FROM "meta" ORDER BY "id" DESC LIMIT 1),
            '{}'::varchar[])"#,
    )
    .fetch_one(pool)
    .await
}

/// pinned-users: findOneBy({ usernameLower: acct.username.toLowerCase(),
/// host: acct.host ?? IsNull() }) per meta.pinnedUsers entry.
pub async fn find_user_id_by_acct(
    pool: &PgPool,
    username_lower: &str,
    host: Option<&str>,
) -> Result<Option<String>, sqlx::Error> {
    match host {
        None => {
            sqlx::query_scalar::<_, String>(
                r#"SELECT "id" FROM "user"
                WHERE "usernameLower" = $1 AND "host" IS NULL
                ORDER BY "id" ASC LIMIT 1"#,
            )
            .bind(username_lower)
            .fetch_optional(pool)
            .await
        }
        Some(h) => {
            sqlx::query_scalar::<_, String>(
                r#"SELECT "id" FROM "user"
                WHERE "usernameLower" = $1 AND "host" = $2
                ORDER BY "id" ASC LIMIT 1"#,
            )
            .bind(username_lower)
            .bind(h)
            .fetch_optional(pool)
            .await
        }
    }
}

/// admin/update-meta pinnedUsers slice: update the singleton row (newest id
/// wins upstream), inserting the MetaService-shaped row when absent.
pub async fn update_meta_pinned_users(pool: &PgPool, pinned: &[String]) -> Result<(), sqlx::Error> {
    let updated = sqlx::query(r#"UPDATE "meta" SET "pinnedUsers" = $1"#)
        .bind(pinned.to_vec())
        .execute(pool)
        .await?
        .rows_affected();
    if updated == 0 {
        sqlx::query(
            r#"INSERT INTO "meta" ("id", "pinnedUsers") VALUES ('x', $1)
            ON CONFLICT ("id") DO UPDATE SET "pinnedUsers" = EXCLUDED."pinnedUsers""#,
        )
        .bind(pinned.to_vec())
        .execute(pool)
        .await?;
    }
    Ok(())
}
