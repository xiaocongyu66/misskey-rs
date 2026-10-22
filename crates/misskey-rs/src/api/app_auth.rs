//! `app/*`, `my/apps`, `i/apps` and the app-session auth plane
//! (`auth/session/{generate,userkey}`, `auth/accept`, `i/regenerate-token`,
//! `i/change-password`).
//!
//! Upstream sources: `endpoints/app/{create,show}.ts`,
//! `endpoints/{my,i}/apps.ts`, `endpoints/auth/session/{generate,userkey}.ts`,
//! `endpoints/auth/accept.ts`, `endpoints/i/{regenerate-token,
//! change-password}.ts`, `core/entities/AppEntityService.ts`. SQL lives in
//! `mk-db/src/app.rs`; the 0001 baseline ships `app`, `auth_session` and
//! `access_token`.
//!
//! Notes:
//! - `AppEntityService.pack` never exposes `description`; `detail` only
//!   gates `isAuthorized` (when `me` is passed) and `includeSecret` the
//!   `secret` field.
//! - `app/create` is guest-ok (`requireCredential: false`): a native token
//!   merely records `userId`, an invalid `i` still fails authentication.
//! - `app/show` includes `secret` only for a native-token owner
//!   (`isSecure = user != null && token == null` upstream; this port's
//!   resolver accepts native 16-char tokens only).
//! - `i/apps` lists access tokens (not apps) per upstream, joined with the
//!   app for name/permission/description fallbacks.
//! - `auth/accept` mints sha256(token + app.secret) into `access_token` when
//!   no row exists and stamps `auth_session.userId`; it is `secure: true`.
//! - `i/regenerate-token` / `i/change-password` verify the password with
//!   bcrypt (upstream throws a plain Error → 500 on mismatch).

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::error::{ApiError, api_err};
use mk_types::api_error::ApiErrorKind;

/// Endpoint names routed into this module (single source of truth for the
/// `// app-auth` dispatch arm in mod.rs).
pub(crate) const ENDPOINTS: &[&str] = &[
    "app/create",
    "app/show",
    "my/apps",
    "i/apps",
    "auth/session/generate",
    "auth/session/userkey",
    "auth/accept",
    "i/regenerate-token",
    "i/change-password",
];

pub(crate) fn handles(name: &str) -> bool {
    ENDPOINTS.contains(&name)
}

/// Entry from the app-auth dispatch arm in mod.rs (body pre-parsed, token +
/// credential-declared probe pre-extracted).
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
    let result: Result<Response, Response> = match name {
        "app/create" => app_create(pool, body, token.as_deref(), credential_declared).await,
        "app/show" => app_show(pool, body, token.as_deref(), credential_declared).await,
        "my/apps" => my_apps(pool, body, token.as_deref(), credential_declared).await,
        "i/apps" => i_apps(pool, body, token.as_deref(), credential_declared).await,
        "auth/session/generate" => session_generate(state, pool, body).await,
        "auth/session/userkey" => session_userkey(state, pool, body).await,
        "auth/accept" => auth_accept(pool, body, token.as_deref(), credential_declared).await,
        "i/regenerate-token" => {
            regenerate_token(pool, body, token.as_deref(), credential_declared).await
        }
        "i/change-password" => {
            change_password(state, pool, body, token.as_deref(), credential_declared).await
        }
        _ => Ok(api_err(ApiError::unknown_endpoint())),
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

type Check<T = Response> = Result<T, Response>;

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn db_err(e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_app_auth", error = %e, "db error");
    internal()
}

fn auth_failed() -> Response {
    api_err(ApiError::new(
        401,
        ApiErrorKind::Client,
        "AUTHENTICATION_FAILED",
        "b0a7f5f8-dc2f-4171-b91f-de88ad238e14",
        "Authentication failed. Please ensure your token is correct.",
    ))
}

/// Meta error without httpStatusCode: kind 'client' → default 400.
fn meta_err(code: &str, id: &str, message: &str) -> Response {
    api_err(ApiError::new(400, ApiErrorKind::Client, code, id, message))
}

fn invalid_param(param: &str, reason: &str) -> Response {
    api_err(ApiError::invalid_param(json!({
        "param": param,
        "reason": reason,
    })))
}

fn no_such_app(id: &str) -> Response {
    meta_err("NO_SUCH_APP", id, "No such app.")
}

fn no_such_session(id: &str) -> Response {
    meta_err("NO_SUCH_SESSION", id, "No such session.")
}

fn pending_session() -> Response {
    meta_err(
        "PENDING_SESSION",
        "8c8a4145-02cc-4cca-8e66-29ba60445a8e",
        "This session is not completed yet.",
    )
}

fn req_str<'a>(body: &'a Value, key: &str) -> Check<&'a str> {
    match body.get(key) {
        Some(Value::String(s)) => Ok(s),
        Some(_) => Err(invalid_param(key, "must be a string")),
        None => Err(invalid_param(key, "must have required property")),
    }
}

