//! `/streaming` timeline-channel half: `homeTimeline` + `localTimeline`
//! channel fanout (r-t-m slice — the renote-mute.ts stream assertions ride
//! these channels' `note` events).
//!
//! Upstream flows mirrored:
//! - `GlobalEventService.publishNotesStream` (`NoteCreateService.create`
//!   L842, `!silent`): the freshly packed note is pushed on the shared
//!   `notesStream` bus; every connected timeline channel gets it and filters
//!   per-viewer.
//! - `stream/channels/local-timeline.ts` `onNote`: local-only + public +
//!   non-channel notes, reply leg (`withReplies`/`following[author].withReplies`),
//!   pure-renote `withRenotes` leg, then `Channel.isNoteMutedOrBlocked`
//!   (user mute / blocked-me / **renote-mute keyed on the renoter** — quotes
//!   bypass that leg, misc/is-user-related.ts + channel.ts L118).
//! - `stream/channels/home-timeline.ts` `onNote`: own + followed authors
//!   (channel notes ride followingChannels — unported, so they never pass),
//!   the `Channel.isNoteVisibleForMe` ladder (specified/followers), reply and
//!   pure-renote legs, then the same mute/block ladder.
//! - The `NoteStreamingHidingService.filter` pass is not ported in this
//!   slice: its blocking legs are already terminal via `is_user_related`
//!   (`isNoteMutedOrBlocked` looks at author + reply + renote users), and its
//!   time-based `makeNotesHiddenBefore` leg has no packed user field in this
//!   port (user_lite carries the field unset). Role `ltlAvailable` and token
//!   `kind` permission gates likewise resolve to the default-allow arms of
//!   the pinned policies subset.
//!
//! The websocket transport (upgrade, auth, `connect`/`connected` handshake)
//! lives in `drive::streaming`; `connect_channel` is that hub's dispatch
//! target for these two channel names, `publish_notes` is the notes/create
//! hook target (marked `// stream-rtm`).

use std::sync::{LazyLock, Mutex};

use mk_db::sqlx::PgPool;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

/// Which timeline channel a subscription rides.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TimelineChannel {
    /// `home-timeline.ts` (requireCredential; own + followed authors).
    Home,
    /// `local-timeline.ts` (local public notes to anyone connected).
    Local,
}

impl TimelineChannel {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "homeTimeline" => Some(Self::Home),
            "localTimeline" => Some(Self::Local),
            _ => None,
        }
    }
}

/// One live timeline subscription (upstream: a Channel instance hanging off a
/// Connection). The `with*` params are the `init(params)` picks (`?? true`
/// for withRenotes, `?? false` for withReplies/withFiles).
#[derive(Clone)]
pub(crate) struct TimelineSub {
    user_id: String,
    ch: TimelineChannel,
    sub_id: String,
    with_renotes: bool,
    with_replies: bool,
    with_files: bool,
    tx: UnboundedSender<String>,
}

/// Live timeline subscriptions, unordered (upstream keeps them in a per-
/// connection Map; the fanout order is unobservable in the e2e asserts).
static TIMELINE_SINKS: LazyLock<Mutex<Vec<TimelineSub>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// `Connection.connectChannel` for the two channel names: parses params and
/// registers. Returned bool: whether the channel connected (unknown names
/// refuse like upstream's `getChannelConstructor` throw; the hub ignores
/// those). The `connected` pong reply stays in the hub's shared tail.
pub(crate) fn connect_channel(
    channel: &str,
    params: &Value,
    sub_id: &str,
    tx: UnboundedSender<String>,
    user_id: &str,
) -> bool {
    let Some(ch) = TimelineChannel::from_name(channel) else {
        return false;
    };
    // homeTimeline is requireCredential and the hub itself is auth-only;
    // localTimeline admits anonymous upstream — the token `kind` permission
    // gate only bites app tokens (upstream `this.token`), which no e2e flow
    // here uses, so no second credential check is carried.
    let flag = |name: &str, default: bool| match params.get(name) {
        Some(Value::Bool(b)) => *b,
        _ => default,
    };
    let (with_renotes, with_replies, with_files) = match ch {
        TimelineChannel::Home => (flag("withRenotes", true), false, flag("withFiles", false)),
        TimelineChannel::Local => (
            flag("withRenotes", true),
            flag("withReplies", false),
            flag("withFiles", false),
        ),
    };
    if let Ok(mut reg) = TIMELINE_SINKS.lock() {
        reg.retain(|s| !(s.user_id == user_id && s.sub_id == sub_id));
        reg.push(TimelineSub {
            user_id: user_id.to_string(),
            ch,
            sub_id: sub_id.to_string(),
            with_renotes,
            with_replies,
            with_files,
            tx,
        });
    }
    true
}

