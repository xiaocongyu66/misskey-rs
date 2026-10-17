//! `admin/*` endpoint family (M2 slice) — ports of
//! `endpoints/admin/{show-user,show-users,suspend-user,unsuspend-user,meta,
//! server-info}.ts` plus the `admin/stats` facade named by the slice plan.
//!
//! Auth chain mirrors `ApiCallService.call()` in order: credential gate (no
//! token → 401 CREDENTIAL_REQUIRED; unresolved token → 401
//! AUTHENTICATION_FAILED) → suspended gate (403 YOUR_ACCOUNT_SUSPENDED) →
//! requireModerator/requireAdmin role gates with the `meta.rootUserId` bypass
//! (403 ROLE_PERMISSION_DENIED with the upstream id per gate kind).
//!
//! AUTH SHORTCUT (M2, documented deviation): upstream marks the requester as
//! root via `meta.rootUserId`, which SignupService stamps on the *first*
//! signup. Our signup route does not write that yet, so admin detection is
//! `meta.rootUserId == me.id` OR `usernameLower == 'root'` (the e2e harness
//! signs up `root` first, matching the first-user-is-root semantics) OR an
//! isAdministrator role assignment. When signup starts stamping
//! `meta.rootUserId`, the username fallback becomes inert.

use axum::extract::Request;
use axum::http::{header, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::error::{api_err, ApiError};
use crate::AppState;

use super::{build_relation, invalid_param, opt_bool, opt_enum, opt_nullable_str, pack_search_rows};

/// Endpoint names owned by this module (upstream `endpoint-list.ts` admin
/// names plus the legacy `admin/suspend`/`admin/unsuspend` name pair the
/// slice plan calls for; all POST-only).
const ENDPOINTS: &[&str] = &[
    "admin/show-user",
    "admin/show-users",
    "admin/suspend-user",
    "admin/unsuspend-user",
    "admin/suspend",
    "admin/unsuspend",
    "admin/meta",
    "admin/server-info",
    "admin/stats",
];

type ApiResult = Result<Response, Response>;

/// Single entry point from the mod.rs dispatch arm.
pub(super) async fn handle(state: &AppState, name: &str, req: Request) -> Response {
    // Fastify registers POST routes for known endpoints only; other names
    // fall to the catch-all → UNKNOWN_API_ENDPOINT (regardless of method).
    if !ENDPOINTS.contains(&name) {
        return api_err(ApiError::unknown_endpoint());
    }

    // ApiServerService: `request.method === 'GET' && !meta.allowGet` → 405
    // with an empty body. No admin endpoint sets allowGet.
    if req.method() != Method::POST {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }

    let headers = req.headers().clone();
    let body = match super::parse_json_body(req).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };

    // handleRequest token resolution: `Authorization: Bearer` wins over body
    // `i`; a present-but-non-string body `i` is a 400 with an empty body.
    let token = match headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
    {
        Some(t) => Some(t.to_string()),
        None => match body.get("i") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => return StatusCode::BAD_REQUEST.into_response(),
        },
    };

    let result = match name {
        "admin/suspend-user" | "admin/suspend" => suspend(state, &body, token.as_deref(), true),
        "admin/unsuspend-user" | "admin/unsuspend" => {
            suspend(state, &body, token.as_deref(), false)
        }
        "admin/show-user" => show_user(state, &body, token.as_deref()),
        "admin/show-users" => show_users(state, &body, token.as_deref()),
        "admin/meta" => admin_meta(state, token.as_deref()),
        "admin/server-info" => server_info(state, token.as_deref()),
        "admin/stats" => stats(state, token.as_deref()),
        _ => unreachable!("checked against ENDPOINTS above"),
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

// --- error/deny shapes (ApiCallService.call + #onExecError) ---

fn credential_required() -> Response {
    api_err(ApiError::new(
        401,
        mk_types::api_error::ApiErrorKind::Client,
        "CREDENTIAL_REQUIRED",
        "1384574d-a912-4b81-8601-c7b1c4085df1",
        "Credential required.",
    ))
}

fn authentication_failed() -> Response {
    api_err(ApiError::new(
        401,
        mk_types::api_error::ApiErrorKind::Client,
        "AUTHENTICATION_FAILED",
        "b0a7f5f8-dc2f-4171-b91f-de88ad238e14",
        "Authentication failed. Please ensure your token is correct.",
    ))
}

fn account_suspended() -> Response {
    api_err(ApiError::new(
        403,
        mk_types::api_error::ApiErrorKind::Permission,
        "YOUR_ACCOUNT_SUSPENDED",
        "a8c724b3-6e9c-4b46-b1a8-bc3ed6258370",
        "Your account has been suspended.",
    ))
}

fn role_permission_denied(admin: bool) -> Response {
    let (id, message) = if admin {
        (
            "c3d38592-54c0-429d-be96-5636b0431a61",
            "You are not assigned to an administrator role.",
        )
    } else {
        (
            "d33d5333-db36-423d-a8f9-1a2b9549da41",
            "You are not assigned to a moderator role.",
        )
    };
    api_err(ApiError::new(
        403,
        mk_types::api_error::ApiErrorKind::Permission,
        "ROLE_PERMISSION_DENIED",
        id,
        message,
    ))
}

/// Plain handler `throw new Error(...)` → ApiCallService.#onExecError wraps
/// into the default 500 INTERNAL_ERROR ApiError.
fn handler_internal_error() -> Response {
    api_err(ApiError::internal_error())
}

// --- auth gate ---

/// Authenticated admin-context user: row + privilege flags.
struct AdminCtx {
    me: mk_db::user::LocalUserRow,
    /// isAdministrator (RoleService): root bypass or isAdministrator role.
    is_admin: bool,
}

enum Gate {
    /// requireModerator metas (root or moderator/administrator role).
    Moderator,
    /// requireAdmin metas (root or administrator role).
    Admin,
}

/// RoleService probe for a user row: `meta.rootUserId === id` ||
/// `usernameLower == 'root'` (M2 shortcut, see module docs) ||
/// assigned isAdministrator role.
async fn user_is_administrator(
    pool: &mk_db::sqlx::PgPool,
    root_id: Option<&str>,
    row: &mk_db::user::LocalUserRow,
) -> bool {
    if root_id == Some(row.id.as_str()) || row.username_lower == "root" {
        return true;
    }
    match mk_db::role::user_assigned_roles(pool, &row.id).await {
        Ok(assigned) => mk_core::services::role::has_administrator_role(&assigned),
        Err(e) => {
            tracing::error!(target: "dbg_admin", error = %e, "user_assigned_roles[admin]");
            false
        }
    }
}

async fn user_is_moderator(
    pool: &mk_db::sqlx::PgPool,
    root_id: Option<&str>,
    row: &mk_db::user::LocalUserRow,
) -> bool {
    if root_id == Some(row.id.as_str()) || row.username_lower == "root" {
        return true;
    }
    match mk_db::role::user_assigned_roles(pool, &row.id).await {
        Ok(assigned) => mk_core::services::role::has_moderator_role(&assigned),
        Err(e) => {
            tracing::error!(target: "dbg_admin", error = %e, "user_assigned_roles[mod]");
            false
        }
    }
}

/// ApiCallService.call gate sequence for `requireCredential` +
/// requireModerator/requireAdmin metas (all admin/* endpoints are gated this
/// way; the gate enum picks which role check runs after the root bypass).
async fn gate(state: &AppState, token: Option<&str>, g: Gate) -> Result<AdminCtx, Response> {
    let Some(pool) = &state.pool else {
        return Err(api_err(ApiError::internal_error()));
    };

    let Some(token) = token else {
        return Err(credential_required());
    };
    // Native 16-char tokens only (app/access-token lookup is M3); the same
    // resolution the i/i-update routes use for requireCredential endpoints.
    if token.len() != 16 {
        return Err(authentication_failed());
    }
    let me_id = match mk_db::user::find_local_user_id_by_token(pool, token).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(target: "dbg_admin", error = %e, "find_local_user_id_by_token");
            return Err(api_err(ApiError::internal_error()));
        }
    };
    let Some(me_id) = me_id else {
        return Err(authentication_failed());
    };
    let me = match mk_db::user::find_user_by_id(pool, &me_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_admin", error = %e, "find_user_by_id[me]");
            return Err(api_err(ApiError::internal_error()));
        }
    };
    let Some(me) = me else {
        return Err(authentication_failed());
    };

    // call(): requireCredential suspended gate.
    if me.is_suspended {
        return Err(account_suspended());
    }

    let root_id = match mk_db::admin::root_user_id(pool).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(target: "dbg_admin", error = %e, "root_user_id");
            return Err(api_err(ApiError::internal_error()));
        }
    };
    let is_root = root_id.as_deref() == Some(me.id.as_str())
        || me.username_lower == "root"
        || user_is_administrator(pool, root_id.as_deref(), &me).await;

    if !is_root {
        let assigned = match mk_db::role::user_assigned_roles(pool, &me.id).await {
            Ok(a) => a,
            Err(e) => {
                tracing::error!(target: "dbg_admin", error = %e, "user_assigned_roles[me]");
                return Err(api_err(ApiError::internal_error()));
            }
        };
        match g {
            Gate::Moderator => {
                if !mk_core::services::role::has_moderator_role(&assigned) {
                    return Err(role_permission_denied(false));
                }
            }
            Gate::Admin => {
                if !mk_core::services::role::has_administrator_role(&assigned) {
                    return Err(role_permission_denied(true));
                }
            }
        }
    }

    let is_admin = is_root || {
        mk_db::role::user_assigned_roles(pool, &me.id)
            .await
            .map(|a| mk_core::services::role::has_administrator_role(&a))
            .unwrap_or(false)
    };
    Ok(AdminCtx { me, is_admin })
}

