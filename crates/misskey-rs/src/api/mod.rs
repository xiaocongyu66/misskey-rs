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

// social-agent: follow/mute/block/renote social actions (M2 truthful slice)
mod social;

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

    // Registry endpoints that need a JSON body parsed before dispatch.
    if name == "users/show"
        || name == "signin"
        || name == "i"
        || name == "i/update"
        || name == "notes/create"
        || name == "notes/show"
        || name == "users/search"
        || name == "users/search-by-username-and-host"
        || name == "hashtags/users"
        || name == "users"
        // -- notifications slice (M2): param line + arm kept at the END of
        // -- the dispatch group so co-agent rebases stay trivial
        || name == "notifications"
        // note-agent: note domain (local-timeline guest-ok)
        || name == "notes/timeline"
        || name == "notes/local-timeline"
        || name == "notes/hybrid-timeline"
        || name == "notes/reactions"
        || name == "notes/reactions/create"
        || name == "notes/delete"
    {
        if method != Method::POST {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let headers = req.headers().clone();
        let body = match parse_json_body(req).await {
            Ok(b) => b,
            Err(resp) => return resp,
        };
        let token = extract_token(&body, &headers);
        if name == "signin" {
            let _ = token;
            return signin_route(&state, &body).await;
        }
        if name == "i" {
            return i_route(&state, &body, &headers, token).await;
        }
        if name == "i/update" {
            return i_update(&state, &body, token).await;
        }
        if name == "notes/create" {
            return notes_create(&state, &body, token).await;
        }
        if name == "notes/show" {
            return notes_show(&state, &body).await;
        }
        if name == "users/search" {
            return users_search(&state, &body, token).await;
        }
        if name == "users/search-by-username-and-host" {
            return users_search_by_username_and_host(&state, &body, token).await;
        }
        if name == "hashtags/users" {
            return hashtags_users(&state, &body, token).await;
        }
        if name == "users" {
            return users_list(&state, &body, token).await;
        }
        // -- notifications slice (M2) — added lines; keep at END of group --
        if name == "notifications" {
            return notifications_route(&state, &body, token).await;
        }
        // -- end notifications slice --

        // note-agent arms
        if name == "notes/local-timeline" {
            return social::notes_local_timeline(&state, &body, token.as_deref()).await;
        }
        if name == "notes/timeline" || name == "notes/hybrid-timeline" {
            return social::timeline_or_hybrid(&state, name.as_str(), &body, token.as_deref())
                .await;
        }
        if name == "notes/reactions" {
            return social::notes_reactions(&state, &body).await;
        }
        if name == "notes/reactions/create" {
            return social::credential_gate(&state, &body, token.as_deref(), |s, b, me| {
                social::notes_reactions_create(s, b, me)
            })
            .await;
        }
        if name == "notes/delete" {
            return social::credential_gate(&state, &body, token.as_deref(), |s, b, me| {
                social::notes_delete(s, b, me)
            })
            .await;
        }
        return users_show(&state, &body, token).await;
    }

    // ==== ADMIN DISPATCH ARM (M2 admin slice) ====
    // Single arm routing every `admin/*` name into api::admin so the mod.rs
    // surface stays one hunk (rebase-trivial per the slice plan). Method
    // checks, body parsing, token resolution and the credential/role gates
    // all live inside the admin module.
    if name.starts_with("admin/") {
        return admin::handle(&state, &name, req).await;
    }
    // ==== END ADMIN DISPATCH ARM ====

    // social-agent dispatch block: follow/mute/block social actions
    // (following/*, mute/*, blocking/*) — requireCredential, JSON body,
    // same token extraction as the group above (upstream
    // endpoints/following + mute + blocking semantics).
    if social::SOCIAL_ENDPOINTS.contains(&name.as_str()) {
        if method != Method::POST {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let headers = req.headers().clone();
        let body = match parse_json_body(req).await {
            Ok(b) => b,
            Err(resp) => return resp,
        };
        // Upstream handleRequest: a non-string `i` → `reply.code(400)` empty.
        let credential_declared = body.get("i").is_some()
            || headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|h| h.to_str().ok())
                .map(|h| h.starts_with("Bearer "))
                .unwrap_or(false);
        let token = extract_token(&body, &headers);
        if token.is_none() {
            if let Some(v) = body.get("i") {
                if v.as_str().is_none() {
                    return StatusCode::BAD_REQUEST.into_response();
                }
            }
        }
        return social::route(&state, name.as_str(), &body, token, credential_declared).await;
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
        if let Err(e) = mk_db::user::username_exists_local(pool, &username_lower).await {
            tracing::error!(target: "dbg_signup", error = %e, "username_exists_local");
        }
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
            tracing::error!(target: "dbg_signup", %err, "signup insert failed");
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

/// `POST /api/signin` — SigninApiService.signin test-mode path (no captcha /
/// IP rate limit in test; 2FA unchanged → fresh users only take the plain
/// password branch).
async fn signin_route(state: &AppState, body: &Value) -> Response {
    let reply_error = |status: u16, id: &str| -> Response {
        (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({ "error": { "id": id } })),
        )
            .into_response()
    };

    let username = match body.get("username").and_then(|v| v.as_str()) {
        Some(u) => u,
        None => return StatusCode::BAD_REQUEST.into_response(),
    };
    if let Some(t) = body.get("token") {
        if t.as_str().is_none() {
            return StatusCode::BAD_REQUEST.into_response();
        }
    }

    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };

    let row = mk_db::user::find_local_user_by_username(pool, &username.to_lowercase())
        .await
        .ok()
        .flatten();
    let Some(user) = row else {
        return reply_error(404, "6cc579cc-885d-43d8-95c2-b8c7fc963280");
    };
    if user.is_suspended {
        return reply_error(403, "e03a5f46-d309-4865-9b69-56282d94e1eb");
    }

    let profile = match mk_db::user::find_profile(pool, &user.id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(target: "dbg_signin", error = %e, "find_profile");
            return api_err(ApiError::internal_error());
        }
    };

    // password flow: SigninFlowResponse { finished } variants
    let Some(password) = body.get("password").and_then(|v| v.as_str()) else {
        return (
            StatusCode::OK,
            Json(json!({ "finished": false, "next": "captcha" })),
        )
            .into_response();
    };

    let same = profile
        .password
        .as_deref()
        .map(|h| mk_core::services::verify_password(password, h).unwrap_or(false))
        .unwrap_or(false);

    if !same {
        let _ = mk_db::user::insert_signin(pool, &mk_types::id::gen_aid_now(), &user.id, "", false)
            .await;
        return reply_error(403, "932c904e-9460-45b7-9ce6-7ed33be7eb2c");
    }

    // success (no-2FA branch); upstream fires the signin record async
    let _ =
        mk_db::user::insert_signin(pool, &mk_types::id::gen_aid_now(), &user.id, "", true).await;
    (
        StatusCode::OK,
        Json(json!({
            "finished": true,
            "id": user.id,
            "i": user.token,
        })),
    )
        .into_response()
}

