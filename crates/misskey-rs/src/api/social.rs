//! Social actions (M2 truthful slice): following/create+delete,
//! following/requests/accept+cancel (+ acceptAllFollowRequests on unlock),
//! mute/create+delete, blocking/create+delete, and the notes/create renote
//! path.
//!
//! Upstream refs (read-only): endpoints/following/{create,delete}.ts,
//! endpoints/following/requests/{accept,cancel}.ts, endpoints/mute/*.ts,
//! endpoints/blocking/*.ts, core/UserFollowingService.ts,
//! core/UserMutingService.ts, core/UserBlockingService.ts,
//! core/NoteCreateService.ts (renote rules).
//!
//! M2 scope: local targets only (remote users need the AP delivery flow —
//! TODO(M2+): remote flow). Meta errors without `httpStatusCode` surface as
//! kind=client 400 (`ApiCallService.#sendApiError`).

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// The endpoint names routed into this module (single source of truth for the
/// `social-agent` dispatch block in mod.rs).
pub(crate) const SOCIAL_ENDPOINTS: &[&str] = &[
    "following/create",
    "following/delete",
    "following/requests/accept",
    "following/requests/cancel",
    "mute/create",
    "mute/delete",
    "blocking/create",
    "blocking/delete",
    // users.ts census: flips the pack's isRenoteMuted relation flag
    "renote-mute/create",
    "renote-mute/delete",
];

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_social", error = %e, "db error: {context}");
    api_err(ApiError::internal_error())
}

/// Meta error without httpStatusCode: kind=client → default 400.
fn meta_err(code: &str, id: &str, message: &str) -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        code,
        id,
        message,
    ))
}

/// requireCredential auth chain (ApiCallService.call + AuthenticateService):
/// no credential at all → CREDENTIAL_REQUIRED; present-but-unresolvable
/// native token → AUTHENTICATION_FAILED; suspended user →
/// YOUR_ACCOUNT_SUSPENDED. Returns the resolved me id.
pub(crate) async fn require_checked_user(
    pool: &mk_db::sqlx::PgPool,
    token: Option<&str>,
    credential_declared: bool,
) -> Result<String, Response> {
    let auth_failed = || {
        api_err(ApiError::new(
            401,
            mk_types::api_error::ApiErrorKind::Client,
            "AUTHENTICATION_FAILED",
            "b0a7f5f8-dc2f-4171-b91f-de88ad238e14",
            "Authentication failed. Please ensure your token is correct.",
        ))
    };
    let Some(token) = token else {
        if credential_declared {
            // a credential was sent but resolves to no native user
            return Err(auth_failed());
        }
        return Err(api_err(ApiError::new(
            401,
            mk_types::api_error::ApiErrorKind::Client,
            "CREDENTIAL_REQUIRED",
            "1384574d-a912-4b81-8601-c7b1c4085df1",
            "Credential required.",
        )));
    };
    if token.len() != 16 {
        return Err(auth_failed());
    }
    let Some(me_id) = super::resolve_me(pool, Some(token)).await else {
        return Err(auth_failed());
    };
    // call(): requireCredential suspended gate
    let row = match mk_db::user::find_user_by_id(pool, &me_id).await {
        Ok(r) => r,
        Err(e) => return Err(db_err("require_checked_user find_user_by_id", e)),
    };
    match row {
        None => Err(auth_failed()), // token hits no user → AuthenticationError
        Some(r) if r.is_suspended => Err(api_err(ApiError::new(
            403,
            mk_types::api_error::ApiErrorKind::Permission,
            "YOUR_ACCOUNT_SUSPENDED",
            "a8c724b3-6e9c-4b46-b1a8-bc3ed6258370",
            "Your account has been suspended.",
        ))),
        Some(_) => Ok(me_id),
    }
}

/// Fetch followee/mutee/blockee row (GetterService.getUser); None → caller
/// raises the endpoint's own NO_SUCH_USER meta error (each has its own id).
async fn get_user(pool: &mk_db::sqlx::PgPool, id: &str) -> Result<Option<User>, Response> {
    mk_db::user::find_user_by_id(pool, id)
        .await
        .map_err(|e| db_err("get_user", e))
}

async fn get_profile(pool: &mk_db::sqlx::PgPool, id: &str) -> Result<Profile, Response> {
    mk_db::user::find_profile(pool, id)
        .await
        .map_err(|e| db_err("get_profile", e))
}

/// Pack the target like users/show (UserDetailedNotMe with the me→target
/// relation fields) — mirrors `userEntityService.pack(followee.id, me)`.
async fn pack_target(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    target: &User,
    me_id: &str,
) -> Result<Value, Response> {
    let profile = get_profile(pool, &target.id).await?;
    let relation = mk_db::relation::get_relation(pool, me_id, &target.id)
        .await
        .map_err(|e| db_err("pack_target get_relation", e))?;
    let ctx = mk_core::services::user_pack::InstanceCtx {
        url: &state.config.url,
        host: &state.config.host(),
    };
    Ok(mk_core::services::user_pack::pack_inline(
        target,
        &profile,
        false,
        Some(&relation),
        ctx,
    ))
}

fn ok_empty() -> Response {
    // upstream `send(reply, undefined)` → 204
    StatusCode::NO_CONTENT.into_response()
}

fn ok_json(v: Value) -> Response {
    (StatusCode::OK, Json(v)).into_response()
}

fn user_id_param<'a>(body: &'a Value) -> Result<&'a str, Response> {
    body.get("userId").and_then(|v| v.as_str()).ok_or_else(|| {
        api_err(ApiError::invalid_param(json!({
            "param": "userId",
            "reason": "expected a string (misskey:id)",
        })))
    })
}

/// NotificationService.createNotification gates for note-reasoned
/// notifications (mention/reply/renote/quote/reaction): a notifier cannot
/// notify itself and is dropped when the notifiee mutes it — the muting gate
/// is TYPE-AGNOSTIC (NotificationService.ts L111-114, checked before the
/// recieveConfig ladder), so the follow-family legs drop the same way
/// (recieveConfig / suspended-notifier pack filtering not modeled —
/// TODO(M2+)). Insert side + push fan-out: the Redis-stream fanout is not
/// ported.
pub(crate) async fn notify_note_event(
    pool: &mk_db::sqlx::PgPool,
    notifiee_id: &str,
    notifier_id: &str,
    reason: &str,
    note_id: &str,
) {
    // ntf-mute: gate + insert centralized in mk-db
    // (notification::insert_if_not_muted) so both notify sites share ONE
    // type-agnostic muting drop. `Ok(false)` = self-notification (L110) or
    // muted notifier (L111-114): upstream `createNotification` returns null
    // there, BEFORE the push tail, so neither a row nor a push is produced.
    match mk_db::notification::insert_if_not_muted(
        pool,
        &mk_db::notification::NewNotification {
            id: &mk_types::id::gen_aid_now(),
            r#type: reason,
            user_id: notifiee_id,
            notifier_id: Some(notifier_id),
            note_id: Some(note_id),
            reaction: None,
        },
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => return,
        Err(e) => {
            tracing::error!(target: "dbg_social", error = %e, "notify_note_event insert");
            return;
        }
    }
    // push-send: NotificationService.createNotification push tail (L196
    // `pushNotification(notifieeId, 'notification', packed)`) — mention /
    // reply / renote / quote / reaction (and the follower 'note' leg) fan out
    // here fire-and-forget; all delivery errors are logged and swallowed
    // inside send_to_user (upstream `.catch`).
    let (pool, notifiee_id, reason, note_id) = (
        pool.clone(),
        notifiee_id.to_string(),
        reason.to_string(),
        note_id.to_string(),
    );
    tokio::spawn(async move {
        let _ = mk_core::push_deliver::send_to_user(
            &pool,
            &notifiee_id,
            &reason,
            &serde_json::json!({ "noteId": note_id }),
        )
        .await;
    });
}

/// note-notify (upstream NoteCreateService.create L790-813): the
/// `data.reply == null` branch — every following row of the author carrying
/// `notify: 'normal'` gets a notification type 'note' with the fresh note id
/// (L806 createNotification('note', { noteId })). The caller owns the
/// `visibility !== 'specified'` guard (L796); `pure_renote` re-adds the
/// per-follower renote-mute drop (L800-804 — `renoteMutingsCache.fetch(
/// followerId).has(author)`); text/quote lanes fall through with false.
/// Muting/self drops ride the shared insert gate as usual.
pub(crate) async fn notify_note_to_followers(
    pool: &mk_db::sqlx::PgPool,
    author_id: &str,
    note_id: &str,
    pure_renote: bool,
) {
    let follower_ids = match mk_db::social::notify_follower_ids(pool, author_id).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(target: "dbg_social", error = %e, "notify_follower_ids");
            return;
        }
    };
    for follower_id in follower_ids {
        if pure_renote {
            match mk_db::social::renote_muting_exists(pool, &follower_id, author_id).await {
                Ok(true) => continue,
                Ok(false) => {}
                Err(e) => {
                    tracing::error!(target: "dbg_social", error = %e, "renote_muting_exists");
                }
            }
        }
        // the per-follower push ('note') rides notify_note_event's
        // insert-gated fire-and-forget spawn — no separate delivery here.
        notify_note_event(pool, &follower_id, author_id, "note", note_id).await;
    }
}

/// createNotification legs WITHOUT a bound note (packed shape keeps
/// id/createdAt/type/userId/user — the follow family):
/// - 'follow' (UserFollowingService.insertFollowingDoc L357),
/// - 'receiveFollowRequest' (createFollowRequest L543),
/// - 'followRequestAccepted' (insertFollowingDoc L296, notifier = followee).
/// Both carry a notifier, so both ride the SAME type-agnostic muting gate
/// (NotificationService.ts L111-114) — the Mute e2e follow/followRequest
/// legs read exactly that drop.
pub(crate) async fn notify_follow_family_event(
    pool: &mk_db::sqlx::PgPool,
    notifiee_id: &str,
    notifier_id: &str,
    reason: &str, // "follow" | "receiveFollowRequest" | "followRequestAccepted"
) {
    // ntf-mute: shared gate/insert helper (self-follow suppression +
    // type-agnostic muting drop, same lane as notify_note_event).
    if let Err(e) = mk_db::notification::insert_if_not_muted(
        pool,
        &mk_db::notification::NewNotification {
            id: &mk_types::id::gen_aid_now(),
            r#type: reason,
            user_id: notifiee_id,
            notifier_id: Some(notifier_id),
            note_id: None,
            reaction: None,
        },
    )
    .await
    {
        tracing::error!(target: "dbg_social", error = %e, "notify_follow_family insert");
    }
}

fn with_replies_param(body: &Value) -> Result<bool, Response> {
    match body.get("withReplies") {
        None => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(super::invalid_param("withReplies", "expected a boolean")),
    }
}

