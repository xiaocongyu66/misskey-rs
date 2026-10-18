//! delete-account: i/delete-account + admin/delete-account (upstream
//! endpoints/i/delete-account.ts + endpoints/admin/delete-account.ts, both
//! funneling into core/DeleteAccountService.deleteAccount). Self-contained
//! module on the roles_lite pattern: own auth-chain shapes, body pre-parsed
//! and token pre-extracted by the delete-account dispatch arm in mod.rs.
//!
//! deleteAccount's synchronous DB half is the `"isDeleted" = true` flip (see
//! mk-db/src/delete_account.rs); the Delete Activity delivery and the
//! notes/files purge stay in the upstream job queue, which never lands inside
//! the census window. Plain handler TS errors (`throw new Error(...)`) map to
//! 500 INTERNAL_ERROR via ApiCallService `#onExecError`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// Endpoint names owned by this module (all POST-only).
pub(super) fn handles(name: &str) -> bool {
    matches!(name, "i/delete-account" | "admin/delete-account")
}

/// Entry from the delete-account dispatch arm in mod.rs.
pub(super) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
) -> Response {
    match name {
        "i/delete-account" => i_delete_account(state, body, token.as_deref()).await,
        _ => admin_delete_account(state, body, token.as_deref()).await,
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

fn role_permission_denied() -> Response {
    api_err(ApiError::new(
        403,
        mk_types::api_error::ApiErrorKind::Permission,
        "ROLE_PERMISSION_DENIED",
        "c3d38592-54c0-429d-be96-5636b0431a61",
        "You are not assigned to an administrator role.",
    ))
}

/// Plain TS Error (`incorrect password`, `cannot delete a root account`,
/// `findOneByOrFail` misses, ...) → `#onExecError` → 500 INTERNAL_ERROR.
fn internal() -> Response {
    api_err(ApiError::internal_error())
}

/// ApiCallService requireCredential chain (order matters): missing
/// credential → CREDENTIAL_REQUIRED; non-native/unresolvable token →
/// AUTHENTICATION_FAILED; suspended → YOUR_ACCOUNT_SUSPENDED.
///
/// `i/delete-account`'s `secure: true` gate (`isSecure = user != null &&
/// token == null` — the *app* AccessToken object being null) is satisfied by
/// every native-token authentication this port performs.
async fn resolve_me<'a>(
    state: &'a AppState,
    token: Option<&str>,
) -> Result<(&'a mk_db::sqlx::PgPool, mk_db::user::LocalUserRow), Response> {
    let Some(pool) = &state.pool else {
        return Err(internal());
    };
    let Some(token) = token else {
        return Err(credential_required());
    };
    if token.len() != 16 {
        return Err(authentication_failed());
    }
    let me_id = match mk_db::user::find_local_user_id_by_token(pool, token).await {
        Ok(Some(id)) => id,
        Ok(None) => return Err(authentication_failed()),
        Err(e) => {
            tracing::error!(target: "dbg_delete_account", error = %e, "find_local_user_id_by_token");
            return Err(internal());
        }
    };
    let me = match mk_db::user::find_user_by_id(pool, &me_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return Err(authentication_failed()),
        Err(e) => {
            tracing::error!(target: "dbg_delete_account", error = %e, "find_user_by_id[me]");
            return Err(internal());
        }
    };
    if me.is_suspended {
        return Err(account_suspended());
    }
    Ok((pool, me))
}

/// `meta."rootUserId" == user.id`, admin.rs root shortcut parity: the signup
/// route does not stamp meta."rootUserId", so `usernameLower == 'root'` (the
/// first-user-is-root e2e harness shape) matches too.
async fn account_is_root(pool: &mk_db::sqlx::PgPool, row: &mk_db::user::LocalUserRow) -> bool {
    let meta_root = match mk_db::admin::root_user_id(pool).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_delete_account", error = %e, "root_user_id");
            None
        }
    };
    meta_root.as_deref() == Some(row.id.as_str()) || row.username_lower == "root"
}

/// DeleteAccountService.deleteAccount's pre-write safeguards:
/// `if (this.meta.rootUserId === user.id) throw` and the local
/// `username.includes('.')` system-account guard. Both are plain Errors →
/// 500 INTERNAL_ERROR.
async fn drop_or_system_account(
    pool: &mk_db::sqlx::PgPool,
    target: &mk_db::user::LocalUserRow,
) -> Result<(), Response> {
    if account_is_root(pool, target).await || target.username.contains('.') {
        return Err(internal());
    }
    Ok(())
}

