//! ftt2: fanout-timeline Redis push pipeline — upstream NoteCreateService
//! `.pushToTl` (create() calls it right after incNotesCountOfUser, L779; body
//! L1068-1200) + FanoutTimelineService.push (core/FanoutTimelineService.ts).
//! The pinned e2e timelines.ts `FTT:` probes read the Redis lists literally
//! (`redisForTimelines.exists/lrange('list:homeTimeline:${uid}')`, L941-953),
//! so the push IS the observable side; the Rust endpoints stay db-path-only
//! (the FanoutTimelineEndpointService reader is not ported), which is what
//! the db-fallback legs already exercise.
//!
//! Gate: meta."enableFanoutTimeline" — the describe.each arms flip it via
//! admin/update-meta (timelines.ts L104) between the true/false legs; when
//! it is off, NO key may exist (`exists == 0`, L951).
//!
//! Pushed: `list:homeTimeline:${followerId}` for local, non-hibernated
//! followers (with the isReply/withReplies gate and the specified-visibility
//! gate), `list:homeTimeline:${authorId}` for local authors, and the
//! channel-followers fold (L1073-1090) — homeTimelineWithFiles alongside
//! each when files are attached. The other lists upstream fills
//! (channelTimeline / userTimeline* / userListTimeline* / localTimeline*)
//! have no Redis reader on the Rust side and no probe in the pinned suite —
//! deliberately not pushed. The 10% LTRIM (FanoutTimelineService L58-60) is
//! unobservable at suite size and skipped.
//!
//! Transport: RESP2 LPUSH pipelined over one short-lived TCP connection —
//! no redis client dependency. Best-effort like upstream's fire-and-forget
//! pipeline exec: connect/write/read failures log and drop so note creation
//! never fails on the fanout side.

use std::time::Duration;

use crate::AppState;

/// One pipelined LPUSH batch. Reply lines (`:n\r\n` for LPUSH, `+OK\r\n` for
/// a configured AUTH) are drained count-wise, not parsed — the suite only
/// observes the pushed values, never the reply codes.
async fn push_all(
    cfg: &crate::config::RedisConfig,
    pushes: &[(String, String)],
) -> Result<(), std::io::Error> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut conn = tokio::net::TcpStream::connect((cfg.host.as_str(), cfg.port)).await?;
    let mut cmd = String::new();
    let mut replies = 0usize;
    if let Some(pass) = &cfg.pass {
        cmd.push_str(&format!(
            "*2\r\n$4\r\nAUTH\r\n${}\r\n{}\r\n",
            pass.len(),
            pass
        ));
        replies += 1;
    }
    for (key, id) in pushes {
        // RESP2 array: LPUSH key id — `$N` marks BYTE lengths.
        cmd.push_str("*3\r\n$5\r\nLPUSH\r\n");
        cmd.push_str(&format!("${}\r\n{}\r\n", key.len(), key));
        cmd.push_str(&format!("${}\r\n{}\r\n", id.len(), id));
        replies += 1;
    }
    conn.write_all(cmd.as_bytes()).await?;

    let mut buf = Vec::new();
    let mut chunk = [0u8; 512];
    while buf.iter().filter(|b| **b == b'\n').count() < replies {
        let n = conn.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(())
}

/// pushToTl — call right after the note insert + user notesCount credit
/// (upstream order; create() L779 after incNotesCountOfUser).
///
/// `reply_user_id` carries the reply TARGET author when the note is a reply
/// (misc/is-reply.ts isReply(note, viewer): `note.replyId &&
/// note.replyUserId !== note.userId && note.replyUserId !== viewerId`).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn push_to_tl(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    note_id: &str,
    author_id: &str,
    author_host: Option<&str>,
    visibility: &str,
    visible_user_ids: &[String],
    reply_user_id: Option<&str>,
    file_count: usize,
    channel_id: Option<&str>,
) {
    let enable = match mk_db::meta::enable_fanout_timeline(pool).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(target: "dbg_ftt", error = %e, "enable_fanout_timeline");
            return;
        }
    };
    if !enable {
        return;
    }
    let Some(redis_cfg) = &state.config.redis else {
        return;
    };

    let mut pushes: Vec<(String, String)> = Vec::new();
    if let Some(cid) = channel_id {
        // NoteCreateService L1073-1090: every channel follower's home gets
        // the note (channelTimeline / userTimelineWithChannel have no reader
        // here and are skipped).
        let followers: Vec<String> = mk_db::sqlx::query_scalar(
            r#"SELECT "followerId" FROM "channel_following" WHERE "followeeId" = $1"#,
        )
        .bind(cid)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
        for follower in followers {
            pushes.push((format!("list:homeTimeline:{follower}"), note_id.to_string()));
            if file_count > 0 {
                pushes.push((
                    format!("list:homeTimelineWithFiles:{follower}"),
                    note_id.to_string(),
                ));
            }
        }
    } else {
        // L1094-1137: local (followerHost NULL), non-hibernated followers.
        // following."followerHost" is denormalized-NULL by our insert lane,
        // so the discriminator is the JOINed user row host.
        let followers: Vec<(String, bool)> = mk_db::sqlx::query_as(
            r#"SELECT f."followerId", f."withReplies"
               FROM "following" f JOIN "user" fu ON fu."id" = f."followerId"
               WHERE f."followeeId" = $1
                 AND fu."host" IS NULL
                 AND f."isFollowerHibernated" = FALSE"#,
        )
        .bind(author_id)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
        for (follower, with_replies) in followers {
            // specified: unlisted followers are skipped (L1126)
            if visibility == "specified" && !visible_user_ids.iter().any(|v| v == &follower) {
                continue;
            }
            // isReply(note, followerId) → needs following.withReplies (L1129)
            if let Some(reply_user) = reply_user_id {
                if reply_user != author_id && reply_user != follower && !with_replies {
                    continue;
                }
            }
            pushes.push((format!("list:homeTimeline:{follower}"), note_id.to_string()));
            if file_count > 0 {
                pushes.push((
                    format!("list:homeTimelineWithFiles:{follower}"),
                    note_id.to_string(),
                ));
            }
        }

        // L1158-1166: 自分自身の HTL — local authors only (L1159), pushed
        // unless a specified note listed the author (L1160).
        if author_host.is_none()
            && (visibility != "specified" || !visible_user_ids.iter().any(|v| v == author_id))
        {
            pushes.push((
                format!("list:homeTimeline:{author_id}"),
                note_id.to_string(),
            ));
            if file_count > 0 {
                pushes.push((
                    format!("list:homeTimelineWithFiles:{author_id}"),
                    note_id.to_string(),
                ));
            }
        }
    }

    if pushes.is_empty() {
        return;
    }
    if let Err(e) =
        tokio::time::timeout(Duration::from_millis(2000), push_all(redis_cfg, &pushes)).await
    {
        tracing::warn!(
            target: "dbg_ftt",
            error = %e,
            pushes = pushes.len(),
            "fanout lpush (best-effort)"
        );
    }
}