/// shouldRequireFollowApproval (misc): local-follower approval window from
/// the followee's profile (`followApprovalLocalSeconds`, unset → not needed;
/// the follower is local, so remote seconds never apply).
fn requires_age_approval(followee_profile: &Profile, follower_id: &str) -> bool {
    let seconds = followee_profile.follow_approval_local_seconds.unwrap_or(0);
    if seconds <= 0 {
        return false;
    }
    match mk_types::id::parse_aid_date(follower_id) {
        Some(created_ms) => super::now_ms() as i64 - created_ms < seconds as i64 * 1000,
        None => false,
    }
}

type User = mk_db::user::LocalUserRow;
type Profile = mk_db::user::ProfileRow;

// ---------------------------------------------------------------------------
// following/create — endpoints/following/create.ts + UserFollowingService
// ---------------------------------------------------------------------------

/// arena-move: UserFollowingService L210-220 — a locked account
/// auto-accepts a follower whose own alsoKnownAs points back at an account
/// the followee has already accepted (AccountMoveService.validateAlsoKnownAs
/// with the `instant` follow-existence check). Local slice: the follower's
/// alias URIs resolve through the server's own /users/ shortcut; the check
/// passes when the aliased user has moved TO the follower
/// (`src.movedToUri === dstUri`) and the followee already follows them.
async fn alias_auto_accept(
    pool: &mk_db::sqlx::PgPool,
    base_url: &str,
    follower_id: &str,
    followee_id: &str,
) -> Result<bool, mk_db::sqlx::Error> {
    let Some(follower) = mk_db::user::find_user_by_id(pool, follower_id).await? else {
        return Ok(false);
    };
    let aliases =
        mk_core::services::move_account::unmarshal_aliases(follower.also_known_as.as_deref());
    if aliases.is_empty() {
        return Ok(false);
    }
    let follower_uri = mk_core::services::move_account::local_user_uri(base_url, follower_id);
    for uri in &aliases {
        let Some(src_id) = mk_core::services::move_account::local_id_from_uri(base_url, uri) else {
            continue;
        };
        let Some(src) = mk_db::user::find_user_by_id(pool, &src_id).await? else {
            continue;
        };
        if src.moved_to_uri.as_deref() != Some(follower_uri.as_str()) {
            continue;
        }
        if mk_db::social::following_exists(pool, &src_id, followee_id).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn following_create(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let user_id = user_id_param(body)?;
    // 自分自身
    if me_id == user_id {
        return Err(meta_err(
            "FOLLOWEE_IS_YOURSELF",
            "26fbe7bb-a331-4857-af17-205b426669a9",
            "Followee is yourself.",
        ));
    }
    let followee = get_user(pool, user_id).await?.ok_or_else(|| {
        meta_err(
            "NO_SUCH_USER",
            "fcd2eef9-a9b2-4c4f-8624-038099e90aa5",
            "No such user.",
        )
    })?;
    let with_replies = with_replies_param(body)?;

    // UserFollowingService.follow: blocking/blocked gates (both directions).
    if mk_db::social::blocking_exists(pool, me_id, user_id)
        .await
        .map_err(|e| db_err("following_create blocking_exists", e))?
    {
        return Err(meta_err(
            "BLOCKING",
            "4e2206ec-aa4f-4960-b865-6c23ac38e2d9",
            "You are blocking that user.",
        ));
    }
    if mk_db::social::blocking_exists(pool, user_id, me_id)
        .await
        .map_err(|e| db_err("following_create blocked_exists", e))?
    {
        return Err(meta_err(
            "BLOCKED",
            "c4ab57cc-4e41-45e9-bfd9-584f61e35ce0",
            "You are blocked by that user.",
        ));
    }
    // already following (local follower → exception)
    if mk_db::social::following_exists(pool, me_id, user_id)
        .await
        .map_err(|e| db_err("following_create following_exists", e))?
    {
        return Err(meta_err(
            "ALREADY_FOLLOWING",
            "35387507-38c7-4cb9-9197-300b93783fa0",
            "You are already following that user.",
        ));
    }

    let followee_profile = get_profile(pool, user_id).await?;

    // follow(): 鍵アカウント・承認期間・carefulBot → フォローリクエスト.
    // (Remote-follower / silenced-host conditions are federation flow; M2 local.)
    let follower_is_bot = mk_db::social::user_is_bot(pool, me_id)
        .await
        .map_err(|e| db_err("following_create user_is_bot", e))?;
    let needs_request = followee.is_locked
        || requires_age_approval(&followee_profile, me_id)
        || (followee_profile.careful_bot && follower_is_bot);

    if needs_request {
        // 鍵アカウントでも既にフォローされている場合はスルー + autoAcceptFollowed
        let auto_accept = mk_db::social::following_exists(pool, user_id, me_id)
            .await
            .map_err(|e| db_err("following_create isFollowed", e))?
            && followee_profile.auto_accept_followed;
        // arena-move: the moved-follower carve-out (UserFollowingService
        // L210-220) rides the same needs_request branch — bob→frank after the
        // move auto-accepts because frank already accepted bob's old account.
        let auto_accept = auto_accept
            || alias_auto_accept(pool, &state.config.url, me_id, user_id)
                .await
                .map_err(|e| db_err("following_create alias_auto_accept", e))?;
        if !auto_accept {
            mk_db::social::insert_follow_request(
                pool,
                &mk_types::id::gen_aid_now(),
                me_id,
                user_id,
                with_replies,
            )
            .await
            .map_err(|e| db_err("following_create insert_follow_request", e))?;
            // UserFollowingService.createFollowRequest L543: the followee
            // gets a 'receiveFollowRequest' notification (muting-gated —
            // the Mute e2e フォローリクエスト leg).
            notify_follow_family_event(pool, user_id, me_id, "receiveFollowRequest").await;
            // the endpoint still answers with the packed followee
            return Ok(ok_json(pack_target(state, pool, &followee, me_id).await?));
        }
    }

    // insertFollowingDoc (+ pending-request cleanup inside) + counter bumps
    let had_request = mk_db::social::insert_following(
        pool,
        &mk_types::id::gen_aid_now(),
        me_id,
        user_id,
        with_replies,
    )
    .await
    .map_err(|e| db_err("following_create insert_following", e))?;
    // arena-move: UserFollowingService.incrementFollowing L305-316 — the
    // counters move only when NEITHER endpoint has moved (the eve→
    // moved-alice row lands with counters untouched).
    if !(mk_db::move_account::get_moved_to_uri(pool, me_id)
        .await
        .map_err(|e| db_err("following_create moved[me]", e))?
        .is_some()
        || mk_db::move_account::get_moved_to_uri(pool, user_id)
            .await
            .map_err(|e| db_err("following_create moved[followee]", e))?
            .is_some())
    {
        mk_db::social::bump_follow_counts(pool, me_id, user_id, 1)
            .await
            .map_err(|e| db_err("following_create bump", e))?;
    }

    // insertFollowingDoc requestExist leg: a swept pending request (re-follow
    // of a locked target that still held the earlier request) still tells the
    // follower their request was accepted (followRequestAccepted,
    // muting-gated).
    if had_request {
        notify_follow_family_event(pool, me_id, user_id, "followRequestAccepted").await;
    }
    // UserFollowingService.insertFollowingDoc L357: the followee is notified
    // 'follow' (notifier = the follower; muting-gated — the Mute e2e
    // フォロー通知 leg).
    notify_follow_family_event(pool, user_id, me_id, "follow").await;
    Ok(ok_json(pack_target(state, pool, &followee, me_id).await?))
}

// ---------------------------------------------------------------------------
// following/delete
// ---------------------------------------------------------------------------

async fn following_delete(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let user_id = user_id_param(body)?;
    if me_id == user_id {
        return Err(meta_err(
            "FOLLOWEE_IS_YOURSELF",
            "d9e400b9-36b0-4808-b1d8-79e707f1296c",
            "Followee is yourself.",
        ));
    }
    let followee = get_user(pool, user_id).await?.ok_or_else(|| {
        meta_err(
            "NO_SUCH_USER",
            "5b12c78d-2b28-4dca-99d2-f56139b42ff8",
            "No such user.",
        )
    })?;

    let exists = mk_db::social::following_exists(pool, me_id, user_id)
        .await
        .map_err(|e| db_err("following_delete exists", e))?;
    if !exists {
        return Err(meta_err(
            "NOT_FOLLOWING",
            "5dbf82f5-c92b-40b1-87d1-6c8c0741fd09",
            "You are not following that user.",
        ));
    }

    mk_db::social::delete_following(pool, me_id, user_id)
        .await
        .map_err(|e| db_err("following_delete", e))?;
    // arena-move: UserFollowingService.decrementFollowing L424-437 — moved
    // endpoints age out silently (eve's unfollow of the moved alice).
    if !(mk_db::move_account::get_moved_to_uri(pool, me_id)
        .await
        .map_err(|e| db_err("following_delete moved[me]", e))?
        .is_some()
        || mk_db::move_account::get_moved_to_uri(pool, user_id)
            .await
            .map_err(|e| db_err("following_delete moved[followee]", e))?
            .is_some())
    {
        mk_db::social::bump_follow_counts(pool, me_id, user_id, -1)
            .await
            .map_err(|e| db_err("following_delete bump", e))?;
    }

    Ok(ok_json(pack_target(state, pool, &followee, me_id).await?))
}

// ---------------------------------------------------------------------------
// following/requests/accept + cancel
// ---------------------------------------------------------------------------

async fn following_requests_accept(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let user_id = user_id_param(body)?;
    // Fetch follower (the requester)
    get_user(pool, user_id).await?.ok_or_else(|| {
        meta_err(
            "NO_SUCH_USER",
            "66ce1645-d66c-46bb-8b79-96739af885bd",
            "No such user.",
        )
    })?;

    let Some(with_replies) = mk_db::social::follow_request_with_replies(pool, me_id, user_id)
        .await
        .map_err(|e| db_err("accept find_request", e))?
    else {
        return Err(meta_err(
            "NO_FOLLOW_REQUEST",
            "bcde4f8b-0913-4614-8881-614e522fb041",
            "No follow request.",
        ));
    };

    // acceptFollowRequest → insertFollowingDoc(followee=me, follower=requester)
    let had_request = mk_db::social::insert_following(
        pool,
        &mk_types::id::gen_aid_now(),
        user_id,
        me_id,
        with_replies,
    )
    .await
    .map_err(|e| db_err("accept insert_following", e))?;
    // arena-move: the same incrementFollowing moved guard as accept's
    // insertFollowingDoc path.
    if !(mk_db::move_account::get_moved_to_uri(pool, user_id)
        .await
        .map_err(|e| db_err("accept moved[follower]", e))?
        .is_some()
        || mk_db::move_account::get_moved_to_uri(pool, me_id)
            .await
            .map_err(|e| db_err("accept moved[followee]", e))?
            .is_some())
    {
        mk_db::social::bump_follow_counts(pool, user_id, me_id, 1)
            .await
            .map_err(|e| db_err("accept bump", e))?;
    }

    // insertFollowingDoc L281-301: the follower is told their request landed
    // ('followRequestAccepted', notifier = the followee) and the followee is
    // notified 'follow' (notifier = the follower) — both muting-gated.
    if had_request {
        notify_follow_family_event(pool, user_id, me_id, "followRequestAccepted").await;
    }
    notify_follow_family_event(pool, me_id, user_id, "follow").await;
    Ok(ok_empty())
}

async fn following_requests_cancel(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let user_id = user_id_param(body)?;
    // Fetch followee (the request target)
    let followee = get_user(pool, user_id).await?.ok_or_else(|| {
        meta_err(
            "NO_SUCH_USER",
            "4e68c551-fc4c-4e46-bb41-7d4a37bf9dab",
            "No such user.",
        )
    })?;

    let exists = mk_db::social::follow_request_exists(pool, me_id, user_id)
        .await
        .map_err(|e| db_err("cancel exists", e))?;
    if !exists {
        return Err(meta_err(
            "FOLLOW_REQUEST_NOT_FOUND",
            "089b125b-d338-482a-9a09-e2622ac9f8d4",
            "Follow request not found.",
        ));
    }

    mk_db::social::delete_follow_request(pool, me_id, user_id)
        .await
        .map_err(|e| db_err("cancel delete", e))?;
    Ok(ok_json(pack_target(state, pool, &followee, me_id).await?))
}

// ---------------------------------------------------------------------------
// acceptAllFollowRequests (UserFollowingService.ts L619, called from
// i/update.ts L556 when a locked account flips isLocked → false)
// ---------------------------------------------------------------------------

/// Re-adjudicate every pending request to `me_id` after an unlock (or any
/// spec flip riding the same i/update): each request is accepted only when
/// shouldRequireFollowApproval against the CURRENT (post-update) profile no
/// longer requires approval for that follower, or `autoAcceptFollowed` says
/// the followee already follows the requester (followedIds set). Requests
/// that still require approval stay pending. Accepted legs mirror
/// acceptFollowRequest: follow edge + counters + followRequestAccepted
/// (follower) / follow (followee) notifications.
pub(crate) async fn accept_all_follow_requests(pool: &mk_db::sqlx::PgPool, me_id: &str) {
    // upstream re-reads the profile AFTER the update persist (cache refresh
    // precedes acceptAllFollowRequests) — the spec flip in the same call is
    // visible here.
    let profile = match get_profile(pool, me_id).await {
        Ok(p) => p,
        Err(_) => return,
    };
    let requesters = match mk_db::social::follow_request_follower_ids(pool, me_id).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(target: "dbg_social", error = %e, "accept_all list");
            return;
        }
    };
    for follower_id in requesters {
        // local-only lane (M2): isRemote is always false — the LOCAL seconds
        // window is the only age gate that can still bind.
        let still_requires = requires_age_approval(&profile, &follower_id);
        if still_requires {
            // followedIds: only when autoAcceptFollowed and me already
            // follows the requester (L640-648).
            let followed = matches!(
                mk_db::social::following_exists(pool, me_id, &follower_id).await,
                Ok(true)
            ) && profile.auto_accept_followed;
            if !followed {
                continue;
            }
        }
        // acceptFollowRequest: the request row may have vanished mid-loop
        // (concurrent cancel) — upstream skips the IdentifiableError.
        let Some(with_replies) =
            mk_db::social::follow_request_with_replies(pool, me_id, &follower_id)
                .await
                .ok()
                .flatten()
        else {
            continue;
        };
        let had_request = match mk_db::social::insert_following(
            pool,
            &mk_types::id::gen_aid_now(),
            &follower_id,
            me_id,
            with_replies,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(target: "dbg_social", error = %e, "accept_all insert");
                continue;
            }
        };
        if let Err(e) = mk_db::social::bump_follow_counts(pool, &follower_id, me_id, 1).await {
            tracing::error!(target: "dbg_social", error = %e, "accept_all bump");
        }
        if had_request {
            notify_follow_family_event(pool, &follower_id, me_id, "followRequestAccepted").await;
        }
        notify_follow_family_event(pool, me_id, &follower_id, "follow").await;
    }
}