/// `POST /api/users/show` — userId | userIds | username, me from `i`/Bearer.
async fn users_show(state: &AppState, body: &Value, token: Option<String>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };

    let me_id: Option<String> = resolve_me(pool, token.as_deref()).await;

    let ctx = mk_core::services::user_pack::InstanceCtx {
        url: &state.config.url,
        host: &state.config.host(),
    };

    let no_such_user = || {
        api_err(ApiError::new(
            404,
            mk_types::api_error::ApiErrorKind::Client,
            "NO_SUCH_USER",
            "4362f8dc-731f-4ad8-a694-be5a88922a24",
            "No such user.",
        ))
    };

    // userIds array form: missing users are suppressed upstream
    if let Some(ids) = body.get("userIds").and_then(|v| v.as_array()) {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids.iter().filter_map(|v| v.as_str()) {
            let row = match mk_db::user::find_user_by_id(pool, id).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(target: "dbg_users_show", error = %e, "find_user_by_id[userIds]");
                    continue;
                }
            };
            let Some(row) = row else { continue };
            let profile = match mk_db::user::find_profile(pool, &row.id).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(target: "dbg_users_show", error = %e, "find_profile[userIds]");
                    continue;
                }
            };
            let is_me = me_id.as_deref() == Some(row.id.as_str());
            let relation = match build_relation(pool, me_id.as_deref(), &row.id, is_me).await {
                Ok(r) => r,
                Err(resp) => return *resp,
            };
            out.push(mk_core::services::user_pack::pack_inline_opts(
                &row,
                &profile,
                is_me,
                relation.as_ref(),
                ctx,
                false, // users/show: secrets stay hidden even for self
            ));
        }
        return (StatusCode::OK, Json(json!(out))).into_response();
    }

    let row = if let Some(id) = body.get("userId").and_then(|v| v.as_str()) {
        match mk_db::user::find_user_by_id(pool, id).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(target: "dbg_users_show", error = %e, "find_user_by_id");
                None
            }
        }
    } else if let Some(username) = body.get("username").and_then(|v| v.as_str()) {
        match mk_db::user::find_local_user_by_username(pool, &username.to_lowercase()).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(target: "dbg_users_show", error = %e, "find_local_user_by_username");
                None
            }
        }
    } else {
        return api_err(ApiError::invalid_param(json!({
            "param": "userId",
            "reason": "either userId or username is required",
        })));
    };

    let Some(row) = row else {
        return no_such_user();
    };
    let profile = match mk_db::user::find_profile(pool, &row.id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(target: "dbg_users_show", error = %e, "find_profile");
            return no_such_user();
        }
    };
    let is_me = me_id.as_deref() == Some(row.id.as_str());
    let relation = match build_relation(pool, me_id.as_deref(), &row.id, is_me).await {
        Ok(r) => r,
        Err(resp) => return *resp,
    };

    (
        StatusCode::OK,
        Json(mk_core::services::user_pack::pack_inline_opts(
            &row,
            &profile,
            is_me,
            relation.as_ref(),
            ctx,
            false, // users/show: secrets stay hidden even for self
        )),
    )
        .into_response()
}

/// Relation lookup for the pack, mirroring upstream `getRelation` scope:
/// only when a me is present AND the target is not me.
async fn build_relation(
    pool: &mk_db::sqlx::PgPool,
    me_id: Option<&str>,
    target_id: &str,
    is_me: bool,
) -> Result<Option<mk_db::relation::UserRelation>, Box<Response>> {
    if is_me || me_id.is_none() {
        return Ok(None);
    }
    match mk_db::relation::get_relation(pool, me_id.unwrap(), target_id).await {
        Ok(r) => Ok(Some(r)),
        Err(e) => {
            tracing::error!(target: "dbg_users_show", error = %e, "get_relation");
            Err(Box::new(api_err(ApiError::internal_error())))
        }
    }
}

/// Shared me-id resolution from a token: only native user tokens (length 16,
/// `misc/token.ts isNativeUserToken`) are matched against `user.token` for
/// local users; anything else resolves to "not me" (DB errors are logged).
async fn resolve_me(pool: &mk_db::sqlx::PgPool, token: Option<&str>) -> Option<String> {
    let Some(t) = token else {
        return None;
    };
    if t.len() != 16 {
        return None;
    }
    match mk_db::user::find_local_user_id_by_token(pool, t).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(target: "dbg_resolve_me", error = %e, "find_local_user_id_by_token");
            None
        }
    }
}

