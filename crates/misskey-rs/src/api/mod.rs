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

use crate::error::ApiError;
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
        return ApiError::unknown_endpoint().into_response();
    };
    let name = name.to_string();
    let method = req.method().clone();

    if !M0_ENDPOINTS.contains(&name.as_str()) {
        return ApiError::unknown_endpoint().into_response();
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

    let _body: Value = if method == Method::POST {
        // Params validated against paramDef from M2; M0 endpoints take none,
        // but body must still be well-formed JSON when present.
        let bytes = match axum::body::to_bytes(req.into_body(), 1024 * 1024).await {
            Ok(b) => b,
            Err(_) => {
                return ApiError::invalid_param(json!({
                    "param": "",
                    "reason": "body could not be read",
                }))
                .into_response()
            }
        };
        if bytes.is_empty() {
            json!({})
        } else {
            match serde_json::from_slice(&bytes) {
                Ok(v) => v,
                Err(err) => {
                    return ApiError::invalid_param(json!({
                        "param": "",
                        "reason": err.to_string(),
                    }))
                    .into_response()
                }
            }
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