// ---------------------------------------------------------------------------
// mute/create + mute/delete
// ---------------------------------------------------------------------------

async fn mute_create(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let user_id = user_id_param(body)?;
    // 自分自身
    if me_id == user_id {
        return Err(meta_err(
            "MUTEE_IS_YOURSELF",
            "a4619cb2-5f23-484b-9301-94c903074e10",
            "Mutee is yourself.",
        ));
    }
    get_user(pool, user_id).await?.ok_or_else(|| {
        meta_err(
            "NO_SUCH_USER",
            "6fef56f3-e765-4957-88e5-c6f65329b8a5",
            "No such user.",
        )
    })?;

    let exists = mk_db::social::muting_exists(pool, me_id, user_id)
        .await
        .map_err(|e| db_err("mute_create exists", e))?;
    if exists {
        return Err(meta_err(
            "ALREADY_MUTING",
            "7e7359cb-160c-4956-b08f-4d1c653cd007",
            "You are already muting that user.",
        ));
    }

    // paramDef: expiresAt integer|null; upstream silently no-ops on a past
    // timestamp (`if (ps.expiresAt && ps.expiresAt <= Date.now()) return;`).
    // JS truthiness: `0` is falsy → not a no-op, and
    // `ps.expiresAt ? new Date(ps.expiresAt) : null` maps 0 → NULL, i.e. an
    // indefinite mute row (upstream mute/create.ts).
    let expires_at_ms = match body.get("expiresAt") {
        None | Some(Value::Null) => None,
        Some(v @ Value::Number(_)) => match v.as_i64() {
            Some(0) => None,
            Some(ms) if ms <= super::now_ms() as i64 => return Ok(ok_empty()),
            Some(ms) => Some(ms),
            None => {
                return Err(super::invalid_param("expiresAt", "expected an integer"));
            }
        },
        Some(_) => {
            return Err(super::invalid_param(
                "expiresAt",
                "expected an integer or null",
            ));
        }
    };

    mk_db::social::insert_muting(
        pool,
        &mk_types::id::gen_aid_now(),
        me_id,
        user_id,
        expires_at_ms,
    )
    .await
    .map_err(|e| db_err("mute_create insert", e))?;
    Ok(ok_empty())
}

async fn mute_delete(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let user_id = user_id_param(body)?;
    if me_id == user_id {
        return Err(meta_err(
            "MUTEE_IS_YOURSELF",
            "f428b029-6b39-4d48-a1d2-cc1ae6dd5cf9",
            "Mutee is yourself.",
        ));
    }
    get_user(pool, user_id).await?.ok_or_else(|| {
        meta_err(
            "NO_SUCH_USER",
            "b851d00b-8ab1-4a56-8b1b-e24187cb48ef",
            "No such user.",
        )
    })?;

    let exists = mk_db::social::muting_exists(pool, me_id, user_id)
        .await
        .map_err(|e| db_err("mute_delete exists", e))?;
    if !exists {
        return Err(meta_err(
            "NOT_MUTING",
            "5467d020-daa9-4553-81e1-135c0c35a96d",
            "You are not muting that user.",
        ));
    }

    mk_db::social::delete_muting(pool, me_id, user_id)
        .await
        .map_err(|e| db_err("mute_delete", e))?;
    Ok(ok_empty())
}

// ---------------------------------------------------------------------------
// blocking/create + blocking/delete
// ---------------------------------------------------------------------------

async fn blocking_create(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let user_id = user_id_param(body)?;
    // 自分自身
    if me_id == user_id {
        return Err(meta_err(
            "BLOCKEE_IS_YOURSELF",
            "88b19138-f28d-42c0-8499-6a31bbd0fdc6",
            "Blockee is yourself.",
        ));
    }
    let blockee = get_user(pool, user_id).await?.ok_or_else(|| {
        meta_err(
            "NO_SUCH_USER",
            "7cc4f851-e2f1-4621-9633-ec9e1d00c01e",
            "No such user.",
        )
    })?;

    let exists = mk_db::social::blocking_exists(pool, me_id, user_id)
        .await
        .map_err(|e| db_err("blocking_create exists", e))?;
    if exists {
        return Err(meta_err(
            "ALREADY_BLOCKING",
            "787fed64-acb9-464a-82eb-afbd745b9614",
            "You are already blocking that user.",
        ));
    }

    // UserBlockingService.block: cancel both pending requests, unfollow both
    // directions (each decrementing counters), insert the row.
    mk_db::social::block(pool, &mk_types::id::gen_aid_now(), me_id, user_id)
        .await
        .map_err(|e| db_err("blocking_create block", e))?;
    Ok(ok_json(pack_target(state, pool, &blockee, me_id).await?))
}

async fn blocking_delete(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let user_id = user_id_param(body)?;
    if me_id == user_id {
        return Err(meta_err(
            "BLOCKEE_IS_YOURSELF",
            "06f6fac6-524b-473c-a354-e97a40ae6eac",
            "Blockee is yourself.",
        ));
    }
    let blockee = get_user(pool, user_id).await?.ok_or_else(|| {
        meta_err(
            "NO_SUCH_USER",
            "8621d8bf-c358-4303-a066-5ea78610eb3f",
            "No such user.",
        )
    })?;

    let exists = mk_db::social::blocking_exists(pool, me_id, user_id)
        .await
        .map_err(|e| db_err("blocking_delete exists", e))?;
    if !exists {
        return Err(meta_err(
            "NOT_BLOCKING",
            "291b2efa-60c6-45c0-9f6a-045c8f9b02cd",
            "You are not blocking that user.",
        ));
    }

    mk_db::social::delete_blocking(pool, me_id, user_id)
        .await
        .map_err(|e| db_err("blocking_delete", e))?;
    Ok(ok_json(pack_target(state, pool, &blockee, me_id).await?))
}

// ---------------------------------------------------------------------------
// renote-mute/create + renote-mute/delete (endpoints/renote-mute/*.ts — the
// users.ts リノートミュート census flips isRenoteMuted through these rows)
// ---------------------------------------------------------------------------

