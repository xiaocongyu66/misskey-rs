//! gate-roles: minimal `admin/roles/create` + `admin/roles/assign`
//! (upstream `endpoints/admin/roles/create.ts` + `assign.ts`), the pair the
//! users.ts e2e census needs to seed `isModerator`/`isAdministrator`/
//! public/badge role fixtures. Self-contained (no admin-module edits): same
//! auth-chain shapes as `api/admin.rs` (credential → suspended →
//! requireAdmin/requireModerator with the root bypass), role inserts through
//! `mk_db::roles_lite`, response packed like `RoleEntityService.pack`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

type ApiResult = Result<Response, Response>;

pub(super) fn handles(name: &str) -> bool {
    matches!(name, "admin/roles/create" | "admin/roles/assign")
}

/// Entry from the `// gate-roles` dispatch arm in mod.rs (body pre-parsed,
/// token pre-extracted — both dispatch-group conventions kept intact).
pub(super) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
) -> Response {
    let result: ApiResult = match name {
        "admin/roles/create" => create(state, body, token.as_deref()).await,
        _ => assign(state, body, token.as_deref()).await,
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

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

fn role_permission_denied(admin_gate: bool) -> Response {
    let (id, message) = if admin_gate {
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

fn meta_err(code: &str, id: &str, message: &str) -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        code,
        id,
        message,
    ))
}

fn no_such_role() -> Response {
    meta_err(
        "NO_SUCH_ROLE",
        "6503c040-6af4-4ed9-bf07-f2dd16678eab",
        "No such role.",
    )
}

fn no_such_user() -> Response {
    meta_err(
        "NO_SUCH_USER",
        "558ea170-f653-4700-94d0-5a818371d0df",
        "No such user.",
    )
}

fn access_denied() -> Response {
    meta_err(
        "ACCESS_DENIED",
        "25b5bc31-dc79-4ebd-9bd2-c84978fd052c",
        "Only administrators can edit members of the role.",
    )
}

/// Resolved credential for the admin-role gates.
struct Ctx {
    me: mk_db::user::LocalUserRow,
}

async fn gate(
    state: &AppState,
    token: Option<&str>,
    moderator_gate: bool,
) -> Result<Ctx, Response> {
    // requireCredential: missing credential entirely → CREDENTIAL_REQUIRED.
    let Some(token) = token else {
        return Err(credential_required());
    };
    let Some(pool) = &state.pool else {
        return Err(api_err(ApiError::internal_error()));
    };
    // Native 16-char tokens only (same resolution as the admin gate).
    if token.len() != 16 {
        return Err(authentication_failed());
    }
    let me_id = match mk_db::user::find_local_user_id_by_token(pool, token).await {
        Ok(Some(id)) => id,
        Ok(None) => return Err(authentication_failed()),
        Err(e) => {
            tracing::error!(target: "dbg_roles", error = %e, "find_local_user_id_by_token");
            return Err(api_err(ApiError::internal_error()));
        }
    };
    let me = match mk_db::user::find_user_by_id(pool, &me_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return Err(authentication_failed()),
        Err(e) => {
            tracing::error!(target: "dbg_roles", error = %e, "find_user_by_id");
            return Err(api_err(ApiError::internal_error()));
        }
    };
    if me.is_suspended {
        return Err(account_suspended());
    }
    // requireAdmin / requireModerator with the root bypass (ApiCallService
    // L432-442: `meta.rootUserId !== user.id` is what hands over to the
    // role checks; root satisfies both gates). The root read is NOT merely
    // the usernameLower shortcut — a suffixed first signup is root once
    // SignupService stamps meta.
    let meta_root = match mk_db::admin::root_user_id(pool).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_roles", error = %e, "root_user_id");
            return Err(api_err(ApiError::internal_error()));
        }
    };
    let is_root = me.username_lower == "root" || meta_root.as_deref() == Some(me.id.as_str());
    let assigned = match mk_db::role::user_assigned_roles(pool, &me.id).await {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(target: "dbg_roles", error = %e, "user_assigned_roles");
            return Err(api_err(ApiError::internal_error()));
        }
    };
    let ok = if moderator_gate {
        is_root || mk_core::services::role::has_moderator_role(&assigned)
    } else {
        is_root || mk_core::services::role::has_administrator_role(&assigned)
    };
    if !ok {
        return Err(role_permission_denied(!moderator_gate));
    }
    Ok(Ctx { me })
}

/// RoleEntityService.pack subset (all keys the Role schema renders; createdAt
/// parsed from the AID, updatedAt = insert stamp).
fn pack_role(created: mk_db::roles_lite::RoleRow, users_count: i64, now_iso: String) -> Value {
    json!({
        "id": created.id,
        "createdAt": mk_types::id::parse_aid_date(&created.id)
            .map(mk_core::services::signup::ms_to_iso)
            .unwrap_or_else(|| now_iso.clone()),
        "updatedAt": now_iso,
        "name": created.name,
        "description": created.description,
        "color": created.color,
        "iconUrl": created.icon_url,
        "target": created.target,
        "condFormula": created.cond_formula,
        "isPublic": created.is_public,
        "isAdministrator": created.is_administrator,
        "isModerator": created.is_moderator,
        "isExplorable": created.is_explorable,
        "asBadge": created.as_badge,
        "preserveAssignmentOnMoveAccount": created.preserve_assignment_on_move_account,
        "canEditMembersByModerator": created.can_edit_members_by_moderator,
        "displayOrder": created.display_order,
        // RoleEntityService.pack fills missing keys with the DEFAULT_POLICIES
        // entry; the e2e `role()` helper already sends every key.
        "policies": created.policies,
        "usersCount": users_count,
    })
}

