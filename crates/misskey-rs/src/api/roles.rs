//! roles-v2: the remainder of the `admin/roles/*` family — `show`, `list`,
//! `users`, `update`, plus `assign` (moves from roles_lite to carry
//! `expiresAt`). Upstream `src/server/api/endpoints/admin/roles/*.ts`
//! (pinned ea2c3aed) + `RoleEntityService.pack`/`RoleService.assign`.
//! Self-contained on the roles_lite pattern (own dispatch arm ahead of the
//! gate-roles arm; SQL in `mk_db::roles`, pack defaults in
//! `mk_core::services::roles_v2`). NOTE the upstream error `id`s are
//! per-endpoint — copies of the same error text carry DIFFERENT ids on
//! show/list/assign/unassign/delete/update/users, and the harness pins
//! them, so each handler owns its id set.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

type ApiResult = Result<Response, Response>;

pub(super) fn handles(name: &str) -> bool {
    matches!(
        name,
        "admin/roles/show"
            | "admin/roles/list"
            | "admin/roles/users"
            | "admin/roles/assign"
            | "admin/roles/update"
    )
}

/// Entry from the `// roles-v2` dispatch arm in mod.rs (body pre-parsed,
/// token pre-extracted — both dispatch-group conventions kept intact).
pub(super) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
) -> Response {
    let result: ApiResult = match name {
        "admin/roles/show" => show(state, body, token.as_deref()).await,
        "admin/roles/list" => list(state, token.as_deref()).await,
        "admin/roles/users" => users(state, body, token.as_deref()).await,
        "admin/roles/assign" => assign(state, body, token.as_deref()).await,
        _ => update(state, body, token.as_deref()).await,
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

/// `show.ts` meta.errors.noSuchRole
fn no_such_role_show() -> Response {
    meta_err(
        "NO_SUCH_ROLE",
        "07dc7d34-c0d8-49b7-96c6-db3ce64ee0b3",
        "No such role.",
    )
}

/// `users.ts` meta.errors.noSuchRole
fn no_such_role_users() -> Response {
    meta_err(
        "NO_SUCH_ROLE",
        "224eff5e-2488-4b18-b3e7-f50d94421648",
        "No such role.",
    )
}

/// `assign.ts` meta.errors.noSuchRole / noSuchUser / accessDenied
fn no_such_role_assign() -> Response {
    meta_err(
        "NO_SUCH_ROLE",
        "6503c040-6af4-4ed9-bf07-f2dd16678eab",
        "No such role.",
    )
}

fn no_such_user_assign() -> Response {
    meta_err(
        "NO_SUCH_USER",
        "558ea170-f653-4700-94d0-5a818371d0df",
        "No such user.",
    )
}

fn access_denied_assign() -> Response {
    meta_err(
        "ACCESS_DENIED",
        "25b5bc31-dc79-4ebd-9bd2-c84978fd052c",
        "Only administrators can edit members of the role.",
    )
}

/// `update.ts` meta.errors.noSuchRole
fn no_such_role_update() -> Response {
    meta_err(
        "NO_SUCH_ROLE",
        "cd23ef55-09ad-428a-ac61-95a45e124b32",
        "No such role.",
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
            tracing::error!(target: "dbg_roles_v2", error = %e, "find_local_user_id_by_token");
            return Err(api_err(ApiError::internal_error()));
        }
    };
    let me = match mk_db::user::find_user_by_id(pool, &me_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return Err(authentication_failed()),
        Err(e) => {
            tracing::error!(target: "dbg_roles_v2", error = %e, "find_user_by_id");
            return Err(api_err(ApiError::internal_error()));
        }
    };
    if me.is_suspended {
        return Err(account_suspended());
    }
    // requireAdmin / requireModerator with the root bypass (ApiCallService
    // L432-442; the first signup is root once SignupService stamps meta).
    let meta_root = match mk_db::admin::root_user_id(pool).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_roles_v2", error = %e, "root_user_id");
            return Err(api_err(ApiError::internal_error()));
        }
    };
    let is_root = me.username_lower == "root" || meta_root.as_deref() == Some(me.id.as_str());
    let assigned = match mk_db::role::user_assigned_roles(pool, &me.id).await {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(target: "dbg_roles_v2", error = %e, "user_assigned_roles");
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

/// RoleEntityService.pack over a fetched row (`policies` runs the
/// roles_v2 DEFAULT fill; `userId`-counted usersCount is in the row).
fn pack_role(row: mk_db::roles::RolePackRow) -> Value {
    json!({
        "id": row.id,
        "createdAt": mk_types::id::parse_aid_date(&row.id)
            .map(mk_core::services::signup::ms_to_iso)
            .unwrap_or_else(|| mk_core::services::signup::ms_to_iso(row.updated_at_ms)),
        "updatedAt": mk_core::services::signup::ms_to_iso(row.updated_at_ms),
        "name": row.name,
        "description": row.description,
        "color": row.color,
        "iconUrl": row.icon_url,
        "target": row.target,
        "condFormula": row.cond_formula,
        "isPublic": row.is_public,
        "isAdministrator": row.is_administrator,
        "isModerator": row.is_moderator,
        "isExplorable": row.is_explorable,
        "asBadge": row.as_badge,
        "preserveAssignmentOnMoveAccount": row.preserve_assignment_on_move_account,
        "canEditMembersByModerator": row.can_edit_members_by_moderator,
        "displayOrder": row.display_order,
        "policies": mk_core::services::roles_v2::pack_policies(&row.policies),
        "usersCount": row.users_count,
    })
}

/// `POST /api/admin/roles/show` (show.ts: moderator gate → role row → pack).
async fn show(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    gate(state, token, true).await?;
    let Some(pool) = &state.pool else {
        return Err(api_err(ApiError::internal_error()));
    };
    let Some(role_id) = body.get("roleId").and_then(|v| v.as_str()) else {
        return Err(api_err(ApiError::invalid_param(json!({
            "param": "roleId",
            "reason": "required",
        }))));
    };
    let row = mk_db::roles::find_role_pack(pool, role_id)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_roles_v2", error = %e, "find_role_pack");
            api_err(ApiError::internal_error())
        })?
        .ok_or_else(no_such_role_show)?;
    Ok((StatusCode::OK, Json(pack_role(row))).into_response())
}

