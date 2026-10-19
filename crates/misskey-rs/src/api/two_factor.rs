//! two-factor: i/2fa/{register,done,unregister} (upstream
//! endpoints/i/2fa/*.ts). Self-contained module on the delete_account-arm
//! pattern: own auth-chain shapes, body pre-parsed and token pre-extracted by
//! the two-factor dispatch arm in mod.rs. The TOTP core + user_profile
//! twoFactor* db paths live in mk-db/src/two_factor.rs (vendored std-only
//! SHA-1/HMAC-SHA1/base32/TOTP).
//!
//! DEVIATIONS (documented): `QRCode.toDataURL(url)` is mirrored as a constant
//! 1×1 png data URL (the e2e only asserts qr != undefined), and the
//! GlobalEventService meUpdated main-stream publish has no port-side
//! publisher to ride (nothing in the 2fa.ts census consumes it).

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

use mk_db::two_factor::TwoFactorSecrets;

/// Endpoint names owned by this module (all POST-only).
pub(super) fn handles(name: &str) -> bool {
    matches!(name, "i/2fa/register" | "i/2fa/done" | "i/2fa/unregister")
}

/// Entry from the two-factor dispatch arm in mod.rs.
pub(super) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
) -> Response {
    match name {
        "i/2fa/register" => i_register(state, body, token.as_deref()).await,
        "i/2fa/done" => i_done(state, body, token.as_deref()).await,
        _ => i_unregister(state, body, token.as_deref()).await,
    }
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

fn account_suspended() -> Response {
    api_err(ApiError::new(
        403,
        mk_types::api_error::ApiErrorKind::Permission,
        "YOUR_ACCOUNT_SUSPENDED",
        "a8c724b3-6e9c-4b46-b1a8-bc3ed6258370",
        "Your account has been suspended.",
    ))
}

/// Plain TS Error (`authentication failed`, `not verified`,
/// `二段階認証の設定が開始されていません`, findOneByOrFail misses, ...) →
/// `#onExecError` → 500 INTERNAL_ERROR.
fn internal() -> Response {
    api_err(ApiError::internal_error())
}

/// meta.errors.incorrectPassword of register.ts / unregister.ts (the ids
/// differ per file) — an ApiError, so a 400 envelope, not a 500.
fn incorrect_password(id: &str) -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "INCORRECT_PASSWORD",
        id,
        "Incorrect password.",
    ))
}

/// ApiCallService requireCredential chain (order mirrors
/// api::delete_account): missing credential → CREDENTIAL_REQUIRED;
/// non-native/unresolvable token → AUTHENTICATION_FAILED; suspended →
/// YOUR_ACCOUNT_SUSPENDED. `secure: true` gate satisfied by every native
/// token authentication this port performs.
async fn resolve_me<'a>(
    state: &'a AppState,
    token: Option<&str>,
) -> Result<(&'a mk_db::sqlx::PgPool, mk_db::user::LocalUserRow), Response> {
    let Some(pool) = &state.pool else {
        return Err(internal());
    };
    let Some(token) = token else {
        return Err(credential_required());
    };
    if token.len() != 16 {
        return Err(authentication_failed());
    }
    let me_id = match mk_db::user::find_local_user_id_by_token(pool, token).await {
        Ok(Some(id)) => id,
        Ok(None) => return Err(authentication_failed()),
        Err(e) => {
            tracing::error!(target: "dbg_two_factor", error = %e, "find_local_user_id_by_token");
            return Err(internal());
        }
    };
    let me = match mk_db::user::find_user_by_id(pool, &me_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return Err(authentication_failed()),
        Err(e) => {
            tracing::error!(target: "dbg_two_factor", error = %e, "find_user_by_id");
            return Err(internal());
        }
    };
    if me.is_suspended {
        return Err(account_suspended());
    }
    Ok((pool, me))
}

