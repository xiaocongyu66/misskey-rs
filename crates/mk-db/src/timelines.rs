//! Timeline quad db-path queries (notes/timeline, notes/local-timeline,
//! notes/hybrid-timeline, notes/global-timeline).
//!
//! Mirrors the pinned upstream db fallback queries
//! (`notes/{timeline,local-timeline,hybrid-timeline}.ts` `getFromDb`,
//! `QueryService.generateVisibilityQuery` / `generateBaseNoteFilteringQuery` /
//! `generateMutedUserRenotesQueryForNotes` / `generateSuspendedUserQueryForNote`)
//! plus the fanout-only behaviors the e2e suite exercises when
//! `enableFanoutTimeline: true` (replies addressed to me, own replies,
//! `following.withReplies` replies and the followers-reply target gate with the
//! two-level `isUserRelated(note.renote, …)` chains of
//! `FanoutTimelineEndpointService.getMiNotes`). Those extra inclusions are
//! unobservable upstream when the flag is off (the affected `timelines.ts`
//! legs are `FIXME #12065`-guarded to skip) and no leg asserts their
//! exclusion, so one unified query serves both flag modes. Upstream makes no
//! `withReplies` distinction for `notes/timeline` (fanout pushes decide via
//! `NoteCreateService.pushToTl` + the endpoint `noteFilter`); the home gate
//! below encodes exactly that pair.
//!
//! Channel fold (chan-note): the db paths fold a viewer's followed channels
//! (minus the viewer's `channel_muting` rows) into home/hybrid —
//! `timeline.ts`/`hybrid-timeline.ts` `getFromDb` channel brackets; local
//! keeps `channelId IS NULL` (upstream local-timeline.ts L157). The renote
//! side's `renoteChannelId` channel-mute gate reads the renote PARENT's
//! channel — the insert-time denormalization (`renoteChannelId ==
//! renote.channelId`) belongs to the shared renote insert (sibling lane).
//! The `FTT:` Redis-literal probes still need the push pipeline (red here).
//!
//! Aid ids sort monotonically as plain strings, so sinceId/untilId paging is
//! a string compare; bounds are EXCLUSIVE (`makePaginationQuery`),
//! sinceId-only → oldest-first ASC, otherwise newest-first DESC.

use sqlx::PgPool;

use crate::note::{NOTE_COLS, NoteRow};

// -- token-substitution plumbing -------------------------------------------
//
// Placeholders are written as :NAME: tokens (never prefixes of one another)
// and rewritten to numbered $N binds for the enclosing statement.

