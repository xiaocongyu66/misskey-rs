//! `/streaming` authentication lane (upstream `server/api` ws hub +
//! `miauth/gen-token`): the access-token rows the streaming.ts
//! Authentication test rides.
//!
//! - `authenticate_hub` — `StreamingApiServerService` L42-78 +
//!   `AuthenticateService.authenticate`: a 16-char native token resolves
//!   against user.token; any other token looks up the `access_token` table
//!   (token/hash equality — gen-token writes both) and returns the row's
//!   `permission` array. Upstream additionally refuses app tokens without
//!   `read:account` right at the hub (`!app.permission.some(p => p ===
//!   'read:account')` → AuthenticationError → bare 401); that gate is folded
//!   here, so `None` means "the upgrade path 401s". Unknown tokens resolve
//!   None (AuthenticationError), and guest (no-token) connections are refused
//!   by this port's auth-only hub although upstream admits them to public
//!   channels — no pinned census leg rides a guest /streaming socket.
//! - ` miauth/gen-token` (`endpoints/miauth/gen-token.ts`): requireCredential
//!   + `{ session, permission }` paramDef (permission: unique string array),
//!   generating a 32-char secureRndstr access token where `token` = `hash`.
//!   Deviation: the `createToken` notification leg is not fired — the
//!   notification lane owns that table and no streaming.ts census leg reads
//!   it.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use mk_db::sqlx::PgPool;
use serde_json::{Value, json};

use super::invalid_param;
use crate::error::{ApiError, api_err};

/// The dispatch-arm gate (announcement.rs pattern) — this module owns only
/// the miauth name.
pub(crate) fn handles(name: &str) -> bool {
    name == "miauth/gen-token"
}

/// The self-contained dispatch arm (roles_lite/delete_account convention):
/// method check + body parse + token extraction live in mod.rs.
pub(crate) async fn handle(
    state: &crate::AppState,
    _name: &str,
    body: &Value,
    token: Option<String>,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    miauth_gen_token(pool, body, token).await
}

/// `POST /api/miauth/gen-token` — requireCredential (endpoints.ts
/// `requireCredential: true`): no token → CREDENTIAL_REQUIRED 401;
/// unresolvable/foreign-shaped token → AUTHENTICATION_FAILED 401.
async fn miauth_gen_token(pool: &PgPool, body: &Value, token: Option<String>) -> Response {
    // ApiCallService auth chain (mirrors notifications_route's gates).
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
        return super::authentication_failed();
    }
    let Some(me_id) = super::resolve_me(pool, Some(token.as_str())).await else {
        return super::authentication_failed();
    };

    // paramDef: `session` (string, nullable, required) + `permission`
    // (unique string array, required); name/description/iconUrl optional
    // strings.
    let Some(session) = body.get("session") else {
        return api_err(ApiError::invalid_param(json!({
            "param": "session",
            "reason": "required",
        })));
    };
    if !session.is_null() && session.as_str().is_none() {
        return invalid_param("session", "must be a string");
    }
    let Some(permissions) = body.get("permission").and_then(Value::as_array) else {
        return api_err(ApiError::invalid_param(json!({
            "param": "permission",
            "reason": "required",
        })));
    };
    let mut perms: Vec<String> = Vec::new();
    for p in permissions {
        match p.as_str() {
            Some(s) => {
                // uniqueItems: true
                if !perms.iter().any(|x| x == s) {
                    perms.push(s.to_string());
                }
            }
            None => return invalid_param("permission", "must be an array of strings"),
        }
    }

    // secureRndstr(32) — the same alphabet/length upstream (test utils then
    // round-trip it straight back through `?i=`).
    let access_token = mk_core::services::signup::secure_rndstr(32);
    let id = mk_types::id::gen_aid_now();
    let opt_str = |k: &str| body.get(k).and_then(Value::as_str).map(String::from);
    let result = mk_db::sqlx::query(
        r#"INSERT INTO "access_token"
           ("id", "token", "hash", "userId", "lastUsedAt", "session", "name", "description", "iconUrl", "permission", "fetched")
           VALUES ($1, $2, $3, $4, now(), $5, $6, $7, $8, $9, false)"#,
    )
    .bind(&id)
    .bind(&access_token)
    .bind(&access_token)
    .bind(&me_id)
    .bind(session.as_str())
    .bind(opt_str("name"))
    .bind(opt_str("description"))
    .bind(opt_str("iconUrl"))
    .bind(&perms)
    .execute(pool)
    .await;
    if let Err(e) = result {
        tracing::error!(target: "dbg_stream_auth", error = %e, "access_token insert");
        return api_err(ApiError::internal_error());
    }
    (StatusCode::OK, axum::Json(json!({ "token": access_token }))).into_response()
}

/// One authenticated websocket identity: the resolved user plus the app
/// permission list when the token was an access-token row (`Some(None)` is
/// not produced — native tokens carry no app).
pub(crate) struct HubAuth {
    pub user_id: String,
    /// `Some(perms)` when the token is an access-token row (app token).
    pub app_permission: Option<Vec<String>>,
}

/// `StreamingApiServerService.onUpgrade` authenticate: native 16-char tokens
/// first (AuthenticateService isNativeUserToken arm), then the access_token
/// table; an app token without `read:account` raises upstream's
/// AuthenticationError → `None` → the hub's bare 401 (L64-72).
pub(crate) async fn authenticate_hub(pool: &PgPool, token: Option<&str>) -> Option<HubAuth> {
    let Some(t) = token else {
        // authenticate(null) succeeds upstream ([null, null]) and lets the
        // socket in as a guest; this port's hub is auth-only (see the entry
        // comment), so guests stay refused here like every pre-stream slice.
        return None;
    };
    if t.len() == 16 {
        match mk_db::user::find_local_user_id_by_token(pool, t).await {
            Ok(Some(user_id)) => {
                return Some(HubAuth {
                    user_id,
                    app_permission: None,
                });
            }
            Ok(None) => return None,
            Err(e) => {
                tracing::error!(target: "dbg_stream_auth", error = %e, "native hub auth");
                return None;
            }
        }
    }
    // AccessToken row lookup — AuthenticateService matches the token then
    // falls back to the hash column (gen-token writes both + fetched=false).
    let row: Option<(String, Vec<String>)> = mk_db::sqlx::query_as(
        r#"SELECT "userId", "permission" FROM "access_token"
           WHERE "token" = $1 OR "hash" = $1
           ORDER BY "id" DESC LIMIT 1"#,
    )
    .bind(t)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let (user_id, permission) = row?;
    // Hub-level scope gate (StreamingApiServerService L64-66).
    if !permission.iter().any(|p| p == "read:account") {
        return None;
    }
    Some(HubAuth {
        user_id,
        app_permission: Some(permission),
    })
}
