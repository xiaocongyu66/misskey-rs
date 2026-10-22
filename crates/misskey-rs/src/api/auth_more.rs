//! `auth/*` slice (upstream `endpoints/auth/session/show.ts` over
//! `core/entities/{AuthSessionEntityService,AppEntityService}.ts`; SQL in
//! `mk-db/src/auth.rs`).
//!
//! Gate: `auth/session/show` is `requireCredential: false` (guest-ok) and
//! takes the session `token` as a body param, so it does not read the
//! request credential at all.
//!
//! Skipped, documented:
//! - `auth/session/userkey` needs the app-secret handshake (`appSecret` +
//!   session token), an access-token lookup/delete and a
//!   `UserDetailedNotMe` pack.
//! - `auth/accept` is the OAuth/MiAuth consent step: it mints a
//!   secure-random access token, hashes it with the app secret, generates an
//!   AID and inserts `access_token`, then stamps `auth_session.userId`.
//! Both stay unhandled so they reach the unknown-endpoint arm.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

use mk_types::api_error::ApiErrorKind;

/// Endpoint names owned by this module (single source of truth for the auth
/// dispatch arm in mod.rs).
pub(crate) const AUTH_ENDPOINTS: &[&str] = &["auth/session/show"];

pub(crate) fn handles(name: &str) -> bool {
    AUTH_ENDPOINTS.contains(&name)
}

fn invalid_param(param: &str, reason: &str) -> Response {
    api_err(ApiError::invalid_param(json!({
        "param": param,
        "reason": reason,
    })))
}

fn no_such_session() -> Response {
    api_err(ApiError::new(
        400,
        ApiErrorKind::Client,
        "NO_SUCH_SESSION",
        "bd72c97d-eba7-4adb-a467-f171b8847250",
        "No such session.",
    ))
}

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

/// Entry from the auth dispatch arm in mod.rs (body pre-parsed).
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
        "auth/session/show" => session_show(pool, body).await,
        _ => Ok(api_err(ApiError::unknown_endpoint())),
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

async fn session_show(pool: &mk_db::sqlx::PgPool, body: &Value) -> Result<Response, Response> {
    let token = body
        .get("token")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_param("token", "expected a string"))?;
    let session = mk_db::auth::find_session_by_token(pool, token)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_auth", error = %e, "auth/session/show");
            internal()
        })?;
    let Some(session) = session else {
        return Err(no_such_session());
    };
    // `AuthSessionEntityService.pack` → { id, app: AppEntityService.pack,
    // token }; the app embed omits secret/description.
    let out = json!({
        "id": session.id,
        "app": {
            "id": session.app_id,
            "name": session.app_name,
            "callbackUrl": session.app_callback_url,
            "permission": session.app_permission,
        },
        "token": session.token,
    });
    Ok((StatusCode::OK, Json(out)).into_response())
}
