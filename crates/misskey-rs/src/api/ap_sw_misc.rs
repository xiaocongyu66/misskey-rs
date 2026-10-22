//! ap/* + sw/* + notifications extras + small federation/invite ports
//! (upstream `server/api/endpoints/{ap,sw,notifications,federation,invite}/*.ts`
//! at the pinned revision).
//!
//! Implemented here:
//! - `ap/show` — the LOCAL-table half of `fetchAny`: a `uri` is resolved to a
//!   user (`user."uri"`, or `/users/<id>` for self-host URLs) or a note
//!   (`note."uri"`, or `/notes/<id>`) and packed. The remote-fetch tail of
//!   upstream `fetchAny` (`ApResolverService.createResolver()` +
//!   `ApPersonService.createPerson` / `ApNoteService.createNote`) is NOT
//!   ported: this workspace has no generic ActivityPub resolver (mk-ap only
//!   fetches actor public keys), so an unknown remote URI answers
//!   NO_SUCH_OBJECT instead of fetching and caching the object.
//! - `sw/show-registration` — `findOneBy({ userId, endpoint })` →
//!   `{ userId, endpoint, sendReadMessage }` or `null`.
//! - `sw/update-registration` — same lookup, optional `sendReadMessage`
//!   write, NO_SUCH_REGISTRATION otherwise.
//! - `notifications/create` — the `app` notification row (custom body /
//!   header / icon + appAccessTokenId columns from 0002).
//! - `notifications/flush` — marks every notification of the caller read
//!   (upstream deletes the Redis notification timeline; the DB-table port
//!   has no stream to delete, so the observable "all read" state is the
//!   closest equivalent — see the handler note).
//! - `notifications/test-notification` — a notifier-less `test` row.
//! - `federation/update-remote-user` — GetterService.getRemoteUser checks
//!   (missing user → NO_SUCH_USER, local user → internal error like the
//!   upstream `throw new Error`) then a no-op 204: `ApPersonService
//!   .updatePerson` is an AP fetch lane that is NOT ported (no generic
//!   resolver; see ap/show).
//! - `invite/delete` — delete a `registration_ticket` the caller created
//!   (moderators may delete any ticket / a used ticket). The upstream
//!   `requiredRolePolicy: 'canInvite'` gate has no role-policy engine in
//!   this repo; the default policy applies, like the rest of the invite lane.
//!
//! Documented skip: `ap/get` — upstream resolves an arbitrary URI through
//! `ApResolverService.createResolver().resolve(uri)`. No such resolver
//! exists in this workspace, so the endpoint is intentionally not registered
//! (unknown endpoint) rather than returning a fabricated object.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// Names routed into this module (single source of truth for the dispatch
/// arm in mod.rs).
pub(crate) const AP_SW_MISC_ENDPOINTS: &[&str] = &[
    "ap/show",
    "sw/show-registration",
    "sw/update-registration",
    "notifications/create",
    "notifications/flush",
    "notifications/test-notification",
    "federation/update-remote-user",
    "invite/delete",
];

pub(crate) fn handles(name: &str) -> bool {
    AP_SW_MISC_ENDPOINTS.contains(&name)
}

// ==== error envelopes =======================================================

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_ap_sw_misc", error = %e, "db error: {context}");
    internal()
}

fn meta_err(status: u16, code: &str, id: &str, message: &str) -> Response {
    api_err(ApiError::new(
        status,
        mk_types::api_error::ApiErrorKind::Client,
        code,
        id,
        message,
    ))
}

fn invalid_param(param: &str, reason: &str) -> Response {
    super::invalid_param(param, reason)
}

// ap/show.ts meta.errors
fn uri_invalid() -> Response {
    meta_err(
        400,
        "URI_INVALID",
        "1a5eab56-e47b-48c2-8d5e-217b897d70db",
        "URI is invalid.",
    )
}

fn no_such_object() -> Response {
    meta_err(
        400,
        "NO_SUCH_OBJECT",
        "dc94d745-1262-4e63-a17d-fecaa57efc82",
        "No such object.",
    )
}

// sw/update-registration.ts meta.errors (id keeps upstream's leading space)
fn no_such_registration() -> Response {
    meta_err(
        400,
        "NO_SUCH_REGISTRATION",
        " b09d8066-8064-5613-efb6-0e963b21d012",
        "No such registration.",
    )
}