async fn renote_mute_create(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let user_id = user_id_param(body)?;
    if me_id == user_id {
        return Err(meta_err(
            "MUTEE_IS_YOURSELF",
            "37285718-52f7-4aef-b7de-c38b8e8a8420",
            "Mutee is yourself.",
        ));
    }
    get_user(pool, user_id).await?.ok_or_else(|| {
        meta_err(
            "NO_SUCH_USER",
            "5e0a5dff-1e94-4202-87ae-4d9c89eb2271",
            "No such user.",
        )
    })?;

    let exists = mk_db::social::renote_muting_exists(pool, me_id, user_id)
        .await
        .map_err(|e| db_err("renote_mute_create exists", e))?;
    if exists {
        return Err(meta_err(
            "ALREADY_MUTING",
            "ccfecbe4-1f1c-4fc2-8a3d-c3ffee61cb7b",
            "You are already muting that user.",
        ));
    }

    mk_db::social::insert_renote_muting(pool, &mk_types::id::gen_aid_now(), me_id, user_id)
        .await
        .map_err(|e| db_err("renote_mute_create insert", e))?;
    Ok(ok_empty())
}

async fn renote_mute_delete(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let user_id = user_id_param(body)?;
    if me_id == user_id {
        return Err(meta_err(
            "MUTEE_IS_YOURSELF",
            "619b1314-0850-4597-a242-e245f3da42af",
            "Mutee is yourself.",
        ));
    }
    get_user(pool, user_id).await?.ok_or_else(|| {
        meta_err(
            "NO_SUCH_USER",
            "9b6728cf-638c-4aa1-bedb-e07d8101474d",
            "No such user.",
        )
    })?;

    let exists = mk_db::social::renote_muting_exists(pool, me_id, user_id)
        .await
        .map_err(|e| db_err("renote_mute_delete exists", e))?;
    if !exists {
        return Err(meta_err(
            "NOT_MUTING",
            "2e4ef874-8bf0-4b4b-b069-4598f6d05817",
            "You are not muting that user.",
        ));
    }

    mk_db::social::delete_renote_muting(pool, me_id, user_id)
        .await
        .map_err(|e| db_err("renote_mute_delete", e))?;
    Ok(ok_empty())
}

// ---------------------------------------------------------------------------
// dispatch entry
// ---------------------------------------------------------------------------

/// Entry from the `social-agent` dispatch block in mod.rs.
pub(crate) async fn route(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
    credential_declared: bool,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = match require_checked_user(pool, token.as_deref(), credential_declared).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let result = match name {
        "following/create" => following_create(state, pool, body, &me_id).await,
        "following/delete" => following_delete(state, pool, body, &me_id).await,
        "following/requests/accept" => following_requests_accept(pool, body, &me_id).await,
        "following/requests/cancel" => following_requests_cancel(state, pool, body, &me_id).await,
        "mute/create" => mute_create(pool, body, &me_id).await,
        "mute/delete" => mute_delete(pool, body, &me_id).await,
        "blocking/create" => blocking_create(state, pool, body, &me_id).await,
        "blocking/delete" => blocking_delete(state, pool, body, &me_id).await,
        "renote-mute/create" => renote_mute_create(pool, body, &me_id).await,
        "renote-mute/delete" => renote_mute_delete(pool, body, &me_id).await,
        _ => unreachable!("dispatch block checked the name"),
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

// ---------------------------------------------------------------------------
// notes/create renote path (upstream notes/create.ts renoteId param + the
// NoteCreateService renote rules; textless pure-renote row)
// ---------------------------------------------------------------------------

/// Quote detection synced with misc/is-renote.ts `isQuote`.
fn target_is_quote(t: &mk_db::social::RenoteTarget) -> bool {
    t.text.is_some()
        || t.cw.is_some()
        || t.reply_id.is_some()
        || t.has_poll
        || !t.file_ids.is_empty()
}

pub(crate) async fn notes_create_renote(state: &AppState, body: &Value, me_id: &str) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let renote_id = body.get("renoteId").and_then(|v| v.as_str()).unwrap_or("");

    let target = match mk_db::social::find_renote_target(pool, renote_id).await {
        Ok(r) => r,
        Err(e) => return db_err("renote find_renote_target", e),
    };
    let Some(target) = target else {
        return meta_err(
            "NO_SUCH_RENOTE_TARGET",
            "b5c90186-4ab0-49c8-9bba-a1f76c282ba4",
            "No such renote target.",
        );
    };
    if target.renote_id.is_some() && !target_is_quote(&target) {
        return meta_err(
            "CANNOT_RENOTE_TO_A_PURE_RENOTE",
            "fd4cc33e-2a37-48dd-99cc-9b806eb2031a",
            "You can not Renote a pure Renote.",
        );
    }
    // blocking check (blockerId = renote author)
    if target.user_id != me_id {
        let blocked = match mk_db::social::blocking_exists(pool, &target.user_id, me_id).await {
            Ok(b) => b,
            Err(e) => return db_err("renote blocking_exists", e),
        };
        if blocked {
            return meta_err(
                "YOU_HAVE_BEEN_BLOCKED",
                "b390d7e1-8a5e-46ed-b625-06271cafd3d3",
                "You have been blocked by this user.",
            );
        }
    }

    // param visibility (default public; enum-checked like the text path)
    let requested = match body.get("visibility").and_then(|v| v.as_str()) {
        None | Some("public") => "public",
        Some(v @ ("home" | "followers" | "specified")) => v,
        Some(_) => {
            return super::invalid_param(
                "visibility",
                "must be one of public/home/followers/specified",
            );
        }
    };
    // NoteCreateService target-visibility rejects (followers-of-others /
    // specified) — both map to meta.errors.cannotRenoteDueToVisibility.
    if target.visibility == "followers" && target.user_id != me_id
        || target.visibility == "specified"
    {
        return meta_err(
            "CANNOT_RENOTE_DUE_TO_VISIBILITY",
            "be9529e9-fe72-4de0-ae43-0b363c4938af",
            "You can not Renote due to target visibility.",
        );
    }
    // renote visibility switch
    let visibility = match target.visibility.as_str() {
        "home" if requested == "public" => "home",
        "followers" if requested == "public" || requested == "home" => "followers",
        _ => requested,
    };
    // ローカルのみをRenoteしたらローカルのみにする
    let local_only = target.local_only;

    let id = mk_types::id::gen_aid_now();
    // text presence makes it a quote (upstream passes both into one create);
    // textNullable paramDef still enforces 1..3000 when a string is present.
    let text: Option<&str> = match body.get("text").and_then(|v| v.as_str()) {
        Some(t) if !t.is_empty() && t.chars().count() <= 3000 => Some(t),
        Some(_) => return super::invalid_param("text", "1..3000 chars when present"),
        // absent or explicit null → pure renote (paramDef text nullable)
        None => None,
    };
    // stream-final: the poll param rides the renote arm of the ONE upstream
    // create() (notes/create { renoteId, poll } → same MiPoll insert,
    // NoteCreateService.create L710-722; mustBeValidPoll arms L405-407). A
    // poll-only renote is a QUOTE (misc/is-renote.ts isQuotePacked
    // `note.poll != null`), so the withRenotes:false home leg must NOT drop
    // it — the row previously never carried hasPoll, misclassifying every
    // poll renote as a pure renote on the stream fanout. Param rules mirror
    // the text lane's parse (choices 2..10 unique 1..50-char strings;
    // multiple bool; expiresAt | expiredAfter ms, expiredAfter resolved
    // against the request clock).
    let request_ms = super::now_ms() as i64;
    struct PollInput {
        choices: Vec<String>,
        multiple: bool,
        expires_at_ms: Option<i64>,
    }
    let poll: Option<PollInput> = match body.get("poll") {
        None | Some(Value::Null) => None,
        Some(Value::Object(o)) => {
            let Some(Value::Array(choices_raw)) = o.get("choices") else {
                return super::invalid_param("poll.choices", "required");
            };
            if !(2..=10).contains(&choices_raw.len()) {
                return super::invalid_param("poll.choices", "must have 2..10 items");
            }
            let mut choices: Vec<String> = Vec::with_capacity(choices_raw.len());
            for v in choices_raw {
                match v.as_str() {
                    Some(c) if (1..=50).contains(&c.chars().count()) => {
                        if choices.contains(&c.to_string()) {
                            return super::invalid_param("poll.choices", "must be unique");
                        }
                        choices.push(c.to_string());
                    }
                    _ => return super::invalid_param("poll.choices", "must be 1..50 char strings"),
                }
            }
            let multiple = match o.get("multiple") {
                None | Some(Value::Null) => false,
                Some(Value::Bool(b)) => *b,
                Some(_) => return super::invalid_param("poll.multiple", "must be a boolean"),
            };
            let expires_at = match o.get("expiresAt") {
                None | Some(Value::Null) => None,
                Some(Value::Number(n)) => n.as_i64(),
                Some(_) => return super::invalid_param("poll.expiresAt", "must be an integer"),
            };
            let expired_after = match o.get("expiredAfter") {
                None | Some(Value::Null) => None,
                Some(Value::Number(n)) => match n.as_i64() {
                    Some(ms) if ms >= 1 => Some(ms),
                    _ => return super::invalid_param("poll.expiredAfter", "must be >= 1"),
                },
                Some(_) => return super::invalid_param("poll.expiredAfter", "must be an integer"),
            };
            let expires_at_ms = match (expires_at, expired_after) {
                (_, Some(ms)) => Some(request_ms + ms),
                (e, None) => e,
            };
            Some(PollInput {
                choices,
                multiple,
                expires_at_ms,
            })
        }
        Some(_) => return super::invalid_param("poll", "must be an object"),
    };
    // incNotesCount + tag merge (a poll-only renote carries no hashtags —
    // text quotes do, the same extract the text lane feeds the row's tags).
    let hashtags = text
        .map(mk_core::services::misc::extract_hashtags)
        .unwrap_or_default();
    if let Err(e) = mk_db::social::insert_renote_note(
        pool,
        &id,
        me_id,
        &target,
        text,
        visibility,
        local_only,
        &hashtags,
        poll.is_some(),
    )
    .await
    {
        return db_err("renote insert", e);
    }
    // NoteCreateService.create L710-722: the poll row rides the create() —
    // hasPoll rides the insert above; populatePoll reads it back through
    // note.has_poll, so the packed stream fanout carries `poll`.
    if let Some(p) = &poll {
        if let Err(e) = mk_db::poll::insert_poll(
            pool,
            &id,
            &p.choices,
            p.multiple,
            p.expires_at_ms,
            &visibility,
            me_id,
        )
        .await
        {
            return db_err("renote insert_poll", e);
        }
        // queue-min (NoteCreateService.ts L819-829): the renote arm of the
        // ONE upstream create() enqueues the same delayed
        // `endedPollNotification` job for an expiring poll (fire-and-forget —
        // see the notes/create text lane for the parity notes).
        if let Some(exp) = p.expires_at_ms {
            let delay = (exp - super::now_ms() as i64).max(0) as u64;
            let payload = json!({ "type": "endedPollNotification", "noteId": id });
            if let Err(e) = mk_queue::QueueHandle::new(pool.clone())
                .enqueue_delayed(
                    "endedPollNotification",
                    &payload,
                    std::time::Duration::from_millis(delay),
                )
                .await
            {
                tracing::error!(target: "dbg_social", error = %e, "renote endedPollNotification enqueue");
            }
        }
    }
    // utl-misc (users/notes [チャンネル外リノート] チャンネルミュート leg +
    // the channels/timeline renote legs): insertNote's channel denormalization
    // (NoteCreateService.ts L651/L680) — the renote row carries its payload
    // channel in channelId and the parent's channel in renoteChannelId; the
    // channel-mute renote gates and the channels/timeline channelId join read
    // both. The shared renote INSERT is a sibling lane's file, so the fresh
    // row rides the channel lane's post-insert stamp (channel.rs
    // stamp_newest_renote). A payload channelId resolves like the text lane's
    // fetchAndCreate leg: NO_SUCH_CHANNEL (b1653923) when missing/archived.
    let channel_id_in: Option<String> = match body.get("channelId") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if super::is_alnum_id(s) => Some(s.to_string()),
        Some(_) => return super::invalid_param("channelId", "must be an alphanumeric id"),
    };
    if let Some(cid) = &channel_id_in {
        match mk_db::channel::find_channel(pool, cid).await {
            Ok(Some(c)) if !c.is_archived => {}
            Ok(_) => {
                return meta_err(
                    "NO_SUCH_CHANNEL",
                    "b1653923-5453-4edc-b786-7c4f39bb0bbb",
                    "No such channel.",
                );
            }
            Err(e) => return db_err("renote find_channel", e),
        }
    }
    if let Err(e) = mk_db::channel::stamp_newest_renote(
        pool,
        me_id,
        renote_id,
        channel_id_in.as_deref(),
        target.channel_id.as_deref(),
    )
    .await
    {
        return db_err("renote channel stamp", e);
    }
    // incNotesCount + tag merge (hashtags extracted above the insert — the
    // same array rides the row's NOTE_TAGS)
    if let Err(e) = mk_db::note::credit_note_to_user(pool, me_id, &hashtags).await {
        tracing::error!(target: "dbg_social", error = %e, "renote credit_note_to_user");
    }
    // incRenoteCount: skipped for own renote and for bot users
    if target.user_id != me_id {
        match mk_db::social::user_is_bot(pool, me_id).await {
            Ok(false) => {
                if let Err(e) = mk_db::social::bump_renote_count(pool, renote_id).await {
                    tracing::error!(target: "dbg_social", error = %e, "renote bump_renote_count");
                }
            }
            Ok(true) => {}
            Err(e) => tracing::error!(target: "dbg_social", error = %e, "renote user_is_bot"),
        }
        // NoteCreateService non-AP notify branch: renote target author
        // (local) is notified as 'renote' — or 'quote' when the new row
        // carries text. Gates (notifiee == notifier, actor muted by the
        // notifiee) mirror NotificationService.createNotification; thread
        // muting only applies to replies upstream, not renotes.
        notify_note_event(
            pool,
            &target.user_id,
            me_id,
            if text.is_some() { "quote" } else { "renote" },
            &id,
        )
        .await;
        // note-notify: the L790 leg covers renotes too (data.reply == null);
        // a PURE renote (textless → not a quote) drops renote-muted
        // followers (L800-804 renoteMutingsCache). visibility 'specified'
        // renotes sit outside the branch like any other note.
        if visibility != "specified" {
            notify_note_to_followers(pool, me_id, &id, text.is_none()).await;
        }
    }

    // charts3: NoteCreateService.postNoteCreated L754 covers the renote arm
    // of the ONE upstream create() too — diffs.renote moves for every
    // renote/quote row (renoteId != null), the renoter's host picks the
    // local/remote prefix, and this lane cannot carry replyId or files.
    // Best-effort like the text lane.
    let renoter_host = mk_db::note::user_host_of(pool, me_id)
        .await
        .unwrap_or_else(|e| {
            tracing::error!(target: "dbg_social", error = %e, "renote user_host_of[charts]");
            None
        });
    for span in ["hour", "day"] {
        if let Err(e) = mk_db::charts::update_note(
            pool,
            span,
            mk_db::charts::bucket(span, super::now_ms() as i64),
            renoter_host.as_deref(),
            None,
            Some(renote_id),
            false,
            true,
        )
        .await
        {
            tracing::error!(target: "dbg_social", error = %e, "charts update_note[renote]");
        }
    }

    // TODO(M2+): replyId+renoteId quote-combo reply-target validation
    match super::pack_note(state, pool, &id).await {
        Ok(n) => {
            // stream-rtm: NoteCreateService.create L842 — the renote row rides
            // the same `publishNotesStream(noteObj)` fanout (the renote-mute.ts
            // stream assertions key on these `note` events).
            crate::api::streaming::publish_notes(pool, &n).await;
            (StatusCode::OK, Json(json!({ "createdNote": n }))).into_response()
        }
        Err(r) => *r,
    }
}