/// `POST /api/i` — requireCredential endpoint returning the caller's
/// MeDetailed. Auth gating mirrors `ApiCallService.call()`: no token →
/// CREDENTIAL_REQUIRED (401); unresolvable token → AUTHENTICATION_FAILED
/// (401, from AuthenticateService's AuthenticationError); suspended →
/// YOUR_ACCOUNT_SUSPENDED (403). The endpoint body is `endpoints/i.ts`.
async fn i_route(
    state: &AppState,
    body: &Value,
    headers: &axum::http::HeaderMap,
    token: Option<String>,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };

    // Upstream handleRequest: a non-string `i` (with no Bearer taking
    // precedence) → `reply.code(400)` with an empty body.
    let has_bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .map(|h| h.starts_with("Bearer "))
        .unwrap_or(false);
    if !has_bearer {
        if let Some(v) = body.get("i") {
            if v.as_str().is_none() {
                return StatusCode::BAD_REQUEST.into_response();
            }
        }
    }

    let credential_required = || {
        api_err(ApiError::new(
            401,
            mk_types::api_error::ApiErrorKind::Client,
            "CREDENTIAL_REQUIRED",
            "1384574d-a912-4b81-8601-c7b1c4085df1",
            "Credential required.",
        ))
    };
    let auth_failed = || {
        api_err(ApiError::new(
            401,
            mk_types::api_error::ApiErrorKind::Client,
            "AUTHENTICATION_FAILED",
            "b0a7f5f8-dc2f-4171-b91f-de88ad238e14",
            "Authentication failed. Please ensure your token is correct.",
        ))
    };

    // AuthenticateService: token absent → [null, null] user; present token
    // that matches no user (native shape) → AuthenticationError.
    let Some(token) = token else {
        return credential_required();
    };
    if token.len() != 16 {
        // Non-native tokens go through the access-token/app lookup, which M1
        // does not implement — same 401 as upstream's invalid signature path.
        return auth_failed();
    }
    let Some(me_id) = resolve_me(pool, Some(token.as_str())).await else {
        return auth_failed();
    };

    let row = match mk_db::user::find_user_by_id(pool, &me_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_i", error = %e, "find_user_by_id");
            return api_err(ApiError::internal_error());
        }
    };
    let Some(row) = row else {
        return auth_failed();
    };

    // call(): requireCredential suspended gate.
    if row.is_suspended {
        return api_err(ApiError::new(
            403,
            mk_types::api_error::ApiErrorKind::Permission,
            "YOUR_ACCOUNT_SUSPENDED",
            "a8c724b3-6e9c-4b46-b1a8-bc3ed6258370",
            "Your account has been suspended.",
        ));
    }

    let mut profile = match mk_db::user::find_profile(pool, &row.id).await {
        Ok(p) => p,
        Err(mk_db::sqlx::Error::RowNotFound) => {
            // endpoint body: userProfile == null → USER_IS_DELETED
            return api_err(ApiError::new(
                403,
                mk_types::api_error::ApiErrorKind::Permission,
                "USER_IS_DELETED",
                "e5b3b9f0-2b8f-4b9f-9c1f-8c5c1b2e1b1a",
                "User is deleted.",
            ));
        }
        Err(e) => {
            tracing::error!(target: "dbg_i", error = %e, "find_profile");
            return api_err(ApiError::internal_error());
        }
    };

    // i.ts: append today to loggedInDates (once per day) before packing.
    let today = js_today();
    if !profile.logged_in_dates.contains(&today) {
        profile.logged_in_dates.push(today.clone());
        // Upstream awaits this update inside the handler; a failure is logged
        // and the response still reflects the in-memory append.
        if let Err(err) = mk_db::user::append_logged_in_date(pool, &row.id, &today).await {
            tracing::error!(target: "dbg_i", error = %err, "append_logged_in_date");
        }
    }

    let ctx = mk_core::services::user_pack::InstanceCtx {
        url: &state.config.url,
        host: &state.config.host(),
    };

    (
        StatusCode::OK,
        Json({
            let mut v = mk_core::services::user_pack::pack_inline(&row, &profile, true, None, ctx);
            apply_me_unread(pool, &mut v).await;
            v
        }),
    )
        .into_response()
}

/// i.ts `today`: `${getFullYear()}/${getMonth() + 1}/${getDate()}` (unpadded).
/// JS `Date` uses server-local time; we use UTC (civil_from_days, the same
/// Howard Hinnant algorithm as `ms_to_iso`).
fn js_today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y}/{m}/{d}")
}

/// Upstream `ApiCallService.#sendAuthenticationError`: 401 with the stable
/// AUTHENTICATION_FAILED error (plus a WWW-Authenticate header, which the
/// `api_err` envelope cannot carry yet).
fn authentication_failed() -> Response {
    api_err(ApiError::new(
        401,
        mk_types::api_error::ApiErrorKind::Client,
        "AUTHENTICATION_FAILED",
        "b0a7f5f8-dc2f-4171-b91f-de88ad238e14",
        "Authentication failed. Please ensure your token is correct.",
    ))
}

fn invalid_param(param: &str, reason: &str) -> Response {
    api_err(ApiError::invalid_param(json!({
        "param": param,
        "reason": reason,
    })))
}

/// Optional nullable string param (`{ type: 'string', nullable: true }`):
/// absent → skip, null → write NULL, other types → INVALID_PARAM.
fn opt_nullable_str(body: &Value, key: &str) -> Result<Option<Option<String>>, Response> {
    match body.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(s)) => Ok(Some(Some(s.clone()))),
        Some(_) => Err(invalid_param(key, "expected a string or null")),
    }
}

fn opt_bool(body: &Value, key: &str) -> Result<Option<bool>, Response> {
    match body.get(key) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(invalid_param(key, "expected a boolean")),
    }
}

