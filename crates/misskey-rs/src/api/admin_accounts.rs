//! `admin/accounts/*` — ports of upstream
//! `endpoints/admin/accounts/{find-by-email,create,delete}.ts` (pinned
//! ea2c3aed).
//!
//! Gate: all three declare `requireCredential` + `requireModerator`, so the
//! shared `admin::gate(Gate::Moderator)` chain (credential → suspended →
//! ROLE_PERMISSION_DENIED with the root bypass) runs first.
//!
//! - `find-by-email`: `userProfilesRepository.findOneBy({ email })` → the
//!   packed `UserDetailedNotMe`, or `null` (→ 204) when no profile carries
//!   that address.
//! - `create`: `UserService.createUser({ username, password })` with no
//!   invitation (upstream's admin path skips the invite check). The row set
//!   mirrors the signup lane's `insert_new_user` (user + user_keypair +
//!   user_profile + used_username); the admin path does NOT stamp
//!   `meta.rootUserId` (only SignupService does that for the first signup).
//! - `delete`: `UserService.delete(user, me)` — the in-tree delete helper
//!   (`mk_db::delete_account::soft_delete`, shared with
//!   i/delete-account + admin/delete-account) flips `isDeleted` and enqueues
//!   the durable purge job; the `deleteAccount` moderation log rides along.
//!
//! DEVIATION (password-less admin create): upstream stores a NULL password
//! hash when `password` is omitted. `insert_new_user` always writes a hash,
//! so the port hashes a random throwaway secret instead — no one can sign in
//! with a guessed password, matching the observable effect.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

use super::admin::{Gate, gate};

/// Endpoint names owned by this module (all POST-only).
pub(super) fn handles(name: &str) -> bool {
    matches!(
        name,
        "admin/accounts/create" | "admin/accounts/delete" | "admin/accounts/find-by-email"
    )
}

type Check<T = Response> = Result<T, Response>;

/// Entry from the admin-accounts dispatch arm in mod.rs (body pre-parsed,
/// token pre-extracted).
pub(super) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<&str>,
) -> Response {
    let ctx = match gate(state, token, Gate::Moderator).await {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    let Some(pool) = &state.pool else {
        return internal();
    };
    let result: Check = match name {
        "admin/accounts/find-by-email" => find_by_email(state, pool, body, &ctx.me.id).await,
        "admin/accounts/create" => create(state, pool, body, &ctx.me.id).await,
        "admin/accounts/delete" => delete(pool, body, &ctx.me.id).await,
        _ => unreachable!("checked by handles()"),
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn used_username() -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "USED_USERNAME",
        "b3b6e2a9-6d5e-4b8e-8b8a-1c0e6b2f3a41",
        "Used username.",
    ))
}

fn invalid_username() -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "INVALID_USERNAME",
        "3a2f0d0e-6c1a-4b2e-9f3a-8d7c5e4b1a20",
        "Invalid username.",
    ))
}

/// `POST /api/admin/accounts/find-by-email` — paramDef `{ email: string }`
/// required. `null` result → 204 (ApiCallService.send).
async fn find_by_email(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Check {
    let Some(email) = body.get("email").and_then(Value::as_str) else {
        return Err(super::invalid_param("email", "required"));
    };
    let user_id = match mk_db::admin::find_user_id_by_email(pool, email).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(target: "dbg_admin_accounts", error = %e, "find_user_id_by_email");
            return Err(internal());
        }
    };
    let Some(user_id) = user_id else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    // `usersRepository.findOneByOrFail` — a profile whose user row vanished
    // throws upstream → 500.
    match super::freq::pack_user_detailed(state, pool, &user_id, Some(me_id)).await {
        Ok(Some(packed)) => Ok(Json(packed).into_response()),
        Ok(None) => Err(internal()),
        Err(resp) => Err(resp),
    }
}