// ===== notes domain (timeline trio / reactions / delete) ====================

/// `POST /api/notes/timeline` (requireCredential): myself + followees.
pub(crate) async fn notes_timeline(
    state: &crate::api::AppState,
    body: &Value,
    me_id: &str,
) -> Response {
    let Some(pool) = &state.pool else {
        return crate::api::api_err(crate::api::ApiError::internal_error());
    };
    let followees = match mk_db::social::followee_ids(pool, me_id).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "followee_ids");
            return crate::api::api_err(crate::api::ApiError::internal_error());
        }
    };
    run_list(
        state,
        pool,
        mk_db::timelines::timeline_notes(
            pool,
            me_id,
            &followees,
            opt_id(body, "sinceId"),
            opt_id(body, "untilId"),
            opt_i(body, "limit", 10).clamp(1, 100),
            body.get("withFiles")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            body.get("withRenotes")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
        )
        .await,
    )
    .await
}

/// `POST /api/notes/local-timeline` (guest ok): public local notes.
pub(crate) async fn notes_local_timeline(
    state: &crate::api::AppState,
    body: &Value,
    token: Option<&str>,
) -> Response {
    let Some(pool) = &state.pool else {
        return crate::api::api_err(crate::api::ApiError::internal_error());
    };
    let me_id: Option<String> = require_checked_user(pool, token, false).await.ok();
    run_list(
        state,
        pool,
        mk_db::timelines::local_timeline_notes(
            pool,
            me_id.as_deref(),
            opt_id(body, "sinceId"),
            opt_id(body, "untilId"),
            opt_i(body, "limit", 10).clamp(1, 100),
            body.get("withFiles")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            body.get("withReplies")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        )
        .await,
    )
    .await
}

/// `POST /api/notes/global-timeline` (guest ok): public non-channel notes
/// across every host (endpoints/notes/global-timeline.ts). Divergence note:
/// the upstream `gtlAvailable` role-policy gate resolves to the default-allow
/// arm the rest of this workspace pins (api::streaming header note) — the
/// GTL_DISABLED error path is unreachable under the default policies.
pub(crate) async fn notes_global_timeline(
    state: &crate::api::AppState,
    body: &Value,
    token: Option<&str>,
) -> Response {
    let Some(pool) = &state.pool else {
        return crate::api::api_err(crate::api::ApiError::internal_error());
    };
    let me_id: Option<String> = require_checked_user(pool, token, false).await.ok();
    // generateUgcVisibilityQueryForVisitor is a guests-only gate; the meta
    // read mirrors fetch_resource::meta_ugc (missing row → the TypeORM column
    // default 'local').
    let guest_ugc: Option<String> = if me_id.is_none() {
        match mk_db::sqlx::query_scalar::<_, Option<String>>(
            r#"SELECT "ugcVisibilityForVisitor" FROM "meta" ORDER BY "id" DESC LIMIT 1"#,
        )
        .fetch_optional(pool)
        .await
        {
            Ok(v) => Some(v.flatten().unwrap_or_else(|| "local".to_string())),
            Err(e) => {
                tracing::error!(target: "dbg_notes", error = %e, "global-timeline meta ugc");
                return crate::api::api_err(crate::api::ApiError::internal_error());
            }
        }
    } else {
        None
    };
    // makePaginationQuery: id params win; the date params fold into generated
    // aid boundary ids (QueryService `idService.gen(sinceDate/untilDate)`).
    let since_id: Option<String> = opt_id(body, "sinceId").map(str::to_string).or_else(|| {
        body.get("sinceDate")
            .and_then(|v| v.as_i64())
            .map(mk_types::id::gen_aid)
    });
    let until_id: Option<String> = opt_id(body, "untilId").map(str::to_string).or_else(|| {
        body.get("untilDate")
            .and_then(|v| v.as_i64())
            .map(mk_types::id::gen_aid)
    });
    run_list(
        state,
        pool,
        mk_db::timelines::global_timeline_notes(
            pool,
            me_id.as_deref(),
            guest_ugc.as_deref(),
            since_id.as_deref(),
            until_id.as_deref(),
            opt_i(body, "limit", 10).clamp(1, 100),
            body.get("withFiles")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            body.get("withRenotes")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
        )
        .await,
    )
    .await
}

/// `POST /api/notes/hybrid-timeline` (requireCredential): social timeline.
pub(crate) async fn notes_hybrid_timeline(
    state: &crate::api::AppState,
    body: &Value,
    me_id: &str,
) -> Response {
    let Some(pool) = &state.pool else {
        return crate::api::api_err(crate::api::ApiError::internal_error());
    };
    let followees = match mk_db::social::followee_ids(pool, me_id).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "followee_ids");
            return crate::api::api_err(crate::api::ApiError::internal_error());
        }
    };
    run_list(
        state,
        pool,
        mk_db::timelines::hybrid_timeline_notes(
            pool,
            me_id,
            &followees,
            opt_id(body, "sinceId"),
            opt_id(body, "untilId"),
            opt_i(body, "limit", 10).clamp(1, 100),
            body.get("withFiles")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            // upstream paramDef default: false (hybrid-timeline.ts L70) — the
            // social-TL reply gate is ON unless the caller opens it
            body.get("withReplies")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        )
        .await,
    )
    .await
}

/// Shared list plumbing: fetch rows then pack each through the M1 pipeline.
async fn run_list(
    state: &crate::api::AppState,
    pool: &mk_db::sqlx::PgPool,
    rows: Result<Vec<mk_db::note::NoteRow>, mk_db::sqlx::Error>,
) -> Response {
    let rows = match rows {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "timeline query");
            return crate::api::api_err(crate::api::ApiError::internal_error());
        }
    };
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        match pack_note_row_social(state, pool, row).await {
            Ok(v) => out.push(v),
            Err(resp) => return *resp,
        }
    }
    (axum::http::StatusCode::OK, axum::Json(json!(out))).into_response()
}

