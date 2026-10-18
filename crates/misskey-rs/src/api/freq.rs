//! freq-agent: users/get-frequently-replied-users + pinned-users, plus the
//! users/notes guest path and the pinnedUsers slice of admin/update-meta
//! that the users.ts census cases call first. db paths live in
//! mk-db/src/freq.rs.
//!
//! Upstream refs (read-only): endpoints/users/get-frequently-replied-users.ts
//! (requireCredential: FALSE, userId + limit{1..100, d10}, UserDetailed pack,
//! weight = replyCount / peak), endpoints/pinned-users.ts (meta.pinnedUsers
//! acct strings, same order, UserDetailed pack, requireCredential: false),
//! endpoints/users/notes.ts (db-fallback shape), core/MetaService.ts,
//! endpoints/admin/update-meta.ts (requireAdministrator).

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::error::{api_err, ApiError};
use crate::AppState;

/// Users-pack pipeline identical to users/show (the e2e compares the packed
/// user against a users/show round-trip byte-for-byte).
async fn pack_user_detailed(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    user_id: &str,
    me_id: Option<&str>,
) -> Result<Option<Value>, Response> {
    let row = match mk_db::user::find_user_by_id(pool, user_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_freq", error = %e, "pack_user find_user_by_id");
            return Err(api_err(ApiError::internal_error()));
        }
    };
    let Some(row) = row else {
        return Ok(None);
    };
    let profile = match mk_db::user::find_profile(pool, &row.id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(target: "dbg_freq", error = %e, "pack_user find_profile");
            return Err(api_err(ApiError::internal_error()));
        }
    };
    let is_me = me_id == Some(row.id.as_str());
    let relation = match super::build_relation(pool, me_id, &row.id, is_me).await {
        Ok(r) => r,
        Err(resp) => return Err(*resp),
    };
    let ctx = mk_core::services::user_pack::InstanceCtx {
        url: &state.config.url,
        host: &state.config.host(),
    };
    Ok(Some(mk_core::services::user_pack::pack_inline_opts(
        &row,
        &profile,
        is_me,
        relation.as_ref(),
        ctx,
        false, // users/show: secrets stay hidden even for self
    )))
}

fn no_such_user_resp() -> Response {
    api_err(ApiError::new(
        404,
        mk_types::api_error::ApiErrorKind::Client,
        "NO_SUCH_USER",
        "e6965129-7b2a-40a4-bae2-cd84cd434822",
        "No such user.",
    ))
}

/// `POST /api/users/get-frequently-replied-users` — the requested user's top
/// replied-to users with normalized reply weights. requireCredential: false
/// (me only feeds the visibility gates).
pub(crate) async fn frequently_replied(
    state: &AppState,
    body: &Value,
    token: Option<&str>,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let Some(user_id) = body.get("userId").and_then(|v| v.as_str()) else {
        return super::invalid_param("userId", "required");
    };
    // limit: integer 1..100, default 10 (paramDef)
    let limit = match body.get("limit") {
        None => 10i64,
        Some(v) => match v.as_i64() {
            Some(l) if (1..=100).contains(&l) => l,
            _ => return super::invalid_param("limit", "must be an integer between 1 and 100"),
        },
    };
    let me_id: Option<String> = super::resolve_me(pool, token).await;

    // GetterService.getUser: missing target → NO_SUCH_USER.
    match mk_db::user::find_user_by_id(pool, user_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return no_such_user_resp(),
        Err(e) => {
            tracing::error!(target: "dbg_freq", error = %e, "target find_user_by_id");
            return api_err(ApiError::internal_error());
        }
    }

    let counts = match mk_db::freq::frequent_replied_counts(pool, user_id, me_id.as_deref()).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(target: "dbg_freq", error = %e, "frequent_replied_counts");
            return api_err(ApiError::internal_error());
        }
    };
    let peak = counts.iter().map(|c| c.count).max().unwrap_or(1) as f64;

    let mut out = Vec::with_capacity(counts.len().min(limit as usize));
    for c in counts.into_iter().take(limit as usize) {
        if c.count <= 0 {
            continue;
        }
        let packed = match pack_user_detailed(state, pool, &c.user_id, me_id.as_deref()).await {
            Ok(p) => p,
            Err(r) => return r,
        };
        let Some(packed) = packed else { continue };
        out.push(json!({
            "user": packed,
            "weight": c.count as f64 / peak,
        }));
    }
    (
        StatusCode::OK,
        Json(serde_json::to_value(out).unwrap_or(json!([]))),
    )
        .into_response()
}