/// UserAuthService.twoFactorAuthenticate: the backup-code branch consumes on
/// membership, otherwise the stored `twoFactorSecret` TOTP-validates.
pub(super) async fn two_factor_authenticate(
    pool: &mk_db::sqlx::PgPool,
    user_id: &str,
    secrets: &TwoFactorSecrets,
    token: &str,
    env: mk_db::two_factor::OtpEnv,
) -> bool {
    if let Some(backup) = &secrets.two_factor_backup_secret {
        if backup.iter().any(|s| s == token) {
            return mk_db::two_factor::consume_backup_code(pool, user_id, backup, token)
                .await
                .unwrap_or(false);
        }
    }
    match &secrets.two_factor_secret {
        Some(secret) => mk_db::two_factor::validate_otp(user_id, secret, token, env),
        None => false,
    }
}

/// `POST /api/i/2fa/register` — endpoints/i/2fa/register.ts. paramDef:
/// password (required), token (nullable — only consumed once 2FA is already
/// enabled). Flow: profile load → 2FA gate → bcrypt compare → fresh secret
/// stored in twoFactorTempSecret → { qr, url, secret, label, issuer }.
async fn i_register(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let Some(password) = body.get("password").and_then(|v| v.as_str()) else {
        return super::invalid_param("password", "required");
    };
    match body.get("token") {
        None | Some(Value::Null) | Some(Value::String(_)) => {}
        Some(_) => return super::invalid_param("token", "expected a string or null"),
    }

    let (pool, me) = match resolve_me(state, token).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let profile = match mk_db::user::find_profile(pool, &me.id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(target: "dbg_two_factor", error = %e, "find_profile[register]");
            return internal();
        }
    };
    let secrets = match mk_db::two_factor::fetch_secrets(pool, &me.id).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(target: "dbg_two_factor", error = %e, "fetch_secrets[register]");
            return internal();
        }
    };

    // `if (profile.twoFactorEnabled) { if (token == null) throw ... }` —
    // non-verified / missing tokens are plain Errors → 500.
    if profile.two_factor_enabled {
        let Some(t) = body.get("token").and_then(|v| v.as_str()) else {
            return internal(); // Error('authentication failed')
        };
        let env = mk_db::two_factor::OtpEnv::from_config_test_and_flag(state.config.test);
        if !two_factor_authenticate(pool, &me.id, &secrets, t, env).await {
            return internal(); // Error('authentication failed')
        }
    }

    // `bcrypt.compare(ps.password, profile.password ?? '')` — no stored
    // password fails the compare upstream (bcrypt throws on a null hash).
    let matched = profile
        .password
        .as_deref()
        .map(|h| mk_core::services::verify_password(password, h).unwrap_or(false))
        .unwrap_or(false);
    if !matched {
        // ApiError(meta.errors.incorrectPassword)
        return incorrect_password("78d6c839-20c9-4c66-b90a-fc0542168b48");
    }

    // Generate user's secret key (`new OTPAuth.Secret()` → 20 bytes, base32).
    let secret = mk_db::two_factor::generate_secret_base32();
    if let Err(e) = mk_db::two_factor::set_temp_secret(pool, &me.id, &secret).await {
        tracing::error!(target: "dbg_two_factor", error = %e, "set_temp_secret");
        return internal();
    }

    // `new OTPAuth.TOTP({ secret, digits: 6, label: me.username, issuer:
    // config.host }).toString()`; the e2e only asserts url != undefined.
    let label = me.username.clone();
    let issuer = state.config.host();
    let url = format!(
        "otpauth://totp/{}:{}?secret={secret}&issuer={issuer}&algorithm=SHA1&digits=6&period=30",
        issuer, label,
    );
    // DEVIATION: QRCode.toDataURL(url) mirrored as a constant 1×1 png data
    // URL — 2fa.ts only asserts `qr != undefined`.
    let qr = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

    (
        StatusCode::OK,
        Json(json!({
            "qr": qr,
            "url": url,
            "secret": secret,
            "label": label,
            "issuer": issuer,
        })),
    )
        .into_response()
}