async fn pack_note_row_social(
    state: &crate::api::AppState,
    pool: &mk_db::sqlx::PgPool,
    row: &mk_db::note::NoteRow,
) -> Result<Value, Box<Response>> {
    let user_lite = super::note_user_lite(state, pool, &row.user_id)
        .await
        .map_err(Box::new)?;
    let created_at = mk_types::id::parse_aid_date(&row.id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default();
    let emojis = json!({});
    Ok(mk_core::packers::note::note_pack_min(
        row,
        user_lite,
        created_at,
        &row.mentions,
        &row.tags,
        emojis,
    ))
}

fn opt_id<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(|v| v.as_str())
}

fn opt_i(body: &Value, key: &str, dflt: i64) -> i64 {
    body.get(key).and_then(|v| v.as_i64()).unwrap_or(dflt)
}

/// notes/reactions.ts meta.errors.noSuchNote (id differs from show.ts's
/// 24fcbfc6 — L37 in the upstream file).
fn reaction_no_such_note() -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "NO_SUCH_NOTE",
        "263fff3d-d0e1-4af4-bea7-8408059b451a",
        "No such note.",
    ))
}

/// `POST /api/notes/reactions` — reaction list for one note. Guest-ok, but a
/// riding credential still authenticates (ApiCallServerService).
pub(crate) async fn notes_reactions(
    state: &crate::api::AppState,
    body: &Value,
    token: Option<&str>,
) -> Response {
    let Some(pool) = &state.pool else {
        return crate::api::api_err(crate::api::ApiError::internal_error());
    };
    // Optional-credential: present-but-unresolvable token → AUTHENTICATION_FAILED.
    let me_id: Option<String> = match token {
        None => None,
        Some(t) => {
            if t.len() != 16 {
                return crate::api::authentication_failed();
            }
            match super::resolve_me(pool, Some(t)).await {
                Some(id) => Some(id),
                None => return crate::api::authentication_failed(),
            }
        }
    };
    let Some(note_id) = opt_id(body, "noteId") else {
        return crate::api::api_err(crate::api::ApiError::invalid_param(
            json!({ "param": "noteId", "reason": "required" }),
        ));
    };
    // arena-apivis: notes/reactions.ts gate (L68-75) — getNote (missing row →
    // 9725d0ce remap) → NO_SUCH_NOTE(263fff3d), then
    // NoteEntityService.isVisibleForMe (invisible → NO_SUCH_NOTE). Guest lanes
    // only reach public/home (isVisibleForMe returns false for
    // followers/specified on meId == null).
    let note = match mk_db::note::find_note_by_id(pool, note_id).await {
        Ok(Some(n)) => n,
        Ok(None) => return reaction_no_such_note(),
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "reactions find_note_by_id");
            return crate::api::api_err(crate::api::ApiError::internal_error());
        }
    };
    let visible = match &me_id {
        Some(me) => crate::api::note_is_visible_for_me(pool, &note, me).await,
        None => Ok(matches!(note.visibility.as_str(), "public" | "home")),
    };
    match visible {
        Ok(true) => {}
        Ok(false) => return reaction_no_such_note(),
        Err(r) => return r,
    }
    let rows = mk_db::note::list_note_reactions(
        pool,
        note_id,
        body.get("type").and_then(|v| v.as_str()),
        opt_id(body, "sinceId"),
        opt_id(body, "untilId"),
        opt_i(body, "limit", 10).clamp(1, 100),
    )
    .await;
    match rows {
        Ok(r) => {
            let out: Vec<Value> = r
                .iter()
                .map(|n| {
                    json!({
                        "id": n.id,
                        "type": n.reaction,
                        "createdAt": mk_types::id::parse_aid_date(&n.id)
                            .map(mk_core::services::signup::ms_to_iso)
                            .unwrap_or_default(),
                        "noteId": note_id,
                        "userId": n.user_id,
                    })
                })
                .collect();
            (axum::http::StatusCode::OK, axum::Json(json!(out))).into_response()
        }
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "list_note_reactions");
            crate::api::api_err(crate::api::ApiError::internal_error())
        }
    }
}

/// ReactionService normalization (FALLBACK '\u2764', L34) — the Rust port of
/// `ReactionService.normalize` L385-402 in upstream order: legacy names →
/// mapped emoji, emojiRegex first-char (ZWJ sequences kept whole, otherwise
/// \ufe0f stripped), everything else (empty string, ASCII text, unknown
/// custom emoji) → FALLBACK. Custom ':name:' L130-163 hits the local emoji
/// registry, empty in this server → FALLBACK.
pub(crate) fn normalize_reaction(reaction: &str) -> String {
    const FALLBACK: &str = "\u{2764}"; // no VS16 — the storage/canonical form
    const LEGACIES: &[(&str, &str)] = &[
        ("like", "👍"),
        ("love", "\u{2764}"),
        ("laugh", "😆"),
        ("hmm", "🤔"),
        ("surprise", "😮"),
        ("congrats", "🎉"),
        ("angry", "💢"),
        ("confused", "😥"),
        ("rip", "😇"),
        ("pudding", "🍮"),
        ("star", "⭐"),
    ];
    if let Some((_, modern)) = LEGACIES.iter().find(|(name, _)| *name == reaction) {
        return (*modern).to_string();
    }
    match first_emoji_span(reaction) {
        // 合字を含む1つの絵文字: ZWJ sequences keep visibility selectors,
        // standalone chars strip \ufe0f (upstream L397-398).
        Some(span) => {
            let matched = &reaction[span.start..span.end];
            if matched.contains('\u{200d}') {
                matched.to_string()
            } else {
                matched.replace("\u{fe0f}", "")
            }
        }
        None => FALLBACK.to_string(),
    }
}

/// Find the byte span of the leading emoji sequence — an approximation of the
/// emoji-regex scan used by ReactionService.normalize (first emoji char + its
/// trailing modifiers: VS16, skin tones, keycap, tags, and ZWJ continuations).
fn first_emoji_span(s: &str) -> Option<std::ops::Range<usize>> {
    // emoji-start property (rounded from Unicode 15 emoji-data; supersets the
    // e2e's 🚀/🥰/❤ class, excludes ASCII/CJK/kana)
    fn emoji_start(c: char) -> bool {
        matches!(c as u32,
            0x00A9 | 0x00AE | 0x203C | 0x2049 | 0x2122 | 0x2139
            | 0x2194..=0x2199 | 0x21A9..=0x21AA
            | 0x231A..=0x231B | 0x2328 | 0x23CF | 0x23E9..=0x23FA
            | 0x24C2 | 0x25AA..=0x25AB | 0x25B6 | 0x25C0 | 0x25FB..=0x25FE
            | 0x2600..=0x27BF | 0x2934..=0x2935
            | 0x2B05..=0x2B07 | 0x2B1B..=0x2B1C | 0x2B50 | 0x2B55
            | 0x3030 | 0x303D | 0x3297 | 0x3299
            | 0x1F004 | 0x1F0CF | 0x1F170..=0x1F251
            | 0x1F1E6..=0x1F1FF
            | 0x1F300..=0x1FAFF
            | 0x1FC00..=0x1FFFD
        )
    }
    // continuation chars that belong to the sequence once started
    fn emoji_continue(c: char) -> bool {
        matches!(c as u32,
            0xFE0F | 0x20E3 | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F
        )
    }
    let start = s
        .char_indices()
        .find(|(_, c)| emoji_start(*c))
        .map(|(i, _)| i)?;
    // first emoji char begins the match; swallow continuation chars and
    // ZWJ-joined emoji chars after it
    let mut end = start + s[start..].chars().next().unwrap().len_utf8();
    let mut zwj_seen = false;
    for (i, c) in s.char_indices() {
        if i < end {
            continue;
        }
        if c == '\u{200d}' {
            zwj_seen = true;
            end = i + c.len_utf8();
        } else if emoji_continue(c) || (zwj_seen && emoji_start(c)) {
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    Some(start..end)
}

/// `POST /api/notes/reactions/create` — toggle-my-reaction semantics
/// (NoteReactionService: existing mine → delete, else create; jsonb map bump).
pub(crate) async fn notes_reactions_create(
    state: &crate::api::AppState,
    body: &Value,
    me_id: &str,
) -> Response {
    let Some(pool) = &state.pool else {
        return crate::api::api_err(crate::api::ApiError::internal_error());
    };
    let Some(note_id) = opt_id(body, "noteId") else {
        return crate::api::api_err(crate::api::ApiError::invalid_param(
            json!({ "param": "noteId", "reason": "required" }),
        ));
    };
    let Some(reaction) = opt_id(body, "reaction") else {
        return crate::api::api_err(crate::api::ApiError::invalid_param(
            json!({ "param": "reaction", "reason": "required" }),
        ));
    };
    // ReactionService.create L125-166 normalization: legacy names → modern
    // emoji, unicode emoji → first emoji char (\ufe0f stripped unless the
    // sequence contains \u200d), custom ':name:' → local emoji lookup (the
    // emoji registry is empty in this server → FALLBACK), everything else →
    // FALLBACK '\u2764' (endpoints.ts ❤フォールバック asserts).
    let reaction = normalize_reaction(reaction);
    // note author for the blocking gate below (reused by the notification leg)
    let note_row = match mk_db::note::find_note_by_id(pool, note_id).await {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "reactions_create find_note_by_id");
            return crate::api::api_err(crate::api::ApiError::internal_error());
        }
    };
    // ReactionService.create L107-113: reacting to a note whose author blocked
    // me throws IdentifiableError e70412a4; the reactions/create endpoint
    // catch-arm (reactions/create.ts L73) remaps it to
    // meta.errors.youHaveBeenBlocked (L34-38). The gate precedes the toggle —
    // an author-side block also rejects removing an older reaction.
    if let Some(note) = &note_row {
        if note.user_id != me_id {
            match mk_db::social::blocking_exists(pool, &note.user_id, me_id).await {
                Ok(true) => {
                    return meta_err(
                        "YOU_HAVE_BEEN_BLOCKED",
                        "20ef5475-9f38-4e4c-bd33-de6d979498ec",
                        "You cannot react this note because you have been blocked by this user.",
                    );
                }
                Ok(false) => {}
                Err(e) => {
                    tracing::error!(target: "dbg_notes", error = %e, "reactions_create blocking_exists");
                    return crate::api::api_err(crate::api::ApiError::internal_error());
                }
            }
        }
    }
    // GetterService.getNote → NO_SUCH_NOTE (reactions/create.ts L71-74):
    // unknown/malformed ids 400 (endpoints.ts 存在しない投稿/間違ったID legs).
    if note_row.is_none() {
        return meta_err(
            "NO_SUCH_NOTE",
            "033d0620-5bfe-4027-965d-980b0c85a3ea",
            "No such note.",
        );
    }
    // ReactionService.create L120-123: reactions key the note itself — a pure
    // renote (renoteId set, no text/cw/reply/poll/files; misc/is-renote.ts
    // isRenote && !isQuote) throws IdentifiableError 12c35529, remapped by
    // reactions/create.ts meta.errors.cannotReactToRenote (endpoints.ts
    // CANNOT_REACT_TO_RENOTE assert; quotes stay reactable).
    if let Some(note) = &note_row {
        if note.renote_id.is_some()
            && note.text.is_none()
            && note.cw.is_none()
            && note.reply_id.is_none()
            && !note.has_poll
            && note.file_ids.is_empty()
        {
            return meta_err(
                "CANNOT_REACT_TO_RENOTE",
                "eaccdc08-ddef-43fe-908f-d108faad57f5",
                "You cannot react to Renote.",
            );
        }
    }
    match mk_db::note::find_note_reaction(pool, note_id, me_id).await {
        Ok(Some(mine)) => {
            if let Err(e) = mk_db::note::delete_note_reaction(pool, note_id, me_id).await {
                tracing::error!(target: "dbg_notes", error = %e, "delete_note_reaction");
                return crate::api::api_err(crate::api::ApiError::internal_error());
            }
            // 二重リアクション上書き: the jsonb map must lose the STORED
            // reaction (ReactionService.delete → the old emoji's count),
            // never the replacing one's (endpoints.ts deepStrictEqual
            // {'🚀': 1} after 🥰 → 🚀 overwrite).
            if let Err(e) = mk_db::note::bump_reactions_map(pool, note_id, &mine.reaction, -1).await
            {
                tracing::error!(target: "dbg_notes", error = %e, "bump_reactions_map");
            }
            // isDuplicateKeyValueError arm (ReactionService.create L176-196):
            // the new reaction is INSERTED after the delete — the duplicate
            // path REPLACES, it is not a bare toggle-off (the map must show
            // the new emoji).
            if let Err(e) = mk_db::note::insert_note_reaction(
                pool,
                &mk_types::id::gen_aid_now(),
                me_id,
                note_id,
                &reaction,
            )
            .await
            {
                tracing::error!(target: "dbg_notes", error = %e, "replace insert_note_reaction");
                return crate::api::api_err(crate::api::ApiError::internal_error());
            }
            if let Err(e) = mk_db::note::bump_reactions_map(pool, note_id, &reaction, 1).await {
                tracing::error!(target: "dbg_notes", error = %e, "replace bump_reactions_map");
            }
            if let Some(note) = &note_row {
                notify_note_event(pool, &note.user_id, me_id, "reaction", note_id).await;
            }
        }
        Ok(None) => {
            if let Err(e) = mk_db::note::insert_note_reaction(
                pool,
                &mk_types::id::gen_aid_now(),
                me_id,
                note_id,
                &reaction,
            )
            .await
            {
                tracing::error!(target: "dbg_notes", error = %e, "insert_note_reaction");
                return crate::api::api_err(crate::api::ApiError::internal_error());
            }
            if let Err(e) = mk_db::note::bump_reactions_map(pool, note_id, &reaction, 1).await {
                tracing::error!(target: "dbg_notes", error = %e, "bump_reactions_map");
            }
            // reaction notification (notifiee = note author, local)
            if let Some(note) = &note_row {
                notify_note_event(pool, &note.user_id, me_id, "reaction", note_id).await;
            }
        }
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "find_note_reaction");
            return crate::api::api_err(crate::api::ApiError::internal_error());
        }
    }
    axum::http::StatusCode::NO_CONTENT.into_response()
}

