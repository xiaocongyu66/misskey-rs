//! Hashtags domain (upstream `endpoints/hashtags/{trend,show,list}.ts` over
//! `core/entities/HashtagEntityService.ts`; SQL in `mk-db/src/hashtag.rs`).
//!
//! Gate: all three are `requireCredential: false` (guest-ok). `trend` is
//! also `allowGet: true`, so the mod.rs arm accepts GET for that name; `show`
//! and `list` are POST-only like the rest of the registry.
//!
//! `trend` reads the `__chart__hashtag` hourly table (no Redis chart plane
//! in this port) and falls back to the `hashtag` counters when the chart is
//! empty — see `mk_db::hashtag` module docs.
//!
//! `hashtags/users` is intentionally absent: upstream selects `user.tags`
//! and returns `UserDetailed` with relationship fields, which is outside this
//! slice, so the name falls through to the unknown-endpoint arm.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

use mk_types::api_error::ApiErrorKind;

/// Endpoint names owned by this module (single source of truth for the
/// hashtags dispatch arm in mod.rs).
pub(crate) const HASHTAG_ENDPOINTS: &[&str] = &["hashtags/trend", "hashtags/show", "hashtags/list"];

pub(crate) fn handles(name: &str) -> bool {
    HASHTAG_ENDPOINTS.contains(&name)
}

fn invalid_param(param: &str, reason: &str) -> Response {
    api_err(ApiError::invalid_param(json!({
        "param": param,
        "reason": reason,
    })))
}

fn no_such_hashtag() -> Response {
    api_err(ApiError::new(
        400,
        ApiErrorKind::Client,
        "NO_SUCH_HASHTAG",
        "110ee688-193e-4a3a-9ecf-c167b2e6981e",
        "No such hashtag.",
    ))
}

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

/// `normalizeForSearch`: upstream NFKC-normalizes then lowercases. The
/// `unicode-normalization` crate is not a direct dependency of this crate, so
/// only the lowercase half is applied (ASCII tags — the common case — are
/// identical).
fn normalize_for_search(tag: &str) -> String {
    tag.to_lowercase()
}

fn pack(row: &mk_db::hashtag::HashtagRow) -> Value {
    json!({
        "tag": row.name,
        "mentionedUsersCount": row.mentioned_users_count,
        "mentionedLocalUsersCount": row.mentioned_local_users_count,
        "mentionedRemoteUsersCount": row.mentioned_remote_users_count,
        "attachedUsersCount": row.attached_users_count,
        "attachedLocalUsersCount": row.attached_local_users_count,
        "attachedRemoteUsersCount": row.attached_remote_users_count,
    })
}

/// Entry from the hashtags dispatch arm in mod.rs (body pre-parsed).
pub(crate) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    _token: Option<&str>,
) -> Response {
    let Some(pool) = &state.pool else {
        return internal();
    };
    let result: Result<Response, Response> = match name {
        "hashtags/trend" => trend(pool).await,
        "hashtags/show" => show(pool, body).await,
        "hashtags/list" => list(pool, body).await,
        _ => Ok(api_err(ApiError::unknown_endpoint())),
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

async fn trend(pool: &mk_db::sqlx::PgPool) -> Result<Response, Response> {
    let entries = mk_db::hashtag::trend(pool, mk_db::charts::now_ms())
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_hashtags", error = %e, "hashtags/trend");
            internal()
        })?;
    let out: Vec<Value> = entries
        .into_iter()
        .map(|e| {
            json!({
                "tag": e.tag,
                "chart": e.chart,
                "usersCount": e.users_count,
            })
        })
        .collect();
    Ok((StatusCode::OK, Json(Value::Array(out))).into_response())
}

async fn show(pool: &mk_db::sqlx::PgPool, body: &Value) -> Result<Response, Response> {
    let tag = body
        .get("tag")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_param("tag", "expected a string"))?;
    let row = mk_db::hashtag::find_by_name(pool, &normalize_for_search(tag))
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_hashtags", error = %e, "hashtags/show");
            internal()
        })?;
    let Some(row) = row else {
        return Err(no_such_hashtag());
    };
    Ok((StatusCode::OK, Json(pack(&row))).into_response())
}

const LIST_SORTS: &[&str] = &[
    "+mentionedUsers",
    "-mentionedUsers",
    "+mentionedLocalUsers",
    "-mentionedLocalUsers",
    "+mentionedRemoteUsers",
    "-mentionedRemoteUsers",
    "+attachedUsers",
    "-attachedUsers",
    "+attachedLocalUsers",
    "-attachedLocalUsers",
    "+attachedRemoteUsers",
    "-attachedRemoteUsers",
];

fn opt_bool(body: &Value, key: &str) -> Result<bool, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(invalid_param(key, "expected a boolean")),
    }
}

async fn list(pool: &mk_db::sqlx::PgPool, body: &Value) -> Result<Response, Response> {
    let sort = body
        .get("sort")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_param("sort", "expected a string"))?;
    if !LIST_SORTS.contains(&sort) {
        return Err(invalid_param("sort", "invalid sort order"));
    }
    let limit = match body.get("limit") {
        None | Some(Value::Null) => 10,
        Some(v) => v
            .as_i64()
            .ok_or_else(|| invalid_param("limit", "expected an integer"))?,
    };
    if !(1..=100).contains(&limit) {
        return Err(invalid_param("limit", "must be between 1 and 100"));
    }
    let attached_to_user_only = opt_bool(body, "attachedToUserOnly")?;
    let attached_to_local_user_only = opt_bool(body, "attachedToLocalUserOnly")?;
    let attached_to_remote_user_only = opt_bool(body, "attachedToRemoteUserOnly")?;

    let rows = mk_db::hashtag::list(
        pool,
        limit,
        attached_to_user_only,
        attached_to_local_user_only,
        attached_to_remote_user_only,
        sort,
    )
    .await
    .map_err(|e| {
        tracing::error!(target: "dbg_hashtags", error = %e, "hashtags/list");
        internal()
    })?;
    let out: Vec<Value> = rows.iter().map(pack).collect();
    Ok((StatusCode::OK, Json(Value::Array(out))).into_response())
}
