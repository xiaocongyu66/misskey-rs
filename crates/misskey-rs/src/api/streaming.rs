//! `/streaming` timeline-channel half: the six `notesStream` channels of the
//! upstream `getChannelConstructor` census (`homeTimeline`, `localTimeline`,
//! `hybridTimeline`, `globalTimeline`, `userList`, `hashtag` — stream/CTAM
//! slice, the streaming.ts waitFire family) + the `main`-channel note events
//! (`mention` / `renote` / `reply`) that fan out beside the note packs.
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
//! - `stream/channels/hybrid-timeline.ts` `onNote`: home's ladder plus the
//!   public-local gate branch (`isMe || following || (user.host == null &&
//!   visibility === 'public')`, L60-71).
//! - `stream/channels/global-timeline.ts` `onNote`: public + non-channel
//!   + pure-renote `withRenotes` legs only — no reply gate (global delivers
//!   replies regardless of `withReplies`, upstream has none).
//! - `stream/channels/user-list.ts` `onNote`: list-membership gate
//!   (`membershipsMap[author]`), per-member `withReplies` reply leg (L107,
//!   WITHOUT the home/hybrid `reply.userId !== me` escape), pure-renote leg.
//! - `stream/channels/hashtag.ts` `onNote`: `q` AND/OR matching on
//!   `note.tags` (normalizeForSearch = NFKC + lowercase; the extractor
//!   lowercases and the census tags are ascii, so lowercase compares).
//! - `stream/channels/main.ts` `onData` `mention` arm: per-viewer
//!   isInstanceMuted / isNoteVisibleForMe / isNoteMutedOrBlocked filters
//!   (L55-61); the `renote`/`reply` arms have no case and pass through
//!   unfiltered. The publish targets mirror `NoteCreateService`
//!   createMentionedEvents (L991-1013, thread-muted targets skipped, local
//!   hosts only) + the reply leg (L853-868) + the renote leg (L872-885,
//!   `user.id !== renote.userId`, target local).
//! - The `isInstanceMuted` legs (misc/is-instance-muted.ts) read the author /
//!   reply / renote author hosts from the users table: the pack's user_lite
//!   carries host unset (local bias), so the host map is fetched per note —
//!   same observable gate, pinned `?? ''` arms degrade to local.
//! - The `NoteStreamingHidingService.filter` pass is not ported in this
//!   slice: its blocking legs are already terminal via `is_user_related`
//!   (`isNoteMutedOrBlocked` looks at author + reply + renote users), and its
//!   time-based `makeNotesHiddenBefore` leg has no packed user field in this
//!   port (user_lite carries the field unset). Role `ltlAvailable`/`gtlAvailable`
//!   and token `kind` permission gates likewise resolve to the default-allow
//!   arms of the pinned policies subset (the hub-level `read:account` app
//!   check lives in api::stream_auth::authenticate_hub).
//!
//! The websocket transport (upgrade, auth, `connect`/`connected` handshake)
//! lives in `drive::streaming`; `connect_channel` is that hub's dispatch
//! target for these channel names, `publish_notes` is the notes/create
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
    /// `hybrid-timeline.ts` (home ladder + the public-local gate branch).
    Hybrid,
    /// `global-timeline.ts` (public non-channel notes; no reply gate).
    Global,
    /// `user-list.ts` (owner list memberships; requireCredential false but
    /// the port hub is auth-only, and `init` derefs `this.user!` upstream).
    UserList,
    /// `hashtag.ts` (q AND/OR matrix over the note tags).
    Hashtag,
}

impl TimelineChannel {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "homeTimeline" => Some(Self::Home),
            "localTimeline" => Some(Self::Local),
            "hybridTimeline" => Some(Self::Hybrid),
            "globalTimeline" => Some(Self::Global),
            "userList" => Some(Self::UserList),
            "hashtag" => Some(Self::Hashtag),
            _ => None,
        }
    }
}

