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

/// users/notes db path: the requested user's notes (channels excluded),
/// visibility-gated, suspended-author gate, mute/block gates for logged-in
/// viewers with the requested user exempted from the mute exclusion
/// (`excludeUserFromMute`). withRenotes=false drops pure renotes authored by
/// the requested user (bracket mirrors upstream). Binds: $1 = requested
/// user; $2 = viewer when logged in; the pager occupies the trailing binds
/// (guest: $2/$3/$4; logged-in: $3/$4/$5).
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
) -> Result<Vec<String>, sqlx::Error> {
    let mut sql = String::from(
        r#"SELECT n."id" FROM "note" n
        WHERE n."userId" = $1 AND n."channelId" IS NULL
        AND NOT EXISTS (
            SELECT 1 FROM "user" u WHERE u."id" = n."userId" AND u."isSuspended")"#,
    );
    if me.is_some() {
        sql.push_str(&VIS_ME.replace("{alias}", "n"));
        sql.push_str(
            r#" AND n."userId" NOT IN (
            SELECT "muteeId" FROM "muting" WHERE "muterId" = $2
              AND ("expiresAt" IS NULL OR "expiresAt" > now())
              AND "muteeId" != $1)"#,
        );
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
    if with_files {
        sql.push_str(r#" AND n."fileIds" != '{}'"#);
    }
    if !with_renotes {
        sql.push_str(
            r#" AND (n."renoteId" IS NULL OR n."userId" != $1 OR n."text" IS NOT NULL
              OR n."fileIds" != '{}' OR n."hasPoll")"#,
        );
    }
    let paged = if me.is_some() {
        format!(
            "{sql} AND ($3::varchar IS NULL OR n.\"id\" < $3)
         AND ($4::varchar IS NULL OR n.\"id\" > $4)
         ORDER BY n.\"id\" DESC LIMIT $5"
        )
    } else {
        format!(
            "{sql} AND ($2::varchar IS NULL OR n.\"id\" < $2)
         AND ($3::varchar IS NULL OR n.\"id\" > $3)
         ORDER BY n.\"id\" DESC LIMIT $4"
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
