//! `admin/federation/*` + `admin/reset-password` — ports of upstream
//! `endpoints/admin/federation/{update-instance,remove-all-following,
//! delete-all-files,refresh-remote-instance-metadata}.ts` and
//! `endpoints/admin/reset-password.ts`.
//!
//! Gate: upstream all five declare `requireCredential` + `requireAdmin`
//! (root bypass). The shared `admin::gate(Gate::Admin)` runs the same
//! credential → suspended → administrator chain as the other admin arms, so a
//! plain user gets ROLE_PERMISSION_DENIED (c3d38592-…) and an unresolved
//! token AUTHENTICATION_FAILED.
//!
//! `admin/delete-account` is deliberately *not* here: it already lives in
//! `api::delete_account` (upstream admin/delete-account.ts funnels into
//! DeleteAccountService, the same helper as i/delete-account) and its arm
//! noses in ahead of the admin/* catch-all.
//!
//! DOCUMENTED SKIP — refresh-remote-instance-metadata: upstream
//! `InstanceService.refreshMetadata` performs a nodeinfo/webfinger fetch over
//! the AP HTTP lane and writes the instance row. This tree's mk-ap lane is an
//! inbound fetch helper without an instance-metadata writer, so the handler
//! is a best-effort no-op after the upstream existence check (a missing row
//! is upstream's plain `throw new Error('no such instance')` →
//! 500 INTERNAL_ERROR); success is void → 204 NO_CONTENT.
//!
//! Plain TS errors (`throw new Error(...)`) map to 500 INTERNAL_ERROR via
//! ApiCallService `#onExecError`, matching the other admin arms.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::AppState;
use crate::error::{ApiError, api_err};

use super::admin::{Gate, gate};

/// Endpoint names owned by this module (all POST-only).
pub(super) fn handles(name: &str) -> bool {
    matches!(
        name,
        "admin/federation/update-instance"
            | "admin/federation/remove-all-following"
            | "admin/federation/delete-all-files"
            | "admin/federation/refresh-remote-instance-metadata"
            | "admin/reset-password"
    )
}

type Check<T = Response> = Result<T, Response>;

/// Entry from the admin-fedops dispatch arm in mod.rs (body pre-parsed,
/// token pre-extracted).
pub(super) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<&str>,
) -> Response {
    // Upstream requireCredential + requireAdmin on every endpoint in this
    // family (admin/federation/*.ts + admin/reset-password.ts).
    if let Err(resp) = gate(state, token, Gate::Admin).await {
        return resp;
    }
    let Some(pool) = &state.pool else {
        return internal();
    };
    let result: Check<Response> = match name {
        "admin/federation/update-instance" => update_instance(pool, body).await,
        "admin/federation/remove-all-following" => remove_all_following(pool, body).await,
        "admin/federation/delete-all-files" => delete_all_files(pool, body).await,
        "admin/federation/refresh-remote-instance-metadata" => {
            refresh_remote_instance_metadata(pool, body).await
        }
        _ => reset_password(pool, body).await,
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_admin_fedops", error = %e, "db error: {context}");
    internal()
}

/// paramDef `{ host: { type: 'string' }, required }` shared by all four
/// federation endpoints.
fn require_host(body: &Value) -> Check<String> {
    match body.get("host") {
        None | Some(Value::Null) => Err(super::invalid_param(
            "host",
            "must have required property 'host'",
        )),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(super::invalid_param("host", "must be string")),
    }
}

/// `admin/federation/update-instance.ts` — find the instance row (missing is
/// upstream's plain Error → 500), flip `suspensionState` when `isSuspended`
/// differs from the current state, then persist a non-null
/// `moderationNote` (`ps.moderationNote ?? instance.moderationNote`).
///
/// The pinned upstream enum spells the manual state `manuallySuspended` (no
/// bare `suspended` value); "currently suspended" is therefore any non-`none`
/// state, and the flip writes `manuallySuspended` / `none` exactly like
/// `InstanceService.suspend`/`unsuspend`.
async fn update_instance(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let host = require_host(body)?;

    // paramDef: isSuspended nullable boolean, moderationNote nullable string.
    let is_suspended = match body.get("isSuspended") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => {
            return Err(super::invalid_param(
                "isSuspended",
                "must be boolean or null",
            ));
        }
    };
    let moderation_note = match body.get("moderationNote") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            return Err(super::invalid_param(
                "moderationNote",
                "must be string or null",
            ));
        }
    };

    let instance = match mk_db::admin::find_instance_for_federation(pool, &host).await {
        Ok(Some(row)) => row,
        Ok(None) => return Err(internal()), // throw new Error('no such instance')
        Err(e) => return Err(db_err("find_instance_for_federation", e)),
    };

    if let Some(suspended) = is_suspended {
        if suspended != (instance.suspension_state != "none") {
            if let Err(e) =
                mk_db::admin::set_instance_suspension(pool, &instance.id, suspended).await
            {
                return Err(db_err("set_instance_suspension", e));
            }
        }
    }

    if let Some(note) = moderation_note {
        if let Err(e) = mk_db::admin::set_instance_moderation_note(pool, &instance.id, &note).await
        {
            return Err(db_err("set_instance_moderation_note", e));
        }
    }

    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `admin/federation/remove-all-following.ts` — deletes the host's edges in