fn opt_str(body: &Value, key: &str) -> Check<Option<String>> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(invalid_param(key, "must be a string or null")),
    }
}

fn req_str_array(body: &Value, key: &str) -> Check<Vec<String>> {
    match body.get(key) {
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(s) => out.push(s.to_string()),
                    None => return Err(invalid_param(key, "must be an array of strings")),
                }
            }
            Ok(out)
        }
        Some(_) => Err(invalid_param(key, "must be an array")),
        None => Err(invalid_param(key, "must have required property")),
    }
}

fn opt_int(body: &Value, key: &str, default: i64) -> Check<i64> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v
            .as_i64()
            .ok_or_else(|| invalid_param(key, "must be an integer")),
    }
}

/// Guest-ok endpoints: no credential → None; a declared-but-unresolvable
/// credential still fails authentication (ApiCallService ordering).
async fn optional_me(
    pool: &mk_db::sqlx::PgPool,
    token: Option<&str>,
    credential_declared: bool,
) -> Check<Option<String>> {
    match token {
        None => {
            if credential_declared {
                return Err(auth_failed());
            }
            Ok(None)
        }
        Some(t) => {
            if t.len() != 16 {
                return Err(auth_failed());
            }
            match super::resolve_me(pool, Some(t)).await {
                Some(id) => Ok(Some(id)),
                None => Err(auth_failed()),
            }
        }
    }
}

/// `AppEntityService.pack`: id/name/callbackUrl/permission (+ secret /
/// isAuthorized per the options).
async fn pack_app(
    pool: &mk_db::sqlx::PgPool,
    app: &mk_db::app::AppRow,
    me: Option<&str>,
    include_secret: bool,
) -> Check<Value> {
    let mut out = json!({
        "id": app.id.clone(),
        "name": app.name.clone(),
        "callbackUrl": app.callback_url.clone(),
        "permission": app.permission.clone(),
    });
    if include_secret {
        out["secret"] = json!(app.secret.clone());
    }
    if let Some(me) = me {
        let count = mk_db::app::count_app_tokens(pool, &app.id, me)
            .await
            .map_err(db_err)?;
        out["isAuthorized"] = json!(count > 0);
    }
    Ok(out)
}

/// `permission.map(v => v.replace(/^(.+)(\/|-)(read|write)$/, '$3:$1'))`
/// + `unique` (order-preserving).
fn normalize_permission(perms: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in perms {
        let normalized = if let Some(prefix) = p
            .strip_suffix("/read")
            .or_else(|| p.strip_suffix("/write"))
        {
            let verb = if p.ends_with("/read") { "read" } else { "write" };
            if prefix.is_empty() {
                p.clone()
            } else {
                format!("{verb}:{prefix}")
            }
        } else if let Some(prefix) = p
            .strip_suffix("-read")
            .or_else(|| p.strip_suffix("-write"))
        {
            let verb = if p.ends_with("-read") { "read" } else { "write" };
            if prefix.is_empty() {
                p.clone()
            } else {
                format!("{verb}:{prefix}")
            }
        } else {
            p.clone()
        };
        if !out.contains(&normalized) {
            out.push(normalized);
        }
    }
    out
}