/// `disconnectChannel` for one subscription id.
pub(crate) fn remove_sub(user_id: &str, sub_id: &str) {
    if let Ok(mut reg) = TIMELINE_SINKS.lock() {
        reg.retain(|s| !(s.user_id == user_id && s.sub_id == sub_id));
    }
}

/// Connection teardown: drop every subscription of a hung-up socket.
pub(crate) fn remove_user(user_id: &str) {
    if let Ok(mut reg) = TIMELINE_SINKS.lock() {
        reg.retain(|s| s.user_id != user_id);
    }
}

/// `GlobalEventService.publishNotesStream` → per-connection `Channel.onNote`
/// ladder → `Channel.send('note', note)`. Best-effort like the Redis pubsub
/// upstream: never fails the note creation. Dead sinks (closed sockets not
/// yet reaped by the hub's cleanup) are dropped from the registry.
pub(crate) async fn publish_notes(pool: &PgPool, note: &Value) {
    let subs: Vec<TimelineSub> = match TIMELINE_SINKS.lock() {
        Ok(reg) => reg.clone(),
        Err(_) => Vec::new(),
    };
    if subs.is_empty() {
        return;
    }
    let mut dead: Vec<(String, String)> = Vec::new();
    for sub in subs {
        let deliver = match sub.ch {
            TimelineChannel::Home => passes_home(pool, &sub, note).await,
            TimelineChannel::Local => passes_local(pool, &sub, note).await,
        };
        if deliver {
            let frame = json!({
                "type": "channel",
                "body": { "id": sub.sub_id, "type": "note", "body": note },
            });
            if sub.tx.send(frame.to_string()).is_err() {
                dead.push((sub.user_id.clone(), sub.sub_id.clone()));
            }
        }
    }
    if !dead.is_empty() {
        if let Ok(mut reg) = TIMELINE_SINKS.lock() {
            reg.retain(|s| !dead.iter().any(|(u, i)| u == &s.user_id && i == &s.sub_id));
        }
    }
}

// ---------------------------------------------------------------------------
// packed-note accessors (misc/is-renote.ts / misc/is-user-related.ts shapes)
// ---------------------------------------------------------------------------

/// `isRenotePacked`: `note.renoteId != null` (the pack repeats renoteId
/// unconditionally, null when unset).
fn is_renote_packed(note: &Value) -> bool {
    !note.get("renoteId").map(Value::is_null).unwrap_or(true)
}

/// `isQuotePacked` (renoteId assumed set): text/cw/replyId/poll/fileIds any.
fn is_quote_packed(note: &Value) -> bool {
    let non_null = |k: &str| note.get(k).map(|v| !v.is_null()).unwrap_or(false);
    let files = note
        .get("fileIds")
        .and_then(Value::as_array)
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    non_null("text") || non_null("cw") || non_null("replyId") || non_null("poll") || files
}

/// `isUserRelated` — `note.userId`, then reply/renote *user* ids when they
/// differ from the author. This port packs no denormalized
/// replyUserId/renoteUserId, so the nested packs carry the ids (the upstream
/// fallback arms read `note.reply?.userId` / `note.renote?.userId`).
fn is_user_related(note: &Value, ids: &[String]) -> bool {
    let Some(author) = note.get("userId").and_then(Value::as_str) else {
        return false;
    };
    if ids.iter().any(|x| x == author) {
        return true;
    }
    for key in ["reply", "renote"] {
        let uid = note
            .get(key)
            .and_then(|n| n.get("userId"))
            .and_then(Value::as_str);
        if let Some(u) = uid {
            if u != author && ids.iter().any(|x| x == u) {
                return true;
            }
        }
    }
    false
}

/// Connection-scoped relation caches, fetched per note fanout (upstream
/// caches per connection in Connection.initialize — the per-note fetch keeps
/// the exact same observable semantics without a TTL re-sweep).
struct ViewerSets {
    /// `userIdsWhoMeMuting` — active rows only (muting carries expiresAt).
    muting: Vec<String>,
    /// `userIdsWhoBlockingMe` — blockers of the viewer.
    blocking_me: Vec<String>,
    /// `userIdsWhoMeMutingRenotes` — renote_muting mutees.
    renote_muting: Vec<String>,
    /// `following` — followee id + withReplies (Connection.initialize map).
    following: Vec<(String, bool)>,
}

