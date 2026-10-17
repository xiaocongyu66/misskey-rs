//! `/api/{name}` dispatch and the M0 endpoint set.
//!
//! The full 438-endpoint registry (CI codegen from upstream metas) replaces
//! the hand-written table here from M2 on; the dispatch chain already mirrors
//! `ApiCallService.handleRequest` ordering so endpoints slot in.

use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::error::{api_err, ApiError};
use crate::ratelimit::Limit;
use crate::AppState;

/// Endpoints that accept GET (upstream `meta.allowGet`) with `cacheSec`.
const GET_ALLOWED: &[(&str, u64)] = &[
    ("server-info", 60),
    ("emojis", 0),
    ("get-online-users-count", 60),
];

const M0_ENDPOINTS: &[&str] = &[
    "ping",
    "meta",
    "endpoints",
    "stats",
    "server-info",
    "get-online-users-count",
    "emojis",
];

pub async fn dispatch(State(state): State<AppState>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let Some(name) = path.strip_prefix("/api/") else {
        return api_err(ApiError::unknown_endpoint());
    };
    let name = name.to_string();
    let method = req.method().clone();

    // Route-like endpoints outside the endpoint registry (mirrors upstream
    // fastify.post handlers inside the /api plugin).
    if name == "signup" {
        if method != Method::POST {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let body = match parse_json_body(req).await {
            Ok(b) => b,
            Err(resp) => return resp,
        };
        return signup_route(&state, &body).await;
    }
    if name == "_test/reset" {
        if method != Method::POST || !state.config.test {
            return api_err(ApiError::unknown_endpoint());
        }
        return reset_route(&state).await;
    }

    if !M0_ENDPOINTS.contains(&name.as_str()) {
        return api_err(ApiError::unknown_endpoint());
    }

    let get_cache = GET_ALLOWED
        .iter()
        .find(|(n, _)| *n == name.as_str())
        .map(|(_, c)| *c);
    let get_allowed = get_cache.is_some();
    if method == Method::GET && !get_allowed {
        // Upstream: `reply.code(405); reply.send();` — empty body.
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }

    // Read endpoints in M0 are unauthenticated; the auth chain drops in at M1
    // here, in ApiCallService order.

    let body: Value = if method == Method::POST {
        match parse_json_body(req).await {
            Ok(b) => b,
            Err(resp) => return resp,
        }
    } else {
        json!({})
    };

    let result: Value = match name.as_str() {
        "ping" => json!({ "pong": now_ms() }),
        "meta" => meta(&state),
        "endpoints" => json!(M0_ENDPOINTS),
        "stats" => stats(&state).await,
        "server-info" => server_info(),
        "get-online-users-count" => json!({ "count": 0 }),
        "emojis" => json!({ "emojis": [] }),
        _ => unreachable!("checked above"),
    };

    if let Some(cache_secs) = get_cache {
        // cacheSec: `Cache-Control: private, max-age=`
        let v = format!("private, max-age={cache_secs}");
        let mut resp = (StatusCode::OK, Json(result)).into_response();
        if let Ok(hv) = header::HeaderValue::from_str(&v) {
            resp.headers_mut().insert(header::CACHE_CONTROL, hv);
        }
        return resp;
    }

    // `null` results become 204 in upstream's ApiCallService.send.
    if result.is_null() {
        return StatusCode::NO_CONTENT.into_response();
    }

    (StatusCode::OK, Json(result)).into_response()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// M0 placeholder driven by static config; the real shape comes from the
/// `meta` table via MetaService in M2 (MiMeta row + Redis invalidation).
fn meta(state: &AppState) -> Value {
    json!({
        "maintainerName": null,
        "maintainerEmail": null,
        "version": env!("CARGO_PKG_VERSION"),
        "provides": {},
        "name": null,
        "shortName": null,
        "uri": state.config.url.clone(),
        "description": null,
        "langs": [],
        "tosUrl": null,
        "repositoryUrl": null,
        "feedbackUrl": null,
        "impressumUrl": null,
        "privacyPolicyUrl": null,
        "disableRegistration": true,
        "emailRequiredForSignup": false,
        "approvalRequiredForSignup": false,
        "enableHcaptcha": false,
        "enableRecaptcha": false,
        "enableTurnstile": false,
        "enableMcaptcha": false,
        "swPublickey": null,
        "themeColor": null,
        "mascotImageUrl": "/assets/ai.png",
        "bannerUrl": null,
        "serverErrorImageUrl": null,
        "infoImageUrl": null,
        "notFoundImageUrl": null,
        "iconUrl": null,
        "backgroundImageUrl": null,
        "logoImageUrl": null,
        "maxNoteTextLength": 3000,
        "defaultLightTheme": null,
        "defaultDarkTheme": null,
        "ads": [],
        "notesPerOneAd": 20,
        "enableEmail": false,
        "enableServiceWorker": false,
        "translatorAvailable": false,
        "serverRules": [],
        "policies": {},
        "mediaProxy": format!("{}/proxy", state.config.url.trim_end_matches('/')),
        "features": {
            "registration": false,
            "emailRequiredForSignup": false,
            "localTimeline": true,
            "globalTimeline": true,
            "hcaptcha": false,
            "recaptcha": false,
            "turnstile": false,
            "objectStorage": false,
            "serviceWorker": false,
            "miauth": false,
        },
        "cacheRemoteFiles": false,
        "cacheRemoteSensitiveFiles": false,
    })
}

async fn stats(state: &AppState) -> Value {
    // Table counts; zeros while the baseline schema is placeholder-only.
    let _ = state;
    json!({
        "notesCount": 0,
        "originalNotesCount": 0,
        "usersCount": 0,
        "originalUsersCount": 0,
        "reactionsCount": 0,
        "instances": 0,
        "driveUsageLocal": 0,
        "driveUsageRemote": 0,
    })
}

/// M0 stand-in for the `systeminformation`-derived payload.
fn server_info() -> Value {
    json!({
        "machine": "misskey-rs",
        "os": std::env::consts::OS,
        "node": "misskey-rs",
        "psql": null,
        "cpu": {
            "model": null,
            "cores": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        },
        "mem": { "total": 0, "used": 0 },
    })
}

/// Rate limit shell wiring for future endpoints (limits arrive with real
/// endpoint metas in M2) — keeps the actor-key logic exercised.
#[allow(dead_code)]
fn limit_actor(state: &AppState, endpoint: &str) {
    let _ = Limit {
        max: 300,
        duration_ms: 60_000,
    };
    let _ = &state.rate_limiter;
    let _ = endpoint;
}

fn parse_json_body(
    req: Request,
) -> impl std::future::Future<Output = Result<Value, Response>> + Send {
    async move {
        let bytes = match axum::body::to_bytes(req.into_body(), 1024 * 1024).await {
            Ok(b) => b,
            Err(_) => {
                return Err(api_err(ApiError::invalid_param(json!({
                    "param": "",
                    "reason": "body could not be read",
                }))))
            }
        };
        if bytes.is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_slice(&bytes).map_err(|err| {
            api_err(ApiError::invalid_param(json!({
                "param": "",
                "reason": err.to_string(),
            })))
        })
    }
}

fn require_str<'a>(body: &'a Value, key: &str) -> Result<&'a str, Response> {
    body.get(key).and_then(|v| v.as_str()).ok_or_else(|| {
        api_err(ApiError::invalid_param(json!({
            "param": key,
            "reason": "expected a string",
        })))
    })
}

