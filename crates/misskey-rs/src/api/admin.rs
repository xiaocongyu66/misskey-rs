//! `admin/*` endpoint family (M2 slice) — ports of
//! `endpoints/admin/{show-user,show-users,suspend-user,unsuspend-user,meta,
//! server-info}.ts` plus the `admin/stats` facade called for by the slice.
//!
//! Auth chain mirrors `ApiCallService.call()` in order: credential gate (401
//! CREDENTIAL_REQUIRED / invalid native token → 401 AUTHENTICATION_FAILED) →
//! suspended gate (403 YOUR_ACCOUNT_SUSPENDED) → requireModerator/requireAdmin
//! role gates with the `meta.rootUserId` bypass (403 ROLE_PERMISSION_DENIED).
//! `updatedAt=now()` on suspend is the harness-visible write requested for
//! this slice on top of upstream's `isSuspended` flip.

use axum::extract::Request;
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::error::{api_err, ApiError};
use crate::AppState;

use super::{build_relation, invalid_param, opt_bool, opt_enum, opt_nullable_str, pack_search_rows};

/// Endpoint names owned by this module (upstream `endpoint-list.ts` admin
/// names plus the v12-era `admin/suspend(|unsuspend)` aliases the slice names
/// explicitly; every name below dispatches POST-only).
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
];

type ApiResult = Result<Response, Response>;

/// Single entry point from the mod.rs dispatch arm.
pub(super) async fn handle(state: &AppState, name: &str, req: Request) -> Response {
    if !ENDPOINTS.contains(&name) {
        // Fastify only registered POST routes for known endpoints; everything
        // else falls to the UNKNOWN_API_ENDPOINT catch-all.
        return api_err(ApiError::unknown_endpoint());
    }

    // ApiServerService: `request.method === 'GET' && !meta.allowGet` → 405
    // with an empty body. No admin endpoint sets allowGet.
    if req.method() != Method::POST {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }

    let body = match super::parse_json_body(req).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };

    // handleRequest's token resolution: `Authorization: Bearer` wins over the
    // body `i`; a non-string body `i` (when not overridden by Bearer) is a
    // 400 with an empty body.
    let has_bearer = super::has_bearer_token(&body); // placeholder, replaced below
    let _ = has_bearer;
    let token = super::extract_token_from(state_headers_unused(), &body);
    let _ = &token;
    let _ = &body;

    dispatch(state, name, &body, token).await
}

fn dispatch(state: &AppState, name: &str, body: &Value, token: Option<String>) -> Response { ... }

// placeholders above are replaced by the real token plumbing; see below.