// invite/delete.ts meta.errors
fn no_such_invite_code() -> Response {
    meta_err(
        400,
        "NO_SUCH_INVITE_CODE",
        "cd4f9ae4-7854-4e3e-8df9-c296f051e634",
        "No such invite code.",
    )
}

fn cant_delete_invite_code() -> Response {
    meta_err(
        400,
        "CAN_NOT_DELETE_INVITE_CODE",
        "ff17af39-000c-4d4e-abdf-848fa30fc1ce",
        "You can't delete this invite code.",
    )
}

fn access_denied() -> Response {
    meta_err(
        400,
        "ACCESS_DENIED",
        "5eb8d909-2540-4970-90b8-dd6f86088121",
        "Access denied.",
    )
}

fn no_such_user() -> Response {
    meta_err(
        400,
        "NO_SUCH_USER",
        "15348ddd-432d-49c2-8a5a-8069753becff",
        "No such user.",
    )
}

// ==== auth / params =========================================================

async fn require_me(
    state: &AppState,
    token: Option<&str>,
    declared: bool,
) -> Result<String, Response> {
    let Some(pool) = &state.pool else {
        return Err(internal());
    };
    super::social::require_checked_user(pool, token, declared).await
}

fn require_str(body: &Value, key: &str) -> Result<String, Response> {
    match body.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        None | Some(Value::Null) => Err(invalid_param(
            key,
            &format!("must have required property '{key}'"),
        )),
        Some(_) => Err(invalid_param(key, "must be string")),
    }
}

fn opt_nullable_str(body: &Value, key: &str) -> Result<Option<String>, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(invalid_param(key, "must be string or null")),
    }
}

fn opt_bool(body: &Value, key: &str) -> Result<Option<bool>, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(invalid_param(key, "must be boolean")),
    }
}

// ==== dispatch ==============================================================

pub(crate) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
    credential_declared: bool,
) -> Response {
    let token = token.as_deref();
    match name {
        "ap/show" => ap_show(state, body, token, credential_declared).await,
        "sw/show-registration" => {
            sw_show_registration(state, body, token, credential_declared).await
        }
        "sw/update-registration" => {
            sw_update_registration(state, body, token, credential_declared).await
        }
        "notifications/create" => {
            notifications_create(state, body, token, credential_declared).await
        }
        "notifications/flush" => notifications_flush(state, token, credential_declared).await,
        "notifications/test-notification" => {
            notifications_test(state, token, credential_declared).await
        }
        "federation/update-remote-user" => {
            update_remote_user(state, body, token, credential_declared).await
        }
        "invite/delete" => invite_delete(state, body, token, credential_declared).await,
        _ => api_err(ApiError::unknown_endpoint()),
    }
}

// ==== ap/show ===============================================================