/// `POST /api/signup` — port of SignupApiService.signup, test-mode path
/// (captcha/invitation/email checks are skipped when config.test, matching
/// upstream `NODE_ENV === 'test'` shortcuts).
async fn signup_route(state: &AppState, body: &Value) -> Response {
    let username = match require_str(body, "username") {
        Ok(v) => v,
        Err(r) => return r,
    };
    let password = match require_str(body, "password") {
        Ok(v) => v,
        Err(r) => return r,
    };

    if let Some(pool) = &state.pool {
        let username_lower = username.to_lowercase();
        if let Ok(true) = mk_db::user::username_exists_local(pool, &username_lower).await {
            return reply_plain(400, "DUPLICATED_USERNAME");
        }
        if let Ok(true) = mk_db::user::used_username_exists(pool, &username_lower).await {
            return reply_plain(400, "USED_USERNAME");
        }
    } else {
        return api_err(ApiError::internal_error());
    }

    let outcome = match mk_core::services::build_signup(username, password, None) {
        Ok(o) => o,
        Err(code) => return reply_plain(400, code),
    };

    if let Some(pool) = &state.pool {
        let new_user = mk_db::user::NewUser {
            id: &outcome.user_id,
            username: &outcome.username,
            username_lower: &outcome.username_lower,
            name: outcome.name.as_deref(),
            token: &outcome.token,
            password_hash: &outcome.password_hash,
            public_key: &outcome.public_key,
            private_key: &outcome.private_key,
        };
        if let Err(err) = mk_db::user::insert_new_user(pool, &new_user).await {
            tracing::error!(%err, "signup insert failed");
            return api_err(ApiError::internal_error());
        }
    }

    let mut packed = serde_json::to_value(mk_core::services::me_detailed_fresh(
        &outcome,
        &state.config.url,
        &state.config.host(),
        true,
    ))
    .unwrap_or(json!({}));
    if let Some(obj) = packed.as_object_mut() {
        obj.insert("token".to_string(), json!(outcome.token.clone()));
    }

    (StatusCode::OK, Json(packed)).into_response()
}

/// Upstream `FastifyReplyError(400, '...')`: plain-text body, no envelope.
fn reply_plain(status: u16, text: &str) -> Response {
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST),
        text.to_string(),
    )
        .into_response()
}

async fn reset_route(state: &AppState) -> Response {
    match &state.pool {
        None => api_err(ApiError::internal_error()),
        Some(pool) => match mk_db::user::truncate_all(pool).await {
            Ok(_) => StatusCode::NO_CONTENT.into_response(),
            Err(err) => {
                tracing::error!(%err, "reset failed");
                api_err(ApiError::internal_error())
            }
        },
    }
}

/// Token extraction: `Authorization: Bearer` wins over body `i` (mirrors
/// upstream AuthenticateService consumers). Component of the M1 auth chain.
#[allow(dead_code)]
fn extract_token(body: &Value, headers: &axum::http::HeaderMap) -> Option<String> {
    if let Some(v) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
    {
        return Some(v.to_string());
    }
    body.get("i")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests;
