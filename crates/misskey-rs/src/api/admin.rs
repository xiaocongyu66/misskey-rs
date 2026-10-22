//! `admin/*` endpoint family (M2 slice) — ports of
//! `endpoints/admin/{show-user,show-users,suspend-user,unsuspend-user,meta,
//! server-info}.ts` plus the `admin/stats` facade the slice plan names.
//!
//! Auth chain mirrors `ApiCallService.call()` in order: credential gate (no
//! token → 401 CREDENTIAL_REQUIRED; unresolved token → 401
//! AUTHENTICATION_FAILED) → suspended gate (403 YOUR_ACCOUNT_SUSPENDED) →
//! requireModerator/requireAdmin role gates with the root bypass (403
//! ROLE_PERMISSION_DENIED, distinct upstream ids per gate kind).
//!
//! AUTH SHORTCUT (M2, documented deviation): upstream tags the requester as
//! root via `meta."rootUserId"`, which SignupService stamps on the *first*
//! signup. Our signup route does not write that (yet), so root/admin
//! detection is `meta."rootUserId" == me.id` OR `usernameLower == 'root'`
//! (the e2e harness signs up `root` first, matching the first-user-is-root
//! semantics) OR an isAdministrator role assignment. Once signup stamps
//! `meta."rootUserId"` the username fallback goes inert.

use axum::Json;
use axum::extract::Request;
use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

use super::{
    build_relation, invalid_param, opt_bool, opt_enum, opt_nullable_str, pack_search_rows,
};

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
    // arena-api: the api.ts 管理者専用のAPIのアクセス制限 fixture —
    // `SELECT * FROM pg_indexes` behind requireAdmin + the app-token kind.
    "admin/get-index-stats",
    // admin/ad/* — upstream endpoints/admin/ad/{create,list,update,delete}.ts
    // (requireModerator; SQL in mk_db::admin).
    "admin/ad/create",
    "admin/ad/list",
    "admin/ad/update",
    "admin/ad/delete",
];

type ApiResult = Result<Response, Response>;

/// Single entry point from the mod.rs dispatch arm.
pub(super) async fn handle(state: &AppState, name: &str, req: Request) -> Response {
    // Only known names route further; everything else is the Fastify
    // catch-all → UNKNOWN_API_ENDPOINT (regardless of method).
    if !ENDPOINTS.contains(&name) {
        return api_err(ApiError::unknown_endpoint());
    }

    // ApiServerService: `request.method === 'GET' && !meta.allowGet` → 405
    // with an empty body (no admin endpoint sets allowGet).
    if req.method() != Method::POST {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }

    let headers = req.headers().clone();
    let body = match super::parse_json_body(req).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };

    // handleRequest token resolution: `Authorization: Bearer` wins over the
    // body `i`; a present-but-non-string body `i` → 400 with an empty body.
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
        "admin/suspend-user" | "admin/suspend" => {
            suspend(state, &body, token.as_deref(), true).await
        }
        "admin/unsuspend-user" | "admin/unsuspend" => {
            suspend(state, &body, token.as_deref(), false).await
        }
        "admin/show-user" => show_user(state, &body, token.as_deref()).await,
        "admin/show-users" => show_users(state, &body, token.as_deref()).await,
        "admin/meta" => admin_meta(state, token.as_deref()).await,
        "admin/server-info" => server_info(state, token.as_deref()).await,
        "admin/get-index-stats" => get_index_stats(state, token.as_deref()).await,
        "admin/stats" => stats(state, token.as_deref()).await,
        "admin/ad/create" => ad_create(state, &body, token.as_deref()).await,
        "admin/ad/list" => ad_list(state, &body, token.as_deref()).await,
        "admin/ad/update" => ad_update(state, &body, token.as_deref()).await,
        "admin/ad/delete" => ad_delete(state, &body, token.as_deref()).await,
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

/// A handler `throw new Error(...)` reaches ApiCallService.#onExecError,
/// which wraps it into the default INTERNAL_ERROR ApiError (500).
fn handler_internal_error() -> Response {
    api_err(ApiError::internal_error())
}

// --- auth gate ---

/// Authenticated admin-context user: row + privilege flag.
// ff-arena: pub(crate) so api/ff_vis.rs can run the shared requireAdmin gate
// for its admin/update-meta federation persistence arm.
pub(crate) struct AdminCtx {
    // admin-abuse: pub(crate) so the abuse endpoints can stamp assigneeId.
    pub(crate) me: mk_db::user::LocalUserRow,
    /// RoleService.isAdministrator(me) — used by show-user's admin-view gate.
    is_admin: bool,
    /// arena-api: the access_token scope list when the credential was an
    /// app/miauth token (the AuthenticateService non-native branch) — None
    /// for native user tokens (upstream `token == null` rides the isSecure
    /// path and skips the ep.meta.kind gate).
    pub(crate) token_permission: Option<Vec<String>>,
}

#[derive(Clone, Copy)]
pub(crate) enum Gate {
    /// requireModerator metas: root or moderator/administrator role.
    Moderator,
    /// requireAdmin metas: root or administrator role.
    Admin,
}

/// RoleService probe for one row: `meta."rootUserId" === id` ||
/// `usernameLower == 'root'` (M2 shortcut, see module docs) || assigned
/// isAdministrator role.
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

async fn root_uid(pool: &mk_db::sqlx::PgPool) -> Result<Option<String>, Response> {
    mk_db::admin::root_user_id(pool).await.map_err(|e| {
        tracing::error!(target: "dbg_admin", error = %e, "root_user_id");
        api_err(ApiError::internal_error())
    })
}