/// `POST /api/admin/accounts/create` — paramDef `{ username: string,
/// password?: string }`. Mirrors `UserService.createUser` (no invitation) and
/// returns the packed `UserDetailedNotMe`.
async fn create(state: &AppState, pool: &mk_db::sqlx::PgPool, body: &Value, me_id: &str) -> Check {
    let Some(username) = body.get("username").and_then(Value::as_str) else {
        return Err(super::invalid_param("username", "required"));
    };
    let password = match body.get("password") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.as_str()),
        Some(_) => return Err(super::invalid_param("password", "expected a string")),
    };

    let username_lower = username.to_lowercase();
    // `usersRepository.exist({ usernameLower, host: IsNull() })` → USED_USERNAME.
    match mk_db::user::username_exists_local(pool, &username_lower).await {
        Ok(true) => return Err(used_username()),
        Ok(false) => {}
        Err(e) => {
            tracing::error!(target: "dbg_admin_accounts", error = %e, "username_exists_local");
            return Err(internal());
        }
    }
    if let Ok(true) = mk_db::user::used_username_exists(pool, &username_lower).await {
        return Err(used_username());
    }

    // No password → hash a random throwaway secret (see module docs).
    let generated;
    let password = match password {
        Some(p) => p,
        None => {
            generated = mk_types::id::gen_aid_now();
            generated.as_str()
        }
    };
    let outcome = match mk_core::services::build_signup(username, password, None, None) {
        Ok(o) => o,
        Err("INVALID_USERNAME") => return Err(invalid_username()),
        Err(code) => {
            tracing::error!(target: "dbg_admin_accounts", code = %code, "create build failure");
            return Err(internal());
        }
    };

    let new_user = mk_db::user::NewUser {
        id: &outcome.user_id,
        username: &outcome.username,
        username_lower: &outcome.username_lower,
        name: outcome.name.as_deref(),
        host: outcome.host.as_deref(),
        token: &outcome.token,
        password_hash: &outcome.password_hash,
        public_key: &outcome.public_key,
        private_key: &outcome.private_key,
    };
    if let Err(e) = mk_db::user::insert_new_user(pool, &new_user).await {
        tracing::error!(target: "dbg_admin_accounts", error = %e, "insert_new_user");
        return Err(internal());
    }

    match super::freq::pack_user_detailed(state, pool, &outcome.user_id, Some(me_id)).await {
        Ok(Some(packed)) => Ok(Json(packed).into_response()),
        Ok(None) => Err(internal()),
        Err(resp) => Err(resp),
    }
}

/// `POST /api/admin/accounts/delete` — paramDef `{ userId: misskey:id }`.
/// `UserService.delete(user, me)`: root/system-account guard, `deleteAccount`
/// moderation log, then the shared soft-delete helper + purge job. Void
/// result → 204.
async fn delete(pool: &mk_db::sqlx::PgPool, body: &Value, me_id: &str) -> Check {
    let Some(user_id) = body.get("userId").and_then(Value::as_str) else {
        return Err(super::invalid_param("userId", "required"));
    };
    let target = match mk_db::user::find_user_by_id(pool, user_id).await {
        Ok(Some(t)) => t,
        Ok(None) => return Err(internal()), // findOneByOrFail
        Err(e) => {
            tracing::error!(target: "dbg_admin_accounts", error = %e, "find_user_by_id");
            return Err(internal());
        }
    };

    // Root / system-account guard (same plain-Error → 500 shape as
    // i/delete-account + admin/delete-account).
    match mk_db::admin::root_user_id(pool).await {
        Ok(root) if root.as_deref() == Some(target.id.as_str()) => return Err(internal()),
        Ok(_) => {}
        Err(e) => {
            tracing::error!(target: "dbg_admin_accounts", error = %e, "root_user_id");
            return Err(internal());
        }
    }
    if target.username.contains('.') {
        return Err(internal());
    }

    // ModerationLogService.log(moderator, 'deleteAccount', {...}) is
    // fire-and-forget upstream.
    let info = json!({
        "userId": target.id,
        "userUsername": target.username,
        "userHost": Value::Null,
    });
    if let Err(e) = mk_db::admin::insert_moderation_log(
        pool,
        &mk_types::id::gen_aid_now(),
        me_id,
        "deleteAccount",
        &info,
    )
    .await
    {
        tracing::error!(target: "dbg_admin_accounts", error = %e, "insert_moderation_log");
    }

    if let Err(e) = mk_db::delete_account::soft_delete(pool, &target.id).await {
        tracing::error!(target: "dbg_admin_accounts", error = %e, "soft_delete");
        return Err(internal());
    }
    // Durable purge job (DeleteAccountService.createDeleteAccountJob).
    let payload = json!({ "type": "deleteAccount", "user": { "id": target.id }, "soft": true });
    if let Err(e) = mk_queue::QueueHandle::new(pool.clone())
        .enqueue("deleteAccount", &payload)
        .await
    {
        tracing::error!(target: "dbg_admin_accounts", error = %e, "deleteAccount enqueue");
    }

    Ok(StatusCode::NO_CONTENT.into_response())
}