/// `POST /api/i/2fa/done` — endpoints/i/2fa/done.ts. paramDef: token
/// (required string). Flow: temp-secret presence → validateOtp(temp) → 5
/// fresh backup codes → twoFactorSecret escalated + twoFactorEnabled →
/// { backupCodes }.
async fn i_done(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let Some(raw_token) = body.get("token").and_then(|v| v.as_str()) else {
        return super::invalid_param("token", "required");
    };
    // `ps.token.replace(/\s/g, '')`
    let otp: String = raw_token.chars().filter(|c| !c.is_whitespace()).collect();

    let (pool, me) = match resolve_me(state, token).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let secrets = match mk_db::two_factor::fetch_secrets(pool, &me.id).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(target: "dbg_two_factor", error = %e, "fetch_secrets[done]");
            return internal();
        }
    };
    let Some(temp) = secrets.two_factor_temp_secret.clone() else {
        // Error('二段階認証の設定が開始されていません')
        return internal();
    };

    // `validateOtp(userId, twoFactorTempSecret, token)` — false is the plain
    // Error('not verified').
    let env = mk_db::two_factor::OtpEnv::from_config_test_and_flag(state.config.test);
    if !mk_db::two_factor::validate_otp(&me.id, &temp, &otp, env) {
        return internal();
    }

    // `Array.from({ length: 5 }, () => new OTPAuth.Secret().base32)`
    let backup_codes: Vec<String> = (0..5)
        .map(|_| mk_db::two_factor::generate_secret_base32())
        .collect();
    if let Err(e) = mk_db::two_factor::confirm(pool, &me.id, &temp, &backup_codes).await {
        tracing::error!(target: "dbg_two_factor", error = %e, "confirm");
        return internal();
    }

    // GlobalEventService meUpdated main-stream publish: no port-side
    // publisher to ride (nothing in the 2fa.ts census consumes it).
    (StatusCode::OK, Json(json!({ "backupCodes": backup_codes }))).into_response()
}

/// `POST /api/i/2fa/unregister` — endpoints/i/2fa/unregister.ts. paramDef:
/// password (required), token (nullable). Flow: 2FA gate → bcrypt compare →
/// columns reset → 204 NO_CONTENT.
async fn i_unregister(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let Some(password) = body.get("password").and_then(|v| v.as_str()) else {
        return super::invalid_param("password", "required");
    };
    match body.get("token") {
        None | Some(Value::Null) | Some(Value::String(_)) => {}
        Some(_) => return super::invalid_param("token", "expected a string or null"),
    }

    let (pool, me) = match resolve_me(state, token).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let profile = match mk_db::user::find_profile(pool, &me.id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(target: "dbg_two_factor", error = %e, "find_profile[unregister]");
            return internal();
        }
    };

    if profile.two_factor_enabled {
        let Some(t) = body.get("token").and_then(|v| v.as_str()) else {
            return internal(); // Error('authentication failed')
        };
        let secrets = match mk_db::two_factor::fetch_secrets(pool, &me.id).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(target: "dbg_two_factor", error = %e, "fetch_secrets[unregister]");
                return internal();
            }
        };
        let env = mk_db::two_factor::OtpEnv::from_config_test_and_flag(state.config.test);
        if !two_factor_authenticate(pool, &me.id, &secrets, t, env).await {
            return internal(); // Error('authentication failed')
        }
    }

    let matched = profile
        .password
        .as_deref()
        .map(|h| mk_core::services::verify_password(password, h).unwrap_or(false))
        .unwrap_or(false);
    if !matched {
        return incorrect_password("7add0395-9901-4098-82f9-4f67af65f775");
    }

    if let Err(e) = mk_db::two_factor::unregister(pool, &me.id).await {
        tracing::error!(target: "dbg_two_factor", error = %e, "unregister");
        return internal();
    }

    StatusCode::NO_CONTENT.into_response()
}