fn opt_enum(body: &Value, key: &str, allowed: &[&str]) -> Result<Option<String>, Response> {
    match body.get(key) {
        None => Ok(None),
        Some(Value::String(s)) if allowed.contains(&s.as_str()) => Ok(Some(s.clone())),
        Some(_) => Err(invalid_param(
            key,
            &format!("expected one of: {}", allowed.join(", ")),
        )),
    }
}

/// Enforces the schema min/maxLength of `nameSchema`/`descriptionSchema`/
/// `locationSchema` (raw value, before post-validation trim like upstream).
fn with_str_limits(
    value: Result<Option<Option<String>>, Response>,
    key: &str,
    min: usize,
    max: usize,
) -> Result<Option<Option<String>>, Response> {
    match value? {
        Some(Some(s)) => {
            let len = s.chars().count();
            if len < min {
                return Err(invalid_param(key, "must not be empty"));
            }
            if len > max {
                return Err(invalid_param(key, "must be shorter than the limit"));
            }
            Ok(Some(Some(s)))
        }
        other => Ok(other),
    }
}

/// `birthdaySchema` pattern: `^([0-9]{4})-([0-9]{2})-([0-9]{2})$`.
fn valid_birthday(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            _ => c.is_ascii_digit(),
        })
}

/// `endpoints/i/update.ts` paramDef M1 subset → [`mk_db::user::ProfileUpdate`].
fn parse_profile_update(body: &Value) -> Result<mk_db::user::ProfileUpdate, Response> {
    let name = match with_str_limits(opt_nullable_str(body, "name"), "name", 1, 50)? {
        None => None,
        Some(v) => Some(match v {
            None => None,
            Some(s) => {
                let t = s.trim();
                if t.is_empty() {
                    None
                } else {
                    Some(t.to_string())
                }
            }
        }),
    };
    let description = with_str_limits(
        opt_nullable_str(body, "description"),
        "description",
        1,
        1500,
    )?;
    let location = with_str_limits(opt_nullable_str(body, "location"), "location", 1, 50)?;
    // TODO(M2): validate `lang` against langmap keys + null (upstream paramDef
    // enum); no langmap port exists yet, so M1 stores the raw string.
    let lang = opt_nullable_str(body, "lang")?;
    let url = opt_nullable_str(body, "url")?;
    // birthday column is character(10): "YYYY-MM-DD" strings persist as-is,
    // undefined/null persist as NULL.
    let birthday = match body.get("birthday") {
        None | Some(Value::Null) => Some(None),
        Some(Value::String(s)) => {
            if !valid_birthday(s) {
                return Err(invalid_param("birthday", "expected YYYY-MM-DD"));
            }
            Some(Some(s.clone()))
        }
        Some(_) => return Err(invalid_param("birthday", "expected a string or null")),
    };

    // fields: array of { name, value }, 0..16 items; entries with an empty
    // trimmed name/value are dropped and survivors are trimmed (upstream's
    // post-ajv filter/map).
    let fields = match body.get("fields") {
        None => None,
        Some(Value::Array(items)) => {
            if items.len() > 16 {
                return Err(invalid_param(
                    "fields",
                    "array must not have more than 16 items",
                ));
            }
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let fname = item.get("name").and_then(|x| x.as_str());
                let fvalue = item.get("value").and_then(|x| x.as_str());
                let (Some(n), Some(v)) = (fname, fvalue) else {
                    return Err(invalid_param(
                        "fields",
                        "items must be { name, value } strings",
                    ));
                };
                let (nt, vt) = (n.trim(), v.trim());
                if nt.is_empty() || vt.is_empty() {
                    continue;
                }
                out.push(json!({ "name": nt, "value": vt }));
            }
            Some(Value::Array(out))
        }
        Some(_) => return Err(invalid_param("fields", "expected an array")),
    };

    // followedMessage: varchar(256)
    let followed_message = with_str_limits(
        opt_nullable_str(body, "followedMessage"),
        "followedMessage",
        1,
        256,
    )?;
    // JSON passthrough fields (array/object checks only; shape validation is
    // the consuming features' business, users.ts fixtures send valid values)
    let json_field = |key: &str| -> Result<Option<Value>, Response> {
        match body.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v @ Value::Array(_)) | Some(v @ Value::Object(_)) => Ok(Some(v.clone())),
            Some(_) => Err(invalid_param(key, "expected an array or object")),
        }
    };
    let muted_words = json_field("mutedWords")?;
    let hard_muted_words = json_field("hardMutedWords")?;
    let muted_instances = json_field("mutedInstances")?;
    let email_notification_types = json_field("emailNotificationTypes")?;
    let notification_recieve_config = json_field("notificationRecieveConfig")?;

    let visibility = ["public", "followers", "private"];
    let chat_scope = ["everyone", "followers", "following", "mutual", "none"];
    Ok(mk_db::user::ProfileUpdate {
        name,
        is_locked: opt_bool(body, "isLocked")?,
        is_explorable: opt_bool(body, "isExplorable")?,
        hide_online_status: opt_bool(body, "hideOnlineStatus")?,
        is_bot: opt_bool(body, "isBot")?,
        is_cat: opt_bool(body, "isCat")?,
        chat_scope: opt_enum(body, "chatScope", &chat_scope)?,
        description,
        location,
        birthday,
        lang,
        url,
        fields,
        no_crawle: opt_bool(body, "noCrawle")?,
        prevent_ai_learning: opt_bool(body, "preventAiLearning")?,
        inject_featured_note: opt_bool(body, "injectFeaturedNote")?,
        receive_announcement_email: opt_bool(body, "receiveAnnouncementEmail")?,
        always_mark_nsfw: opt_bool(body, "alwaysMarkNsfw")?,
        auto_sensitive: opt_bool(body, "autoSensitive")?,
        careful_bot: opt_bool(body, "carefulBot")?,
        auto_accept_followed: opt_bool(body, "autoAcceptFollowed")?,
        public_reactions: opt_bool(body, "publicReactions")?,
        following_visibility: opt_enum(body, "followingVisibility", &visibility)?,
        followers_visibility: opt_enum(body, "followersVisibility", &visibility)?,
        followed_message,
        muted_words,
        hard_muted_words,
        muted_instances,
        email_notification_types,
        notification_recieve_config,
    })
}