/// ApiCallService.call gate sequence for the requireCredential (+
/// requireModerator/requireAdmin) admin metas.
// ff-arena: pub(crate) — shared with api/ff_vis.rs (federation side-patch).
pub(crate) async fn gate(
    state: &AppState,
    token: Option<&str>,
    g: Gate,
) -> Result<AdminCtx, Response> {
    let Some(pool) = &state.pool else {
        return Err(api_err(ApiError::internal_error()));
    };

    let Some(token) = token else {
        return Err(credential_required());
    };
    // arena-api: AuthenticateService branch split — native 16-char tokens
    // match user.token; everything else resolves through the access_token
    // hash column and rides its permission list into the ep.meta.kind gate.
    let (me_id, token_permission) = if token.len() != 16 {
        match mk_db::access_token::find_by_hash(pool, token).await {
            Ok(Some(at)) => (at.user_id, Some(at.permission)),
            Ok(None) => return Err(authentication_failed()),
            Err(e) => {
                tracing::error!(target: "dbg_admin", error = %e, "access_token find_by_hash");
                return Err(api_err(ApiError::internal_error()));
            }
        }
    } else {
        let me_id = match mk_db::user::find_local_user_id_by_token(pool, token).await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(target: "dbg_admin", error = %e, "find_local_user_id_by_token");
                return Err(api_err(ApiError::internal_error()));
            }
        };
        match me_id {
            Some(v) => (v, None),
            None => return Err(authentication_failed()),
        }
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

    let root_id = root_uid(pool).await?;
    let is_root = root_id.as_deref() == Some(me.id.as_str()) || me.username_lower == "root";

    if is_root {
        // call(): the meta.rootUserId bypass skips both role gates.
        return Ok(AdminCtx {
            me,
            is_admin: true,
            token_permission,
        });
    }

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

    Ok(AdminCtx {
        me,
        is_admin: mk_core::services::role::has_administrator_role(&assigned),
        token_permission,
    })
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

fn role_value(r: &mk_db::role::AssignedRole) -> Value {
    // RoleEntityService.pack fields asserted by upstream e2e.
    json!({
        "id": r.id,
        "name": r.name,
        "color": r.color,
        "iconUrl": r.icon_url,
        "description": r.description,
        "isModerator": r.is_moderator,
        "isAdministrator": r.is_administrator,
        "displayOrder": r.display_order,
    })
}

fn ms_iso(ms: Option<i64>) -> Value {
    ms.map(mk_core::services::signup::ms_to_iso)
        .map(Value::String)
        .unwrap_or(Value::Null)
}

// --- endpoints ---