/// `POST /api/notes/delete` — author-only delete (upstream 403 on others).
pub(crate) async fn notes_delete(
    state: &crate::api::AppState,
    body: &Value,
    me_id: &str,
) -> Response {
    let Some(pool) = &state.pool else {
        return crate::api::api_err(crate::api::ApiError::internal_error());
    };
    let Some(note_id) = opt_id(body, "noteId") else {
        return crate::api::api_err(crate::api::ApiError::invalid_param(
            json!({ "param": "noteId", "reason": "required" }),
        ));
    };
    // meili-idx: SearchService.unindexNote's visibility gate
    // (`!['home','public'].includes(note.visibility)` → skip) reads the row
    // BEFORE the DELETE takes it, and charts3 needs the same row for the
    // NoteDeleteService.ts L94 `notesChart.update(note, false)` commit
    // (userHost prefix + reply/renote/file diffs) — fetched unconditionally
    // now; upstream's endpoint fetched the note for the service call anyway.
    // A read failure stays swallowed (best-effort lane), the chart commit
    // just skips.
    let row = mk_db::note::find_note_by_id(pool, note_id)
        .await
        .ok()
        .flatten();
    let unindex_visibility = if state.search_engine.is_noop() {
        None
    } else {
        row.as_ref().map(|n| n.visibility.clone())
    };
    match mk_db::note::delete_note(pool, note_id, me_id).await {
        Ok(1) => {
            if let Err(e) = mk_db::note::dec_note_count(pool, me_id).await {
                tracing::error!(target: "dbg_notes", error = %e, "dec_note_count");
            }
            // meili-idx: NoteDeleteService.ts L111 searchService.unindexNote —
            // public/home rows leave the index with the row (best-effort: a
            // failed remove logs, it must not fail the already-committed
            // delete).
            if matches!(unindex_visibility.as_deref(), Some("public" | "home")) {
                if let Err(e) = state.search_engine.remove(note_id).await {
                    tracing::error!(target: "dbg_notes", error = %e, "search engine unindex");
                }
            }
            // charts3: NoteDeleteService.ts L94 `notesChart.update(note,
            // false)` — the hour+day decrement pair read off the pre-delete
            // row. Best-effort: a chart failure logs, never fails the
            // already-committed delete.
            if let Some(n) = &row {
                for span in ["hour", "day"] {
                    if let Err(e) = mk_db::charts::update_note(
                        pool,
                        span,
                        mk_db::charts::bucket(span, super::now_ms() as i64),
                        n.user_host.as_deref(),
                        n.reply_id.as_deref(),
                        n.renote_id.as_deref(),
                        !n.file_ids.is_empty(),
                        false,
                    )
                    .await
                    {
                        tracing::error!(target: "dbg_notes", error = %e, "charts update_note[delete]");
                    }
                }
            }
            axum::http::StatusCode::NO_CONTENT.into_response()
        }
        Ok(_) => crate::api::api_err(crate::api::ApiError::new(
            400,
            mk_types::api_error::ApiErrorKind::Client,
            "NO_SUCH_NOTE",
            "24fcbfc6-2e37-42b6-8388-c29b3861a08d",
            "No such note.",
        )),
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "delete_note");
            crate::api::api_err(crate::api::ApiError::internal_error())
        }
    }
}

/// token → me (requireCredential chain) for the note-agent timeline pair.
pub(crate) async fn timeline_or_hybrid(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<&str>,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = match require_checked_user(pool, token, true).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if name == "notes/timeline" {
        notes_timeline(state, body, &me_id).await
    } else {
        notes_hybrid_timeline(state, body, &me_id).await
    }
}

/// Credential gate + explicit dispatch for the two (me, body) note endpoints
/// `POST /api/following/update` — notify / withReplies flag flip on an
/// existing follow edge (upstream following/update.ts; the timelines matrix
/// legs flip `following.withReplies` through this, so it rides the timeline
/// dispatch block until the social lane absorbs the broader following
/// family). Returns the FOLLOWER packed (upstream `pack(follower.id, me)`).
pub(crate) async fn following_update(
    state: &AppState,
    body: &Value,
    token: Option<&str>,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = match require_checked_user(pool, token, true).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let user_id = match user_id_param(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if me_id == user_id {
        return meta_err(
            "FOLLOWEE_IS_YOURSELF",
            "4c4cbaf9-962a-463b-8418-a5e365dbf2eb",
            "Followee is yourself.",
        );
    }
    // upstream GetterService.getUser → NO_SUCH_USER before the edge probe
    match get_user(pool, user_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return meta_err(
                "NO_SUCH_USER",
                "14318698-f67e-492a-99da-5353a5ac52be",
                "No such user.",
            );
        }
        Err(resp) => return resp,
    }
    let exists = match mk_db::social::following_exists(pool, &me_id, user_id).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(target: "dbg_social", error = %e, "following_update following_exists");
            return api_err(ApiError::internal_error());
        }
    };
    if !exists {
        return meta_err(
            "NOT_FOLLOWING",
            "b8dc75cf-1cb5-46c9-b14b-5f1ffbd782c9",
            "You are not following that user.",
        );
    }
    // ps.notify: 'none' → NULL, 'normal' → 'normal' (absent = untouched)
    let notify = match body.get("notify") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s == "normal" => Some(Some("normal".to_string())),
        Some(Value::String(s)) if s == "none" => Some(None),
        Some(_) => {
            return super::invalid_param("notify", "must be one of normal/none");
        }
    };
    let with_replies = match body.get("withReplies") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => {
            return super::invalid_param("withReplies", "expected a boolean");
        }
    };
    if let Err(e) =
        mk_db::social::update_following_flags(pool, &me_id, user_id, notify, with_replies).await
    {
        tracing::error!(target: "dbg_social", error = %e, "update_following_flags");
        return api_err(ApiError::internal_error());
    }
    let me = match get_user(pool, &me_id).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            return meta_err(
                "NO_SUCH_USER",
                "14318698-f67e-492a-99da-5353a5ac52be",
                "No such user.",
            );
        }
        Err(resp) => return resp,
    };
    match pack_target(state, pool, &me, &me_id).await {
        Ok(v) => ok_json(v),
        Err(resp) => resp,
    }
}

/// (closure-based generic gate trips HRTB lifetimes — keep this concrete).
pub(crate) async fn note_credential_dispatch(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<&str>,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = match require_checked_user(pool, token, true).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if name == "notes/reactions/create" {
        notes_reactions_create(state, body, &me_id).await
    } else {
        notes_delete(state, body, &me_id).await
    }
}