/// `POST /api/i/update` — requireCredential profile update, M1 subset of
/// upstream `endpoints/i/update.ts` (name/description/location/birthday/lang/
/// url/fields + the boolean/visibility switch set incl. isBot/isCat; avatar,
/// banner, decorations, mutedWords, room, pinnedPageId, alsoKnownAs etc. are
/// TODO).
async fn i_update(state: &AppState, body: &Value, token: Option<String>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };

    // requireCredential: only native 16-char tokens resolve to a local user;
    // anything else is the 401 AUTHENTICATION_FAILED shape.
    let Some(tok) = token.as_deref().filter(|t| t.len() == 16) else {
        return authentication_failed();
    };
    let me_id: Option<String> = match mk_db::user::find_local_user_id_by_token(pool, tok).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(target: "dbg_i_update", error = %e, "find_local_user_id_by_token");
            return api_err(ApiError::internal_error());
        }
    };
    let Some(me_id) = me_id else {
        return authentication_failed();
    };

    let row = match mk_db::user::find_user_by_id(pool, &me_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_i_update", error = %e, "find_user_by_id");
            return api_err(ApiError::internal_error());
        }
    };
    let Some(row) = row else {
        return authentication_failed();
    };
    // call(): requireCredential suspended gate.
    if row.is_suspended {
        return api_err(ApiError::new(
            403,
            mk_types::api_error::ApiErrorKind::Permission,
            "YOUR_ACCOUNT_SUSPENDED",
            "a8c724b3-6e9c-4b46-b1a8-bc3ed6258370",
            "Your account has been suspended.",
        ));
    }

    // --- paramDef subset ---
    let update = match parse_profile_update(body) {
        Ok(u) => u,
        Err(resp) => return resp,
    };

    if let Err(e) = mk_db::user::update_profile_subset(pool, &me_id, &update).await {
        tracing::error!(target: "dbg_i_update", error = %e, "update_profile_subset");
        return api_err(ApiError::internal_error());
    }

    // i/update.ts L483-531: hashtags from the new description join user.tags
    // (normalized lowercase, capped at 32; de-dup in SQL).
    if let Some(desc) = body.get("description").and_then(|v| v.as_str()) {
        let tags = mk_core::services::misc::extract_hashtags(desc);
        if !tags.is_empty() {
            if let Err(e) = mk_db::user::merge_user_tags(pool, &me_id, &tags).await {
                tracing::error!(target: "dbg_i_update", error = %e, "merge_user_tags");
            }
        }
    }

    // Fresh persist + pack (MeDetailed, includeSecrets path).
    let row = match mk_db::user::find_user_by_id(pool, &me_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_i_update", error = %e, "find_user_by_id[fresh]");
            return api_err(ApiError::internal_error());
        }
    };
    let Some(row) = row else {
        return api_err(ApiError::internal_error());
    };
    let profile = match mk_db::user::find_profile(pool, &row.id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(target: "dbg_i_update", error = %e, "find_profile[fresh]");
            return api_err(ApiError::internal_error());
        }
    };

    let ctx = mk_core::services::user_pack::InstanceCtx {
        url: &state.config.url,
        host: &state.config.host(),
    };

    (
        StatusCode::OK,
        Json({
            let mut v = mk_core::services::user_pack::pack_inline(&row, &profile, true, None, ctx);
            apply_me_unread(pool, &mut v).await;
            v
        }),
    )
        .into_response()
}

/// Me-view unread flag injection: `hasUnreadNotification` reflects the
/// notification table at request time (pack is pure; upstream computes it
/// in UserEntityService.pack per call).
async fn apply_me_unread(pool: &mk_db::sqlx::PgPool, packed: &mut Value) {
    match mk_db::notification::has_unread(pool, packed["id"].as_str().unwrap_or("")).await {
        Ok(v) => {
            if let Some(obj) = packed.as_object_mut() {
                obj.insert("hasUnreadNotification".into(), Value::Bool(v));
            }
        }
        Err(e) => tracing::error!(target: "dbg_i", error = %e, "has_unread"),
    }
}

/// `POST /api/users` — explorable-user list with the sort/state/origin matrix.
async fn users_list(state: &AppState, body: &Value, token: Option<String>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = resolve_me(pool, token.as_deref()).await;
    let sort = body.get("sort").and_then(|v| v.as_str());
    let state_p = body.get("state").and_then(|v| v.as_str());
    let origin = body
        .get("origin")
        .and_then(|v| v.as_str())
        .or(Some("local"));
    let hostname = body
        .get("hostname")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let limit = body
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(10)
        .clamp(1, 100);
    let offset = body
        .get("offset")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0);

    let rows = match mk_db::user::users_explore(
        pool,
        sort,
        state_p,
        origin,
        hostname.as_deref(),
        me_id.as_deref(),
        limit,
        offset,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_search", error = %e, "users_explore");
            return api_err(ApiError::internal_error());
        }
    };

    let packed = match pack_search_rows(state, pool, rows, me_id.as_deref()).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    (StatusCode::OK, Json(json!(packed))).into_response()
}