fn iso_from_id(id: &str) -> String {
    mk_types::id::parse_aid_date(id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default()
}

/// `randomUUID()` stand-in: a UUID-shaped opaque token built from
/// secureRndstr(32) (the alphabet is lowercase alnum, not strictly hex, but
/// it is the same 32 random chars upstream consumes).
fn pseudo_uuid() -> String {
    let s = mk_core::services::signup::secure_rndstr(32);
    format!(
        "{}-{}-{}-{}-{}",
        &s[0..8],
        &s[8..12],
        &s[12..16],
        &s[16..20],
        &s[20..32]
    )
}

// ---------------------------------------------------------------------------
// app/create + app/show
// ---------------------------------------------------------------------------

async fn app_create(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    token: Option<&str>,
    credential_declared: bool,
) -> Check<Response> {
    let me = optional_me(pool, token, credential_declared).await?;
    let name = req_str(body, "name")?;
    let description = opt_str(body, "description")?.unwrap_or_default();
    let permission = normalize_permission(req_str_array(body, "permission")?);
    let callback_url = opt_str(body, "callbackUrl")?;

    let secret = mk_core::services::signup::secure_rndstr(32);
    let id = mk_types::id::gen_aid_now();
    mk_db::app::insert_app(
        pool,
        &id,
        me.as_deref(),
        &secret,
        name,
        &description,
        &permission,
        callback_url.as_deref(),
    )
    .await
    .map_err(db_err)?;

    let app = mk_db::app::find_app_by_id(pool, &id)
        .await
        .map_err(db_err)?
        .ok_or_else(internal)?;
    // appEntityService.pack(app, null, { detail: true, includeSecret: true })
    let packed = pack_app(pool, &app, None, true).await?;
    Ok((StatusCode::OK, Json(packed)).into_response())
}

async fn app_show(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    token: Option<&str>,
    credential_declared: bool,
) -> Check<Response> {
    let me = optional_me(pool, token, credential_declared).await?;
    let app_id = req_str(body, "appId")?;
    let Some(app) = mk_db::app::find_app_by_id(pool, app_id)
        .await
        .map_err(db_err)?
    else {
        return Err(no_such_app("dce83913-2dc6-4093-8a7b-71dbb11718a3"));
    };
    // isSecure = user != null && token == null (native token only); the
    // secret is exposed only to the app owner.
    let include_secret = match me.as_deref() {
        Some(me) => app.user_id.as_deref() == Some(me),
        None => false,
    };
    let packed = pack_app(pool, &app, me.as_deref(), include_secret).await?;
    Ok((StatusCode::OK, Json(packed)).into_response())
}

// ---------------------------------------------------------------------------
// my/apps + i/apps
// ---------------------------------------------------------------------------

async fn my_apps(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    token: Option<&str>,
    credential_declared: bool,
) -> Check<Response> {
    let me = super::social::require_checked_user(pool, token, credential_declared).await?;
    let limit = opt_int(body, "limit", 10)?;
    if !(1..=100).contains(&limit) {
        return Err(invalid_param(
            "limit",
            "must be an integer between 1 and 100",
        ));
    }
    let offset = opt_int(body, "offset", 0)?;
    let apps = mk_db::app::list_apps_by_user(pool, &me, limit, offset)
        .await
        .map_err(db_err)?;
    let mut out = Vec::with_capacity(apps.len());
    for app in &apps {
        // pack(app, me, { detail: true }) — isAuthorized included, no secret.
        out.push(pack_app(pool, app, Some(&me), false).await?);
    }
    Ok((StatusCode::OK, Json(Value::Array(out))).into_response())
}

async fn i_apps(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    token: Option<&str>,
    credential_declared: bool,
) -> Check<Response> {
    let me = super::social::require_checked_user(pool, token, credential_declared).await?;
    let sort = match body.get("sort") {
        None | Some(Value::Null) => None,
        Some(Value::String(s))
            if matches!(
                s.as_str(),
                "+createdAt" | "-createdAt" | "+lastUsedAt" | "-lastUsedAt"
            ) =>
        {
            Some(s.as_str())
        }
        Some(_) => {
            return Err(invalid_param(
                "sort",
                "must be one of +createdAt, -createdAt, +lastUsedAt, -lastUsedAt",
            ));
        }
    };
    let rows = mk_db::app::list_app_tokens_by_user(pool, &me, sort)
        .await
        .map_err(db_err)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let mut item = json!({
            "id": row.id.clone(),
            "createdAt": iso_from_id(&row.id),
            "permission": row.app_permission.clone().unwrap_or(row.permission.clone()),
            "iconUrl": row.icon_url.clone(),
            "description": row.description.clone().or(row.app_description.clone()),
        });
        if let Some(name) = row.name.clone().or(row.app_name.clone()) {
            item["name"] = json!(name);
        }
        if let Some(last_used) = row.last_used_at.clone() {
            item["lastUsedAt"] = json!(last_used);
        }
        out.push(item);
    }
    Ok((StatusCode::OK, Json(Value::Array(out))).into_response())
}