/// `POST /api/admin/roles/list` (list.ts: moderator gate → every role,
/// `lastUsedAt` DESC → packMany).
async fn list(state: &AppState, token: Option<&str>) -> ApiResult {
    gate(state, token, true).await?;
    let Some(pool) = &state.pool else {
        return Err(api_err(ApiError::internal_error()));
    };
    let rows = mk_db::roles::list_role_packs(pool).await.map_err(|e| {
        tracing::error!(target: "dbg_roles_v2", error = %e, "list_role_packs");
        api_err(ApiError::internal_error())
    })?;
    Ok((
        StatusCode::OK,
        Json(json!(rows.into_iter().map(pack_role).collect::<Vec<_>>())),
    )
        .into_response())
}

/// `POST /api/admin/roles/users` (users.ts: moderator gate → role row
/// (224eff5e) → live assignments page (id DESC, default limit 10,
/// expired rows hidden) → UserDetailed packs; `createdAt` parses from the
/// assignment AID, `expiresAt` renders ISO or null).
async fn users(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    gate(state, token, true).await?;
    let Some(pool) = &state.pool else {
        return Err(api_err(ApiError::internal_error()));
    };
    let Some(role_id) = body.get("roleId").and_then(|v| v.as_str()) else {
        return Err(api_err(ApiError::invalid_param(json!({
            "param": "roleId",
            "reason": "required",
        }))));
    };
    mk_db::roles::find_role_pack(pool, role_id)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_roles_v2", error = %e, "find_role_pack[users]");
            api_err(ApiError::internal_error())
        })?
        .ok_or_else(no_such_role_users)?;
    let limit = body
        .get("limit")
        .and_then(|v| v.as_i64())
        .filter(|v| (1..=100).contains(v))
        .unwrap_or(10);
    let since_id = body.get("sinceId").and_then(|v| v.as_str());
    let until_id = body.get("untilId").and_then(|v| v.as_str());
    let assigns = mk_db::roles::role_assignments_page(pool, role_id, since_id, until_id, limit)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_roles_v2", error = %e, "role_assignments_page");
            api_err(ApiError::internal_error())
        })?;
    let ctx = mk_core::services::user_pack::InstanceCtx {
        url: &state.config.url,
        host: &state.config.host(),
    };
    let mut out = Vec::with_capacity(assigns.len());
    for a in assigns {
        let user_row = match mk_db::user::find_user_by_id(pool, &a.user_id).await {
            Ok(Some(r)) => r,
            Ok(None) => continue,
            Err(e) => {
                tracing::error!(target: "dbg_roles_v2", error = %e, "find_user_by_id[assign]");
                continue;
            }
        };
        let profile = match mk_db::user::find_profile(pool, &user_row.id).await {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(target: "dbg_roles_v2", error = %e, "find_profile[assign]");
                continue;
            }
        };
        let packed = mk_core::services::user_pack::pack_inline_with_roles(
            &user_row, &profile, false, None, ctx,
            false, // users/show-parity: secrets stay hidden
            None,
        );
        out.push(json!({
            "id": a.id,
            "createdAt": mk_types::id::parse_aid_date(&a.id)
                .map(mk_core::services::signup::ms_to_iso)
                .unwrap_or_default(),
            "user": packed,
            "expiresAt": a
                .expires_at_ms
                .map(mk_core::services::signup::ms_to_iso),
        }));
    }
    Ok((StatusCode::OK, Json(json!(out))).into_response())
}