/// `endpoints/admin/suspend-user.ts` / `unsuspend-user.ts` (plus the legacy
/// `admin/suspend`/`admin/unsuspend` names → same handlers). Moderator-target
/// guard only on suspend; plain-TS-Error paths map to 500 INTERNAL_ERROR via
/// #onExecError; a void handler result is a 204 NO_CONTENT (ApiCallService.send).
async fn suspend(state: &AppState, body: &Value, token: Option<&str>, suspend: bool) -> ApiResult {
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
    // `user == null` → Error('user not found') → 500.
    let Some(target) = target else {
        return Err(handler_internal_error());
    };

    let root_id = root_uid(pool).await?;
    if suspend {
        // suspend-user.ts: `isModerator(user) → Error('cannot suspend moderator account')`
        // — includes the root user (meta.rootUserId / the M2 username shortcut).
        if user_is_moderator(pool, root_id.as_deref(), &target).await {
            return Err(handler_internal_error());
        }
        if let Err(e) = mk_db::admin::suspend_user(pool, &target.id).await {
            tracing::error!(target: "dbg_admin", error = %e, "suspend_user");
            return Err(handler_internal_error());
        }
    } else if let Err(e) = mk_db::admin::unsuspend_user(pool, &target.id).await {
        tracing::error!(target: "dbg_admin", error = %e, "unsuspend_user");
        return Err(handler_internal_error());
    }

    // ModerationLogService.log is fire-and-forget upstream — a logging
    // failure must not fail the (already applied) request.
    let info = json!({
        "userId": target.id,
        "userUsername": target.username,
        "userHost": Value::Null, // local users carry no host
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

/// `endpoints/admin/show-user.ts`. Body = the upstream moderation-data
/// object (flat keys, meta.res shapes) plus the packed user under `user`
/// (the slice plan's `{ user, ...moderation }` shape). Secrets: the
/// includeSecrets spread (email / emailVerified / securityKeysList) is added
/// to the packed user only with `includeSecrets: true` AND isAdministrator;
/// without it the flat `email`/`emailVerified` fax back null/false.
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

    let root_id = root_uid(pool).await?;
    let target_is_admin = user_is_administrator(pool, root_id.as_deref(), &target).await;
    // show-user.ts: `!isAdministrator(me) && isAdministrator(user)` → Error.
    if !ctx.is_admin && target_is_admin {
        return Err(handler_internal_error());
    }
    let target_is_moderator = user_is_moderator(pool, root_id.as_deref(), &target).await;

    // includeSecrets: pass through the gate only for administrators.
    let include_secrets = opt_bool(body, "includeSecrets")? == Some(true);
    let secrets_allowed = include_secrets && ctx.is_admin;

    let is_me = ctx.me.id == target.id;
    let relation = match build_relation(pool, Some(ctx.me.id.as_str()), &target.id, is_me).await {
        Ok(r) => r,
        Err(resp) => return Err(*resp),
    };
    let mut packed = mk_core::services::user_pack::pack_inline(
        &target,
        &profile,
        is_me,
        relation.as_ref(),
        mk_core::services::user_pack::InstanceCtx {
            url: &state.config.url,
            host: &state.config.host(),
        },
    );
    if secrets_allowed && !is_me {
        // UserEntityService.pack `opts.includeSecrets` spread.
        if let Some(obj) = packed.as_object_mut() {
            obj.insert("email".to_string(), json!(profile.email));
            obj.insert("emailVerified".to_string(), json!(profile.email_verified));
            // user_security_keys rows are an M3 read (Mfresh users have none).
            obj.insert("securityKeysList".to_string(), json!([]));
        }
    }

    // --- moderation data (endpoints/admin/show-user.ts return object) ---
    let (last_active_ms, is_hibernated) =
        match mk_db::admin::user_moderation_columns(pool, &target.id).await {
            Ok(v) => v.unwrap_or((None, false)),
            Err(e) => {
                tracing::error!(target: "dbg_admin", error = %e, "user_moderation_columns");
                return Err(handler_internal_error());
            }
        };
    let assigned_target = match mk_db::role::user_assigned_roles(pool, &target.id).await {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(target: "dbg_admin", error = %e, "user_assigned_roles[target]");
            return Err(handler_internal_error());
        }
    };
    let meta_row = mk_db::admin::fetch_meta_row_json(pool).await.map_err(|e| {
        tracing::error!(target: "dbg_admin", error = %e, "fetch_meta_row_json");
        handler_internal_error()
    })?;
    let meta_policies = meta_row
        .as_ref()
        .and_then(|r| r.get("policies"))
        .cloned()
        .unwrap_or(Value::Null);
    // RoleService.getUserPolicies = DEFAULT_POLICIES → meta.policies → roles.
    let policy = mk_core::services::role::UserPolicy::from_base_and_roles(
        &assigned_target,
        &mk_core::services::role::UserPolicy::base_with_meta(&meta_policies),
    );
    let moderation_note = mk_db::admin::moderation_note(pool, &target.id)
        .await
        .unwrap_or(None);
    let signins = mk_db::admin::user_signins(pool, &target.id)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_admin", error = %e, "user_signins");
            handler_internal_error()
        })?
        .iter()
        .map(|s| {
            json!({
                "id": s.id,
                "ip": s.ip,
                "headers": s.headers,
                "success": s.success,
            })
        })
        .collect::<Vec<_>>();
    let role_assigns: Vec<Value> = mk_db::admin::user_role_assigns(pool, &target.id)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_admin", error = %e, "user_role_assigns");
            handler_internal_error()
        })?
        .iter()
        .map(|a| {
            json!({
                // RoleService.getUserAssigns maps the AID's embedded date.
                "createdAt": ms_iso(mk_types::id::parse_aid_date(&a.id)),
                "expiresAt": ms_iso(a.expires_at_ms),
                "roleId": a.role_id,
            })
        })
        .collect();
    let roles: Vec<Value> = assigned_target.iter().map(role_value).collect();

    let resp = json!({
        "user": packed,
        // Secrets-gated flat copies (upstream sends the moderator the target's
        // email unconditionally; the slice plan gates it behind includeSecrets).
        "email": if secrets_allowed { json!(profile.email) } else { Value::Null },
        "emailVerified": if secrets_allowed { json!(profile.email_verified) } else { json!(false) },
        "followedMessage": profile.followed_message,
        "autoAcceptFollowed": profile.auto_accept_followed,
        "noCrawle": profile.no_crawle,
        "preventAiLearning": profile.prevent_ai_learning,
        "alwaysMarkNsfw": profile.always_mark_nsfw,
        "autoSensitive": profile.auto_sensitive,
        "carefulBot": profile.careful_bot,
        "injectFeaturedNote": profile.inject_featured_note,
        "receiveAnnouncementEmail": profile.receive_announcement_email,
        "mutedWords": profile.muted_words,
        "mutedInstances": profile.muted_instances,
        "notificationRecieveConfig": profile.notification_recieve_config,
        "isModerator": target_is_moderator,
        // `!policies.canPublicNote` with the M2 policy merge.
        "isSilenced": policy.is_silenced(),
        "isSuspended": target.is_suspended,
        "isHibernated": is_hibernated,
        "lastActiveDate": ms_iso(last_active_ms),
        "moderationNote": moderation_note.unwrap_or_default(),
        "signins": signins,
        "policies": serde_json::to_value(&policy).unwrap_or(json!({})),
        // RoleEntityService.packMany — empty for fresh instances (no roles).
        "roles": roles,
        "roleAssigns": role_assigns,
    });

    Ok((StatusCode::OK, Json(resp)).into_response())
}

/// `endpoints/admin/show-users.ts` — state/origin/username/hostname gates and
/// the sort switch, packing `UserDetailed` with the requesting moderator.
async fn show_users(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let ctx = gate(state, token, Gate::Moderator).await?;
    let pool = state.pool.as_ref().expect("gated");

    let limit = parse_limit(body)?;
    let offset = parse_offset(body)?;
    let sort = opt_enum(
        body,
        "sort",
        &[
            "+follower",
            "-follower",
            "+createdAt",
            "-createdAt",
            "+updatedAt",
            "-updatedAt",
            "+lastActiveDate",
            "-lastActiveDate",
        ],
    )?;
    let state_p = opt_enum(
        body,
        "state",
        &[
            "all",
            "alive",
            "available",
            "admin",
            "moderator",
            "adminOrModerator",
            "suspended",
        ],
    )?;
    let origin = opt_enum(body, "origin", &["combined", "local", "remote"])?;
    let username = opt_nullable_str(body, "username")?.flatten();
    let hostname = opt_nullable_str(body, "hostname")?.flatten();

    // Role-based states resolve id sets first; an empty set short-circuits
    // exactly like upstream's `if (adminIds.length === 0) return []`.
    let role_ids = match state_p.as_deref() {
        Some("admin") => {
            let ids = mk_db::admin::administrator_user_ids(pool)
                .await
                .map_err(db_err)?;
            if ids.is_empty() {
                return Ok((StatusCode::OK, Json(json!([]))).into_response());
            }
            Some(ids)
        }
        Some("moderator") => {
            let ids = mk_db::admin::moderator_user_ids(pool, false)
                .await
                .map_err(db_err)?;
            if ids.is_empty() {
                return Ok((StatusCode::OK, Json(json!([]))).into_response());
            }
            Some(ids)
        }
        Some("adminOrModerator") => {
            let ids = mk_db::admin::moderator_user_ids(pool, true)
                .await
                .map_err(db_err)?;
            if ids.is_empty() {
                return Ok((StatusCode::OK, Json(json!([]))).into_response());
            }
            Some(ids)
        }
        _ => None,
    };

    let q = mk_db::admin::AdminUserListQuery {
        sort: sort.as_deref(),
        state: state_p.as_deref(),
        origin: origin.as_deref(),
        username_pattern: mk_db::admin::username_prefix_pattern(username.as_deref()),
        hostname: hostname.as_deref(),
        role_ids,
        limit,
        offset,
    };
    let rows = mk_db::admin::list_users_for_admin(pool, &q)
        .await
        .map_err(db_err)?;

    let me_id = ctx.me.id.clone();
    let packed = pack_search_rows(state, pool, rows, Some(me_id.as_str()), true)
        .await
        .map_err(|r| *r)?;
    Ok((StatusCode::OK, Json(json!(packed))).into_response())
}