// ---------------------------------------------------------------------------
// auth/session/generate + userkey + auth/accept
// ---------------------------------------------------------------------------

async fn session_generate(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
) -> Check<Response> {
    let app_secret = req_str(body, "appSecret")?;
    let Some(app) = mk_db::app::find_app_by_secret(pool, app_secret)
        .await
        .map_err(db_err)?
    else {
        return Err(no_such_app("92f93e63-428e-4f2f-a5a4-39e1407fe998"));
    };
    let token = pseudo_uuid();
    let id = mk_types::id::gen_aid_now();
    mk_db::app::insert_session(pool, &id, &token, &app.id)
        .await
        .map_err(db_err)?;
    // config.authUrl = `${scheme}://${host}/auth`
    let url = format!(
        "{}/auth/{}",
        state.config.url.trim_end_matches('/'),
        token
    );
    Ok((StatusCode::OK, Json(json!({ "token": token, "url": url }))).into_response())
}

async fn session_userkey(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
) -> Check<Response> {
    let app_secret = req_str(body, "appSecret")?;
    let token = req_str(body, "token")?;
    let Some(app) = mk_db::app::find_app_by_secret(pool, app_secret)
        .await
        .map_err(db_err)?
    else {
        return Err(no_such_app("fcab192a-2c5a-43b7-8ad8-9b7054d8d40d"));
    };
    let Some(session) = mk_db::app::find_session_by_token_app(pool, token, &app.id)
        .await
        .map_err(db_err)?
    else {
        return Err(no_such_session("5b5a1503-8bc8-4bd0-8054-dc189e8cdcb3"));
    };
    let Some(user_id) = session.user_id.clone() else {
        return Err(pending_session());
    };
    // findOneByOrFail: a completed session always has its access-token row.
    let Some(access_token) = mk_db::app::find_app_access_token(pool, &app.id, &user_id)
        .await
        .map_err(db_err)?
    else {
        return Err(internal());
    };
    mk_db::app::delete_session(pool, &session.id)
        .await
        .map_err(db_err)?;
    let user = super::freq::pack_user_detailed(state, pool, &user_id, None).await?;
    let Some(user) = user else {
        return Err(internal());
    };
    Ok((StatusCode::OK, Json(json!({
        "accessToken": access_token,
        "user": user,
    })))
    .into_response())
}