/// `POST /api/pinned-users` — meta.pinnedUsers acct entries resolved to
/// UserDetailed (order preserved, unknown accts skipped). requireCredential:
/// false.
pub(crate) async fn pinned_users(state: &AppState, token: Option<&str>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id: Option<String> = super::resolve_me(pool, token).await;

    let accts = match mk_db::freq::meta_pinned_user_accts(pool).await {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(target: "dbg_freq", error = %e, "meta_pinned_user_accts");
            return api_err(ApiError::internal_error());
        }
    };

    let mut out = Vec::with_capacity(accts.len());
    for acct in &accts {
        // Acct.parse: leading '@' stripped, one '@' split → (username, host)
        let acct = acct.strip_prefix('@').unwrap_or(acct);
        let (username, host) = match acct.split_once('@') {
            Some((u, h)) => (u, Some(h)),
            None => (acct, None),
        };
        if username.is_empty() {
            continue;
        }
        let id = match mk_db::freq::find_user_id_by_acct(pool, &username.to_lowercase(), host).await
        {
            Ok(i) => i,
            Err(e) => {
                tracing::error!(target: "dbg_freq", error = %e, "find_user_id_by_acct");
                return api_err(ApiError::internal_error());
            }
        };
        let Some(id) = id else { continue };
        let packed = match pack_user_detailed(state, pool, &id, me_id.as_deref()).await {
            Ok(p) => p,
            Err(r) => return r,
        };
        if let Some(packed) = packed {
            out.push(packed);
        }
    }
    (
        StatusCode::OK,
        Json(serde_json::to_value(out).unwrap_or(json!([]))),
    )
        .into_response()
}

/// `POST /api/users/notes` (db-fallback shape) — the requested user's notes;
/// guest-visible. Census cases fetch `[0]` of a user's timeline.
pub(crate) async fn user_notes(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let Some(user_id) = body.get("userId").and_then(|v| v.as_str()) else {
        return super::invalid_param("userId", "required");
    };
    let limit = match body.get("limit") {
        None => 10i64,
        Some(v) => match v.as_i64() {
            Some(l) if (1..=100).contains(&l) => l,
            _ => return super::invalid_param("limit", "must be an integer between 1 and 100"),
        },
    };
    let since_id = match body.get("sinceId") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.as_str()),
        Some(_) => return super::invalid_param("sinceId", "expected a string"),
    };
    let until_id = match body.get("untilId") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.as_str()),
        Some(_) => return super::invalid_param("untilId", "expected a string"),
    };
    let with_files = match body.get("withFiles") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return super::invalid_param("withFiles", "expected a boolean"),
    };
    let with_renotes = match body.get("withRenotes") {
        None => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => return super::invalid_param("withRenotes", "expected a boolean"),
    };
    let me_id: Option<String> = super::resolve_me(pool, token).await;

    let ids = match mk_db::freq::user_note_ids(
        pool,
        user_id,
        me_id.as_deref(),
        since_id,
        until_id,
        limit,
        with_files,
        with_renotes,
    )
    .await
    {
        Ok(i) => i,
        Err(e) => {
            tracing::error!(target: "dbg_freq", error = %e, "user_note_ids");
            return api_err(ApiError::internal_error());
        }
    };

    let mut out = Vec::with_capacity(ids.len());
    for id in &ids {
        // notes pack pipeline identical to /api/notes/show (pack_note).
        match super::pack_note(state, pool, id).await {
            Ok(note) => out.push(note),
            Err(resp) => return *resp,
        }
    }
    (
        StatusCode::OK,
        Json(serde_json::to_value(out).unwrap_or(json!([]))),
    )
        .into_response()
}