fn db_err(e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_admin", error = %e, "db");
    api_err(ApiError::internal_error())
}

/// `endpoints/admin/meta.ts` (requireAdmin). Composition reads the actual
/// `meta` row (to_jsonb); keys without a backing column keep the schema
/// default / null with the TODO(M3) marker, never fabricated values.
/// `endpoints/admin/get-index-stats.ts` (requireCredential + requireAdmin,
/// kind `read:admin:index-stats`) — the handler body is literally
/// `SELECT * FROM pg_indexes` mapped to `{ tablename, indexname }` records.
/// The api.ts 管理者専用のAPIのアクセス制限 matrix rides the requireAdmin
/// role gate (root alice passes, bob 403s) plus the app-token kind gate
/// (ApiCallService.call L465: an access token whose scope list misses the
/// kind → PERMISSION_DENIED, AFTER the role gates — bob's scoped app token
/// 403s ROLE_PERMISSION_DENIED before the scope check ever runs).
async fn get_index_stats(state: &AppState, token: Option<&str>) -> ApiResult {
    let ctx = gate(state, token, Gate::Admin).await?;
    // ep.meta.kind gate (call() L465): app/miauth tokens only — native
    // user tokens ride the token == null path upstream and skip this.
    if let Some(perms) = &ctx.token_permission {
        if !perms.iter().any(|p| p == "read:admin:index-stats") {
            return Err(api_err(ApiError::new(
                403,
                mk_types::api_error::ApiErrorKind::Permission,
                "PERMISSION_DENIED",
                "1370e5b7-d4eb-4566-bb1d-7748ee6a1838",
                "Your app does not have the necessary permissions to use this endpoint.",
            )));
        }
    }
    let pool = state.pool.as_ref().expect("gated");

    let rows: Vec<(String, String)> =
        mk_db::sqlx::query_as("SELECT tablename, indexname FROM pg_indexes")
            .fetch_all(pool)
            .await
            .map_err(db_err)?;
    let stats: Vec<Value> = rows
        .into_iter()
        .map(|(tablename, indexname)| json!({ "tablename": tablename, "indexname": indexname }))
        .collect();
    Ok((StatusCode::OK, Json(json!(stats))).into_response())
}