async fn viewer_sets(pool: &PgPool, me: &str) -> ViewerSets {
    let muting: Vec<String> = mk_db::sqlx::query_scalar(
        r#"SELECT "muteeId" FROM "muting" WHERE "muterId" = $1 AND ("expiresAt" IS NULL OR "expiresAt" > now())"#,
    )
    .bind(me)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let blocking_me: Vec<String> =
        mk_db::sqlx::query_scalar(r#"SELECT "blockerId" FROM "blocking" WHERE "blockeeId" = $1"#)
            .bind(me)
            .fetch_all(pool)
            .await
            .unwrap_or_default();
    let renote_muting: Vec<String> =
        mk_db::sqlx::query_scalar(r#"SELECT "muteeId" FROM "renote_muting" WHERE "muterId" = $1"#)
            .bind(me)
            .fetch_all(pool)
            .await
            .unwrap_or_default();
    let following: Vec<(String, Option<bool>)> = mk_db::sqlx::query_as(
        r#"SELECT "followeeId", "withReplies" FROM "following" WHERE "followerId" = $1"#,
    )
    .bind(me)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    ViewerSets {
        muting,
        blocking_me,
        renote_muting,
        following: following
            .into_iter()
            .map(|(id, wr)| (id, wr.unwrap_or(false)))
            .collect(),
    }
}

impl ViewerSets {
    fn follows(&self, user_id: &str) -> bool {
        self.following.iter().any(|(id, _)| id == user_id)
    }
    fn with_replies_of(&self, user_id: &str) -> bool {
        self.following
            .iter()
            .find(|(id, _)| id == user_id)
            .map(|(_, wr)| *wr)
            .unwrap_or(false)
    }
}

/// `Channel.isNoteMutedOrBlocked` (channel.ts L108-124): instance mute and
/// channel mute legs resolve to the empty-set arms — unmuted instances /
/// channels are not tracked in this slice. User mute, blocked-me, and the
/// renote-mute leg (renoter in the set + pure renote — quotes bypass,
/// channel.ts L118) carry the renote-mute.ts behavior.
fn is_note_muted_or_blocked(note: &Value, sets: &ViewerSets) -> bool {
    if is_user_related(note, &sets.muting) || is_user_related(note, &sets.blocking_me) {
        return true;
    }
    // 流れてきたNoteがリノートをミュートしてるユーザが行ったもの
    if is_renote_packed(note)
        && !is_quote_packed(note)
        && note
            .get("userId")
            .and_then(Value::as_str)
            .map(|u| sets.renote_muting.iter().any(|x| x == u))
            .unwrap_or(false)
    {
        return true;
    }
    false
}

/// `Channel.isNoteVisibleForMe` (channel.ts L68-103): the specified /
/// followers ladder; public/home pass unconditionally.
fn is_note_visible_for_me(note: &Value, sub: &TimelineSub, sets: &ViewerSets) -> bool {
    let me = sub.user_id.as_str();
    let Some(author) = note.get("userId").and_then(Value::as_str) else {
        return false;
    };
    let visibility = note.get("visibility").and_then(Value::as_str);
    match visibility {
        Some("specified") => {
            author == me
                || note
                    .get("visibleUserIds")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().any(|v| v.as_str() == Some(me)))
                    .unwrap_or(false)
        }
        Some("followers") => {
            if author == me {
                return true;
            }
            // 自分の投稿に対するリプライ
            if note
                .get("reply")
                .and_then(|r| r.get("userId"))
                .and_then(Value::as_str)
                == Some(me)
            {
                return true;
            }
            // 自分へのメンション
            if note
                .get("mentions")
                .and_then(Value::as_array)
                .map(|a| a.iter().any(|v| v.as_str() == Some(me)))
                .unwrap_or(false)
            {
                return true;
            }
            // フォロワーかどうか
            sets.follows(author)
        }
        _ => true,
    }
}

fn with_files_filter(note: &Value, sub: &TimelineSub) -> bool {
    !(sub.with_files
        && note
            .get("fileIds")
            .and_then(Value::as_array)
            .map(|a| a.is_empty())
            .unwrap_or(true))
}

/// `LocalTimelineChannel.onNote` (local-timeline.ts L54-90). Deviations: the
/// `requireSigninToViewContents` trio resolves to the pass arms — the port's
/// user_lite never sets the flag and remote users do not exist here; the
/// `note.user.host !== null` leg evaluates on the packed user (always a null
/// host locally).
async fn passes_local(pool: &PgPool, sub: &TimelineSub, note: &Value) -> bool {
    if !with_files_filter(note, sub) {
        return false;
    }

    if note
        .get("user")
        .and_then(|u| u.get("host"))
        .map(|h| !h.is_null())
        .unwrap_or(false)
    {
        return false;
    }
    if note.get("visibility").and_then(Value::as_str) != Some("public") {
        return false;
    }
    if !note.get("channelId").map(Value::is_null).unwrap_or(true) {
        return false;
    }

    let sets = viewer_sets(pool, &sub.user_id).await;
    let me = sub.user_id.as_str();
    let author = note.get("userId").and_then(Value::as_str);
    // 関係ない返信は除外 (L65-69)
    if let Some(reply_uid) = note
        .get("reply")
        .and_then(|r| r.get("userId"))
        .and_then(Value::as_str)
    {
        let author_with_replies_sub = author.map(|a| sets.with_replies_of(a)).unwrap_or(false);
        if !author_with_replies_sub && !sub.with_replies {
            let is_me = author == Some(me);
            if reply_uid != me && !is_me && reply_uid != author.unwrap_or_default() {
                return false;
            }
        }
    }

    // 純粋なリノート（引用リノートでないリノート）の場合 (L71)
    if is_renote_packed(note) && !is_quote_packed(note) && !sub.with_renotes {
        return false;
    }

    if is_note_muted_or_blocked(note, &sets) {
        return false;
    }
    true
}

/// `HomeTimelineChannel.onNote` (home-timeline.ts L45-100). Deviation: the
/// `followingChannels` leg of the channel-note branch is an empty set in this
/// slice, so channel notes never pass (unfollowed would fail the same check).
async fn passes_home(pool: &PgPool, sub: &TimelineSub, note: &Value) -> bool {
    if !with_files_filter(note, sub) {
        return false;
    }

    let sets = viewer_sets(pool, &sub.user_id).await;
    let me = sub.user_id.as_str();
    let author = note
        .get("userId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let is_me = author == me;

    if note.get("channelId").map(|c| !c.is_null()).unwrap_or(false) {
        // そのチャンネルをフォローしていない（followingChannels は未導入 —
        // 空集合のためチャンネルノートは常に弾かれる）
        return false;
    }
    // その投稿のユーザーをフォローしていなかったら弾く
    if !is_me && !sets.follows(author) {
        return false;
    }

    if !is_note_visible_for_me(note, sub, &sets) {
        return false;
    }

    if let Some(reply_uid) = note
        .get("reply")
        .and_then(|r| r.get("userId"))
        .and_then(Value::as_str)
    {
        if sets.with_replies_of(author) {
            // 自分のフォローしていないユーザーの visibility: followers な投稿への返信は弾く
            if note
                .get("reply")
                .and_then(|r| r.get("visibility"))
                .and_then(Value::as_str)
                == Some("followers")
                && !sets.follows(reply_uid)
                && reply_uid != me
            {
                return false;
            }
        } else {
            // 「チャンネル接続主への返信」でもなければ、「チャンネル接続主が行った返信」でもなければ、
            // 「投稿者の投稿者自身への返信」でもない場合
            if reply_uid != me && !is_me && reply_uid != author {
                return false;
            }
        }
    }

    // 純粋なリノート（引用リノートでないリノート）の場合
    if is_renote_packed(note)
        && !is_quote_packed(note)
        && note.get("renote").map(|r| !r.is_null()).unwrap_or(false)
    {
        if !sub.with_renotes {
            return false;
        }
        if let Some(renote_reply_vis) = nested_followers_reply_leg(note) {
            let (reply_uid, reply_vis) = renote_reply_vis;
            if reply_vis == "followers" && !sets.follows(&reply_uid) && reply_uid != me {
                return false;
            }
        }
    }

    if is_note_muted_or_blocked(note, &sets) {
        return false;
    }
    true
}

/// The L78-79 renote.reply leg: `(reply.userId, reply.visibility)` when the
/// renote target carries a reply pack.
fn nested_followers_reply_leg(note: &Value) -> Option<(String, String)> {
    let r = note.get("renote")?.get("reply")?;
    let uid = r.get("userId")?.as_str()?.to_string();
    let vis = r.get("visibility")?.as_str()?.to_string();
    Some((uid, vis))
}