/// `POST /api/admin/update-meta` (pinnedUsers slice) — requireAdministrator;
/// body taken as a partial patch: only `pinnedUsers` is applied, other keys
/// are ignored (their endpoint handlers land with the admin meta wave).
pub(crate) async fn update_meta(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };

    // ApiCallService.call gates: requireCredential → suspended → admin role
    // (mirrors admin.rs `gate`).
    let Some(token) = token else {
        return api_err(ApiError::new(
            401,
            mk_types::api_error::ApiErrorKind::Client,
            "CREDENTIAL_REQUIRED",
            "1384574d-a912-4b81-8601-c7b1c4085df1",
            "Credential required.",
        ));
    };
    if token.len() != 16 {
        return api_err(ApiError::new(
            401,
            mk_types::api_error::ApiErrorKind::Client,
            "AUTHENTICATION_FAILED",
            "b0a7f5f8-dc2f-4171-b91f-de88ad238e14",
            "Authentication failed. Please ensure your token is correct.",
        ));
    }
    let me_id = match mk_db::user::find_local_user_id_by_token(pool, token).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            return api_err(ApiError::new(
                401,
                mk_types::api_error::ApiErrorKind::Client,
                "AUTHENTICATION_FAILED",
                "b0a7f5f8-dc2f-4171-b91f-de88ad238e14",
                "Authentication failed. Please ensure your token is correct.",
            ))
        }
        Err(e) => {
            tracing::error!(target: "dbg_freq", error = %e, "find_local_user_id_by_token");
            return api_err(ApiError::internal_error());
        }
    };
    let me = match mk_db::user::find_user_by_id(pool, &me_id).await {
        Ok(Some(m)) => m,
        Ok(None) => {
            return api_err(ApiError::new(
                401,
                mk_types::api_error::ApiErrorKind::Client,
                "AUTHENTICATION_FAILED",
                "b0a7f5f8-dc2f-4171-b91f-de88ad238e14",
                "Authentication failed. Please ensure your token is correct.",
            ))
        }
        Err(e) => {
            tracing::error!(target: "dbg_freq", error = %e, "find_user_by_id[me]");
            return api_err(ApiError::internal_error());
        }
    };
    if me.is_suspended {
        return api_err(ApiError::new(
            403,
            mk_types::api_error::ApiErrorKind::Permission,
            "YOUR_ACCOUNT_SUSPENDED",
            "a8c724b3-6e9c-4b46-b1a8-bc3ed6258370",
            "Your account has been suspended.",
        ));
    }
    let is_admin = {
        let root_id = match mk_db::admin::root_user_id(pool).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(target: "dbg_freq", error = %e, "root_user_id");
                return api_err(ApiError::internal_error());
            }
        };
        if root_id.as_deref() == Some(me.id.as_str()) || me.username_lower == "root" {
            true
        } else {
            match mk_db::role::user_assigned_roles(pool, &me.id).await {
                Ok(assigned) => mk_core::services::role::has_administrator_role(&assigned),
                Err(e) => {
                    tracing::error!(target: "dbg_freq", error = %e, "user_assigned_roles");
                    return api_err(ApiError::internal_error());
                }
            }
        }
    };
    if !is_admin {
        return api_err(ApiError::new(
            403,
            mk_types::api_error::ApiErrorKind::Permission,
            "ROLE_PERMISSION_DENIED",
            "c3d38592-54c0-429d-be96-5636b0431a61",
            "You are not assigned to an administrator role.",
        ));
    }

    if let Some(pinned) = body.get("pinnedUsers") {
        let Some(arr) = pinned.as_array() else {
            return super::invalid_param("pinnedUsers", "expected an array of strings");
        };
        let mut accts: Vec<String> = Vec::with_capacity(arr.len());
        for v in arr {
            match v.as_str() {
                Some(s) => accts.push(s.to_string()),
                None => return super::invalid_param("pinnedUsers", "expected an array of strings"),
            }
        }
        if let Err(e) = mk_db::freq::update_meta_pinned_users(pool, &accts).await {
            tracing::error!(target: "dbg_freq", error = %e, "update_meta_pinned_users");
            return api_err(ApiError::internal_error());
        }
    }

    // endpoint returns undefined → ApiCallService sends 204
    StatusCode::NO_CONTENT.into_response()
}
