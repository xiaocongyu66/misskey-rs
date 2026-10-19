//! api.ts arena — `endpoints/test.ts` paramDef roundtrip (upstream
//! `test/e2e/api.ts` "General validation" suite).
//!
//! Dispatch arm lives in mod.rs (method check + body parse there; the
//! endpoint body here). The `miauth/gen-token` half of this arena is owned
//! by the streaming lane (`api::stream_auth`); the admin/app-token scope
//! gate lives in `api::admin` (AdminCtx.token_permission + mk-db
//! access_token lookups).

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::error::{ApiError, api_err};

/// `Ajv addFormat('misskey:id', /^[a-zA-Z0-9]+$/)` — non-empty ASCII
/// alphanumeric (endpoint-base.ts L20; the api.ts empty-string probe rides
/// exactly the `+` quantifier).
fn is_misskey_id(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric())
}

fn invalid_param_err(param: &str, reason: &str) -> Response {
    api_err(ApiError::invalid_param(json!({
        "param": param,
        "reason": reason,
    })))
}

/// `POST /api/test` — `endpoints/test.ts` (requireCredential: false,
/// tags non-productive). Ajv semantics the suite pins:
/// - `required` (boolean) is the paramDef's sole required key — missing or
///   non-boolean → INVALID_PARAM 400 ("missing require param" / "wrong type").
/// - `id` is `format: 'misskey:id'` — empty string → 400, `8wvhjghbxu` → 200.
/// - `useDefaults: true` fills `default`/`nullableDefault` when absent
///   ("default value" → body.default === 'hello'), an explicit null passes
///   through the nullable `nullableDefault` un-defaulted ("can set null
///   even if it has default value"), and `undefined` never overrides a
///   default ("cannot set undefined if it has default value").
/// The handler echoes `ps` — the ajv-cleaned params object.
pub(super) async fn test_roundtrip(body: &Value) -> Response {
    if !body.is_object() {
        return invalid_param_err("(root)", "must be an object");
    }

    let required = match body.get("required") {
        Some(Value::Bool(b)) => *b,
        Some(_) => return invalid_param_err("required", "must be a boolean"),
        None => return invalid_param_err("required", "required"),
    };

    let mut out = json!({ "required": required });

    if let Some(v) = body.get("string") {
        match v {
            Value::String(s) => out["string"] = json!(s),
            _ => return invalid_param_err("string", "must be a string"),
        }
    }
    if let Some(v) = body.get("id") {
        match v {
            Value::String(s) if is_misskey_id(s) => out["id"] = json!(s),
            Value::String(_) => {
                return invalid_param_err("id", "must be a misskey:id (alphanumeric)");
            }
            _ => return invalid_param_err("id", "must be a string"),
        }
    }
    // `default`: non-nullable, default 'hello' (useDefaults fills undefined).
    match body.get("default") {
        None => out["default"] = json!("hello"),
        Some(Value::String(s)) => out["default"] = json!(s),
        Some(_) => return invalid_param_err("default", "must be a string"),
    }
    // `nullableDefault`: default 'hello', nullable — an explicit null stays
    // null (ajv useDefaults never fires for an existing key).
    match body.get("nullableDefault") {
        None => out["nullableDefault"] = json!("hello"),
        Some(Value::Null) => out["nullableDefault"] = json!(null),
        Some(Value::String(s)) => out["nullableDefault"] = json!(s),
        Some(_) => {
            return invalid_param_err("nullableDefault", "must be a string or null");
        }
    }

    (StatusCode::OK, Json(out)).into_response()
}

fn credential_required() -> Response {
    api_err(ApiError::new(
        401,
        mk_types::api_error::ApiErrorKind::Client,
        "CREDENTIAL_REQUIRED",
        "1384574d-a912-4b81-8601-c7b1c4085df1",
        "Credential required.",
    ))
}

fn authentication_failed() -> Response {
    api_err(ApiError::new(
        401,
        mk_types::api_error::ApiErrorKind::Client,
        "AUTHENTICATION_FAILED",
        "b0a7f5f8-dc2f-4171-b91f-de88ad238e14",
        "Authentication failed. Please ensure your token is correct.",
    ))
}

/// `POST /api/miauth/gen-token` — `endpoints/miauth/gen-token.ts`
/// (requireCredential + secure). The e2e `createAppToken` fixture: validates
/// `session` (required nullable string) + `permission` (required string
/// array), mints a secureRndstr(32) token, inserts the access_token row and
/// returns it. Deviation note: upstream authenticates non-native tokens via
/// the access_token table *then* hits the `secure: true` isSecure check
/// (403 ACCESS_DENIED); app tokens calling gen-token have no e2e leg here,
/// so the native-only auth chain (auth_failed 401) stands in.
pub(super) async fn gen_token(
    state: &crate::AppState,
    body: &Value,
    token: Option<String>,
) -> Response {
    // paramDef: session required (nullable string), permission required
    // (string array); name/description/iconUrl nullable strings.
    let session = match body.get("session") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Null) => None,
        Some(_) => return invalid_param_err("session", "must be a string or null"),
        None => return invalid_param_err("session", "required"),
    };
    let permission = match body.get("permission") {
        Some(Value::Array(items)) => {
            let mut perms = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(s) => perms.push(s.to_string()),
                    None => {
                        return invalid_param_err("permission", "must be an array of strings");
                    }
                }
            }
            perms
        }
        Some(_) => return invalid_param_err("permission", "must be an array"),
        None => return invalid_param_err("permission", "required"),
    };

    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };

    // requireCredential gates (ApiCallService.call order): no credential →
    // CREDENTIAL_REQUIRED; present-but-unresolvable → AUTHENTICATION_FAILED;
    // suspended owner → YOUR_ACCOUNT_SUSPENDED.
    let Some(token) = token else {
        return credential_required();
    };
    if token.len() != 16 {
        return authentication_failed();
    }
    let Some(me_id) = super::resolve_me(pool, Some(token.as_str())).await else {
        return authentication_failed();
    };
    let row = match mk_db::user::find_user_by_id(pool, &me_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_miauth", error = %e, "find_user_by_id");
            return api_err(ApiError::internal_error());
        }
    };
    let Some(row) = row else {
        return authentication_failed();
    };
    if row.is_suspended {
        return api_err(ApiError::new(
            403,
            mk_types::api_error::ApiErrorKind::Permission,
            "YOUR_ACCOUNT_SUSPENDED",
            "a8c724b3-6e9c-4b46-b1a8-bc3ed6258370",
            "Your account has been suspended.",
        ));
    }

    // Mint + insert: token doubles as the hash row (upstream stores the
    // secureRndstr in both columns; lowercase alnum by construction).
    let id = mk_types::id::gen_aid_now();
    let access_token = mk_core::services::signup::secure_rndstr(32);
    if let Err(e) = mk_db::access_token::insert(
        pool,
        &id,
        &access_token,
        &me_id,
        session.as_deref(),
        &permission,
    )
    .await
    {
        tracing::error!(target: "dbg_miauth", error = %e, "access_token insert");
        return api_err(ApiError::internal_error());
    }

    // The createToken notification upstream fires is unasserted in the
    // api.ts mint path — skipped.
    (StatusCode::OK, Json(json!({ "token": access_token }))).into_response()
}