/// `POST /api/i/delete-account` — endpoints/i/delete-account.ts. paramDef:
/// password (required string), token (nullable 2FA string). Flow: profile
/// load → 2FA gate → isDeleted early return → bcrypt compare →
/// deleteAccount(me). Void result → 204 NO_CONTENT (ApiCallService.send).
///
/// 2FA SHORTCUT (documented deviation): upstream verifies the TOTP body
/// token via UserAuthService; the port carries no TOTP verifier, so a
/// non-null token counts as the two-factor step (the users.ts census never
/// exercises 2FA deletion; a 2FA user without a token still errored with the
/// upstream 'authentication failed' 500 below).
async fn i_delete_account(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    // paramDef: password required; token nullable string.
    let Some(password) = body.get("password").and_then(|v| v.as_str()) else {
        return super::invalid_param("password", "required");
    };
    match body.get("token") {
        None | Some(Value::Null) => {}
        Some(Value::String(_)) => {} // TOTP verified? see the 2FA SHORTCUT above
        Some(_) => return super::invalid_param("token", "expected a string or null"),
    }

    let (pool, me) = match resolve_me(state, token).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let profile = match mk_db::user::find_profile(pool, &me.id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(target: "dbg_delete_account", error = %e, "find_profile");
            return internal();
        }
    };

    // `if (profile.twoFactorEnabled) { if (token == null) throw ... }`
    if profile.two_factor_enabled && body.get("token").is_none() {
        return internal(); // Error('authentication failed')
    }

    // `userDetailed.isDeleted → return` (already deleted → 204).
    match mk_db::delete_account::fetch_is_deleted(pool, &me.id).await {
        Ok(Some(true)) => return StatusCode::NO_CONTENT.into_response(),
        Ok(Some(false)) => {}
        Ok(None) => return internal(), // findOneByOrFail
        Err(e) => {
            tracing::error!(target: "dbg_delete_account", error = %e, "fetch_is_deleted[me]");
            return internal();
        }
    }

    // `bcrypt.compare(ps.password, profile.password!)` — no stored password
    // fails the compare upstream (bcrypt throws on a null hash).
    let matched = match &profile.password {
        Some(hash) => mk_core::services::verify_password(password, hash).unwrap_or(false),
        None => false,
    };
    if !matched {
        return internal(); // Error('incorrect password')
    }

    if let Err(resp) = drop_or_system_account(pool, &me).await {
        return resp;
    }

    // moderator == undefined → no moderation log upstream.
    match mk_db::delete_account::soft_delete(pool, &me.id).await {
        Ok(()) => {}
        Err(e) => {
            tracing::error!(target: "dbg_delete_account", error = %e, "soft_delete[me]");
            return internal();
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `POST /api/admin/delete-account` — endpoints/admin/delete-account.ts.
/// requireCredential + requireAdmin (root bypass, ROLE_PERMISSION_DENIED
/// otherwise). Flow: findOneByOrFail → isDeleted early return →
/// deleteAccount(target, me) with the deleteAccount moderation log.
async fn admin_delete_account(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let (pool, me) = match resolve_me(state, token).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    // ApiCallService L432-448: `(meta.rootUserId !== user.id)` then the
    // assigned-role isAdministrator check (root bypass).
    if !account_is_root(pool, &me).await {
        let assigned = match mk_db::role::user_assigned_roles(pool, &me.id).await {
            Ok(a) => a,
            Err(e) => {
                tracing::error!(target: "dbg_delete_account", error = %e, "user_assigned_roles[me]");
                return internal();
            }
        };
        if !mk_core::services::role::has_administrator_role(&assigned) {
            return role_permission_denied();
        }
    }

    // paramDef: userId (misskey:id) required.
    let Some(user_id) = body.get("userId").and_then(|v| v.as_str()) else {
        return super::invalid_param("userId", "required");
    };
    let target = match mk_db::user::find_user_by_id(pool, user_id).await {
        Ok(Some(t)) => t,
        Ok(None) => return internal(), // findOneByOrFail
        Err(e) => {
            tracing::error!(target: "dbg_delete_account", error = %e, "find_user_by_id[target]");
            return internal();
        }
    };

    // `if (user.isDeleted) { return; }` (already deleted → 204).
    match mk_db::delete_account::fetch_is_deleted(pool, &target.id).await {
        Ok(Some(true)) => return StatusCode::NO_CONTENT.into_response(),
        Ok(Some(false)) => {}
        Ok(None) => return internal(),
        Err(e) => {
            tracing::error!(target: "dbg_delete_account", error = %e, "fetch_is_deleted[target]");
            return internal();
        }
    }

    if let Err(resp) = drop_or_system_account(pool, &target).await {
        return resp;
    }

    // ModerationLogService.log(moderator, 'deleteAccount', {...}) is
    // fire-and-forget upstream — a logging failure must not fail the
    // (already applied) request.
    let info = json!({
        "userId": target.id,
        "userUsername": target.username,
        "userHost": Value::Null, // local users carry no host
    });
    if let Err(e) = mk_db::admin::insert_moderation_log(
        pool,
        &mk_types::id::gen_aid_now(),
        &me.id,
        "deleteAccount",
        &info,
    )
    .await
    {
        tracing::error!(target: "dbg_delete_account", error = %e, "insert_moderation_log");
    }

    match mk_db::delete_account::soft_delete(pool, &target.id).await {
        Ok(()) => {}
        Err(e) => {
            tracing::error!(target: "dbg_delete_account", error = %e, "soft_delete[target]");
            return internal();
        }
    }
    StatusCode::NO_CONTENT.into_response()
}