/// One live timeline subscription (upstream: a Channel instance hanging off a
/// Connection). The `with*` params are the `init(params)` picks (`?? true`
/// for withRenotes, `?? false` for withReplies/withFiles; home-timeline.ts
/// has no withReplies param — permanently false, fixed L38-40).
#[derive(Clone)]
pub(crate) struct TimelineSub {
    user_id: String,
    ch: TimelineChannel,
    sub_id: String,
    with_renotes: bool,
    with_replies: bool,
    with_files: bool,
    /// `userList` param — `listId` (init returns false when missing).
    list_id: String,
    /// `hashtag` param — `q: string[][]` (init validates the array shape).
    q: Vec<Vec<String>>,
    tx: UnboundedSender<String>,
}

/// Live timeline subscriptions, unordered (upstream keeps them in a per-
/// connection Map; the fanout order is unobservable in the e2e asserts).
static TIMELINE_SINKS: LazyLock<Mutex<Vec<TimelineSub>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// `Connection.connectChannel` for the six channel names: parses params,
/// validates (userList ownership against the pool, user-hub auth-only
/// connections all carry a user) and registers. Returned bool: whether the
/// channel connected (unknown names / failed init refuse like upstream's
/// `getChannelConstructor` throw / `init(..) === false` disconnect; the hub
/// then skips the `connected` pong). The `connected` pong reply stays in the
/// hub's shared tail.
pub(crate) async fn connect_channel(
    pool: &PgPool,
    channel: &str,
    params: &Value,
    sub_id: &str,
    tx: UnboundedSender<String>,
    user_id: &str,
) -> bool {
    let Some(ch) = TimelineChannel::from_name(channel) else {
        return false;
    };
    // The `kind` permission gate only bites app tokens (upstream
    // `this.token.permission.some(...)`) — app-token connections carry
    // `read:account` by construction (hub-level check in stream_auth), so
    // every censed channel's arm is the allow one.
    let flag = |name: &str, default: bool| match params.get(name) {
        Some(Value::Bool(b)) => *b,
        _ => default,
    };
    let (with_renotes, with_replies, with_files) = match ch {
        TimelineChannel::Home => (flag("withRenotes", true), false, flag("withFiles", false)),
        TimelineChannel::Local | TimelineChannel::Hybrid => (
            flag("withRenotes", true),
            flag("withReplies", false),
            flag("withFiles", false),
        ),
        // global-timeline.ts has no withReplies param (L44-45).
        TimelineChannel::Global => (flag("withRenotes", true), false, flag("withFiles", false)),
        // user-list.ts: withRenotes/withFiles only (L51-52).
        TimelineChannel::UserList => (flag("withRenotes", true), false, flag("withFiles", false)),
        // hashtag.ts: no with* params.
        TimelineChannel::Hashtag => (true, false, false),
    };
    // userList init: listId must be a string and the list must exist with
    // this connection's user as owner (user-list.ts L49-61; non-owner →
    // init false → disconnect, no pong).
    let mut list_id = String::new();
    if ch == TimelineChannel::UserList {
        let Some(id) = params.get("listId").and_then(Value::as_str) else {
            return false;
        };
        let owned = mk_db::lists::find_list(pool, id, user_id)
            .await
            .ok()
            .flatten()
            .is_some();
        if !owned {
            return false;
        }
        list_id = id.to_string();
    }
    // hashtag init: q must be an array of non-empty string arrays
    // (hashtag.ts L35-42).
    let mut q: Vec<Vec<String>> = Vec::new();
    if ch == TimelineChannel::Hashtag {
        let Some(rows) = params.get("q").and_then(Value::as_array) else {
            return false;
        };
        for row in rows {
            let Some(ands) = row.as_array().filter(|a| !a.is_empty()) else {
                return false;
            };
            let mut ands_v: Vec<String> = Vec::with_capacity(ands.len());
            for t in ands {
                match t.as_str() {
                    Some(s) => ands_v.push(s.to_string()),
                    None => return false,
                }
            }
            q.push(ands_v);
        }
    }
    if let Ok(mut reg) = TIMELINE_SINKS.lock() {
        reg.retain(|s| !(s.user_id == user_id && s.sub_id == sub_id));
        reg.push(TimelineSub {
            user_id: user_id.to_string(),
            ch,
            sub_id: sub_id.to_string(),
            with_renotes,
            with_replies,
            with_files,
            list_id,
            q,
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

/// Author / reply / renote / mention-target hosts, fetched from the users
/// table (misc/is-instance-muted.ts reads packed hosts; the port's user_lite
/// packs host unset, so the same gate reads the row hosts instead).
#[derive(Default, Clone)]
struct NoteHosts {
    /// user id → users."host" value (None = local, host column NULL).
    by_id: std::collections::HashMap<String, Option<String>>,
}

impl NoteHosts {
    async fn fetch(pool: &PgPool, note: &Value) -> Self {
        let author_id = note.get("userId").and_then(Value::as_str);
        let reply_id = note
            .get("reply")
            .and_then(|r| r.get("userId"))
            .and_then(Value::as_str);
        let renote_id = note
            .get("renote")
            .and_then(|r| r.get("userId"))
            .and_then(Value::as_str);
        let mut ids: Vec<String> = [author_id, reply_id, renote_id]
            .into_iter()
            .flatten()
            .map(String::from)
            .collect();
        // mention targets need their own hosts for the main-event ladder
        if let Some(ms) = note.get("mentions").and_then(Value::as_array) {
            for m in ms.iter().filter_map(Value::as_str) {
                if !ids.iter().any(|x| x == m) {
                    ids.push(m.to_string());
                }
            }
        }
        let mut out = Self::default();
        if ids.is_empty() {
            return out;
        }
        let rows: Vec<(String, Option<String>)> =
            mk_db::sqlx::query_as(r#"SELECT "id", "host" FROM "user" WHERE "id" = ANY($1)"#)
                .bind(&ids)
                .fetch_all(pool)
                .await
                .unwrap_or_default();
        for (id, host) in rows {
            out.by_id.insert(id, host);
        }
        out
    }

    /// The user's host value (None = the row is local / was not fetched —
    /// unknown ids degrade local like the packed `?? ''` arms).
    fn host(&self, uid: &str) -> Option<&str> {
        self.by_id.get(uid).and_then(|h| h.as_deref())
    }
}

/// `GlobalEventService.publishNotesStream` → per-connection `Channel.onNote`
/// ladder → `Channel.send('note', note)`, then the `main`-channel note
/// events (NoteCreateService L848-885: createMentionedEvents → reply leg →
/// renote leg). Best-effort like the Redis pubsub upstream: never fails the
/// note creation. Dead sinks (closed sockets not yet reaped by the hub's
/// cleanup) are dropped from the registry.
pub(crate) async fn publish_notes(pool: &PgPool, note: &Value) {
    let subs: Vec<TimelineSub> = match TIMELINE_SINKS.lock() {
        Ok(reg) => reg.clone(),
        Err(_) => Vec::new(),
    };
    // isInstanceMuted host map — one db probe shared by every channel pass
    // and the main-event ladder (lazy: notes with no connected sinks never
    // fetch; the main legs are unconditional below).
    let hosts = tokio::sync::OnceCell::new();
    let mut dead: Vec<(String, String)> = Vec::new();
    for sub in subs {
        let deliver = match sub.ch {
            TimelineChannel::Home => passes_home(pool, &sub, note, &hosts).await,
            TimelineChannel::Local => passes_local(pool, &sub, note, &hosts).await,
            TimelineChannel::Hybrid => passes_hybrid(pool, &sub, note, &hosts).await,
            TimelineChannel::Global => passes_global(pool, &sub, note, &hosts).await,
            TimelineChannel::UserList => passes_user_list(pool, &sub, note, &hosts).await,
            TimelineChannel::Hashtag => passes_hashtag(pool, &sub, note, &hosts).await,
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
    publish_main_note_events(pool, note, &hosts).await;
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
    /// `userMutedInstances` (Connection.fetch L115) — profile.mutedInstances.
    muted_hosts: Vec<String>,
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
    // userMutedInstances (user_profile."mutedInstances" jsonb string array).
    let muted_hosts: Vec<String> = match mk_db::user::find_profile(pool, me).await {
        Ok(p) => p
            .muted_instances
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };
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
        muted_hosts,
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

/// `isInstanceMuted` (misc/is-instance-muted.ts): the author / reply author /
/// renote author host in the viewer's muted set. The `?? ''` arms of the
/// upstream helper drop out here — a missing host (local) never matches.
fn is_instance_muted(note: &Value, hosts: &NoteHosts, sets: &ViewerSets) -> bool {
    if sets.muted_hosts.is_empty() {
        return false;
    }
    [
        note.get("userId")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        note.get("reply")
            .and_then(|r| r.get("userId"))
            .and_then(Value::as_str)
            .unwrap_or_default(),
        note.get("renote")
            .and_then(|r| r.get("userId"))
            .and_then(Value::as_str)
            .unwrap_or_default(),
    ]
    .into_iter()
    .filter(|uid| !uid.is_empty())
    .any(|uid| {
        hosts
            .host(uid)
            .map(|h| sets.muted_hosts.iter().any(|x| x == h))
            .unwrap_or(false)
    })
}

/// `Channel.isNoteMutedOrBlocked` (channel.ts L108-124): instances + users.
/// The channel-mute leg resolves to the empty-set arms — muted channels are
/// not tracked in this slice. User mute, blocked-me, and the renote-mute
/// leg (renoter in the set + pure renote — quotes bypass, channel.ts L118)
/// carry the renote-mute.ts behavior; the instance-mute leg is above.
fn is_note_muted_or_blocked(note: &Value, sets: &ViewerSets, hosts: Option<&NoteHosts>) -> bool {
    if let Some(hosts) = hosts {
        if is_instance_muted(note, hosts, sets) {
            return true;
        }
    }
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
/// followers ladder; public/home pass unconditionally. The followers leg
/// ends on `Object.hasOwn(following, note.userId)` → `author_followed`.
fn is_note_visible_for_me(note: &Value, me: &str, author_followed: bool) -> bool {
    let author = note.get("userId").and_then(Value::as_str);
    let visibility = note.get("visibility").and_then(Value::as_str);
    match visibility {
        Some("specified") => {
            author == Some(me)
                || note
                    .get("visibleUserIds")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().any(|v| v.as_str() == Some(me)))
                    .unwrap_or(false)
        }
        Some("followers") => {
            if author == Some(me) {
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
            author_followed
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
/// host locally — a signed-up-with-host user's posts are unasserted by the
/// pinned census, whose leg is TODO'd).
async fn passes_local(
    pool: &PgPool,
    sub: &TimelineSub,
    note: &Value,
    hosts_cell: &tokio::sync::OnceCell<NoteHosts>,
) -> bool {
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

    let hosts = settled_hosts(pool, note, hosts_cell, &sets).await;
    if is_note_muted_or_blocked(note, &sets, hosts.as_ref()) {
        return false;
    }
    true
}

/// `HomeTimelineChannel.onNote` (home-timeline.ts L45-100). Deviation: the
/// `followingChannels` leg of the channel-note branch is an empty set in this
/// slice, so channel notes never pass (unfollowed would fail the same check).
async fn passes_home(
    pool: &PgPool,
    sub: &TimelineSub,
    note: &Value,
    hosts_cell: &tokio::sync::OnceCell<NoteHosts>,
) -> bool {
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

    if !is_note_visible_for_me(note, me, sets.follows(author)) {
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

    let hosts = settled_hosts(pool, note, hosts_cell, &sets).await;
    if is_note_muted_or_blocked(note, &sets, hosts.as_ref()) {
        return false;
    }
    true
}

/// `HybridTimelineChannel.onNote` (hybrid-timeline.ts L55-119): home's ladder
/// with the widened gate branch — non-followed notes pass when the author is
/// local and the visibility public (L60-71); the reply/renote legs are home's
/// exactly (L83-102).
async fn passes_hybrid(
    pool: &PgPool,
    sub: &TimelineSub,
    note: &Value,
    hosts_cell: &tokio::sync::OnceCell<NoteHosts>,
) -> bool {
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

    if !note.get("channelId").map(Value::is_null).unwrap_or(true) {
        // そのチャンネルをフォローしていない（followingChannels は未導入 —
        // 空集合のためチャンネルノートは常に弾かれる）
        return false;
    }
    // 以下の条件に該当するノートのみ後続処理に通す (L61-71): 自分自身 / フォ
    // ロー中 / 全体公開のローカルの投稿。user.host は pack が运ばないため、
    // 非フォロー分岐で users テーブルから採る（リモートと署名 host は弾く）。
    if !is_me && !sets.follows(author) {
        let public_local = note.get("visibility").and_then(Value::as_str) == Some("public")
            && note
                .get("user")
                .and_then(|u| u.get("host"))
                .map(|h| h.is_null())
                .unwrap_or(true)
            && author_host_local(pool, note, hosts_cell).await;
        if !public_local {
            return false;
        }
    }

    if !is_note_visible_for_me(note, me, sets.follows(author)) {
        return false;
    }

    if let Some(reply_uid) = note
        .get("reply")
        .and_then(|r| r.get("userId"))
        .and_then(Value::as_str)
    {
        if sets.with_replies_of(author) || sub.with_replies {
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

    let hosts = settled_hosts(pool, note, hosts_cell, &sets).await;
    if is_note_muted_or_blocked(note, &sets, hosts.as_ref()) {
        return false;
    }
    true
}

/// `GlobalTimelineChannel.onNote` (global-timeline.ts L52-80): public +
/// non-channel + pure-renote legs — NO reply gate, upstream has none.
async fn passes_global(
    pool: &PgPool,
    sub: &TimelineSub,
    note: &Value,
    hosts_cell: &tokio::sync::OnceCell<NoteHosts>,
) -> bool {
    if !with_files_filter(note, sub) {
        return false;
    }

    if note.get("visibility").and_then(Value::as_str) != Some("public") {
        return false;
    }
    if !note.get("channelId").map(Value::is_null).unwrap_or(true) {
        return false;
    }
    // requireSigninToViewContents legs (L57-59) resolve to the pass arms —
    // the port's user_lite never sets the flag.

    // 純粋なリノート（引用リノートでないリノート）の場合
    if is_renote_packed(note) && !is_quote_packed(note) && !sub.with_renotes {
        return false;
    }

    let sets = viewer_sets(pool, &sub.user_id).await;
    let hosts = settled_hosts(pool, note, hosts_cell, &sets).await;
    if is_note_muted_or_blocked(note, &sets, hosts.as_ref()) {
        return false;
    }
    true
}

/// `UserListChannel.onNote` (user-list.ts L93-135). Deviation: the 5s
/// `updateListUsers` clock is folded into a per-note membership probe (same
/// `membershipsMap[userId]` observable); `isNoteMutedOrBlocked` carries the
/// muted-instance legs the #10443 census asserts.
async fn passes_user_list(
    pool: &PgPool,
    sub: &TimelineSub,
    note: &Value,
    hosts_cell: &tokio::sync::OnceCell<NoteHosts>,
) -> bool {
    // チャンネル投稿は無視する (L97)
    if !note.get("channelId").map(Value::is_null).unwrap_or(true) {
        return false;
    }

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

    // membershipsMap gate (L101) — the row also carries the member's
    // withReplies pick for the reply leg.
    let member_with_replies: Option<bool> = mk_db::sqlx::query_scalar(
        r#"SELECT "withReplies" FROM "user_list_membership"
           WHERE "userListId" = $1 AND "userId" = $2"#,
    )
    .bind(&sub.list_id)
    .bind(author)
    .fetch_optional(pool)
    .await
    .unwrap_or(None);
    let Some(member_with_replies) = member_with_replies else {
        return false;
    };

    if !is_note_visible_for_me(note, me, sets.follows(author)) {
        return false;
    }

    if let Some(reply_uid) = note
        .get("reply")
        .and_then(|r| r.get("userId"))
        .and_then(Value::as_str)
    {
        if member_with_replies {
            // 自分のフォローしていないユーザーの visibility: followers な投稿への返信は弾く
            // (L109 — NO `reply.userId !== this.user.id` escape here, unlike
            // home/hybrid: the pinned members/following map only)
            if note
                .get("reply")
                .and_then(|r| r.get("visibility"))
                .and_then(Value::as_str)
                == Some("followers")
                && !sets.follows(reply_uid)
            {
                return false;
            }
        } else {
            // 「チャンネル接続主への返信」でもなければ、「チャンネル接続主が行った返信」でもなければ、
            // 「投稿者の投稿者自身への返信」でもない場合 (L112)
            if reply_uid != me && !is_me && reply_uid != author {
                return false;
            }
        }
    }

    // 純粋なリノート（引用リノートでないリノート）の場合 (L116)
    if is_renote_packed(note) && !is_quote_packed(note) && !sub.with_renotes {
        return false;
    }

    let hosts = settled_hosts(pool, note, hosts_cell, &sets).await;
    if is_note_muted_or_blocked(note, &sets, hosts.as_ref()) {
        return false;
    }
    true
}

/// `HashtagChannel.onNote` (hashtag.ts L51-77): the q matrix over note.tags —
/// `q.some(someANDed => ANDed.every(tag => noteTags.includes(tag)))` with
/// normalizeForSearch folding (NFKC + lowercase upstream; the extractor
/// lowercases and the pinned census drives ascii tags, so lowercase).
async fn passes_hashtag(
    pool: &PgPool,
    sub: &TimelineSub,
    note: &Value,
    hosts_cell: &tokio::sync::OnceCell<NoteHosts>,
) -> bool {
    let note_tags: Vec<String> = note
        .get("tags")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_lowercase()))
                .collect()
        })
        .unwrap_or_default();
    let matched = sub
        .q
        .iter()
        .any(|anded| anded.iter().all(|tag| note_tags.iter().any(|t| t == tag)));
    if !matched {
        return false;
    }

    if !is_note_visible_for_me(note, &sub.user_id, false) {
        return false;
    }
    // requireSigninToViewContents legs (L57-59) resolve to the pass arms.

    let sets = viewer_sets(pool, &sub.user_id).await;
    let hosts = settled_hosts(pool, note, hosts_cell, &sets).await;
    if is_note_muted_or_blocked(note, &sets, hosts.as_ref()) {
        return false;
    }
    true
}

/// main.ts `onData` 'mention' arm filters + the pass-through 'renote' /
/// 'reply' arms, for the publish targets of NoteCreateService (create
/// L848-885 + createMentionedEvents L991-1013): mentioned LOCAL users minus
/// thread-muted get the 'mention' event (main.ts filters per viewer); the
/// reply target gets 'reply' (thread-muted gated, L856-868); the renote
/// target gets 'renote' when not the author (L881-885, target local). The
/// body is the packed note — upstream re-packs per viewer for mentions, the
/// census only reads userId/text.
async fn publish_main_note_events(
    pool: &PgPool,
    note: &Value,
    hosts_cell: &tokio::sync::OnceCell<NoteHosts>,
) {
    let author = note
        .get("userId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let thread_id = match mk_db::note::find_note_by_id(
        pool,
        note.get("id").and_then(Value::as_str).unwrap_or_default(),
    )
    .await
    {
        Ok(Some(row)) => row.thread_id.clone().unwrap_or_else(|| row.id.clone()),
        _ => note
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    };

    // --- 'mention' events: mentions ∖ author, local hosts, thread gates ---
    // (visibleUserIds merge into the mentions array mirrors upstream
    // extractMentionedUsers; targets = the pack's own array.)
    if let Some(mentions) = note.get("mentions").and_then(Value::as_array) {
        if !mentions.is_empty() {
            let hosts = note_hosts(pool, note, hosts_cell).await;
            for target in mentions.iter().filter_map(Value::as_str) {
                if target == author {
                    continue;
                }
                // ローカルユーザーのみ (createMentionedEvents isLocalUser)
                if hosts.host(target).is_some() {
                    continue;
                }
                if mk_db::note::thread_muted(pool, target, &thread_id)
                    .await
                    .unwrap_or(true)
                {
                    continue;
                }
                // main.ts 'mention' arm: per-viewer filters (L55-61).
                if !main_mention_visible(pool, target, note).await {
                    continue;
                }
                super::drive::streaming::publish_main(target, "mention", note.clone());
            }
        }
    }

    // --- 'reply' event to the reply target, thread-muted gated (L853-868) ---
    if let Some(reply_uid) = note
        .get("reply")
        .and_then(|r| r.get("userId"))
        .and_then(Value::as_str)
    {
        if reply_uid != author {
            let hosts = note_hosts(pool, note, hosts_cell).await;
            if hosts.host(reply_uid).is_none()
                && !mk_db::note::thread_muted(pool, reply_uid, &thread_id)
                    .await
                    .unwrap_or(true)
            {
                super::drive::streaming::publish_main(reply_uid, "reply", note.clone());
            }
        }
    }

    // --- 'renote' event to the renote target (L872-885) ---
    if note.get("renoteId").map(|r| !r.is_null()).unwrap_or(false) {
        if let Some(renote_uid) = note
            .get("renote")
            .and_then(|r| r.get("userId"))
            .and_then(Value::as_str)
        {
            if renote_uid != author {
                let hosts = note_hosts(pool, note, hosts_cell).await;
                if hosts.host(renote_uid).is_none() {
                    super::drive::streaming::publish_main(renote_uid, "renote", note.clone());
                }
            }
        }
    }
}

/// main.ts 'mention' arm visibility filters (pass-through for the other
/// event types): per-viewer mutedInstances + isNoteVisibleForMe +
/// isNoteMutedOrBlocked with the target's own relation sets.
async fn main_mention_visible(pool: &PgPool, target: &str, note: &Value) -> bool {
    let sets = viewer_sets(pool, target).await;
    let hosts_cell = tokio::sync::OnceCell::new();
    let hosts = note_hosts(pool, note, &hosts_cell).await;
    if is_instance_muted(note, hosts, &sets) {
        return false;
    }
    let author = note
        .get("userId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !is_note_visible_for_me(note, target, sets.follows(author)) {
        return false;
    }
    if is_note_muted_or_blocked(note, &sets, Some(hosts)) {
        return false;
    }
    true
}

// shared cell resolver
async fn note_hosts<'a>(
    pool: &PgPool,
    note: &Value,
    cell: &'a tokio::sync::OnceCell<NoteHosts>,
) -> &'a NoteHosts {
    cell.get_or_init(|| NoteHosts::fetch(pool, note)).await
}

/// The per-publish host cell resolver for the mute ladder: only viewers that
/// actually carry mutedInstances trigger the single shared db probe everyone
/// else passes None hosts (whose `?? ''` arms never match a muted set).
async fn settled_hosts(
    pool: &PgPool,
    note: &Value,
    hosts_cell: &tokio::sync::OnceCell<NoteHosts>,
    sets: &ViewerSets,
) -> Option<NoteHosts> {
    if sets.muted_hosts.is_empty() {
        return None;
    }
    Some(note_hosts(pool, note, hosts_cell).await.clone())
}

/// The hybrid public-local leg's host probe: the db-resolved author host is
/// None (= local) only for rows whose "host" IS NULL.
async fn author_host_local(
    pool: &PgPool,
    note: &Value,
    hosts_cell: &tokio::sync::OnceCell<NoteHosts>,
) -> bool {
    let author = note
        .get("userId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    note_hosts(pool, note, hosts_cell)
        .await
        .host(author)
        .is_none()
}

/// The L78-79 renote.reply leg: `(reply.userId, reply.visibility)` when the
/// renote target carries a reply pack.
fn nested_followers_reply_leg(note: &Value) -> Option<(String, String)> {
    let r = note.get("renote")?.get("reply")?;
    let uid = r.get("userId")?.as_str()?.to_string();
    let vis = r.get("visibility")?.as_str()?.to_string();
    Some((uid, vis))
}