/// both directions (`followerHost` then `followeeHost`), exactly the two
/// upstream `followingsRepository.delete` calls.
async fn remove_all_following(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let host = require_host(body)?;
    if let Err(e) = mk_db::admin::delete_following_by_host(pool, &host).await {
        return Err(db_err("delete_following_by_host", e));
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `admin/federation/delete-all-files.ts` — `driveFilesRepository.delete({
/// userHost: ps.host })`: every remote drive file owned by that host (the
/// `userHost` column is NULL for local owners, so locals never match).
async fn delete_all_files(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let host = require_host(body)?;
    if let Err(e) = mk_db::admin::delete_drive_files_by_host(pool, &host).await {
        return Err(db_err("delete_drive_files_by_host", e));
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `admin/federation/refresh-remote-instance-metadata.ts` — see the module
/// DOCUMENTED SKIP: existence check only, then a no-op 204.
async fn refresh_remote_instance_metadata(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
) -> Check<Response> {
    let host = require_host(body)?;
    match mk_db::admin::find_instance_for_federation(pool, &host).await {
        Ok(Some(_)) => Ok(StatusCode::NO_CONTENT.into_response()),
        Ok(None) => Err(internal()), // throw new Error('no such instance')
        Err(e) => Err(db_err("find_instance_for_federation[refresh]", e)),
    }
}

/// `admin/reset-password.ts` — `findOneByOrFail({ userId })` then
/// `userProfilesRepository.update(userId, { password: hash(ps.password) })`.
/// A missing profile is the uncaught findOneByOrFail → 500; success is void
/// → 204. No moderation log upstream.
async fn reset_password(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    // paramDef: userId (misskey:id) + password (minLength 1), both required.
    let user_id = match body.get("userId") {
        None | Some(Value::Null) => {
            return Err(super::invalid_param(
                "userId",
                "must have required property 'userId'",
            ));
        }
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        Some(_) => return Err(super::invalid_param("userId", "must be string")),
    };
    let password = match body.get("password") {
        None | Some(Value::Null) => {
            return Err(super::invalid_param(
                "password",
                "must have required property 'password'",
            ));
        }
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        Some(Value::String(_)) => {
            return Err(super::invalid_param(
                "password",
                "must NOT have fewer than 1 characters",
            ));
        }
        Some(_) => return Err(super::invalid_param("password", "must be string")),
    };

    // findOneByOrFail on user_profile.
    if let Err(e) = mk_db::user::find_profile(pool, &user_id).await {
        tracing::error!(target: "dbg_admin_fedops", error = %e, "find_profile[reset-password]");
        return Err(internal());
    }

    let hash = match mk_core::services::hash_password(&password) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(target: "dbg_admin_fedops", error = %e, "hash_password");
            return Err(internal());
        }
    };
    if let Err(e) = mk_db::user::set_password_hash(pool, &user_id, &hash).await {
        return Err(db_err("set_password_hash", e));
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}