pub(crate) async fn admin_meta(state: &AppState, token: Option<&str>) -> ApiResult {
    let _ctx = gate(state, token, Gate::Admin).await?;
    let pool = state.pool.as_ref().expect("gated");

    let meta = mk_db::admin::fetch_meta_row_json(pool)
        .await
        .map_err(db_err)?;
    let col = |key: &str| -> Value {
        meta.as_ref()
            .and_then(|r| r.get(key))
            .cloned()
            .unwrap_or(Value::Null)
    };
    // Schema-typed columns with NOT NULL defaults (meta DDL) keep those
    // defaults even when no row exists yet — matches MetaService.fetch.
    let col_or = |key: &str, default: Value| -> Value {
        match &meta {
            Some(row) => match row.get(key) {
                Some(v) if !v.is_null() => v.clone(),
                _ => default,
            },
            None => default,
        }
    };

    let policies_col = col("policies");
    let policy = mk_core::services::role::UserPolicy::base_with_meta(&policies_col);
    let policies = {
        // `{ ...DEFAULT_POLICIES, ...instance.policies }` over the M2 policy
        // view (partial key set — remaining RolePolicies keys are M3).
        let mut v = serde_json::to_value(&policy).unwrap_or(json!({}));
        if let (Some(dst), Some(src)) = (v.as_object_mut(), policies_col.as_object()) {
            for (k, val) in src {
                dst.insert(k.clone(), val.clone());
            }
        }
        v
    };

    // json! handles `{ ... }` in value position as a JSON object, so Rust
    // block expressions must be computed BEFORE the macro.
    let mascot_image_url = {
        // upstream default '/assets/ai.png' (MiMeta column default)
        let v = col("mascotImageUrl");
        if v.is_null() {
            json!("/assets/ai.png")
        } else {
            v
        }
    };
    let translator_available = !col("deeplAuthKey").is_null(); // deeplAuthKey != null

    let resp = json!({
        "maintainerName": col("maintainerName"),
        "maintainerEmail": col("maintainerEmail"),
        "version": env!("CARGO_PKG_VERSION"),
        "name": col("name"),
        "shortName": col("shortName"),
        "uri": state.config.url,
        "description": col("description"),
        "langs": col_or("langs", json!([])),
        "tosUrl": col("termsOfServiceUrl"),
        "repositoryUrl": col("repositoryUrl"),
        "feedbackUrl": col("feedbackUrl"),
        "impressumUrl": col("impressumUrl"),
        "privacyPolicyUrl": col("privacyPolicyUrl"),
        "inquiryUrl": col("inquiryUrl"),
        "disableRegistration": col_or("disableRegistration", json!(true)),
        "emailRequiredForSignup": col_or("emailRequiredForSignup", json!(false)),
        "enableHcaptcha": col_or("enableHcaptcha", json!(false)),
        "hcaptchaSiteKey": col("hcaptchaSiteKey"),
        "enableMcaptcha": col_or("enableMcaptcha", json!(false)),
        "mcaptchaSiteKey": col("mcaptchaSitekey"),
        "mcaptchaInstanceUrl": col("mcaptchaInstanceUrl"),
        "enableRecaptcha": col_or("enableRecaptcha", json!(false)),
        "recaptchaSiteKey": col("recaptchaSiteKey"),
        "enableTurnstile": col_or("enableTurnstile", json!(false)),
        "turnstileSiteKey": col("turnstileSiteKey"),
        "enableTestcaptcha": col_or("enableTestcaptcha", json!(false)),
        "googleAnalyticsMeasurementId": col("googleAnalyticsMeasurementId"),
        "swPublickey": col("swPublicKey"),
        "themeColor": col("themeColor"),
        "mascotImageUrl": mascot_image_url,
        "bannerUrl": col("bannerUrl"),
        "serverErrorImageUrl": col("serverErrorImageUrl"),
        "infoImageUrl": col("infoImageUrl"),
        "notFoundImageUrl": col("notFoundImageUrl"),
        "iconUrl": col("iconUrl"),
        "app192IconUrl": col("app192IconUrl"),
        "app512IconUrl": col("app512IconUrl"),
        "backgroundImageUrl": col("backgroundImageUrl"),
        "logoImageUrl": col("logoImageUrl"),
        "defaultLightTheme": col("defaultLightTheme"),
        "defaultDarkTheme": col("defaultDarkTheme"),
        "clientOptions": col_or("clientOptions", json!({})),
        "enableEmail": col_or("enableEmail", json!(false)),
        "enableServiceWorker": col_or("enableServiceWorker", json!(false)),
        "translatorAvailable": translator_available,
        "cacheRemoteFiles": col_or("cacheRemoteFiles", json!(false)),
        "cacheRemoteSensitiveFiles": col_or("cacheRemoteSensitiveFiles", json!(false)),
        "pinnedUsers": col_or("pinnedUsers", json!([])),
        "hiddenTags": col_or("hiddenTags", json!([])),
        "blockedHosts": col_or("blockedHosts", json!([])),
        "silencedHosts": col_or("silencedHosts", json!([])),
        "mediaSilencedHosts": col_or("mediaSilencedHosts", json!([])),
        "sensitiveWords": col_or("sensitiveWords", json!([])),
        "prohibitedWords": col_or("prohibitedWords", json!([])),
        "prohibitedWordsForNameOfUser": col_or("prohibitedWordsForNameOfUser", json!([])),
        "preservedUsernames": col_or("preservedUsernames", json!([])),
        "hcaptchaSecretKey": col("hcaptchaSecretKey"),
        "mcaptchaSecretKey": col("mcaptchaSecretKey"),
        "recaptchaSecretKey": col("recaptchaSecretKey"),
        "turnstileSecretKey": col("turnstileSecretKey"),
        "sensitiveMediaDetection": col_or("sensitiveMediaDetection", json!("none")),
        "sensitiveMediaDetectionSensitivity": col_or(
            "sensitiveMediaDetectionSensitivity",
            json!("medium"),
        ),
        "setSensitiveFlagAutomatically": col_or("setSensitiveFlagAutomatically", json!(false)),
        "enableSensitiveMediaDetectionForVideos": col_or(
            "enableSensitiveMediaDetectionForVideos",
            json!(false),
        ),
        "sensitiveMediaDetectionApiUrl": col("sensitiveMediaDetectionApiUrl"),
        "sensitiveMediaDetectionApiKey": col("sensitiveMediaDetectionApiKey"),
        // TODO(M3): read these numeric meta columns instead of the upstream
        // defaults (fields exist in the schema; not surfaced in M2).
        "sensitiveMediaDetectionTimeout": 180_000,
        "sensitiveMediaDetectionMaxImagesPerRequest": 16,
        "proxyAccountId": Value::Null, // SystemAccountService.fetch('proxy') — none in M2
        "email": col("email"),
        "smtpSecure": col_or("smtpSecure", json!(false)),
        "smtpHost": col("smtpHost"),
        "smtpPort": col("smtpPort"),
        "smtpUser": col("smtpUser"),
        "smtpPass": col("smtpPass"),
        "swPrivateKey": col("swPrivateKey"),
        "useObjectStorage": col_or("useObjectStorage", json!(false)),
        "objectStorageBaseUrl": col("objectStorageBaseUrl"),
        "objectStorageBucket": col("objectStorageBucket"),
        "objectStoragePrefix": col("objectStoragePrefix"),
        "objectStorageEndpoint": col("objectStorageEndpoint"),
        "objectStorageRegion": col("objectStorageRegion"),
        "objectStoragePort": col("objectStoragePort"),
        "objectStorageAccessKey": col("objectStorageAccessKey"),
        "objectStorageSecretKey": col("objectStorageSecretKey"),
        "objectStorageUseSSL": col_or("objectStorageUseSSL", json!(true)),
        "objectStorageUseProxy": col_or("objectStorageUseProxy", json!(false)),
        "objectStorageSetPublicRead": col_or("objectStorageSetPublicRead", json!(false)),
        "deeplAuthKey": col("deeplAuthKey"),
        "deeplIsPro": col_or("deeplIsPro", json!(false)),
        "enableIpLogging": col_or("enableIpLogging", json!(false)),
        "enableActiveEmailValidation": col_or("enableActiveEmailValidation", json!(true)),
        "enableVerifymailApi": col_or("enableVerifymailApi", json!(false)),
        "verifymailAuthKey": col("verifymailAuthKey"),
        "enableTruemailApi": col_or("enableTruemailApi", json!(false)),
        "truemailInstance": col("truemailInstance"),
        "truemailAuthKey": col("truemailAuthKey"),
        "enableChartsForRemoteUser": col_or("enableChartsForRemoteUser", json!(true)),
        "enableChartsForFederatedInstances": col_or(
            "enableChartsForFederatedInstances",
            json!(true),
        ),
        "enableStatsForFederatedInstances": col_or(
            "enableStatsForFederatedInstances",
            json!(false),
        ),
        "enableServerMachineStats": col_or("enableServerMachineStats", json!(false)),
        "enableIdenticonGeneration": col_or("enableIdenticonGeneration", json!(true)),
        "bannedEmailDomains": col_or("bannedEmailDomains", json!([])),
        "policies": policies,
        "manifestJsonOverride": col_or("manifestJsonOverride", json!("{}")),
        "enableFanoutTimeline": col_or("enableFanoutTimeline", json!(true)),
        "enableFanoutTimelineDbFallback": col_or("enableFanoutTimelineDbFallback", json!(true)),
        // numeric cache knobs: schema columns exist; upstream defaults until the
        // read lands with the timeline work.
        "perLocalUserUserTimelineCacheMax": 300,
        "perRemoteUserUserTimelineCacheMax": 300,
        "perUserHomeTimelineCacheMax": 300,
        "perUserListTimelineCacheMax": 300,
        "enableReactionsBuffering": col_or("enableReactionsBuffering", json!(false)),
        "notesPerOneAd": col_or("notesPerOneAd", json!(20)),
        "summalyProxy": col("urlPreviewSummaryProxyUrl"),
        "urlPreviewEnabled": col_or("urlPreviewEnabled", json!(true)),
        "urlPreviewAllowRedirect": col_or("urlPreviewAllowRedirect", json!(true)),
        "urlPreviewTimeout": col_or("urlPreviewTimeout", json!(10_000)),
        "urlPreviewMaximumContentLength": col_or(
            "urlPreviewMaximumContentLength",
            json!(10_485_760),
        ),
        "urlPreviewRequireContentLength": col_or(
            "urlPreviewRequireContentLength",
            json!(false),
        ),
        "urlPreviewUserAgent": col("urlPreviewUserAgent"),
        "urlPreviewSummaryProxyUrl": col("urlPreviewSummaryProxyUrl"),
        "urlPreviewSensitiveList": col_or("urlPreviewSensitiveList", json!([])),
        "federation": col_or("federation", json!("none")),
        "federationHosts": col_or("federationHosts", json!([])),
        "deliverSuspendedSoftware": col_or("deliverSuspendedSoftware", json!([])),
        "singleUserMode": col_or("singleUserMode", json!(false)),
        // schema default 'local'
        "ugcVisibilityForVisitor": col_or("ugcVisibilityForVisitor", json!("local")),
        "proxyRemoteFiles": col_or("proxyRemoteFiles", json!(false)),
        "signToActivityPubGet": col_or("signToActivityPubGet", json!(true)),
        "allowExternalApRedirect": col_or("allowExternalApRedirect", json!(true)),
        "enableRemoteNotesCleaning": col_or("enableRemoteNotesCleaning", json!(false)),
        "remoteNotesCleaningExpiryDaysForEachNotes": col_or(
            "remoteNotesCleaningExpiryDaysForEachNotes",
            json!(30),
        ),
        "remoteNotesCleaningMaxProcessingDurationInMinutes": col_or(
            "remoteNotesCleaningMaxProcessingDurationInMinutes",
            json!(30),
        ),
        "showRoleBadgesOfRemoteUsers": col_or("showRoleBadgesOfRemoteUsers", json!(false)),
    });

    Ok((StatusCode::OK, Json(resp)).into_response())
}