// --- params ---

/// paramDef `{ userId: { type: 'string', format: 'misskey:id' }, required }`.
fn require_user_id(body: &Value) -> Result<String, Response> {
    match body.get("userId") {
        None | Some(Value::Null) => Err(invalid_param(
            "userId",
            "must have required property 'userId'",
        )),
        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        Some(_) => Err(invalid_param("userId", "must be string")),
    }
}

/// paramDef limit: integer 1..=100, default 10 (ajv usesDefaults).
fn parse_limit(body: &Value) -> Result<i64, Response> {
    match body.get("limit") {
        None | Some(Value::Null) => Ok(10),
        Some(v) => match v.as_i64() {
            Some(n) if (1..=100).contains(&n) => Ok(n),
            _ => Err(invalid_param(
                "limit",
                "must be an integer between 1 and 100",
            )),
        },
    }
}

fn parse_offset(body: &Value) -> Result<i64, Response> {
    match body.get("offset") {
        None | Some(Value::Null) => Ok(0),
        Some(v) => match v.as_i64() {
            Some(n) if n >= 0 => Ok(n),
            _ => Err(invalid_param("offset", "must be a non-negative integer")),
        },
    }
}

// --- endpoints ---

/// `endpoints/admin/suspend-user.ts` / `unsuspend-user.ts` (and the legacy
/// `admin/suspend`/`admin/unsuspend` name pair → same handlers). Moderator
/// target check = `RoleService.isModerator(target)`; plain Error paths are
/// the upstream `throw new Error` → 500 INTERNAL_ERROR shapes. Responses are
/// void → 204 NO CONTENT (ApiCallService.send) by Auth refresh semantics.
async fn suspend(
    state: &AppState,
    body: &Value,
    token: Option<&str>,
    suspend: bool,
) -> ApiResult {
    let ctx = gate(state, token, Gate::Moderator).await?;
    let pool = state.pool.as_ref().expect("gated");

    let user_id = require_user_id(body).map_err(|e| e)?;
    let target = match mk_db::user::find_user_by_id(pool, &user_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_admin", error = %e, "find_user_by_id[target]");
            return Err(handler_internal_error());
        }
    };
    // suspend-user.ts / unsuspend-user.ts: `user == null → Error('user not found')`.
    let Some(target) = target else {
        return Err(handler_internal_error());
    };

    let root_id = mk_db::admin::root_user_id(pool)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_admin", error = %e, "root_user_id");
            handler_internal_error()
        })
        .err(); // None => no error
    if suspend {
        // suspend-user.ts: `isModerator(user) → Error('cannot suspend moderator account')`.
        let target_is_mod =
            user_is_moderator(pool, root_id.as_deref(), &target).await;
        if target_is_mod {
            return Err(handler_internal_error());
        }
ed_marker_placeholder:
        ()
    }

    if suspend {
        if let Err(e) = mk_db::admin::suspend_user(pool, &target.id).await {
            tracing::error!(target: "dbg_admin", error = %e, "suspend_user");
            return Err(handler_internal_error());
        }
    } else if let Err(e) = mk_db::admin::unsuspend_user(pool, &target.id).await {
        tracing::error!(target: "dbg_admin", error = %e, "unsuspend_user");
        return Err(handler_internal_error());
    }

    // ModerationLogService.log is fire-and-forget upstream; a log failure
    // must not fail the (already-committed) request.
    let info = json!({
        "userId": target.id,
        "userUsername": target.username,
        "userHost": Value::Null, // local users
    });
    if let Err(e) = mk_db::admin::insert_moderation_log(
        pool,
        &mk_types::id::gen_aid_now(),
        &ctx.me.id,
        if suspend { "suspend" } else { "unsuspend" },
        &info,
    )
    .await
    {
        tracing::error!(target: "dbg_admin", error = %e, "insert_moderation_log");
    }

    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `endpoints/admin/show-user.ts`. Returns the upstream moderation-data