/// `POST /api/following/list` (endpoints/following/list.ts, requireCredential
/// read:following — the note-notify arena's read side of the
/// follow-edge notify flag). `followerId = me` (L54), the `notification:
/// true` flag adds `following.notify IS NOT NULL` (L56-58), then
/// QueryService pagination (exclusive aid bounds / sinceId-ASC, limit
/// 1..100 default 10) and FollowingEntityService.packMany(,
/// { populateFollowee: true }) — { id, createdAt, followeeId, followerId,
/// followee }.
pub(crate) async fn following_list(
    state: &AppState,
    body: &Value,
    token: Option<&str>,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = match require_checked_user(pool, token, true).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    // paramDef: notification boolean default false; sinceId/untilId/sinceDate/
    // untilDate cursors; limit 1..100 default 10.
    let notification = match body.get("notification") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return super::invalid_param("notification", "expected a boolean"),
    };
    let id_param = |key: &str| -> Result<Option<String>, Response> {
        match body.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if super::is_alnum_id(s) => Ok(Some(s.clone())),
            Some(_) => Err(super::invalid_param(key, "expected an alphanumeric id")),
        }
    };
    let since_id = id_param("sinceId");
    let until_id = id_param("untilId");
    let date_param = |key: &str| -> Result<Option<i64>, Response> {
        match body.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => match v.as_i64() {
                Some(ms) => Ok(Some(ms)),
                None => Err(super::invalid_param(key, "expected an integer (ms)")),
            },
        }
    };
    let since_date = date_param("sinceDate");
    let until_date = date_param("untilDate");
    let limit = match body.get("limit") {
        None | Some(Value::Null) => Ok(10u32),
        Some(v) => match v.as_u64() {
            Some(n) if (1..=100).contains(&n) => Ok(n as u32),
            Some(_) => Err(super::invalid_param("limit", "must be >= 1 and <= 100")),
            None => Err(super::invalid_param("limit", "expected an integer")),
        },
    };
    let (since_id, until_id, since_date, until_date, limit) =
        match (since_id, until_id, since_date, until_date, limit) {
            (Ok(a), Ok(b), Ok(c), Ok(d), Ok(e)) => (a, b, c, d, e),
            (Err(r), _, _, _, _)
            | (_, Err(r), _, _, _)
            | (_, _, Err(r), _, _)
            | (_, _, _, Err(r), _)
            | (_, _, _, _, Err(r)) => return r,
        };
    // makePaginationQuery (same converter ff-arena lists run)
    let (from_id, until_bound, desc) = super::ff_vis::paginate_bounds(
        since_id.as_deref(),
        until_id.as_deref(),
        since_date,
        until_date,
    );
    let edges = match mk_db::social::list_following_edges(
        pool,
        &me_id,
        notification,
        from_id.as_deref(),
        until_bound.as_deref(),
        desc,
        limit,
    )
    .await
    {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(target: "dbg_social", error = %e, "list_following_edges");
            return api_err(ApiError::internal_error());
        }
    };
    let mut out = Vec::with_capacity(edges.len());
    for edge in edges {
        let mut item = json!({
            "id": edge.id,
            "createdAt": mk_core::services::signup::ms_to_iso(
                mk_types::id::parse_aid_date(&edge.id).unwrap_or_default(),
            ),
            "followeeId": edge.followee_id,
            "followerId": edge.follower_id,
        });
        match mk_db::user::find_user_by_id(pool, &edge.followee_id).await {
            Ok(Some(row)) => {
                item["followee"] =
                    match super::ff_vis::pack_edge_user(state, pool, row, Some(&me_id)).await {
                        Ok(v) => v,
                        // pack errors surface as their own api response
                        Err(resp) => return resp,
                    };
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!(target: "dbg_social", error = %e, "following_list followee row");
                return api_err(ApiError::internal_error());
            }
        }
        out.push(item);
    }
    ok_json(Value::Array(out))
}

// unit-misc: upstream test/unit/misc/should-require-follow-approval.ts on the
// local arm. The upstream shape (profile, isRemote, createdAt, now) maps onto
// requires_age_approval(profile, follower_id), which derives created/now from
// the follower's AID and the wall clock, so the exact-'+1ms' boundary probes
// become 5-second margins (drift between the two clock reads is milliseconds).
// The remote arm (isRemote=true → followApprovalRemoteSeconds) is
// upstream-only: the M2 social lane is local-only and remote followers never
// reach this gate (documented at the call sites).
#[cfg(test)]
mod unit_misc_should_require_follow_approval {
    use super::*;

    const DAY_MS: i64 = 86_400_000;
    const HOUR_MS: i64 = 3_600_000;

    fn profile_with(local_seconds: Option<i32>) -> Profile {
        Profile {
            user_id: String::new(),
            location: None,
            birthday: None,
            description: None,
            lang: None,
            fields: serde_json::Value::Null,
            url: None,
            email: None,
            email_verified: false,
            two_factor_enabled: false,
            password: None,
            auto_accept_followed: false,
            always_mark_nsfw: false,
            careful_bot: false,
            security_keys_available: false,
            use_password_less_login: false,
            inject_featured_note: false,
            receive_announcement_email: false,
            email_notification_types: serde_json::Value::Null,
            public_reactions: false,
            auto_sensitive: false,
            achievements: serde_json::Value::Null,
            logged_in_dates: Vec::new(),
            prevent_ai_learning: false,
            no_crawle: false,
            verified_links: Vec::new(),
            notification_recieve_config: serde_json::Value::Null,
            hard_muted_words: serde_json::Value::Null,
            muted_words: serde_json::Value::Null,
            muted_instances: serde_json::Value::Null,
            following_visibility: String::new(),
            followers_visibility: String::new(),
            followed_message: None,
            follow_approval_local_seconds: local_seconds,
            follow_approval_remote_seconds: None,
            two_factor_backup_secret: None,
        }
    }

    fn follower_created_at(age_ms: i64) -> String {
        mk_types::id::gen_aid(super::super::now_ms() as i64 - age_ms)
    }

    #[test]
    fn local_window_binds_then_ages_out() {
        // upstream case 1, local half: a 10-day-old account against a 7-day
        // window no longer requires approval.
        let settings = profile_with(Some(7 * 86_400));
        assert!(!requires_age_approval(
            &settings,
            &follower_created_at(10 * DAY_MS)
        ));
    }

    #[test]
    fn window_boundary_ages_out_at_full_span() {
        // upstream test.each (local: 7 days): just within the span still
        // requires; just past it does not. Upstream's `now - days*day`
        // equality maps to the past side because `<` is strict.
        let settings = profile_with(Some(7 * 86_400));
        assert!(requires_age_approval(
            &settings,
            &follower_created_at(7 * DAY_MS - 5_000)
        ));
        assert!(!requires_age_approval(
            &settings,
            &follower_created_at(7 * DAY_MS + 5_000)
        ));
    }

    #[test]
    fn half_day_window() {
        // upstream: second-granular settings (43200 = half a day) bind too.
        let half_day = profile_with(Some(43_200));
        assert!(requires_age_approval(
            &half_day,
            &follower_created_at(12 * HOUR_MS - 5_000)
        ));
        assert!(!requires_age_approval(
            &half_day,
            &follower_created_at(12 * HOUR_MS + 5_000)
        ));
    }

    #[test]
    fn disabled_settings_never_require() {
        // upstream: null and 0 disable the gate entirely (a just-created
        // account needs no approval).
        assert!(!requires_age_approval(
            &profile_with(None),
            &follower_created_at(0)
        ));
        assert!(!requires_age_approval(
            &profile_with(Some(0)),
            &follower_created_at(0)
        ));
    }

    #[test]
    fn unparseable_follower_id_never_requires() {
        // port detail (upstream-only): with no AID timestamp the gate cannot
        // bind, so the request stays on the manual path.
        assert!(!requires_age_approval(
            &profile_with(Some(86_400)),
            "notanaid"
        ));
    }
}

#[cfg(test)]
mod unit_services_reaction_normalize {
    use super::normalize_reaction as normalize;

    // unit-services — 1:1 port of upstream test/unit/ReactionService.ts
    // describe('normalize'). The end-to-end create-side asserts (block gate,
    // renote gate, toggle semantics) live in api/tests.rs + the reactions
    // e2e; upstream's `create` spec that needs an emoji REGISTRY (the
    // local-only-emoji fallback) degenerates here: this server wires no emoji
    // table, so every custom ':name[:@host:]' token already folds to the
    // FALLBACK heart — pinned by custom_emoji_token_falls_back below.

    #[test]
    fn emoji_reaction_kept_as_is() {
        // upstream '絵文字リアクションはそのまま'
        assert_eq!(normalize("👍"), "👍");
        assert_eq!(normalize("🍅"), "🍅");
    }

    #[test]
    fn legacy_short_forms_emoji_fied() {
        // upstream '既存のリアクションは絵文字化する' pudding / like / love /
        // laugh / hmm / surprise / congrats / angry / confused / rip / star
        assert_eq!(normalize("pudding"), "🍮");
        assert_eq!(normalize("like"), "👍");
        // upstream keeps U+2764 WITHOUT the variation selector
        assert_eq!(normalize("love"), "\u{2764}");
        assert_eq!(normalize("laugh"), "😆");
        assert_eq!(normalize("hmm"), "🤔");
        assert_eq!(normalize("surprise"), "😮");
        assert_eq!(normalize("congrats"), "🎉");
        assert_eq!(normalize("angry"), "💢");
        assert_eq!(normalize("confused"), "😥");
        assert_eq!(normalize("rip"), "😇");
        assert_eq!(normalize("star"), "⭐");
    }

    #[test]
    fn variation_selector_stripped() {
        // upstream '異体字セレクタ除去'
        assert_eq!(normalize("㊗️"), "㊗");
        // upstream '異体字セレクタ除去 必要なし'
        assert_eq!(normalize("㊗"), "㊗");
    }

    #[test]
    fn fallback_heart_for_unknown() {
        // upstream 'fallback - empty' (upstream `fallback - null` is
        // unreachable in the Rust signature: the endpoint's reaction param is
        // required, and the omitted-_-reaction create() default rides the
        // same FALLBACK constant)
        assert_eq!(normalize(""), "\u{2764}");
        // upstream 'fallback - unknown'
        assert_eq!(normalize("unknown"), "\u{2764}");
    }

    #[test]
    fn custom_emoji_token_falls_back() {
        // upstream 'create ローカル限定カスタム絵文字はリモートノートへの
        // リアクションではハートにフォールバックする' (degenerate slice — see
        // the module note; upstream gates this inside create() when the
        // fetched emoji is localOnly and the note author is remote)
        assert_eq!(normalize(":localonly:"), "\u{2764}");
        assert_eq!(normalize(":pudding@remote.example.tld:"), "\u{2764}");
    }

    #[test]
    fn zwj_sequences_keep_visibility_selectors() {
        // upstream normalize: a matched sequence containing U+200D keeps its
        // U+FE0F parts (合字を含む1つの絵文字) while standalone chars strip.
        assert_eq!(normalize("❤️‍🔥"), "❤️‍🔥");
        assert_eq!(normalize("🙂"), "🙂");
    }
}