/// `POST /api/admin/roles/assign` (assign.ts: role lookup (6503c040) →
/// canEditMembersByModerator/isAdministrator gate (25b5bc31) → user lookup
/// (558ea170) → `ps.expiresAt <= now` returns SILENTLY (no row) →
/// RoleService.assign; the duplicate-live case is the private
/// AlreadyAssignedError → 500 shape).
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
    let role = mk_db::roles::find_role_pack(pool, role_id)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_roles_v2", error = %e, "find_role_pack[assign]");
            api_err(ApiError::internal_error())
        })?
        .ok_or_else(no_such_role_assign)?;
    // assign.ts: only administrators manage members unless the role opts in
    let meta_root = match mk_db::admin::root_user_id(pool).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_roles_v2", error = %e, "root_user_id[assign]");
            return Err(api_err(ApiError::internal_error()));
        }
    };
    let is_admin = ctx.me.username_lower == "root"
        || meta_root.as_deref() == Some(ctx.me.id.as_str())
        || {
            let assigned = mk_db::role::user_assigned_roles(pool, &ctx.me.id)
                .await
                .map_err(|e| {
                    tracing::error!(target: "dbg_roles_v2", error = %e, "user_assigned_roles[assign]");
                    api_err(ApiError::internal_error())
                })?;
            mk_core::services::role::has_administrator_role(&assigned)
        };
    if !role.can_edit_members_by_moderator && !is_admin {
        return Err(access_denied_assign());
    }
    match mk_db::user::find_user_by_id(pool, user_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return Err(no_such_user_assign()),
        Err(e) => {
            tracing::error!(target: "dbg_roles_v2", error = %e, "find_user_by_id[assign]");
            return Err(api_err(ApiError::internal_error()));
        }
    }
    let expires_ms = match body.get("expiresAt") {
        None | Some(Value::Null) => None,
        Some(v) => match v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)) {
            Some(ms) => Some(ms),
            None => {
                return Err(api_err(ApiError::invalid_param(json!({
                    "param": "expiresAt",
                    "reason": "must be an integer",
                }))));
            }
        },
    };
    // assign.ts: `if (ps.expiresAt && ps.expiresAt <= Date.now()) return;`
    // — an already-expired requested expiry is dropped SILENTLY (204, no
    // row, no usersCount effect).
    if let Some(ms) = expires_ms {
        if ms <= super::now_ms() as i64 {
            return Ok(StatusCode::NO_CONTENT.into_response());
        }
    }
    let assign_ok = {
        match mk_db::roles::assign_role_expiring(
            pool,
            &mk_types::id::gen_aid_now(),
            user_id,
            role_id,
            expires_ms,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(target: "dbg_roles_v2", error = %e, "assign_role_expiring");
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

/// `POST /api/admin/roles/update` (update.ts: ADMIN gate — requireAdmin →
/// c3d38592 — then role lookup (cd23ef55) → RoleService.update partial
/// merge; void → 204).
async fn update(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    gate(state, token, false).await?;
    let Some(pool) = &state.pool else {
        return Err(api_err(ApiError::internal_error()));
    };
    let Some(role_id) = body.get("roleId").and_then(|v| v.as_str()) else {
        return Err(api_err(ApiError::invalid_param(json!({
            "param": "roleId",
            "reason": "required",
        }))));
    };
    mk_db::roles::find_role_pack(pool, role_id)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_roles_v2", error = %e, "find_role_pack[update]");
            api_err(ApiError::internal_error())
        })?
        .ok_or_else(no_such_role_update)?;
    let dstr = |k: &str| body.get(k).and_then(|v| v.as_str()).map(|s| s.to_string());
    let dopt = |k: &str| -> Option<Option<String>> {
        match body.get(k) {
            None => None,
            Some(Value::Null) => Some(None),
            Some(v) => v.as_str().map(|s| Some(s.to_string())),
        }
    };
    let dbool = |k: &str| body.get(k).and_then(|v| v.as_bool());
    let dnum = |k: &str| {
        body.get(k)
            .and_then(|v| v.as_i64())
            .or_else(|| body.get(k).and_then(|v| v.as_f64()).map(|f| f as i64))
    };
    let upd = mk_db::roles::RoleUpdate {
        name: dstr("name"),
        description: dstr("description"),
        color: dopt("color"),
        icon_url: dopt("iconUrl"),
        target: dstr("target"),
        cond_formula: body.get("condFormula").cloned(),
        is_public: dbool("isPublic"),
        is_moderator: dbool("isModerator"),
        is_administrator: dbool("isAdministrator"),
        is_explorable: dbool("isExplorable"),
        as_badge: dbool("asBadge"),
        preserve_assignment_on_move_account: dbool("preserveAssignmentOnMoveAccount"),
        can_edit_members_by_moderator: dbool("canEditMembersByModerator"),
        display_order: dnum("displayOrder"),
        policies: body.get("policies").cloned(),
    };
    mk_db::roles::update_role(pool, role_id, &upd)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_roles_v2", error = %e, "update_role");
            api_err(ApiError::internal_error())
        })?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
