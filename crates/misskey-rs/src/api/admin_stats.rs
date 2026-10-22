//! `admin/meta` + the admin stats reads — ports of
//! `endpoints/admin/{meta,get-table-stats,get-index-stats,get-user-ips}.ts`.
//!
//! Auth chain is the shared `api::admin::gate` (credential gate → suspended
//! gate → requireAdmin role gate with the root bypass). App/miauth tokens
//! also ride the per-endpoint `ep.meta.kind` scope gate (ApiCallService.call
//! L465); native user tokens skip it, matching upstream's `token == null`
//! path.
//!
//! `admin/meta` delegates to the existing detailed handler in `api::admin`
//! (whole meta row + schema defaults; `MetaService.fetch(true)` semantics)
//! so this module only owns the routing. The stats reads are new SQL in
//! `mk-db/src/admin.rs`.

use axum::Json;
use axum::extract::Request;
use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

use super::admin::{self, Gate};
use super::{invalid_param, opt_int_param};

/// Endpoint names owned by this module (all POST-only, requireAdmin).
const ENDPOINTS: &[&str] = &[
    "admin/meta",
    "admin/get-table-stats",
    "admin/get-index-stats",
    "admin/get-user-ips",
];

pub(super) fn handles(name: &str) -> bool {
    ENDPOINTS.contains(&name)
}

/// Single entry point from the mod.rs dispatch arm (nosed in ahead of the
/// admin/* catch-all).
pub(super) async fn handle(state: &AppState, name: &str, req: Request) -> Response {
    if !ENDPOINTS.contains(&name) {
        return api_err(ApiError::unknown_endpoint());
    }
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
        "admin/meta" => admin::admin_meta(state, token.as_deref()).await,
        "admin/get-table-stats" => table_stats(state, token.as_deref()).await,
        "admin/get-index-stats" => index_stats(state, token.as_deref()).await,
        "admin/get-user-ips" => user_ips(state, &body, token.as_deref()).await,
        _ => unreachable!("checked against ENDPOINTS above"),
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

fn db_err(e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_admin_stats", error = %e, "admin stats query");
    api_err(ApiError::internal_error())
}

/// `ep.meta.kind` gate for app/miauth tokens (native user tokens carry no
/// permission list and skip the check, as upstream does).
fn require_kind(ctx: &admin::AdminCtx, kind: &str) -> Result<(), Response> {
    if let Some(perms) = &ctx.token_permission {
        if !perms.iter().any(|p| p == kind) {
            return Err(api_err(ApiError::new(
                403,
                mk_types::api_error::ApiErrorKind::Permission,
                "PERMISSION_DENIED",
                "1370e5b7-d4eb-4566-bb1d-7748ee6a1838",
                "Your app does not have the necessary permissions to use this endpoint.",
            )));
        }
    }
    Ok(())
}

/// `admin/get-table-stats.ts` (requireAdmin, kind `read:admin:table-stats`).
async fn table_stats(state: &AppState, token: Option<&str>) -> ApiResult {
    let ctx = admin::gate(state, token, Gate::Admin).await?;
    require_kind(&ctx, "read:admin:table-stats")?;
    let pool = state.pool.as_ref().expect("gated");

    let rows = mk_db::admin::table_stats(pool).await.map_err(db_err)?;
    let stats: Vec<Value> = rows
        .into_iter()
        .map(|(relname, n_live_tup)| json!({ "relname": relname, "n_live_tup": n_live_tup }))
        .collect();
    Ok((StatusCode::OK, Json(json!(stats))).into_response())
}

/// `admin/get-index-stats.ts` (requireAdmin, kind `read:admin:index-stats`) —
/// index sizes from `pg_stat_user_indexes` as `{ table, index, size, scans }`.
async fn index_stats(state: &AppState, token: Option<&str>) -> ApiResult {
    let ctx = admin::gate(state, token, Gate::Admin).await?;
    require_kind(&ctx, "read:admin:index-stats")?;
    let pool = state.pool.as_ref().expect("gated");

    let rows = mk_db::admin::index_stats(pool).await.map_err(db_err)?;
    let stats: Vec<Value> = rows
        .into_iter()
        .map(|(table, index, size, scans)| {
            json!({ "table": table, "index": index, "size": size, "scans": scans })
        })
        .collect();
    Ok((StatusCode::OK, Json(json!(stats))).into_response())
}

/// `admin/get-user-ips.ts` (requireAdmin, kind `read:admin:user-ips`) —
/// `{ userId, limit?, offset? }` → `[{ ip, createdAt }]`, newest first. The
/// signin AID carries the timestamp (the table has no createdAt column).
async fn user_ips(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let ctx = admin::gate(state, token, Gate::Admin).await?;
    require_kind(&ctx, "read:admin:user-ips")?;

    let user_id = match body.get("userId") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        Some(Value::String(_)) => return Err(invalid_param("userId", "must not be empty")),
        None | Some(Value::Null) => {
            return Err(invalid_param(
                "userId",
                "must have required property 'userId'",
            ))
        }
        Some(_) => return Err(invalid_param("userId", "must be string")),
    };
    let limit = opt_int_param(body, "limit")?.unwrap_or(30).clamp(1, 200);
    let offset = opt_int_param(body, "offset")?.unwrap_or(0).max(0);

    let pool = state.pool.as_ref().expect("gated");
    let rows = mk_db::admin::user_ips(pool, &user_id, limit, offset)
        .await
        .map_err(db_err)?;
    let ips: Vec<Value> = rows
        .into_iter()
        .map(|(id, ip)| {
            json!({
                "ip": ip,
                "createdAt": ms_iso(mk_types::id::parse_aid_date(&id)),
            })
        })
        .collect();
    Ok((StatusCode::OK, Json(json!(ips))).into_response())
}

fn ms_iso(ms: Option<i64>) -> Value {
    ms.map(mk_core::services::signup::ms_to_iso)
        .map(Value::String)
        .unwrap_or(Value::Null)
}
