//! User lists domain (upstream `endpoints/users/lists/create.ts`,
//! `users/lists/push.ts`, `users/lists/update-membership.ts` +
//! `notes/user-list-timeline.ts`, plus the `channels/mute/create.ts`
//! requisite the list-TL channel legs mute with).
//!
//! Upstream refs (read-only): core/UserListService.ts (addMember/
//! updateMembership — `withReplies ?? false` default), core/entities/
//! UserListEntityService.ts (pack = { id, createdAt, name, userIds,
//! isPublic }), core/ChannelMutingService.ts. Db paths live in `mk_db::lists`
//! (the timeline query mirrors notes/user-list-timeline.ts `getFromDb`).
//!
//! All five endpoints are `requireCredential: true` (Notes require
//! `read:account`, list writes `write:account`, channel mute
//! `write:channels` — the kind scopes collapse into the standard CREDENTIAL_
//! REQUIRED / AUTHENTICATION_FAILED / YOUR_ACCOUNT_SUSPENDED chain here; the
//! e2e clients assert only code/message/shape). Meta errors without
//! `httpStatusCode` surface as kind=client 400.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// The endpoint names routed into this module (single source of truth for the
/// `// ul-tl-agent` dispatch arm in mod.rs).
pub(crate) const LIST_ENDPOINTS: &[&str] = &[
    "users/lists/create",
    "users/lists/push",
    "users/lists/update-membership",
    "notes/user-list-timeline",
    // list-TL requisite (the 16 チャンネルミュート legs of the timelines.ts
    // User List TL census mute via this endpoint before asserting)
    "channels/mute/create",
];

pub(crate) fn handles(name: &str) -> bool {
    LIST_ENDPOINTS.contains(&name)
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_lists", error = %e, "db error: {context}");
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

fn ok_empty() -> Response {
    // upstream `send(reply, undefined)` → 204
    StatusCode::NO_CONTENT.into_response()
}

fn ok_json(v: Value) -> Response {
    (StatusCode::OK, Json(v)).into_response()
}

fn invalid_param(param: &str, reason: &str) -> Response {
    api_err(ApiError::invalid_param(json!({
        "param": param,
        "reason": reason,
    })))
}

fn opt_id<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(|v| v.as_str())
}

fn required_id<'a>(body: &'a Value, key: &'a str) -> Result<&'a str, Response> {
    opt_id(body, key).ok_or_else(|| invalid_param(key, "required (misskey:id format)"))
}

fn opt_bool(body: &Value, key: &str, dflt: bool) -> Result<bool, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(dflt),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(invalid_param(key, "expected a boolean")),
    }
}