fn expand(
    sql: &str,
    me: u8,
    me_list: u8,
    fol: u8,
    until: u8,
    since: u8,
    limit: u8,
    dir: &str,
) -> String {
    // Single-pass scanner: tokens are `:NAME:` with the closing colon as the
    // delimiter. The one ambiguity is the bind-cast idiom `:MELIST::varchar[]`
    // / `:UNTIL::varchar` — there the closing colon is the FIRST half of a
    // postgres `::` cast, so the expansion must re-emit it ("$3" + ":" +
    // ":varchar[]"). A blind `replace(":MELIST:", "$3")` ate that colon and
    // produced `$3:varchar[]` — a postgres syntax error at the cast ("syntax
    // error at or near "":""), the 500 behind timelines_matrix_home_local_hybrid.
    // Longest-name-first matching keeps :ME: from clobbering :MELIST:.
    //
    // Bare-token fallback: the comparison half of the PAGER bounds was once
    // written `n."id" < :UNTIL` (no closing colon); neither the old replace
    // nor a strict scanner matched it, the naked `:UNTIL` reached postgres
    // and 500ed EVERY timeline fetch. A token followed by a non-word char
    // now binds too; `:MEx`-style alnum followers stay verbatim.
    const NAMES: [&str; 7] = ["MELIST", "ME", "FOL", "UNTIL", "SINCE", "LIMIT", "DIR"];
    let bind_of = |name: &str| -> String {
        match name {
            "MELIST" => format!("${me_list}"),
            "ME" => format!("${me}"),
            "FOL" => format!("${fol}"),
            "UNTIL" => format!("${until}"),
            "SINCE" => format!("${since}"),
            "LIMIT" => format!("${limit}"),
            _ => dir.to_string(), // DIR
        }
    };

    let mut out = String::with_capacity(sql.len() + 32);
    let bytes = sql.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' {
            let mut matched = false;
            for name in NAMES.iter() {
                let rest = &sql[i + 1..];
                if rest.starts_with(name) {
                    let close = i + 1 + name.len();
                    match bytes.get(close) {
                        // Proper `:NAME:` token — and when the closing colon
                        // is the first half of a `::` cast, re-emit it.
                        Some(&b':') => {
                            out.push_str(&bind_of(name));
                            if bytes.get(close + 1) == Some(&b':') {
                                out.push(':');
                            }
                            i = close + 1;
                            matched = true;
                        }
                        // Bare-token fallback (see doc above): `:NAME)` /
                        // `:NAME ` binds instead of leaking the token.
                        Some(&c) if !c.is_ascii_alphanumeric() && c != b'_' => {
                            out.push_str(&bind_of(name));
                            i = close;
                            matched = true;
                        }
                        _ => {}
                    }
                    break;
                }
            }
            if matched {
                continue;
            }
        }
        // Verbatim copy (single chars only; scans forward one at a time).
        out.push_str(&sql[i..i + 1]);
        i += 1;
    }
    out
}

fn dir_asc(since_id: Option<&str>) -> &'static str {
    if since_id.is_some() { "ASC" } else { "DESC" }
}

const INNER: &str =
    r#"SELECT n."id" FROM "note" n INNER JOIN "user" u ON u."id" = n."userId" WHERE "#;

const PAGER: &str = r#" AND (:UNTIL::varchar IS NULL OR n."id" < :UNTIL::varchar)
            AND (:SINCE::varchar IS NULL OR n."id" > :SINCE::varchar)
            ORDER BY n."id" :DIR: LIMIT :LIMIT:"#;

/// keep non-renotes and quotes only (withRenotes=false /
/// FanoutTimelineEndpointService excludePureRenotes; isQuote = text OR files
/// OR poll on top of renoteId).
const EXCLUDE_PURE_RENOTES: &str = r#" AND (n."renoteId" IS NULL OR n."text" IS NOT NULL
            OR n."fileIds" != '{}' OR EXISTS (SELECT 1 FROM "poll" WHERE "poll"."noteId" = n."id")) "#;

const WITH_FILES: &str = r#" AND n."fileIds" != '{}' "#;

/// generateVisibilityQuery for a logged-in viewer: public/home, self,
/// mentioned, visibleUser targets, followers-of-author, replies to me.
const VIS_ME: &str = r#" AND (
          (n."visibility" IN ('public','home'))
          OR n."userId" = :ME:
          OR (:MELIST::varchar[] <@ n."visibleUserIds")
          OR (:MELIST::varchar[] <@ n."mentions")
          OR (
            n."visibility" = 'followers'
            AND (n."userId" = ANY(:FOL:) OR n."replyUserId" = :ME:)
          )
        )"#;

const MUTE_SEL: &str = r#"(SELECT "muteeId" FROM "muting" WHERE "muterId" = :ME:
              AND ("expiresAt" IS NULL OR "expiresAt" > now()))"#;

const BLOCK_SEL: &str = r#"(SELECT "blockerId" FROM "blocking" WHERE "blockeeId" = :ME:)"#;