/// `endpoints/admin/server-info.ts` (requireModerator). Honest reads: psql
/// version from the DB, machine/hostname + default iface from /proc, memory
/// from /proc/meminfo; the systeminformation fs fields stay 0 with a marker
/// until a statvfs port lands.
async fn server_info(state: &AppState, token: Option<&str>) -> ApiResult {
    let _ctx = gate(state, token, Gate::Moderator).await?;
    let pool = state.pool.as_ref().expect("gated");

    let psql = mk_db::admin::pg_server_version(pool)
        .await
        .map_err(db_err)?;
    let machine = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "misskey-rs".to_string());
    let net_interface = std::fs::read_to_string("/proc/net/route")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.split_whitespace().nth(1) == Some("00000000"))
                .and_then(|l| l.split_whitespace().next().map(str::to_string))
        })
        .unwrap_or_else(|| "eth0".to_string());
    // /proc/meminfo `MemTotal:  N kB` → bytes.
    let mem_total = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix("MemTotal:")
                    .and_then(|rest| {
                        rest.trim()
                            .split_ascii_whitespace()
                            .next()?
                            .parse::<i64>()
                            .ok()
                    })
                    .map(|kb| kb * 1024)
            })
        })
        .unwrap_or(0);

    let resp = json!({
        "machine": machine,
        "os": std::env::consts::OS,
        "node": "misskey-rs", // process.version stand-in for the Rust runtime
        "psql": psql,
        "redis": Value::Null, // no redis client wired in M2
        "cpu": {
            "model": Value::Null,
            "cores": std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1),
        },
        "mem": { "total": mem_total },
        // TODO(M3): statvfs port for the fs probe (0 bytes for now).
        "fs": { "total": 0, "used": 0 },
        "net": { "interface": net_interface },
    });
    Ok((StatusCode::OK, Json(resp)).into_response())
}