/// Entry from the `// ul-tl-agent` dispatch arm in mod.rs (body pre-parsed,
/// token + credential-declared pre-extracted).
pub(crate) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
    credential_declared: bool,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    // Every endpoint in this module is requireCredential upstream.
    let me_id = match super::social::require_checked_user(
        pool,
        token.as_deref(),
        credential_declared,
    )
    .await
    {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let result: Result<Response, Response> = match name {
        "users/lists/create" => lists_create(pool, body, &me_id).await,
        "users/lists/push" => lists_push(pool, body, &me_id).await,
        "users/lists/update-membership" => lists_update_membership(pool, body, &me_id).await,
        "notes/user-list-timeline" => user_list_timeline(state, pool, body, &me_id).await,
        "channels/mute/create" => channels_mute_create(pool, body, &me_id).await,
        _ => Ok(api_err(ApiError::unknown_endpoint())),
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

// ---------------------------------------------------------------------------
// UserListEntityService.pack
// ---------------------------------------------------------------------------

async fn pack_list(
    pool: &mk_db::sqlx::PgPool,
    row: &mk_db::lists::UserListRow,
) -> Result<Value, Response> {
    let user_ids = mk_db::lists::list_member_ids(pool, &row.id)
        .await
        .map_err(|e| db_err("pack_list list_member_ids", e))?;
    let created_at = mk_types::id::parse_aid_date(&row.id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default();
    Ok(json!({
        "id": row.id,
        "createdAt": created_at,
        "name": row.name,
        "userIds": user_ids,
        "isPublic": row.is_public,
    }))
}

// ---------------------------------------------------------------------------
// users/lists/create — create.ts
// ---------------------------------------------------------------------------

async fn lists_create(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let name = match body.get("name") {
        Some(Value::String(s)) if !s.is_empty() && s.chars().count() <= 100 => s.as_str(),
        Some(Value::String(_)) => {
            return Err(invalid_param("name", "must be 1..100 characters"));
        }
        _ => return Err(invalid_param("name", "required (1..100 characters)")),
    };
    // RoleService.getUserPolicies(me).userListLimit — DEFAULT_POLICIES value
    // (mod.rs default_policies_value) — no roles infrastructure rides here.
    let count = mk_db::lists::count_lists_owned(pool, me_id)
        .await
        .map_err(|e| db_err("lists_create count", e))?;
    if count >= 10 {
        return Err(meta_err(
            "TOO_MANY_USERLISTS",
            "0cf21a28-7715-4f39-a20d-777bfdb8d138",
            "You cannot create user list any more.",
        ));
    }
    let id = mk_types::id::gen_aid_now();
    mk_db::lists::insert_list(pool, &id, me_id, name)
        .await
        .map_err(|e| db_err("lists_create insert", e))?;
    let row = mk_db::lists::find_list(pool, &id, me_id)
        .await
        .map_err(|e| db_err("lists_create refetch", e))?
        .ok_or_else(|| api_err(ApiError::internal_error()))?;
    Ok(ok_json(pack_list(pool, &row).await?))
}

// ---------------------------------------------------------------------------
// users/lists/push — push.ts + UserListService.addMember
// ---------------------------------------------------------------------------

async fn lists_push(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let list_id = required_id(body, "listId")?;
    let user_id = required_id(body, "userId")?;
    let list = mk_db::lists::find_list(pool, list_id, me_id)
        .await
        .map_err(|e| db_err("lists_push find_list", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_LIST",
                "2214501d-ac96-4049-b717-91e42272a711",
                "No such list.",
            )
        })?;
    mk_db::user::find_user_by_id(pool, user_id)
        .await
        .map_err(|e| db_err("lists_push find_user", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_USER",
                "a89abd3d-f0bc-4cce-beb1-2f446f4f1e6a",
                "No such user.",
            )
        })?;
    // push.ts blocks only when the TARGET blocked me (user.id !== me.id gate).
    if user_id != me_id
        && mk_db::social::blocking_exists(pool, user_id, me_id)
            .await
            .map_err(|e| db_err("lists_push blocking_exists", e))?
    {
        return Err(meta_err(
            "YOU_HAVE_BEEN_BLOCKED",
            "990232c5-3f9d-4d83-9f3f-ef27b6332a4b",
            "You cannot push this user because you have been blocked by this user.",
        ));
    }
    if mk_db::lists::membership_exists(pool, list_id, user_id)
        .await
        .map_err(|e| db_err("lists_push membership_exists", e))?
    {
        return Err(meta_err(
            "ALREADY_ADDED",
            "1de7c884-1595-49e9-857e-61f12f4d4fc5",
            "That user has already been added to that list.",
        ));
    }
    // getUserPolicies(me).userEachUserListsLimit — DEFAULT_POLICIES value.
    let count = mk_db::lists::count_memberships(pool, list_id)
        .await
        .map_err(|e| db_err("lists_push count_memberships", e))?;
    if count >= 50 {
        return Err(meta_err(
            "TOO_MANY_USERS",
            "2dd9752e-a338-413d-8eec-41814430989b",
            "You can not push users any more.",
        ));
    }
    mk_db::lists::insert_membership(
        pool,
        &mk_types::id::gen_aid_now(),
        user_id,
        &list.id,
        me_id,
        // UserListService.addMember: `withReplies: options.withReplies ?? false`
        false,
    )
    .await
    .map_err(|e| db_err("lists_push insert_membership", e))?;
    Ok(ok_empty())
}

// ---------------------------------------------------------------------------
// users/lists/update-membership — update-membership.ts + updateMembership
// ---------------------------------------------------------------------------

async fn lists_update_membership(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let list_id = required_id(body, "listId")?;
    let user_id = required_id(body, "userId")?;
    let with_replies = opt_bool(body, "withReplies", false)?;
    mk_db::lists::find_list(pool, list_id, me_id)
        .await
        .map_err(|e| db_err("update_membership find_list", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_LIST",
                "7f44670e-ab16-43b8-b4c1-ccd2ee89cc02",
                "No such list.",
            )
        })?;
    mk_db::user::find_user_by_id(pool, user_id)
        .await
        .map_err(|e| db_err("update_membership find_user", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_USER",
                "588e7f72-c744-4a61-b180-d354e912bda2",
                "No such user.",
            )
        })?;
    // updateMembership throws `Error('User is not a member of the list')` on
    // the missing row — an untyped error the endpoint does not catch → 500 ISE.
    let updated =
        mk_db::lists::update_membership_with_replies(pool, list_id, user_id, with_replies)
            .await
            .map_err(|e| db_err("update_membership update", e))?;
    if !updated {
        return Err(api_err(ApiError::internal_error()));
    }
    Ok(ok_empty())
}

// ---------------------------------------------------------------------------
// notes/user-list-timeline — user-list-timeline.ts
// ---------------------------------------------------------------------------