/// generateBaseNoteFilteringQuery (author / reply-target / renote-target
/// muting and blocking, renote-muting for pure renotes — the isUserRelated
/// set membership nuances of the fanout chain folded in) fused with the
/// fanout isUserRelated(note.renote, …) chains: the same mute/block checks
/// against the renote parent (a renote of a quote/reply of a muted user must
/// vanish; the chains never reach depth 3 in these timelines).
const BASE_ME: &str = r#" AND u."isSuspended" = FALSE
        AND (n."userId" IS NULL OR n."userId" NOT IN (MUTE_SEL))
        AND (n."replyUserId" IS NULL OR n."replyUserId" NOT IN (MUTE_SEL))
        AND (n."renoteUserId" IS NULL OR n."renoteUserId" NOT IN (MUTE_SEL))
        AND (n."userId" IS NULL OR n."userId" NOT IN (BLOCK_SEL))
        AND (n."replyUserId" IS NULL OR n."replyUserId" NOT IN (BLOCK_SEL))
        AND (n."renoteUserId" IS NULL OR n."renoteUserId" NOT IN (BLOCK_SEL))
        AND (n."renoteId" IS NULL OR n."text" IS NOT NULL
            OR n."userId" NOT IN (SELECT "muteeId" FROM "renote_muting" WHERE "muterId" = :ME:))
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

/// Suspended-author gate for guests (the only generateBaseNoteFilteringQuery
/// part that applies without a viewer).
const BASE_GUEST: &str = r#" AND u."isSuspended" = FALSE"#;

/// channelMuting renote-side gate (db paths' `note.renoteChannelId IS NULL OR
/// NOT IN mutedChannelIds`; fanout isChannelRelated): a renote/quote of a
/// muted channel's note drops out. chan-note: the denormalized
/// `renoteChannelId` column is written by the shared renote insert (sibling
/// lane), so the fold reads the renote parent's channel — upstream writes
/// `renoteChannelId = renote.channelId` at insert time and channelId rows are
/// never re-scoped afterwards, making the two forms equivalent. Binds NULL
/// for guests — the sub-select degrades to no-op, matching the me!=null-only
/// upstream application.
const RENOTE_CHANNEL_MUTE: &str = r#" AND (n."renoteId" IS NULL OR NOT EXISTS (
              SELECT 1 FROM "note" rc WHERE rc."id" = n."renoteId"
                AND rc."channelId" IS NOT NULL
                AND rc."channelId" IN (
                    SELECT "channelId" FROM "channel_muting" WHERE "userId" = :ME:)))"#;

/// Reply plane for the home/hybrid timelines:
/// - non-replies and self-replies always pass (the `getFromDb` reply filter);
/// - replies to me / my own replies ride the fanout `alwaysIncludeMyNotes` /
///   `localTimelineWithReplyTo:${me.id}` semantics;
/// - a followee's reply to a third party rides the
///   `NoteCreateService.pushToTl` `following.withReplies` push gate, but stays
///   subject to the endpoint `noteFilter` (a followers-visibility parent must
///   be authored by someone I follow or by me).
const REPLY_GATE_ME: &str = r#" AND (
          n."replyId" IS NULL
          OR n."replyUserId" = n."userId"
          OR n."userId" = :ME:
          OR n."replyUserId" = :ME:
          OR (
            EXISTS (SELECT 1 FROM "following" f WHERE f."followerId" = :ME:
                  AND f."followeeId" = n."userId" AND f."withReplies" = TRUE)
            AND (
              n."replyUserId" = ANY(:FOL:)
              OR EXISTS (SELECT 1 FROM "note" p WHERE p."id" = n."replyId"
                    AND p."visibility" <> 'followers')
            )
          )
        )"#;

/// local-timeline reply plane with the default withReplies=false: self-replies
/// plus, for logged-in viewers, replies addressed to me (upstream redis list
/// `localTimelineWithReplyTo:${me.id}`; the upstream db-only path diverges and
/// the asserting legs are FIXME #12065-skipped). withReplies=true drops the
/// gate (via `localTimelineWithReplies`).
const REPLY_GATE_LOCAL: &str = r#" AND (
          n."replyId" IS NULL
          OR n."replyUserId" = n."userId"
          OR n."replyUserId" = :ME:
        )"#;