/// `POST /api/hashtags/users` — tag membership + sort/state/origin switch.
async fn hashtags_users(state: &AppState, body: &Value, token: Option<String>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = resolve_me(pool, token.as_deref()).await;
    let Some(tag) = body.get("tag").and_then(|v| v.as_str()) else {
        return api_err(ApiError::invalid_param(json!({
            "param": "tag",
            "reason": "required",
        })));
    };
    let sort = body
        .get("sort")
        .and_then(|v| v.as_str())
        .unwrap_or("+follower");
    let state_p = body.get("state").and_then(|v| v.as_str());
    let origin = body.get("origin").and_then(|v| v.as_str());
    let limit = body
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(10)
        .clamp(1, 100);
    let offset = body
        .get("offset")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0);

    let rows =
        match mk_db::user::users_by_tag(pool, tag, sort, state_p, origin, limit, offset).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(target: "dbg_search", error = %e, "users_by_tag");
                return api_err(ApiError::internal_error());
            }
        };

    let packed = match pack_search_rows(state, pool, rows, me_id.as_deref()).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    (StatusCode::OK, Json(json!(packed))).into_response()
}

/// Shared result packing for user-search endpoints (upstream packs with the
/// requesting me; schema 'UserDetailed' by default).
async fn pack_search_rows(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    rows: Vec<mk_db::user::LocalUserRow>,
    me_id: Option<&str>,
) -> Result<Vec<Value>, Box<Response>> {
    let ctx = mk_core::services::user_pack::InstanceCtx {
        url: &state.config.url,
        host: &state.config.host(),
    };
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let is_me = me_id == Some(row.id.as_str());
        let profile = match mk_db::user::find_profile(pool, &row.id).await {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(target: "dbg_search", error = %e, "find_profile");
                return Err(Box::new(api_err(ApiError::internal_error())));
            }
        };
        let relation = match build_relation(pool, me_id, &row.id, is_me).await {
            Ok(r) => r,
            Err(resp) => return Err(resp),
        };
        out.push(mk_core::services::user_pack::pack_inline_opts(
            row,
            &profile,
            is_me,
            relation.as_ref(),
            ctx,
            false,
        ));
    }
    Ok(out)
}

/// `POST /api/users/search-by-username-and-host`
async fn users_search_by_username_and_host(
    state: &AppState,
    body: &Value,
    token: Option<String>,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = resolve_me(pool, token.as_deref()).await;

    let username = body.get("username").and_then(|v| v.as_str());
    let host = body
        .get("host")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let limit = body
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(10)
        .clamp(1, 100);
    let own_host = state.config.host();

    let rows = match mk_db::user::search_users_by_username_and_host(
        pool,
        username,
        host.as_deref(),
        &own_host,
        limit,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_search", error = %e, "search_username_host");
            return api_err(ApiError::internal_error());
        }
    };

    let packed = match pack_search_rows(state, pool, rows, me_id.as_deref()).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    (StatusCode::OK, Json(json!(packed))).into_response()
}

/// `POST /api/users/search`
async fn users_search(state: &AppState, body: &Value, token: Option<String>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = resolve_me(pool, token.as_deref()).await;

    let Some(query) = body.get("query").and_then(|v| v.as_str()) else {
        return api_err(ApiError::invalid_param(json!({
            "param": "query",
            "reason": "required",
        })));
    };
    if query.is_empty() || query.chars().count() > 100 {
        return api_err(ApiError::invalid_param(json!({
            "param": "query",
            "reason": "1..100 chars",
        })));
    }
    let limit = body
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(10)
        .clamp(1, 100);
    let offset = body
        .get("offset")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0);
    let origin = body.get("origin").and_then(|v| v.as_str());

    let rows = match mk_db::user::search_users(pool, query, me_id.as_deref(), origin, limit, offset)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_search", error = %e, "search_users");
            return api_err(ApiError::internal_error());
        }
    };

    let packed = match pack_search_rows(state, pool, rows, me_id.as_deref()).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    (StatusCode::OK, Json(json!(packed))).into_response()
}

/// Shared note-packing for the M1 slice: user row + profile → UserLite value.
async fn note_user_lite(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    user_id: &str,
) -> Result<Value, Response> {
    let row = match mk_db::user::find_user_by_id(pool, user_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "note find_user_by_id");
            return Err(api_err(ApiError::internal_error()));
        }
    };
    let Some(row) = row else {
        return Err(api_err(ApiError::new(
            404,
            mk_types::api_error::ApiErrorKind::Client,
            "NO_SUCH_USER",
            "4362f8dc-731f-4ad8-a694-be5a88922a24",
            "No such user.",
        )));
    };
    let profile = match mk_db::user::find_profile(pool, user_id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "note find_profile");
            return Err(api_err(ApiError::internal_error()));
        }
    };
    let ctx = mk_core::services::user_pack::InstanceCtx {
        url: &state.config.url,
        host: &state.config.host(),
    };
    Ok(mk_core::services::user_pack::user_lite_value(
        &row, &profile, ctx,
    ))
}

