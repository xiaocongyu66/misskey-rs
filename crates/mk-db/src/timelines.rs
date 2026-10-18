//! Timeline trio db-path queries (notes/timeline, notes/local-timeline,
//! notes/hybrid-timeline).
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
//! Deliberate divergence from upstream: channel notes are excluded from all
//! three timelines here (`channelId IS NULL`; the renote side keeps flowing
//! minus the `renoteChannelId` channel-muting gate — `isChannelRelated` /
//! `note.renoteChannelId NOT IN mutedChannelIds` in the db paths). Upstream
//! folds followed channels into home/hybrid, so its Channel-inclusion legs
//! relying on that stay red (they belong to the channels lane); the
//! Redis-literal `FTT:` probes likewise need the push pipeline.
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
    sql.replace(":MELIST:", &format!("${me_list}"))
        .replace(":ME:", &format!("${me}"))
        .replace(":FOL:", &format!("${fol}"))
        .replace(":UNTIL:", &format!("${until}"))
        .replace(":SINCE:", &format!("${since}"))
        .replace(":LIMIT:", &format!("${limit}"))
        .replace(":DIR:", dir)
}

fn dir_asc(since_id: Option<&str>) -> &'static str {
    if since_id.is_some() { "ASC" } else { "DESC" }
}

const INNER: &str =
    r#"SELECT n."id" FROM "note" n INNER JOIN "user" u ON u."id" = n."userId" WHERE "#;

const PAGER: &str = r#" AND (:UNTIL::varchar IS NULL OR n."id" < :UNTIL)
            AND (:SINCE::varchar IS NULL OR n."id" > :SINCE)
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
/// muted channel's note drops out. Binds NULL for guests — the sub-select
/// degrades to no-op, matching the me!=null-only upstream application.
const RENOTE_CHANNEL_MUTE: &str = r#" AND (n."renoteChannelId" IS NULL OR n."renoteChannelId" NOT IN (
              SELECT "channelId" FROM "channel_muting" WHERE "userId" = :ME:))"#;

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
    inner.push_str(r#"n."channelId" IS NULL AND n."userId" = ANY(:FOL:)"#);
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
    inner.push_str(
        r#"( ( n."userId" = ANY(:FOL:) OR ((n."visibility" = 'public') AND (n."userHost" IS NULL)) )
             AND n."channelId" IS NULL )"#,
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
}