fn wrap_outer(inner: &str, asc: bool) -> String {
    let dir = if asc { "ASC" } else { "DESC" };
    format!(r#"SELECT {NOTE_COLS} FROM "note" WHERE "id" IN ({inner}) ORDER BY "id" {dir}"#)
}

/// notes/timeline (home): own + followees, channel rows excluded, per-viewer
/// visibility, unified reply plane. Bind map:
/// $1 me+followees, $2 me, $3 [me], $4 untilId, $5 sinceId, $6 limit.
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

    let mut inner = String::from(INNER);
    // chan-note: followed-channel fold (timeline.ts getFromDb following-
    // Channels brackets) — non-channel rows ride the user plane, rows of the
    // viewer's followed channels (minus their channelMuting rows) flow for
    // any author. A channel note of a followed USER still stays out when the
    // CHANNEL is not followed (upstream case B keeps channelId IS NULL on
    // the user plane).
    inner.push_str(
        r#"( (n."channelId" IS NULL AND n."userId" = ANY(:FOL:))
          OR n."channelId" IN (SELECT cf."followeeId" FROM "channel_following" cf
                WHERE cf."followerId" = :ME:
                  AND cf."followeeId" NOT IN (
                      SELECT "channelId" FROM "channel_muting" WHERE "userId" = :ME:)) )"#,
    );
    inner.push_str(REPLY_GATE_ME);
    inner.push_str(VIS_ME);
    inner.push_str(BASE_ME);
    inner.push_str(RENOTE_CHANNEL_MUTE);
    if with_files {
        inner.push_str(WITH_FILES);
    }
    if !with_renotes {
        inner.push_str(EXCLUDE_PURE_RENOTES);
    }
    inner.push_str(PAGER);

    let inner = expand(&inner, 2, 3, 1, 4, 5, 6, dir_asc(since_id));
    let sql = inner
        .replace("MUTE_SEL", &expand(MUTE_SEL, 2, 3, 1, 4, 5, 6, ""))
        .replace("BLOCK_SEL", &expand(BLOCK_SEL, 2, 3, 1, 4, 5, 6, ""));

    sqlx::query_as::<_, NoteRow>(&wrap_outer(&sql, asc))
        .bind(me_or_followees)
        .bind(me_id)
        .bind(me_list)
        .bind(until_id)
        .bind(since_id)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// generateMutedNoteThreadQuery (QueryService.ts L178-192): the note's own row
/// and its thread root must BOTH sit outside the viewer's note_thread_muting
/// set (mute key = `note.threadId ?? note.id`, so an unthreaded root is
/// excluded by the first clause alone).
const THREAD_MUTE_ME: &str = r#" AND n."id" NOT IN
              (SELECT "threadId" FROM "note_thread_muting" WHERE "userId" = :ME:)
        AND (n."threadId" IS NULL OR n."threadId" NOT IN
              (SELECT "threadId" FROM "note_thread_muting" WHERE "userId" = :ME:))"#;