/// `admin/stats` — not in the upstream endpoint list at ea2c3aed; named by
/// the slice plan as the admin-gated mirror of the public `stats` shape with
/// honest row counts from the tables we have.
async fn stats(state: &AppState, token: Option<&str>) -> ApiResult {
    let _ctx = gate(state, token, Gate::Moderator).await?;
    let pool = state.pool.as_ref().expect("gated");

    let users = mk_db::admin::count_users(pool, false)
        .await
        .map_err(db_err)?;
    let original_users = mk_db::admin::count_users(pool, true)
        .await
        .map_err(db_err)?;
    let notes = mk_db::admin::count_notes(pool, false)
        .await
        .map_err(db_err)?;
    let original_notes = mk_db::admin::count_notes(pool, true)
        .await
        .map_err(db_err)?;
    let instances = mk_db::admin::count_instances(pool).await.map_err(db_err)?;

    let resp = json!({
        "notesCount": notes,
        "originalNotesCount": original_notes,
        "usersCount": users,
        "originalUsersCount": original_users,
        "reactionsCount": 0, // no reaction aggregate source in M2
        "instances": instances,
        "driveUsageLocal": 0, // drive tables are unwired (M3)
        "driveUsageRemote": 0,
    });
    Ok((StatusCode::OK, Json(resp)).into_response())
}

// --- admin/ad/* (upstream endpoints/admin/ad/*.ts, requireModerator) ---

/// `update.ts` meta.errors.noSuchAd
fn no_such_ad_update() -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "NO_SUCH_AD",
        "b7aa1727-1354-47bc-a182-3a9c3973d300",
        "No such ad.",
    ))
}

/// `delete.ts` meta.errors.noSuchAd (a distinct id from update's)
fn no_such_ad_delete() -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "NO_SUCH_AD",
        "ccac9863-3a03-416e-b899-8a64041118b1",
        "No such ad.",
    ))
}

/// Required string param (ajv `required` + optional minLength 1).
fn ad_required_str(body: &Value, key: &str, non_empty: bool) -> Result<String, Response> {
    match body.get(key) {
        Some(Value::String(s)) if !non_empty || !s.is_empty() => Ok(s.clone()),
        Some(Value::String(_)) => Err(invalid_param(key, "must not be empty")),
        None | Some(Value::Null) => Err(invalid_param(
            key,
            &format!("must have required property '{key}'"),
        )),
        Some(_) => Err(invalid_param(key, "must be string")),
    }
}

/// Required integer param (`{ type: 'integer' }`).
fn ad_required_int(body: &Value, key: &str) -> Result<i64, Response> {
    match body.get(key) {
        Some(Value::Number(n)) => n
            .as_i64()
            .ok_or_else(|| invalid_param(key, "must be integer")),
        None | Some(Value::Null) => Err(invalid_param(
            key,
            &format!("must have required property '{key}'"),
        )),
        Some(_) => Err(invalid_param(key, "must be integer")),
    }
}

/// Optional integer param (absent/null → skip).
fn ad_opt_int(body: &Value, key: &str) -> Result<Option<i64>, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n
            .as_i64()
            .map(Some)
            .ok_or_else(|| invalid_param(key, "must be integer")),
        Some(_) => Err(invalid_param(key, "must be integer")),
    }
}

/// Optional nullable boolean (`{ type: 'boolean', nullable: true }`).
fn ad_opt_nullable_bool(body: &Value, key: &str) -> Result<Option<bool>, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(invalid_param(key, "must be boolean")),
    }
}

/// Optional string param (absent/null → skip).
fn ad_opt_str(body: &Value, key: &str, non_empty: bool) -> Result<Option<String>, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !non_empty || !s.is_empty() => Ok(Some(s.clone())),
        Some(Value::String(_)) => Err(invalid_param(key, "must not be empty")),
        Some(_) => Err(invalid_param(key, "must be string")),
    }
}

/// Ad response object (identical shape in create/list/update).
fn pack_ad(row: &mk_db::admin::AdRow) -> Value {
    json!({
        "id": row.id,
        "expiresAt": mk_core::services::signup::ms_to_iso(row.expires_at_ms),
        "startsAt": mk_core::services::signup::ms_to_iso(row.starts_at_ms),
        "dayOfWeek": row.day_of_week,
        "isSensitive": row.is_sensitive,
        "url": row.url,
        "imageUrl": row.image_url,
        "priority": row.priority,
        "ratio": row.ratio,
        "place": row.place,
        "memo": row.memo,
    })
}

/// ModerationLogService.log is fire-and-forget upstream — a logging failure
/// must not fail the (already applied) request.
async fn log_ad_moderation(
    pool: &mk_db::sqlx::PgPool,
    moderator_id: &str,
    kind: &str,
    info: Value,
) {
    if let Err(e) = mk_db::admin::insert_moderation_log(
        pool,
        &mk_types::id::gen_aid_now(),
        moderator_id,
        kind,
        &info,
    )
    .await
    {
        tracing::error!(target: "dbg_admin", error = %e, "insert_moderation_log[{kind}]");
    }
}