async fn auth_accept(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    token: Option<&str>,
    credential_declared: bool,
) -> Check<Response> {
    let me = super::social::require_checked_user(pool, token, credential_declared).await?;
    let session_token = req_str(body, "token")?;
    let Some(session) = mk_db::app::find_session_by_token(pool, session_token)
        .await
        .map_err(db_err)?
    else {
        return Err(no_such_session("9c72d8de-391a-43c1-9d06-08d29efde8df"));
    };

    let exists = mk_db::app::app_access_token_exists(pool, &session.app_id, &me)
        .await
        .map_err(db_err)?;
    if !exists {
        let access_token = mk_core::services::signup::secure_rndstr(32);
        let Some(app) = mk_db::app::find_app_by_id(pool, &session.app_id)
            .await
            .map_err(db_err)?
        else {
            return Err(internal());
        };
        let mut sha = Sha256::new();
        sha.update(access_token.as_bytes());
        sha.update(app.secret.as_bytes());
        let hash = format!("{:x}", sha.finalize());
        let id = mk_types::id::gen_aid_now();
        mk_db::app::insert_app_access_token(pool, &id, &access_token, &hash, &session.app_id, &me)
            .await
            .map_err(db_err)?;
    }
    mk_db::app::update_session_user(pool, &session.id, &me)
        .await
        .map_err(db_err)?;
    // Upstream returns undefined → 204.
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---------------------------------------------------------------------------
// i/regenerate-token + i/change-password
// ---------------------------------------------------------------------------

async fn regenerate_token(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    token: Option<&str>,
    credential_declared: bool,
) -> Check<Response> {
    let me = super::social::require_checked_user(pool, token, credential_declared).await?;
    let password = req_str(body, "password")?;
    let profile = mk_db::user::find_profile(pool, &me)
        .await
        .map_err(db_err)?;
    // bcrypt.compare against a NULL hash throws upstream.
    let Some(hash) = profile.password.as_deref() else {
        return Err(internal());
    };
    let same = mk_core::services::signup::verify_password(password, hash).unwrap_or(false);
    if !same {
        return Err(internal());
    }
    let new_token = mk_core::services::signup::secure_rndstr(16);
    mk_db::app::set_user_token(pool, &me, &new_token)
        .await
        .map_err(db_err)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn change_password(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    token: Option<&str>,
    credential_declared: bool,
) -> Check<Response> {
    let me = super::social::require_checked_user(pool, token, credential_declared).await?;
    let current = req_str(body, "currentPassword")?;
    let new_password = req_str(body, "newPassword")?;
    if new_password.is_empty() {
        return Err(invalid_param("newPassword", "must NOT have fewer than 1 characters"));
    }
    let two_factor_token = opt_str(body, "token")?;

    let profile = mk_db::user::find_profile(pool, &me)
        .await
        .map_err(db_err)?;
    if profile.two_factor_enabled {
        let Some(t) = two_factor_token.as_deref() else {
            return Err(internal());
        };
        let secrets = mk_db::two_factor::fetch_secrets(pool, &me)
            .await
            .map_err(db_err)?;
        let env = mk_db::two_factor::OtpEnv::from_config_test_and_flag(state.config.test);
        if !super::two_factor::two_factor_authenticate(pool, &me, &secrets, t, env).await {
            return Err(internal());
        }
    }

    let Some(hash) = profile.password.as_deref() else {
        return Err(internal());
    };
    if !mk_core::services::signup::verify_password(current, hash).unwrap_or(false) {
        return Err(internal());
    }
    // bcrypt.genSalt(8) + bcrypt.hash
    let new_hash = mk_core::services::signup::hash_password(new_password).map_err(|e| {
        tracing::error!(target: "dbg_app_auth", error = %e, "bcrypt hash");
        internal()
    })?;
    mk_db::user::set_password_hash(pool, &me, &new_hash)
        .await
        .map_err(db_err)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