/// notes/mentions (endpoints/notes/mentions.ts getFromDb): notes addressing
/// the viewer — `me <@ mentions OR me <@ visibleUserIds` — paged newest-first
/// (sinceId-only → oldest-first), through generateVisibilityQuery
/// (VIS_ME), generateBaseNoteFilteringQuery (BASE_ME incl. muted/blocked
/// author / reply / renote target) and generateMutedNoteThreadQuery
/// (THREAD_MUTE_ME). Upstream's CONCAT(id) ordering degenerates to the plain
/// id sort (aid ids are monotonically sortable strings).
/// Bind map: $1 followees (unused — kept for the shared VIS_ME followers
/// branch), $2 me, $3 [me], $4 untilId, $5 sinceId, $6 limit, $7 visibility.
#[allow(clippy::too_many_arguments)]
pub async fn mentions_notes(
    pool: &PgPool,
    me_id: &str,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
    visibility: Option<&str>,
) -> Result<Vec<NoteRow>, sqlx::Error> {
    let me_list = vec![me_id.to_string()];
    let no_followees: Vec<String> = Vec::new();
    let asc = since_id.is_some();

    let mut inner = String::from(INNER);
    // mentions.ts L63-67: `:meIdAsList <@ note.mentions OR
    // :meIdAsList <@ note.visibleUserIds` (array-contains-me, the same
    // `<@` shape VIS_ME's specified-visibility branches use).
    inner.push_str(
        r#"((:MELIST::varchar[] <@ n."mentions") OR (:MELIST::varchar[] <@ n."visibleUserIds"))"#,
    );
    inner.push_str(VIS_ME);
    inner.push_str(BASE_ME);
    inner.push_str(THREAD_MUTE_ME);
    if visibility.is_some() {
        // mentions.ts L80-82 (`note.visibility = :visibility`).
        inner.push_str(r#" AND n."visibility" = $7::varchar"#);
    }
    inner.push_str(PAGER);

    let inner = expand(&inner, 2, 3, 1, 4, 5, 6, dir_asc(since_id));
    let sql = inner
        .replace("MUTE_SEL", &expand(MUTE_SEL, 2, 3, 1, 4, 5, 6, ""))
        .replace("BLOCK_SEL", &expand(BLOCK_SEL, 2, 3, 1, 4, 5, 6, ""));
    // local binding: the QueryAs borrows the SQL string, so the temporary
    // from wrap_outer must outlive the builder (E0716).
    let outer = wrap_outer(&sql, asc);

    let mut q = sqlx::query_as::<_, NoteRow>(&outer)
        .bind(no_followees)
        .bind(me_id)
        .bind(me_list)
        .bind(until_id)
        .bind(since_id)
        .bind(limit);
    if let Some(vis) = visibility {
        q = q.bind(vis);
    }
    q.fetch_all(pool).await
}

/// notes/local-timeline: `visibility='public' AND userHost IS NULL AND
/// channelId IS NULL`, guest-visible, mute/block gates for logged-in viewers
/// (NULL me degrades the sub-selects to no-ops). Upstream's db path applies no
/// withRenotes filter here — mirrored as-is. Bind map:
/// $1 untilId, $2 sinceId, $3 limit, $4 me (nullable).
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

    let mut inner = String::from(INNER);
    inner.push_str(
        r#"(n."visibility" = 'public') AND (n."userHost" IS NULL) AND (n."channelId" IS NULL)"#,
    );
    inner.push_str(BASE_ME);
    inner.push_str(BASE_GUEST);
    inner.push_str(RENOTE_CHANNEL_MUTE);
    if with_files {
        inner.push_str(WITH_FILES);
    }
    if !with_replies {
        inner.push_str(REPLY_GATE_LOCAL);
    }
    inner.push_str(PAGER);

    let inner = expand(&inner, 4, 4, 1, 1, 2, 3, dir_asc(since_id));
    let sql = inner
        .replace("MUTE_SEL", &expand(MUTE_SEL, 4, 4, 1, 1, 2, 3, ""))
        .replace("BLOCK_SEL", &expand(BLOCK_SEL, 4, 4, 1, 1, 2, 3, ""));

    sqlx::query_as::<_, NoteRow>(&wrap_outer(&sql, asc))
        .bind(until_id)
        .bind(since_id)
        .bind(limit)
        .bind(me_id)
        .fetch_all(pool)
        .await
}