async fn create(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let _ctx = gate(state, token, false).await?;
    let Some(pool) = &state.pool else {
        return Err(api_err(ApiError::internal_error()));
    };
    // paramDef required: name (the rest carry defaults in the harness call; a
    // missing name is upstream's AJV rejection — accept-with-default would
    // diverge, so keep the required-only check).
    let Some(name) = body.get("name").and_then(|v| v.as_str()) else {
        return Err(api_err(ApiError::invalid_param(json!({
            "param": "name",
            "reason": "required",
        }))));
    };
    let dstr = |k: &str| body.get(k).and_then(|v| v.as_str());
    let dbool = |k: &str, default: bool| body.get(k).and_then(|v| v.as_bool()).unwrap_or(default);
    let dnum = |k: &str, default: i64| {
        body.get(k)
            .and_then(|v| v.as_i64())
            .or_else(|| body.get(k).and_then(|v| v.as_f64()).map(|f| f as i64))
            .unwrap_or(default)
    };
    let id = mk_types::id::gen_aid_now();
    let now_iso = mk_core::services::signup::ms_to_iso(super::now_ms() as i64);
    mk_db::roles_lite::create_role(
        pool,
        &id,
        name,
        dstr("description").unwrap_or(""),
        dstr("color"),
        dstr("iconUrl"),
        dbool("isPublic", false),
        dbool("isModerator", false),
        dbool("isAdministrator", false),
        dstr("target").unwrap_or("manual"),
        body.get("condFormula").unwrap_or(&json!({})),
        dbool("asBadge", false),
        dbool("isExplorable", false),
        dbool("preserveAssignmentOnMoveAccount", false),
        dbool("canEditMembersByModerator", false),
        dnum("displayOrder", 0),
        body.get("policies").unwrap_or(&json!({})),
    )
    .await
    .map_err(|e| {
        tracing::error!(target: "dbg_roles", error = %e, "create_role");
        api_err(ApiError::internal_error())
    })?;
    let created = mk_db::roles_lite::find_role(pool, &id)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_roles", error = %e, "find_role");
            api_err(ApiError::internal_error())
        })?
        .ok_or_else(|| api_err(ApiError::internal_error()))?;
    Ok((StatusCode::OK, Json(pack_role(created, 0, now_iso))).into_response())
}

async fn assign(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let ctx = gate(state, token, true).await?;
    let Some(pool) = &state.pool else {
        return Err(api_err(ApiError::internal_error()));
    };
    let Some(role_id) = body.get("roleId").and_then(|v| v.as_str()) else {
        return Err(api_err(ApiError::invalid_param(json!({
            "param": "roleId",
            "reason": "required",
        }))));
    };
    let Some(user_id) = body.get("userId").and_then(|v| v.as_str()) else {
        return Err(api_err(ApiError::invalid_param(json!({
            "param": "userId",
            "reason": "required",
        }))));
    };
    let role = mk_db::roles_lite::find_role(pool, role_id)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_roles", error = %e, "find_role[assign]");
            api_err(ApiError::internal_error())
        })?
        .ok_or_else(no_such_role)?;
    // assign.ts: only administrators manage members unless the role opts in
    // (RoleService.isAdministrator L467 — meta.rootUserId is administrator).
    let meta_root = match mk_db::admin::root_user_id(pool).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_roles", error = %e, "root_user_id[assign]");
            return Err(api_err(ApiError::internal_error()));
        }
    };
    let is_admin =
        ctx.me.username_lower == "root" || meta_root.as_deref() == Some(ctx.me.id.as_str()) || {
            let assigned = mk_db::role::user_assigned_roles(pool, &ctx.me.id)
                .await
                .map_err(|e| {
                    tracing::error!(target: "dbg_roles", error = %e, "user_assigned_roles[assign]");
                    api_err(ApiError::internal_error())
                })?;
            mk_core::services::role::has_administrator_role(&assigned)
        };
    if !role.can_edit_members_by_moderator && !is_admin {
        return Err(access_denied());
    }
    match mk_db::user::find_user_by_id(pool, user_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return Err(no_such_user()),
        Err(e) => {
            tracing::error!(target: "dbg_roles", error = %e, "find_user_by_id[assign]");
            return Err(api_err(ApiError::internal_error()));
        }
    }
    let assign_ok = {
        match mk_db::roles_lite::assign_role(pool, &mk_types::id::gen_aid_now(), user_id, role_id)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(target: "dbg_roles", error = %e, "assign_role");
                return Err(api_err(ApiError::internal_error()));
            }
        }
    };
    if !assign_ok {
        // upstream throws AlreadyAssignedError (private → 500 shape).
        return Err(api_err(ApiError::internal_error()));
    }
    // Void result → ApiCallService.send answers 204.
    Ok(StatusCode::NO_CONTENT.into_response())
}
