//! `following/*` remainder — slice 1: `following/invalidate` and
//! `following/requests/list` (upstream `endpoints/following/invalidate.ts`,
//! `endpoints/following/requests/list.ts`). The create/delete/accept/cancel
//! writes + `following/update`/`following/list` already live in `social.rs`.
//!
//! - `following/invalidate` — requireCredential `write:following`. Loads the
//!   target (`usersRepository.findOneByOrFail` — a miss is a plain uncaught
//!   EntityNotFoundError → 500 INTERNAL_ERROR, not a meta error), then, when
//!   the caller follows it, runs `UserFollowingService.unfollow` (delete the
//!   edge + decrement the denormalized counters under the moved-account
//!   guard following/delete uses). Unlike following/delete, a missing edge is
//!   a silent no-op rather than NOT_FOLLOWING. Answers the packed followee
//!   (`userEntityService.pack(followee.id, me)`).
//! - `following/requests/list` — requireCredential `read:following`. Pending
//!   `follow_request` rows addressed to the caller (`followeeId = me`),
//!   id DESC, packed as UserDetailedNotMe followers (upstream
//!   `packMany(reqs.map(x => x.follower), me)`). paramDef: `limit` 1..=100
//!   default 10, `sinceId`/`untilId` TypeORM cursor bounds — the upstream
//!   find spreads `cursor: { id }, skip: 1` for each, so both walk id DESC
//!   (`id < bound`; untilId wins when both are sent).
//!
//! Both names were catalog-only before this slice; the `following` /
//! `follow_request` tables ship in the 0001 baseline, so no schema gap.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// Endpoint names routed into this module (single source of truth for the
/// `following-more` dispatch arm in mod.rs).
pub(crate) const ENDPOINTS: &[&str] = &["following/invalidate", "following/requests/list"];

pub(crate) fn handles(name: &str) -> bool {
    ENDPOINTS.contains(&name)
}

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn ok_json(v: Value) -> Response {
    (StatusCode::OK, Json(v)).into_response()
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_following", error = %e, "db error: {context}");
    internal()
}

/// Entry from the `following-more` dispatch arm in mod.rs (body pre-parsed,
/// token pre-extracted). Auth runs before paramDef validation
/// (ApiCallService authenticates first).
pub(crate) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
    credential_declared: bool,
) -> Response {
    let Some(pool) = &state.pool else {
        return internal();
    };
    let me_id = match super::social::require_checked_user(
        pool,
        token.as_deref(),
        credential_declared,
    )
    .await
    {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    match name {
        "following/invalidate" => invalidate(state, pool, body, &me_id).await,
        "following/requests/list" => requests_list(state, pool, body, &me_id).await,
        _ => unreachable!("dispatch block checked the name"),
    }
}

/// `POST /api/following/invalidate` — UserFollowingService.invalidate:
/// unfollow the target if an edge exists, then pack the followee.
async fn invalidate(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Response {
    let Some(user_id) = body.get("userId").and_then(|v| v.as_str()) else {
        return super::invalid_param("userId", "expected a string (misskey:id)");
    };
    // findOneByOrFail: a miss surfaces as a plain 500 upstream.
    let followee = match mk_db::user::find_user_by_id(pool, user_id).await {
        Ok(Some(u)) => u,
        Ok(None) => return internal(),
        Err(e) => return db_err("invalidate find_user_by_id", e),
    };
    let exists = match mk_db::social::following_exists(pool, me_id, user_id).await {
        Ok(v) => v,
        Err(e) => return db_err("invalidate following_exists", e),
    };
    if exists {
        if let Err(e) = mk_db::social::delete_following(pool, me_id, user_id).await {
            return db_err("invalidate delete_following", e);
        }
        // UserFollowingService.decrementFollowing: moved endpoints age out
        // silently (same guard as following/delete).
        let me_moved = mk_db::move_account::get_moved_to_uri(pool, me_id).await;
        let followee_moved = mk_db::move_account::get_moved_to_uri(pool, user_id).await;
        match (me_moved, followee_moved) {
            (Ok(a), Ok(b)) => {
                if a.is_none() && b.is_none() {
                    if let Err(e) =
                        mk_db::social::bump_follow_counts(pool, me_id, user_id, -1).await
                    {
                        return db_err("invalidate bump", e);
                    }
                }
            }
            (Err(e), _) | (_, Err(e)) => return db_err("invalidate moved guard", e),
        }
    }
    match super::ff_vis::pack_edge_user(state, pool, followee, Some(me_id)).await {
        Ok(v) => ok_json(v),
        Err(resp) => resp,
    }
}

/// `POST /api/following/requests/list` — pending requests to the caller,
/// id DESC, packed followers.
async fn requests_list(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Response {
    let limit = match body.get("limit") {
        None | Some(Value::Null) => 10u32,
        Some(v) => match v.as_u64() {
            Some(n) if (1..=100).contains(&n) => n as u32,
            Some(_) => return super::invalid_param("limit", "must be >= 1 and <= 100"),
            None => return super::invalid_param("limit", "expected an integer"),
        },
    };
    let id_param = |key: &str| -> Result<Option<String>, Response> {
        match body.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if super::is_alnum_id(s) => Ok(Some(s.clone())),
            Some(_) => Err(super::invalid_param(key, "expected an alphanumeric id")),
        }
    };
    let since_id = match id_param("sinceId") {
        Ok(v) => v,
        Err(r) => return r,
    };
    let until_id = match id_param("untilId") {
        Ok(v) => v,
        Err(r) => return r,
    };
    // TypeORM cursor spread: untilId overrides sinceId when both are present.
    let cursor = until_id.as_deref().or(since_id.as_deref());
    let rows = match mk_db::following::list_follow_requests(pool, me_id, cursor, limit).await {
        Ok(r) => r,
        Err(e) => return db_err("requests_list list_follow_requests", e),
    };
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        match mk_db::user::find_user_by_id(pool, &row.follower_id).await {
            Ok(Some(user)) => {
                match super::ff_vis::pack_edge_user(state, pool, user, Some(me_id)).await {
                    Ok(v) => out.push(v),
                    Err(resp) => return resp,
                }
            }
            // relation join miss (deleted account): upstream relation would
            // be undefined; skip rather than fail the page.
            Ok(None) => {}
            Err(e) => return db_err("requests_list find_user_by_id", e),
        }
    }
    ok_json(Value::Array(out))
}
