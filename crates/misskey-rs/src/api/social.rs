//! Social actions (M2 truthful slice): following/create+delete,
//! following/requests/accept+cancel, mute/create+delete, blocking/create+
//! delete, and the notes/create renote path.
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

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::error::{api_err, ApiError};
use crate::AppState;

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
/// notify itself and is dropped when the notifiee mutes it (recieveConfig /
/// suspended-notifier pack filtering not modeled — TODO(M2+)).
pub(crate) async fn notify_note_event(
    pool: &mk_db::sqlx::PgPool,
    notifiee_id: &str,
    notifier_id: &str,
    reason: &str,
    note_id: &str,
) {
    if notifiee_id == notifier_id {
        return;
    }
    match mk_db::social::muting_exists(pool, notifiee_id, notifier_id).await {
        Ok(true) => return,
        Ok(false) => {}
        Err(e) => {
            tracing::error!(target: "dbg_social", error = %e, "notify_note_event muting_exists");
            return;
        }
    }
    if let Err(e) = mk_db::notification::insert_notification(
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
        tracing::error!(target: "dbg_social", error = %e, "notify_note_event insert");
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
            // the endpoint still answers with the packed followee
            return Ok(ok_json(pack_target(state, pool, &followee, me_id).await?));
        }
    }

    // insertFollowingDoc (+ pending-request cleanup inside) + counter bumps
    mk_db::social::insert_following(
        pool,
        &mk_types::id::gen_aid_now(),
        me_id,
        user_id,
        with_replies,
    )
    .await
    .map_err(|e| db_err("following_create insert_following", e))?;
    mk_db::social::bump_follow_counts(pool, me_id, user_id, 1)
        .await
        .map_err(|e| db_err("following_create bump", e))?;

    // TODO(M2+): 'follow'/'followed' notifications (notifications family)
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
    mk_db::social::bump_follow_counts(pool, me_id, user_id, -1)
        .await
        .map_err(|e| db_err("following_delete bump", e))?;

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
    mk_db::social::insert_following(
        pool,
        &mk_types::id::gen_aid_now(),
        user_id,
        me_id,
        with_replies,
    )
    .await
    .map_err(|e| db_err("accept insert_following", e))?;
    mk_db::social::bump_follow_counts(pool, user_id, me_id, 1)
        .await
        .map_err(|e| db_err("accept bump", e))?;

    // TODO(M2+): 'followRequestAccepted' notification
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
    if let Err(e) =
        mk_db::social::insert_renote_note(pool, &id, me_id, &target, text, visibility, local_only)
            .await
    {
        return db_err("renote insert", e);
    }
    // incNotesCount + tag merge (a textless renote carries no hashtags)
    let hashtags = text
        .map(mk_core::services::misc::extract_hashtags)
        .unwrap_or_default();
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
    }

    // TODO(M2+): replyId+renoteId quote-combo reply-target validation
    match super::pack_note(state, pool, &id).await {
        Ok(n) => (StatusCode::OK, Json(json!({ "createdNote": n }))).into_response(),
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
        mk_db::note::timeline_notes(
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
        ),
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
        mk_db::note::local_timeline_notes(
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
        ),
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
        mk_db::note::hybrid_timeline_notes(
            pool,
            me_id,
            &followees,
            opt_id(body, "sinceId"),
            opt_id(body, "untilId"),
            opt_i(body, "limit", 10).clamp(1, 100),
            body.get("withFiles")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            body.get("withReplies")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
        ),
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

/// `POST /api/notes/reactions` — reaction list for one note.
pub(crate) async fn notes_reactions(state: &crate::api::AppState, body: &Value) -> Response {
    let Some(pool) = &state.pool else {
        return crate::api::api_err(crate::api::ApiError::internal_error());
    };
    let Some(note_id) = opt_id(body, "noteId") else {
        return crate::api::api_err(crate::api::ApiError::invalid_param(
            json!({ "param": "noteId", "reason": "required" }),
        ));
    };
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
    match mk_db::note::find_note_reaction(pool, note_id, me_id).await {
        Ok(Some(_mine)) => {
            if let Err(e) = mk_db::note::delete_note_reaction(pool, note_id, me_id).await {
                tracing::error!(target: "dbg_notes", error = %e, "delete_note_reaction");
                return crate::api::api_err(crate::api::ApiError::internal_error());
            }
            if let Err(e) = mk_db::note::bump_reactions_map(pool, note_id, reaction, -1).await {
                tracing::error!(target: "dbg_notes", error = %e, "bump_reactions_map");
            }
        }
        Ok(None) => {
            if let Err(e) = mk_db::note::insert_note_reaction(pool, me_id, note_id, reaction).await
            {
                tracing::error!(target: "dbg_notes", error = %e, "insert_note_reaction");
                return crate::api::api_err(crate::api::ApiError::internal_error());
            }
            if let Err(e) = mk_db::note::bump_reactions_map(pool, note_id, reaction, 1).await {
                tracing::error!(target: "dbg_notes", error = %e, "bump_reactions_map");
            }
            // reaction notification (notifiee = note author, local)
            if let Ok(Some(note)) = mk_db::note::find_note_by_id(pool, note_id).await {
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
    match mk_db::note::delete_note(pool, note_id, me_id).await {
        Ok(1) => {
            if let Err(e) = mk_db::note::dec_note_count(pool, me_id).await {
                tracing::error!(target: "dbg_notes", error = %e, "dec_note_count");
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

/// Credential gate for simple (me, body) note endpoints.
pub(crate) async fn credential_gate<F, Fut>(
    state: &AppState,
    body: &Value,
    token: Option<&str>,
    handler: F,
) -> Response
where
    F: FnOnce(&AppState, &Value, &str) -> Fut,
    Fut: std::future::Future<Output = Response>,
{
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = match require_checked_user(pool, token, true).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    handler(state, body, &me_id).await
}