/// `POST /api/notes/create` — M1 subset: text (+optional visibility/replyId),
/// credential required (401 shapes shared with i_route flow via resolve_me).
async fn notes_create(state: &AppState, body: &Value, token: Option<String>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let Some(me_id) = resolve_me(pool, token.as_deref()).await else {
        return api_err(ApiError::new(
            401,
            mk_types::api_error::ApiErrorKind::Client,
            "AUTHENTICATION_FAILED",
            "b0a7f5f8-dc2f-4171-b91f-de88ad238e14",
            "Authentication failed. Please ensure your token is correct.",
        ));
    };

    // social-agent: renote path — upstream notes/create renoteId param with
    // NoteCreateService renote rules (textless row; M2 local-only; the
    // target note's notifications stay with the notifications agent).
    if body.get("renoteId").and_then(|v| v.as_str()).is_some() {
        return social::notes_create_renote(state, body, &me_id).await;
    }

    let text = match body.get("text").and_then(|v| v.as_str()) {
        Some(t) if !t.is_empty() && t.chars().count() <= 3000 => t,
        _ => {
            return api_err(ApiError::invalid_param(json!({
                "param": "text",
                "reason": "required, 1..3000 chars in M1 (poll/file notes land in M2)",
            })));
        }
    };
    let visibility = match body.get("visibility").and_then(|v| v.as_str()) {
        None | Some("public") => "public",
        Some(v @ ("home" | "followers" | "specified")) => v,
        Some(_) => {
            return api_err(ApiError::invalid_param(json!({
                "param": "visibility",
                "reason": "must be one of public/home/followers/specified",
            })));
        }
    };
    let reply_id = body.get("replyId").and_then(|v| v.as_str());

    let id = mk_types::id::gen_aid_now();
    let hashtags = mk_core::services::misc::extract_hashtags(text);
    if let Err(e) = mk_db::note::insert_note(
        pool,
        &mk_db::note::NewNote {
            id: &id,
            text: Some(text),
            user_id: &me_id,
            visibility,
            reply_id,
            hashtags: &hashtags,
        },
    )
    .await
    {
        tracing::error!(target: "dbg_notes", error = %e, "insert_note");
        return api_err(ApiError::internal_error());
    }
    if let Err(e) = mk_db::note::credit_note_to_user(pool, &me_id, &hashtags).await {
        tracing::error!(target: "dbg_notes", error = %e, "credit_note_to_user");
    }

    // NoteCreateService non-AP notify branch for reply: the replied-to
    // author (local, not self) gets a 'reply' notification when the target
    // still exists.
    if let Some(rid) = reply_id {
        if let Ok(Some(target)) = mk_db::note::find_note_by_id(pool, rid).await {
            social::notify_note_event(pool, &target.user_id, &me_id, "reply", &id).await;
        }
    }
    // Mentions: every resolved local username distinct from the author.
    for (username, host) in mk_core::services::misc::extract_mentions(text) {
        if host.is_some() {
            continue; // remote mentions need AP resolution — TODO(M4)
        }
        if let Ok(Some(target)) =
            mk_db::user::find_local_user_by_username(pool, &username.to_lowercase()).await
        {
            if target.id != me_id {
                social::notify_note_event(pool, &target.id, &me_id, "mention", &id).await;
            }
        }
    }

    let note = match pack_note(state, pool, &id).await {
        Ok(n) => n,
        Err(r) => return *r,
    };
    (StatusCode::OK, Json(json!({ "createdNote": note }))).into_response()
}

/// M1 note packing pipeline (row + author lite + derived createdAt).
async fn pack_note(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    note_id: &str,
) -> Result<Value, Box<Response>> {
    let row = match mk_db::note::find_note_by_id(pool, note_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "find_note_by_id");
            return Err(Box::new(api_err(ApiError::internal_error())));
        }
    };
    let Some(row) = row else {
        return Err(Box::new(api_err(ApiError::new(
            404,
            mk_types::api_error::ApiErrorKind::Client,
            "NO_SUCH_NOTE",
            "24fcbfc6-2e37-42b6-8388-c29b3861a08d",
            "No such note.",
        ))));
    };
    let user_lite = match note_user_lite(state, pool, &row.user_id).await {
        Ok(v) => v,
        Err(r) => return Err(Box::new(r)),
    };
    let created_at = mk_types::id::parse_aid_date(&row.id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default();
    // populated emoji URLs map is M2; the row field is the raw emoji-name array
    let emojis = json!({});
    Ok(mk_core::packers::note::note_pack_min(
        &row,
        user_lite,
        created_at,
        &row.mentions,
        &row.tags,
        emojis,
    ))
}

/// `POST /api/notes/show` — noteId lookup.
async fn notes_show(state: &AppState, body: &Value) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let Some(note_id) = body.get("noteId").and_then(|v| v.as_str()) else {
        return api_err(ApiError::invalid_param(json!({
            "param": "noteId",
            "reason": "required",
        })));
    };
    match pack_note(state, pool, note_id).await {
        Ok(n) => (StatusCode::OK, Json(n)).into_response(),
        Err(r) => *r,
    }
}

// ---------------------------------------------------------------------------
// notifications slice (M2) — truthful subset of upstream
// endpoints/i/notifications.ts (pinned upstream keeps the `i/` namespacing;
// the coordinator-facing name is `notifications`) + NotificationEntityService.
// ---------------------------------------------------------------------------

/// Optional nullable integer param (`{ type: 'integer' }`).
fn opt_int_param(body: &Value, key: &str) -> Result<Option<i64>, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_i64() {
            Some(n) => Ok(Some(n)),
            None => Err(invalid_param(key, "expected an integer")),
        },
    }
}

/// Optional nullable string param for misskey:id Cursors.
fn opt_id_param<'a>(body: &'a Value, key: &str) -> Result<Option<&'a str>, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(invalid_param(key, "expected a string")),
    }
}