/// notes/global-timeline (endpoints/notes/global-timeline.ts): public
/// non-channel notes across every host, guest-visible. Logged-in viewers ride
/// generateBaseNoteFilteringQuery + generateMutedUserRenotesQueryForNotes
/// (BASE_ME); guests ride generateUgcVisibilityQueryForVisitor, resolved by
/// the caller from `meta.ugcVisibilityForVisitor` (missing row → the TypeORM
/// column default 'local', fetch_resource.rs meta_ugc idiom): 'none' → 1=0,
/// 'local' → userHost IS NULL, 'all'/absent → no extra gate. Upstream applies
/// NO reply gate and NO renote-channel-mute gate here (unlike the local db
/// path — global-timeline.ts carries no ChannelMutingService leg), and no
/// withReplies param exists in the pinned paramDef. Bind map:
/// $1 untilId, $2 sinceId, $3 limit, $4 me (nullable).
#[allow(clippy::too_many_arguments)]
pub async fn global_timeline_notes(
    pool: &PgPool,
    me_id: Option<&str>,
    guest_ugc: Option<&str>,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
    with_files: bool,
    with_renotes: bool,
) -> Result<Vec<NoteRow>, sqlx::Error> {
    let asc = since_id.is_some();

    let mut inner = String::from(INNER);
    inner.push_str(r#"(n."visibility" = 'public') AND (n."channelId" IS NULL)"#);
    if me_id.is_none() {
        match guest_ugc {
            // generateUgcVisibilityQueryForVisitor 'none' → `1=0`.
            Some("none") => inner.push_str(" AND FALSE"),
            // 'local' → the visitor sees local rows only.
            Some("local") => inner.push_str(r#" AND (n."userHost" IS NULL)"#),
            _ => {}
        }
    }
    inner.push_str(BASE_ME);
    if with_files {
        inner.push_str(WITH_FILES);
    }
    if !with_renotes {
        inner.push_str(EXCLUDE_PURE_RENOTES);
    }
    inner.push_str(PAGER);

    let inner = expand(&inner, 4, 4, 1, 1, 2, 3, dir_asc(since_id));
    let sql = inner
        .replace("MUTE_SEL", &expand(MUTE_SEL, 4, 4, 1, 1, 2, 3, ""))
        .replace("BLOCK_SEL", &expand(BLOCK_SEL, 4, 4, 1, 1, 2, 3, ""));

    sqlx::query_as::<_, NoteRow>(&wrap_outer(&sql, asc))
        .bind(until_id)
        .bind(since_id)
        .bind(limit)
        .bind(me_id)
        .fetch_all(pool)
        .await
}

/// notes/hybrid-timeline (social): own + followees ∪ local public, channel
/// rows excluded, per-viewer visibility, unified reply plane (withReplies=true
/// drops the reply gate entirely — upstream db path). Bind map:
/// $1 me+followees, $2 me, $3 [me], $4 untilId, $5 sinceId, $6 limit.
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

    let mut inner = String::from(INNER);
    // chan-note: followed-channel fold (hybrid-timeline.ts getFromDb) —
    // followed channels (minus the viewer's channelMuting rows) join the
    // user/local public plane; without follows the channel plane stays NULL.
    inner.push_str(
        r#"( ( n."userId" = ANY(:FOL:) OR ((n."visibility" = 'public') AND (n."userHost" IS NULL)) )
             AND ( n."channelId" IS NULL
                   OR n."channelId" IN (SELECT cf."followeeId" FROM "channel_following" cf
                         WHERE cf."followerId" = :ME:
                           AND cf."followeeId" NOT IN (
                               SELECT "channelId" FROM "channel_muting" WHERE "userId" = :ME:)) ) )"#,
    );
    if !with_replies {
        inner.push_str(REPLY_GATE_ME);
    }
    inner.push_str(VIS_ME);
    inner.push_str(BASE_ME);
    inner.push_str(RENOTE_CHANNEL_MUTE);
    if with_files {
        inner.push_str(WITH_FILES);
    }
    inner.push_str(PAGER);

    let inner = expand(&inner, 2, 3, 1, 4, 5, 6, dir_asc(since_id));
    let sql = inner
        .replace("MUTE_SEL", &expand(MUTE_SEL, 2, 3, 1, 4, 5, 6, ""))
        .replace("BLOCK_SEL", &expand(BLOCK_SEL, 2, 3, 1, 4, 5, 6, ""));

    sqlx::query_as::<_, NoteRow>(&wrap_outer(&sql, asc))
        .bind(me_or_followees)
        .bind(me_id)
        .bind(me_list)
        .bind(until_id)
        .bind(since_id)
        .bind(limit)
        .fetch_all(pool)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Token order must never let a shorter token clobber a longer one (the
    /// historical note.rs `$MELIST`-vs-`$ME` mangling bug).
    #[test]
    fn expand_keeps_distinct_tokens() {
        let sql = ":MELIST::varchar[] <@ n.x AND n.y = :ME: AND z = ANY(:FOL:)";
        let out = expand(sql, 2, 3, 1, 4, 5, 6, "DESC");
        assert_eq!(out, "$3::varchar[] <@ n.x AND n.y = $2 AND z = ANY($1)");
    }

    /// The shared mute/block selectors splice into BASE_ME with the right
    /// bind numbers for each timeline shape.
    #[test]
    fn selector_splice_uses_bind_map() {
        let out = expand(MUTE_SEL, 4, 4, 1, 1, 2, 3, "");
        assert!(out.contains(r#""muterId" = $4"#), "{out}");
        let out = expand(BLOCK_SEL, 2, 3, 1, 4, 5, 6, "");
        assert!(out.contains(r#""blockeeId" = $2"#), "{out}");
    }

    /// The renote-channel mute gate collapses for guests (NULL me sub-select
    /// matches nothing) and binds the viewer id for logged-in shapes.
    #[test]
    fn channel_mute_gate_splices_bind_map() {
        let guest = expand(RENOTE_CHANNEL_MUTE, 4, 4, 1, 1, 2, 3, "");
        assert!(guest.contains(r#""userId" = $4"#), "{guest}");
        let me = expand(RENOTE_CHANNEL_MUTE, 2, 3, 1, 4, 5, 6, "");
        assert!(me.contains(r#""userId" = $2"#), "{me}");
    }

    /// The PAGER bounds must expand BOTH halves: the cast half
    /// (`:UNTIL::varchar`) and the comparison half. A leak of a naked
    /// `:UNTIL` (tl-v3c: the 500 behind every timeline fetch — postgres
    /// "syntax error at or near "":"") or any other token is fatal.
    #[test]
    fn pager_expands_cast_and_comparison_bounds() {
        let out = expand(PAGER, 2, 3, 1, 4, 5, 6, "DESC");
        assert!(
            out.contains(r#"($4::varchar IS NULL OR n."id" < $4::varchar)"#),
            "{out}"
        );
        assert!(
            out.contains(r#"($5::varchar IS NULL OR n."id" > $5::varchar)"#),
            "{out}"
        );
        assert!(out.contains(r#"ORDER BY n."id" DESC LIMIT $6"#), "{out}");
        for leak in [
            ":UNTIL", ":SINCE", ":LIMIT", ":DIR", ":ME", ":FOL", ":MELIST",
        ] {
            assert!(!out.contains(leak), "leaked {leak}: {out}");
        }
    }

    /// Bare-token fallback: a token without its closing colon (template
    /// leak, delimiter-followed) still expands — and an alnum follower
    /// (`:MEx`) stays verbatim so false positives stay impossible.
    #[test]
    fn expand_binds_bare_token_but_not_alnum_follower() {
        let out = expand(r#"n."id" < :UNTIL) AND x = :ME:"#, 2, 3, 1, 4, 5, 6, "");
        assert_eq!(out, r#"n."id" < $4) AND x = $2"#);
        assert_eq!(expand(":MEx", 2, 3, 1, 4, 5, 6, ""), ":MEx");
    }
}