/// `POST /api/admin/ad/create` — requireModerator; inserts the ad and
/// returns the packed row (moderation log `createAd`).
async fn ad_create(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let ctx = gate(state, token, Gate::Moderator).await?;
    let pool = state.pool.as_ref().expect("gated");

    let url = ad_required_str(body, "url", true)?;
    let memo = ad_required_str(body, "memo", false)?;
    let place = ad_required_str(body, "place", false)?;
    let priority = ad_required_str(body, "priority", false)?;
    let ratio = ad_required_int(body, "ratio")?;
    let expires_at_ms = ad_required_int(body, "expiresAt")?;
    let starts_at_ms = ad_required_int(body, "startsAt")?;
    let image_url = ad_required_str(body, "imageUrl", true)?;
    let day_of_week = ad_required_int(body, "dayOfWeek")?;
    let is_sensitive = opt_bool(body, "isSensitive")?.unwrap_or(false);

    let row = mk_db::admin::insert_ad(
        pool,
        &mk_db::admin::NewAd {
            id: mk_types::id::gen_aid_now(),
            url,
            image_url,
            place,
            priority,
            ratio: ratio as i32,
            memo,
            is_sensitive,
            day_of_week: day_of_week as i32,
            expires_at_ms,
            starts_at_ms,
        },
    )
    .await
    .map_err(db_err)?;

    let ad = pack_ad(&row);
    log_ad_moderation(
        pool,
        &ctx.me.id,
        "createAd",
        json!({ "adId": row.id, "ad": ad.clone() }),
    )
    .await;
    Ok((StatusCode::OK, Json(ad)).into_response())
}

/// `POST /api/admin/ad/list` — requireModerator; `makePaginationQuery`
/// (sinceId/untilId then sinceDate/untilDate, AID ids) + optional publishing
/// window, `limit` default 10.
async fn ad_list(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let _ctx = gate(state, token, Gate::Moderator).await?;
    let pool = state.pool.as_ref().expect("gated");

    let limit = parse_limit(body)?;
    let publishing = ad_opt_nullable_bool(body, "publishing")?;

    let since_id = ad_opt_str(body, "sinceId", false)?.filter(|s| !s.is_empty());
    let until_id = ad_opt_str(body, "untilId", false)?.filter(|s| !s.is_empty());
    let since_date = ad_opt_int(body, "sinceDate")?
        .filter(|ms| *ms != 0)
        .map(mk_types::id::gen_aid);
    let until_date = ad_opt_int(body, "untilDate")?
        .filter(|ms| *ms != 0)
        .map(mk_types::id::gen_aid);
    // QueryService.makePaginationQuery precedence: id cursors win, dates are
    // only consulted when both id cursors are absent.
    let (since_id, until_id) = match (since_id, until_id) {
        (Some(s), Some(u)) => (Some(s), Some(u)),
        (Some(s), None) => (Some(s), None),
        (None, Some(u)) => (None, Some(u)),
        (None, None) => (since_date, until_date),
    };

    let rows = mk_db::admin::list_ads(
        pool,
        &mk_db::admin::AdListQuery {
            since_id: since_id.as_deref(),
            until_id: until_id.as_deref(),
            publishing,
            limit,
        },
    )
    .await
    .map_err(db_err)?;
    Ok((
        StatusCode::OK,
        Json(json!(rows.iter().map(pack_ad).collect::<Vec<_>>())),
    )
        .into_response())
}

/// `POST /api/admin/ad/update` — requireModerator; role lookup →
/// NO_SUCH_AD (b7aa1727) → partial update (void → 204) + `updateAd` log.
async fn ad_update(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let ctx = gate(state, token, Gate::Moderator).await?;
    let pool = state.pool.as_ref().expect("gated");

    let id = ad_required_str(body, "id", true)?;
    let before = mk_db::admin::find_ad(pool, &id)
        .await
        .map_err(db_err)?
        .ok_or_else(no_such_ad_update)?;

    let upd = mk_db::admin::AdUpdate {
        url: ad_opt_str(body, "url", true)?,
        image_url: ad_opt_str(body, "imageUrl", true)?,
        place: ad_opt_str(body, "place", false)?,
        priority: ad_opt_str(body, "priority", false)?,
        ratio: ad_opt_int(body, "ratio")?.map(|v| v as i32),
        memo: ad_opt_str(body, "memo", false)?,
        // `ps.expiresAt ? new Date(...) : undefined` — a zero is skipped.
        expires_at_ms: ad_opt_int(body, "expiresAt")?.filter(|ms| *ms != 0),
        starts_at_ms: ad_opt_int(body, "startsAt")?.filter(|ms| *ms != 0),
        day_of_week: ad_opt_int(body, "dayOfWeek")?.map(|v| v as i32),
        is_sensitive: opt_bool(body, "isSensitive")?,
    };
    mk_db::admin::update_ad(pool, &id, &upd)
        .await
        .map_err(db_err)?;

    let after = mk_db::admin::find_ad(pool, &id)
        .await
        .map_err(db_err)?
        .ok_or_else(no_such_ad_update)?;
    log_ad_moderation(
        pool,
        &ctx.me.id,
        "updateAd",
        json!({
            "adId": id,
            "before": pack_ad(&before),
            "after": pack_ad(&after),
        }),
    )
    .await;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `POST /api/admin/ad/delete` — requireModerator; role lookup →
/// NO_SUCH_AD (ccac9863) → delete (void → 204) + `deleteAd` log.
async fn ad_delete(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let ctx = gate(state, token, Gate::Moderator).await?;
    let pool = state.pool.as_ref().expect("gated");

    let id = ad_required_str(body, "id", true)?;
    let ad = mk_db::admin::find_ad(pool, &id)
        .await
        .map_err(db_err)?
        .ok_or_else(no_such_ad_delete)?;
    mk_db::admin::delete_ad(pool, &id).await.map_err(db_err)?;
    log_ad_moderation(
        pool,
        &ctx.me.id,
        "deleteAd",
        json!({ "adId": id, "ad": pack_ad(&ad) }),
    )
    .await;
    Ok(StatusCode::NO_CONTENT.into_response())
}