/// `POST /api/notifications` — requireCredential list + mark-as-read flow.
/// Upstream order: fetch → `readAllNotification` when `markAsRead` (default
/// true) → pack. Params: limit (1..=100, default 10), sinceId/untilId
/// (exclusive aid cursors), markAsRead, unreadOnly, includeTypes ([] → []
/// short-circuit BEFORE marking read; unknown type strings tolerated as
/// no-ops in the pinned upstream). `following` not ported (TODO(M2+):
/// needs a following-table join). Stored types outside this slice's
/// producible set (mention/reply/renote/reaction/follow) degrade to the
/// id/createdAt/type/userId/user shape.
async fn notifications_route(state: &AppState, body: &Value, token: Option<String>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };

    // requireCredential chain, mirrors i_route's call() order.
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
        return authentication_failed();
    }
    let Some(me_id) = resolve_me(pool, Some(token.as_str())).await else {
        return authentication_failed();
    };
    match mk_db::user::find_user_by_id(pool, &me_id).await {
        Ok(Some(row)) => {
            if row.is_suspended {
                return api_err(ApiError::new(
                    403,
                    mk_types::api_error::ApiErrorKind::Permission,
                    "YOUR_ACCOUNT_SUSPENDED",
                    "a8c724b3-6e9c-4b46-b1a8-bc3ed6258370",
                    "Your account has been suspended.",
                ));
            }
        }
        Ok(None) => return authentication_failed(),
        Err(e) => {
            tracing::error!(target: "dbg_notifications", error = %e, "find_user_by_id");
            return api_err(ApiError::internal_error());
        }
    }

    // paramDef: limit minimum 1, maximum 100, default 10.
    // Helpers return Result<_, Response>; this handler returns Response, so
    // parse in a scoped block and unwrap the first error (lease-safe: no `?`).
    let (limit, since_id, until_id, mark_as_read, unread_only) = {
        let limit = opt_int_param(body, "limit").map(|v| v.unwrap_or(10).clamp(1, 100));
        let since_id = opt_id_param(body, "sinceId").map(|v| v.map(String::from));
        let until_id = opt_id_param(body, "untilId").map(|v| v.map(String::from));
        let mark_as_read = opt_bool(body, "markAsRead").map(|v| v.unwrap_or(true));
        let unread_only = opt_bool(body, "unreadOnly").map(|v| v.unwrap_or(false));
        match (limit, since_id, until_id, mark_as_read, unread_only) {
            (Ok(l), Ok(s), Ok(u), Ok(m), Ok(r)) => (l, s, u, m, r),
            (Err(resp), _, _, _, _)
            | (_, Err(resp), _, _, _)
            | (_, _, Err(resp), _, _)
            | (_, _, _, Err(resp), _)
            | (_, _, _, _, Err(resp)) => return resp,
        }
    };
    let include_types: Vec<String> = match body.get("includeTypes") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        Some(_) => return invalid_param("includeTypes", "expected an array of strings"),
    };

    // upstream: empty includeTypes queries nothing and skips mark-as-read.
    if body
        .get("includeTypes")
        .is_some_and(|v| v.as_array().is_some_and(|a| a.is_empty()))
    {
        return (StatusCode::OK, Json(json!([]))).into_response();
    }

    let rows = match mk_db::notification::list_notifications(
        pool,
        &me_id,
        since_id.as_deref(),
        until_id.as_deref(),
        limit,
        unread_only,
        &include_types,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_notifications", error = %e, "list_notifications");
            return api_err(ApiError::internal_error());
        }
    };

    if mark_as_read {
        if let Err(e) = mk_db::notification::mark_as_read_all(pool, &me_id).await {
            tracing::error!(target: "dbg_notifications", error = %e, "mark_as_read_all");
        }
    }

    let cfg_url = state.config.url.clone();
    let cfg_host = state.config.host();

    let mut out: Vec<Value> = Vec::with_capacity(rows.len());
    for n in &rows {
        // Note-bound types: a deleted note hides the notification (upstream
        // #packInternal "if the note has been deleted, don't show this").
        let note_val = if mk_core::services::notification_pack::is_note_bound(&n.r#type) {
            let Some(note_id) = n.note_id.as_deref() else {
                continue;
            };
            let Some(nrow) = (match mk_db::note::find_note_by_id(pool, note_id).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(target: "dbg_notifications", error = %e, "find_note_by_id");
                    return api_err(ApiError::internal_error());
                }
            }) else {
                continue;
            };
            let Ok(lite) = note_user_lite(state, pool, &nrow.user_id).await else {
                // deleted author → pack null upstream → notification dropped
                continue;
            };
            let note_created = mk_types::id::parse_aid_date(&nrow.id)
                .map(mk_core::services::signup::ms_to_iso)
                .unwrap_or_default();
            Some(mk_core::packers::note::note_pack_min(
                &nrow,
                lite,
                note_created,
                &nrow.mentions,
                &nrow.tags,
                json!({}),
            ))
        } else {
            None
        };

        // Notifier: deleted or suspended users hide the notification
        // (upstream #validateNotifier: isSuspended → false).
        let lite_val = if let Some(notifier_id) = n.notifier_id.as_deref() {
            let Some(urow) = (match mk_db::user::find_user_by_id(pool, notifier_id).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(target: "dbg_notifications", error = %e, "find_user_by_id[notifier]");
                    return api_err(ApiError::internal_error());
                }
            }) else {
                continue;
            };
            if urow.is_suspended {
                continue;
            }
            let Ok(profile) = mk_db::user::find_profile(pool, notifier_id).await else {
                continue;
            };
            let ctx = mk_core::services::user_pack::InstanceCtx {
                url: &cfg_url,
                host: &cfg_host,
            };
            Some(mk_core::services::user_pack::user_lite_value(
                &urow, &profile, ctx,
            ))
        } else {
            None
        };

        let created_at = mk_types::id::parse_aid_date(&n.id)
            .map(mk_core::services::signup::ms_to_iso)
            .unwrap_or_default();
        out.push(mk_core::services::notification_pack::pack_notification(
            n, lite_val, note_val, created_at,
        ));
    }

    (StatusCode::OK, Json(json!(out))).into_response()
}

#[cfg(test)]
mod tests;

/// `/api/admin/*` family (M2 slice). Module file `api/admin.rs` owns the
/// endpoint table, the ApiCallService-style auth chain (credential gate →
/// suspended gate → requireModerator/requireAdmin ROLE_PERMISSION_DENIED
/// gates with the meta.rootUserId bypass) and every admin handler; mod.rs
/// only registers the module and the single `admin/*` dispatch arm above.
mod admin;