async fn user_list_timeline(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let list_id = required_id(body, "listId")?;
    mk_db::lists::find_list(pool, list_id, me_id)
        .await
        .map_err(|e| db_err("user_list_timeline find_list", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_LIST",
                "8fb1fbd5-e476-4c37-9fb0-43d55b63a2ff",
                "No such list.",
            )
        })?;
    // paramDef pager: limit 1..100 default 10; sinceId/untilId with the
    // sinceDate/untilDate ms → aid-gen fallback (paramDef L52-53); when both
    // forms arrive the id form wins (`ps.untilId ?? idService.gen(untilDate)`).
    let limit = body
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(10)
        .clamp(1, 100);
    let until_id = match opt_id(body, "untilId") {
        Some(u) => Some(u.to_string()),
        // `ps.untilId ?? (ps.untilDate ? idService.gen(...) : null)` — a 0
        // date is falsy upstream and resolves to no bound.
        None => body
            .get("untilDate")
            .and_then(|v| v.as_i64())
            .filter(|&ms| ms != 0)
            .map(mk_types::id::gen_aid),
    };
    let since_id = match opt_id(body, "sinceId") {
        Some(s) => Some(s.to_string()),
        None => body
            .get("sinceDate")
            .and_then(|v| v.as_i64())
            .filter(|&ms| ms != 0)
            .map(mk_types::id::gen_aid),
    };
    let with_files = opt_bool(body, "withFiles", false)?;
    let with_renotes = opt_bool(body, "withRenotes", true)?;
    let include_my_renotes = opt_bool(body, "includeMyRenotes", true)?;
    let include_renoted_my_notes = opt_bool(body, "includeRenotedMyNotes", true)?;
    let include_local_renotes = opt_bool(body, "includeLocalRenotes", true)?;

    let followees = mk_db::social::followee_ids(pool, me_id)
        .await
        .map_err(|e| db_err("user_list_timeline followee_ids", e))?;
    let rows = mk_db::lists::user_list_timeline_notes(
        pool,
        list_id,
        me_id,
        &followees,
        since_id.as_deref(),
        until_id.as_deref(),
        limit,
        with_files,
        with_renotes,
        include_my_renotes,
        include_renoted_my_notes,
        include_local_renotes,
    )
    .await
    .map_err(|e| db_err("user_list_timeline_notes", e))?;

    // Shared pack pipeline (notes/show / social.rs pack_note_row shape).
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let user_lite = super::note_user_lite(state, pool, &row.user_id).await?;
        let created_at = mk_types::id::parse_aid_date(&row.id)
            .map(mk_core::services::signup::ms_to_iso)
            .unwrap_or_default();
        let emojis = json!({});
        out.push(mk_core::packers::note::note_pack_min(
            row,
            user_lite,
            created_at,
            &row.mentions,
            &row.tags,
            emojis,
        ));
    }
    Ok(ok_json(json!(out)))
}

// ---------------------------------------------------------------------------
// channels/mute/create — list-TL requisite (ChannelMutingService.mute)
// ---------------------------------------------------------------------------

async fn channels_mute_create(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let channel_id = required_id(body, "channelId")?;
    let target = mk_db::channel::find_channel(pool, channel_id)
        .await
        .map_err(|e| db_err("channels_mute find_channel", e))?;
    if target.is_none() {
        return Err(meta_err(
            "NO_SUCH_CHANNEL",
            "7174361e-d58f-31d6-2e7c-6fb830786a3f",
            "No such Channel.",
        ));
    }
    if mk_db::channel::is_muting(pool, me_id, channel_id)
        .await
        .map_err(|e| db_err("channels_mute is_muting", e))?
    {
        return Err(meta_err(
            "ALREADY_MUTING_CHANNEL",
            "5a251978-769a-da44-3e89-3931e43bb592",
            "You are already muting that user.",
        ));
    }
    // expiresAt: null → indefinite mute; a unix-ms int must lie in the future.
    let expires_at: Option<i64> = match body.get("expiresAt") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let ms = v
                .as_i64()
                .ok_or_else(|| invalid_param("expiresAt", "expected an integer or null"))?;
            if ms <= super::now_ms() as i64 {
                return Err(meta_err(
                    "EXPIRES_AT_IS_PAST",
                    "42b32236-df2c-a45f-fdbf-def67268f749",
                    "Cannot set past date to \"expiresAt\".",
                ));
            }
            Some(ms)
        }
    };
    mk_db::lists::insert_channel_muting(
        pool,
        &mk_types::id::gen_aid_now(),
        me_id,
        channel_id,
        expires_at,
    )
    .await
    .map_err(|e| db_err("channels_mute insert", e))?;
    Ok(ok_empty())
}