/// Upstream `fetchAny` local half: self-host `/users/<id>` + `/notes/<id>`
/// paths resolve by id, remote URIs resolve against the `uri` columns. The
/// remote fetch tail is intentionally absent (module header).
async fn ap_show(state: &AppState, body: &Value, token: Option<&str>, declared: bool) -> Response {
    let Some(pool) = &state.pool else {
        return internal();
    };
    let me = match require_me(state, token, declared).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let uri = match require_str(body, "uri") {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    // UtilityService.isFederationAllowedUri is not ported (needs the meta
    // blocked-host list); only the URL shape is validated here.
    let Ok(parsed) = url::Url::parse(&uri) else {
        return uri_invalid();
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return uri_invalid();
    }
    let host = parsed.host_str().unwrap_or_default();
    let self_host = state.config.host();
    let self_host_only = self_host.split(':').next().unwrap_or(self_host.as_str());

    if host == self_host_only {
        // ApDbResolverService.parseUri local branch: /users/<id>, /notes/<id>.
        let segs: Vec<&str> = parsed
            .path_segments()
            .map(|s| s.filter(|p| !p.is_empty()).collect())
            .unwrap_or_default();
        match segs.as_slice() {
            ["users", id] => return pack_user(state, pool, *id, &me).await,
            ["notes", id] => return pack_note(state, pool, *id, &me).await,
            // local object, not found in db? fail (upstream returns null).
            _ => return no_such_object(),
        }
    }

    // Remote branch: DB-only lookup (no resolver fetch).
    match mk_db::user::find_user_id_by_uri(pool, &uri).await {
        Ok(Some(id)) => return pack_user(state, pool, &id, &me).await,
        Ok(None) => {}
        Err(e) => return db_err("find_user_id_by_uri", e),
    }
    match mk_db::note::find_note_id_by_uri(pool, &uri).await {
        Ok(Some(id)) => return pack_note(state, pool, &id, &me).await,
        Ok(None) => no_such_object(),
        Err(e) => db_err("find_note_id_by_uri", e),
    }
}

async fn pack_user(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    user_id: &str,
    me: &str,
) -> Response {
    match super::freq::pack_user_detailed(state, pool, user_id, Some(me)).await {
        Ok(Some(object)) => (
            StatusCode::OK,
            Json(json!({ "type": "User", "object": object })),
        )
            .into_response(),
        Ok(None) => no_such_object(),
        Err(resp) => resp,
    }
}

async fn pack_note(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    note_id: &str,
    me: &str,
) -> Response {
    match super::pack_note_for(state, pool, note_id, Some(me)).await {
        Ok(object) => (
            StatusCode::OK,
            Json(json!({ "type": "Note", "object": object })),
        )
            .into_response(),
        // upstream mergePack swallows a note pack failure → null → NO_SUCH_OBJECT
        Err(_) => no_such_object(),
    }
}

// ==== sw/* ==================================================================

async fn sw_show_registration(
    state: &AppState,
    body: &Value,
    token: Option<&str>,
    declared: bool,
) -> Response {
    let Some(pool) = &state.pool else {
        return internal();
    };
    let me = match require_me(state, token, declared).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let endpoint = match require_str(body, "endpoint") {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    match mk_db::sw::find_registration(pool, &me, &endpoint).await {
        Ok(Some(row)) => (
            StatusCode::OK,
            Json(json!({
                "userId": me,
                "endpoint": row.endpoint,
                "sendReadMessage": row.send_read_message,
            })),
        )
            .into_response(),
        // res nullable: upstream returns null (200 with a null body).
        Ok(None) => (StatusCode::OK, Json(json!(null))).into_response(),
        Err(e) => db_err("find_registration", e),
    }
}

async fn sw_update_registration(
    state: &AppState,
    body: &Value,
    token: Option<&str>,
    declared: bool,
) -> Response {
    let Some(pool) = &state.pool else {
        return internal();
    };
    let me = match require_me(state, token, declared).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let endpoint = match require_str(body, "endpoint") {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let send_read = match opt_bool(body, "sendReadMessage") {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let row = match mk_db::sw::find_registration(pool, &me, &endpoint).await {
        Ok(Some(r)) => r,
        Ok(None) => return no_such_registration(),
        Err(e) => return db_err("find_registration", e),
    };
    let new_value = send_read.unwrap_or(row.send_read_message);
    if let Err(e) = mk_db::sw::update_send_read_message(pool, &row.id, new_value).await {
        return db_err("update_send_read_message", e);
    }
    (
        StatusCode::OK,
        Json(json!({
            "userId": me,
            "endpoint": row.endpoint,
            "sendReadMessage": new_value,
        })),
    )
        .into_response()
}

// ==== notifications/* =======================================================

async fn notifications_create(
    state: &AppState,
    body: &Value,
    token: Option<&str>,
    declared: bool,
) -> Response {
    let Some(pool) = &state.pool else {
        return internal();
    };
    let me = match require_me(state, token, declared).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let custom_body = match require_str(body, "body") {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let custom_header = match opt_nullable_str(body, "header") {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let custom_icon = match opt_nullable_str(body, "icon") {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    // The credential chain here resolves native user tokens only (the shared
    // social::require_checked_user), so appAccessTokenId stays NULL and the
    // upstream `token?.name/iconUrl` defaults are null.
    let id = mk_types::id::gen_aid(super::now_ms() as i64);
    if let Err(e) = mk_db::notification::insert_app_notification(
        pool,
        &id,
        &me,
        &custom_body,
        custom_header.as_deref(),
        custom_icon.as_deref(),
        None,
    )
    .await
    {
        return db_err("insert_app_notification", e);
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn notifications_flush(state: &AppState, token: Option<&str>, declared: bool) -> Response {
    let Some(pool) = &state.pool else {
        return internal();
    };
    let me = match require_me(state, token, declared).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    // Upstream flushAllNotifications DELETEs the Redis notification timeline.
    // The DB-table port has no stream to delete; marking every row read is
    // the observable equivalent (a subsequent unreadOnly list is empty).
    if let Err(e) = mk_db::notification::mark_as_read_all(pool, &me).await {
        return db_err("mark_as_read_all", e);
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn notifications_test(state: &AppState, token: Option<&str>, declared: bool) -> Response {
    let Some(pool) = &state.pool else {
        return internal();
    };
    let me = match require_me(state, token, declared).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let id = mk_types::id::gen_aid(super::now_ms() as i64);
    if let Err(e) = mk_db::notification::insert_test_notification(pool, &id, &me).await {
        return db_err("insert_test_notification", e);
    }
    StatusCode::NO_CONTENT.into_response()
}

// ==== federation/update-remote-user ========================================

async fn update_remote_user(
    state: &AppState,
    body: &Value,
    token: Option<&str>,
    declared: bool,
) -> Response {
    let Some(pool) = &state.pool else {
        return internal();
    };
    let _me = match require_me(state, token, declared).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let user_id = match require_str(body, "userId") {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let row = match mk_db::user::find_user_by_id(pool, &user_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return no_such_user(),
        Err(e) => return db_err("find_user_by_id", e),
    };
    let host = match mk_db::note::user_host_of(pool, &row.id).await {
        Ok(h) => h,
        Err(e) => return db_err("user_host_of", e),
    };
    if host.is_none() {
        // GetterService.getRemoteUser: `throw new Error('user is not a remote
        // user')` → ApiCallService 500. Keep the upstream shape.
        tracing::error!(target: "dbg_ap_sw_misc", user = %user_id, "update-remote-user: local user");
        return internal();
    }
    // ApPersonService.updatePerson(uri) is an AP fetch lane with no generic
    // resolver in this workspace — the refresh is a documented no-op; the
    // endpoint answers the upstream void/204 response.
    StatusCode::NO_CONTENT.into_response()
}

// ==== invite/delete ========================================================

/// RoleService.isModerator mirror (root id / username shortcut + assigned
/// role), same chain as api::admin.
async fn is_moderator(pool: &mk_db::sqlx::PgPool, me_id: &str) -> bool {
    if let Ok(Some(root)) = mk_db::admin::root_user_id(pool).await {
        if root == me_id {
            return true;
        }
    }
    if let Ok(Some(row)) = mk_db::user::find_user_by_id(pool, me_id).await {
        if row.username_lower == "root" {
            return true;
        }
    }
    match mk_db::role::user_assigned_roles(pool, me_id).await {
        Ok(assigned) => mk_core::services::role::has_moderator_role(&assigned),
        Err(e) => {
            tracing::error!(target: "dbg_ap_sw_misc", error = %e, "user_assigned_roles");
            false
        }
    }
}

async fn invite_delete(
    state: &AppState,
    body: &Value,
    token: Option<&str>,
    declared: bool,
) -> Response {
    let Some(pool) = &state.pool else {
        return internal();
    };
    let me = match require_me(state, token, declared).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let invite_id = match require_str(body, "inviteId") {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let ticket = match mk_db::invite::find_by_id(pool, &invite_id).await {
        Ok(Some(t)) => t,
        Ok(None) => return no_such_invite_code(),
        Err(e) => return db_err("invite find_by_id", e),
    };
    let moderator = is_moderator(pool, &me).await;
    if ticket.created_by_id.as_deref() != Some(me.as_str()) && !moderator {
        return access_denied();
    }
    if ticket.used_at_ms.is_some() && !moderator {
        return cant_delete_invite_code();
    }
    if let Err(e) = mk_db::invite::delete_by_id(pool, &ticket.id).await {
        return db_err("invite delete_by_id", e);
    }
    StatusCode::NO_CONTENT.into_response()
}