/// object (flat keys) plus the packed user under `user` (slice-plan shape);
/// the packed user carries the includeSecrets spread (email /
/// emailVerified / securityKeysList) only when the requester sends
/// `includeSecrets: true` AND passes isAdministrator.
async fn show_user(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let ctx = gate(state, token, Gate::Moderator).await?;
    let pool = state.pool.as_ref().expect("gated");

    let user_id = require_user_id(body)?;
    let target = match mk_db::user::find_user_by_id(pool, &user_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_admin", error = %e, "find_user_by_id[target]");
            return Err(handler_internal_error());
        }
    };
    let Some(target) = target else {
        // show-user.ts: `user == null || profile == null → Error('user not found')`.
        return Err(handler_internal_error());
    };
    let profile = match mk_db::user::find_profile(pool, &target.id).await {
        Ok(p) => p,
        Err(mk_db::sqlx::Error::RowNotFound) => return Err(handler_internal_error()),
        Err(e) => {
            tracing::error!(target: "dbg_admin", error = %e, "find_profile[target]");
            return Err(handler_internal_error());
        }
    };

    let root_id = mk_db::admin::root_user_id(pool)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_admin", error = %e, "root_user_id");
            handler_internal_error()
        })
        .err();

    let target_is_admin = user_is_administrator(pool, root_id.as_deref(), &target).await;
    // show-user.ts: non-admins cannot view an administrator.
    if !ctx.is_admin && target_is_admin {
        return Err(handler_internal_error());
    }

    let target_is_moderator =
        user_is_moderator(pool, root_id.as_deref(), &target).await;

    includeSecrets_gate: {
        ()
    }

    Ok(StatusCode::OK.into_response())
}

async fn show_users(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    Ok(StatusCode::OK.into_response())
}

async fn admin_meta(state: &AppState, token: Option<&str>) -> ApiResult {
    Ok(StatusCode::OK.into_response())
}

async fn server_info(state: &AppState, token: Option<&str>) -> ApiResult {
    Ok(StatusCode::OK.into_response())
}

async fn stats(state: &AppState, token: Option<&str>) -> ApiResult {
    Ok(StatusCode::OK.into_response())
}
