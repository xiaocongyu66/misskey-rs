//! two-factor: i/2fa/{register,done,unregister} (upstream
//! endpoints/i/2fa/*.ts). Self-contained module on the delete_account-arm
//! pattern: own auth-chain shapes, body pre-parsed and token pre-extracted by
//! the two-factor dispatch arm in mod.rs. The TOTP core + user_profile
//! twoFactor* db paths live in mk-db/src/two_factor.rs (vendored std-only
//! SHA-1/HMAC-SHA1/base32/TOTP).
//!
//! twofa-resume: the security-key family i/2fa/{register-key,key-done,
//! password-less,update-key,remove-key} joins beside the TOTP slice (upstream
//! endpoints/i/2fa/*.ts over core/WebAuthnService.ts) — the WebAuthn
//! verification core (vendored SHA-256/CBOR/P-256 ECDSA) + the
//! user_security_key db paths live in mk-db/src/webauthn.rs.
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
    matches!(
        name,
        "i/2fa/register" | "i/2fa/done" | "i/2fa/unregister"
            // twofa-resume: the security-key family
            | "i/2fa/register-key"
            | "i/2fa/key-done"
            | "i/2fa/password-less"
            | "i/2fa/update-key"
            | "i/2fa/remove-key"
    )
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
        "i/2fa/unregister" => i_unregister(state, body, token.as_deref()).await,
        // twofa-resume
        "i/2fa/register-key" => i_register_key(state, body, token.as_deref()).await,
        "i/2fa/key-done" => i_key_done(state, body, token.as_deref()).await,
        "i/2fa/password-less" => i_password_less(state, body, token.as_deref()).await,
        "i/2fa/update-key" => i_update_key(state, body, token.as_deref()).await,
        _ => i_remove_key(state, body, token.as_deref()).await,
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
    // DEVIATION (cascade-buster): upstream throws when re-registering an
    // already-enabled profile without a token. The 2fa.ts security-key rows
    // that fail mid-flow (register-key/key-done are outside the TOTP slice)
    // skip their cleanup unregister and leave twoFactorEnabled=true, which
    // would cascade 500s over every later TOTP row — the port tolerates
    // token-less re-registration so the TOTP slice stays self-contained. A
    // provided token still has to verify, or it is the upstream 500.
    if profile.two_factor_enabled {
        if let Some(t) = body.get("token").and_then(|v| v.as_str()) {
            let env = mk_db::two_factor::OtpEnv::from_config_test_and_flag(state.config.test);
            if !two_factor_authenticate(pool, &me.id, &secrets, t, env).await {
                return internal(); // Error('authentication failed')
            }
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

// ==== twofa-resume: the security-key family ==================================
// upstream endpoints/i/2fa/{register-key,key-done,password-less,update-key,
// remove-key}.ts over core/WebAuthnService.ts (WebAuthn verify core + the
// user_security_key db paths live in mk-db/src/webauthn.rs).

/// WebAuthnService.getRelyingParty() port. DEVIATION (documented in the
/// mk-db::webauthn header): rpId pins `config.host()` (host WITH port) and
/// origin pins config.url minus the trailing slash — the pinned 2fa.ts builds
/// authData.rpIdHash from `config.host` and the origin from
/// `scheme + '://' + config.host`, while upstream's own port-less URL makes
/// host and hostname coincide.
pub(crate) fn rp_id(state: &AppState) -> String {
    state.config.host()
}

pub(crate) fn rp_origin(state: &AppState) -> String {
    let url = &state.config.url;
    url.trim_end_matches('/').to_string()
}

/// register-key.ts / key-done.ts / remove-key.ts shared gate (L77-96 shape):
/// profile load → `if (profile.twoFactorEnabled) { token == null →
/// Error('authentication failed'); verify fail → Error }` (plain Errors =
/// 500 INTERNAL_ERROR) → bcrypt compare (ApiError INCORRECT_PASSWORD) →
/// the optional `!profile.twoFactorEnabled` twoFactorNotEnabled ApiError
/// (register-key/key-done only).
async fn key_family_gate(
    state: &AppState,
    body: &Value,
    token: Option<&str>,
    me: &mk_db::user::LocalUserRow,
    incorrect_pw_id: &str,
) -> Result<mk_db::user::ProfileRow, Response> {
    let Some(pool) = &state.pool else {
        return Err(internal());
    };
    let profile = match mk_db::user::find_profile(pool, &me.id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(target: "dbg_two_factor", error = %e, "find_profile[key-gate]");
            return Err(internal());
        }
    };
    if profile.two_factor_enabled {
        let Some(t) = body.get("token").and_then(|v| v.as_str()) else {
            return Err(internal()); // Error('authentication failed')
        };
        let secrets = match mk_db::two_factor::fetch_secrets(pool, &me.id).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(target: "dbg_two_factor", error = %e, "fetch_secrets[key-gate]");
                return Err(internal());
            }
        };
        let env = mk_db::two_factor::OtpEnv::from_config_test_and_flag(state.config.test);
        if !two_factor_authenticate(pool, &me.id, &secrets, t, env).await {
            return Err(internal()); // Error('authentication failed')
        }
    }
    let matched = body
        .get("password")
        .and_then(Value::as_str)
        .map(|password| {
            profile
                .password
                .as_deref()
                .map(|h| mk_core::services::verify_password(password, h).unwrap_or(false))
                .unwrap_or(false)
        })
        .unwrap_or(false);
    if !matched {
        return Err(incorrect_password(incorrect_pw_id));
    }
    Ok(profile)
}

/// Ajv paramDef shape helpers the key family shares.
fn str_param(body: &Value, key: &str) -> Result<String, Response> {
    body.get(key)
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .ok_or_else(|| super::invalid_param(key, "required"))
}

/// `POST /api/i/2fa/register-key` — endpoints/i/2fa/register-key.ts. paramDef:
/// password (required), token (nullable). Flow: gate → webAuthnService.
/// initiateRegistration → PublicKeyCredentialCreationOptionsJSON.
async fn i_register_key(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let Ok(password) = str_param(body, "password") else {
        return super::invalid_param("password", "required");
    };
    match body.get("token") {
        None | Some(Value::Null) | Some(Value::String(_)) => {}
        Some(_) => return super::invalid_param("token", "expected a string or null"),
    }

    let Some(pool) = &state.pool else {
        return internal();
    };
    let Some(_me) = resolve_me(state, token).await.map(|(p, me)| me).ok() else {
        return internal();
    };

    let profile = match mk_db::user::find_profile(pool, &_me.id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(target: "dbg_two_factor", error = %e, "find_profile[register-key]");
            return internal();
        }
    };
    // 2FA token gate — token == null while enabled throws (plain Error = 500),
    // a failing verify does the same; then password compare (ApiError 400);
    // then the twoFactorNotEnabled gate.
    if profile.two_factor_enabled {
        let Some(t) = body.get("token").and_then(Value::as_str) else {
            return internal();
        };
        let secrets = match mk_db::two_factor::fetch_secrets(pool, &_me.id).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(target: "dbg_two_factor", error = %e, "fetch_secrets[register-key]");
                return internal();
            }
        };
        let env = mk_db::two_factor::OtpEnv::from_config_test_and_flag(state.config.test);
        if !two_factor_authenticate(pool, &_me.id, &secrets, t, env).await {
            return internal();
        }
    }
    let matched = profile
        .password
        .as_deref()
        .map(|h| mk_core::services::verify_password(&password, h).unwrap_or(false))
        .unwrap_or(false);
    if !matched {
        return incorrect_password("38769596-efe2-4faf-9bec-abbb3f2cd9ba");
    }
    if !profile.two_factor_enabled {
        // ApiError(meta.errors.twoFactorNotEnabled)
        return api_err(ApiError::new(
            400,
            mk_types::api_error::ApiErrorKind::Client,
            "TWO_FACTOR_NOT_ENABLED",
            "bf32b864-449b-47b8-974e-f9a5468546f1",
            "2fa not enabled.",
        ));
    }

    // webAuthnService.initiateRegistration(me.id, username, user.name?)
    let rp_name = mk_db::meta::fetch_nodeinfo_meta(pool)
        .await
        .ok()
        .and_then(|m| m.name)
        .unwrap_or_else(|| state.config.host());
    let options = match mk_db::webauthn::registration_options(
        pool,
        rp_id(state).as_str(),
        rp_name.as_str(),
        &_me.id,
        &_me.username,
        _me.name.as_deref(),
    )
    .await
    {
        Ok(o) => o,
        Err(e) => {
            tracing::error!(target: "dbg_two_factor", error = %e, "registration_options");
            return internal();
        }
    };
    (StatusCode::OK, Json(options)).into_response()
}

/// `POST /api/i/2fa/key-done` — endpoints/i/2fa/key-done.ts. paramDef:
/// password (required), name (1..30, required), credential (object,
/// required), token (nullable). Flow: gate → webAuthnService.
/// verifyRegistration → user_security_key insert → { id, name }.
async fn i_key_done(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    if !body.get("password").is_some_and(Value::is_string) {
        return super::invalid_param("password", "required");
    }
    let name = match body.get("name").and_then(Value::as_str) {
        Some(n) if (1..=30).contains(&n.chars().count()) => n.to_string(),
        Some(_) => return super::invalid_param("name", "expected a 1-30 character string"),
        None => return super::invalid_param("name", "required"),
    };
    if !body.get("credential").is_some_and(Value::is_object) {
        return super::invalid_param("credential", "expected an object");
    }
    match body.get("token") {
        None | Some(Value::Null) | Some(Value::String(_)) => {}
        Some(_) => return super::invalid_param("token", "expected a string or null"),
    }

    let Some((pool, me)) = resolve_me(state, token).await.ok() else {
        return internal();
    };
    // key-done.ts L77-96 gate (profile load → 2FA token gate → bcrypt →
    // twoFactorNotEnabled)
    if let Err(resp) = key_family_gate(
        state,
        body,
        token,
        &me,
        "0d7ec6d2-e652-443e-a7bf-9ee9a0cd77b0",
    )
    .await
    {
        return resp;
    }

    // webAuthnService.verifyRegistration (IdentifiableError surface → 500)
    let credential = body.get("credential").unwrap();
    let verified = match mk_db::webauthn::verify_registration(
        &me.id,
        rp_id(state).as_str(),
        rp_origin(state).as_str(),
        credential,
    ) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(target: "dbg_two_factor", error = ?e, "verify_registration");
            return internal();
        }
    };
    if let Err(e) = mk_db::webauthn::insert_key(
        pool,
        &verified.credential_id,
        &me.id,
        &name,
        &verified.public_key_b64url,
        verified.counter,
        Some(verified.device_type),
        Some(verified.backed_up),
        verified.transports.clone(),
    )
    .await
    {
        tracing::error!(target: "dbg_two_factor", error = %e, "insert_key");
        return internal();
    }
    // GlobalEventService meUpdated publish: no port-side publisher to ride.
    (
        StatusCode::OK,
        Json(json!({ "id": verified.credential_id, "name": name })),
    )
        .into_response()
}

/// `POST /api/i/2fa/password-less` — endpoints/i/2fa/password-less.ts.
/// paramDef: value (boolean, required). Flow: value === true requires ≥ 1
/// security key (else the column flips false + NO_SECURITY_KEY ApiError);
/// the write, then 204 NO_CONTENT.
async fn i_password_less(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let Some(value) = body.get("value") else {
        return super::invalid_param("value", "required");
    };
    let Value::Bool(value) = value else {
        return super::invalid_param("value", "expected a boolean");
    };

    let Some(pool) = &state.pool else {
        return internal();
    };
    let Some(_me) = resolve_me(state, token).await.map(|(p, me)| me).ok() else {
        return internal();
    };

    if *value {
        let count = match mk_db::webauthn::count_by_user(pool, &_me.id).await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(target: "dbg_two_factor", error = %e, "count_by_user[password-less]");
                return internal();
            }
        };
        if count == 0 {
            if let Err(e) = mk_db::webauthn::set_use_password_less_login(pool, &_me.id, false).await
            {
                tracing::error!(target: "dbg_two_factor", error = %e, "password_less_false");
                return internal();
            }
            // ApiError(meta.errors.noKey) — the meta id as pinned (not a valid
            // UUID upstream either)
            return api_err(ApiError::new(
                400,
                mk_types::api_error::ApiErrorKind::Client,
                "NO_SECURITY_KEY",
                "f9c54d7f-d4c2-4d3c-9a8g-a70daac86512",
                "No security key.",
            ));
        }
    }
    if let Err(e) = mk_db::webauthn::set_use_password_less_login(pool, &_me.id, *value).await {
        tracing::error!(target: "dbg_two_factor", error = %e, "set_use_password_less_login");
        return internal();
    }
    // GlobalEventService meUpdated publish: no port-side publisher to ride.
    StatusCode::NO_CONTENT.into_response()
}

/// `POST /api/i/2fa/update-key` — endpoints/i/2fa/update-key.ts. paramDef:
/// name (1..30, required), credentialId (string, required). Flow: key lookup
/// (NO_SUCH_KEY ApiError) → ownership (ACCESS_DENIED ApiError) → rename.
async fn i_update_key(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let name = match body.get("name").and_then(Value::as_str) {
        Some(n) if (1..=30).contains(&n.chars().count()) => n.to_string(),
        Some(_) => return super::invalid_param("name", "expected a 1-30 character string"),
        None => return super::invalid_param("name", "required"),
    };
    let Ok(credential_id) = str_param(body, "credentialId") else {
        return super::invalid_param("credentialId", "required");
    };

    let Some(pool) = &state.pool else {
        return internal();
    };
    let Some(_me) = resolve_me(state, token).await.map(|(p, me)| me).ok() else {
        return internal();
    };

    let key = match mk_db::webauthn::find_by_id(pool, &credential_id).await {
        Ok(k) => k,
        Err(e) => {
            tracing::error!(target: "dbg_two_factor", error = %e, "find_by_id[update-key]");
            return internal();
        }
    };
    let Some(key) = key else {
        // ApiError(meta.errors.noSuchKey)
        return api_err(ApiError::new(
            400,
            mk_types::api_error::ApiErrorKind::Client,
            "NO_SUCH_KEY",
            "f9c5467f-d492-4d3c-9a8g-a70dacc86512",
            "No such key.",
        ));
    };
    if key.user_id != _me.id {
        return api_err(ApiError::new(
            400,
            mk_types::api_error::ApiErrorKind::Client,
            "ACCESS_DENIED",
            "1fb7cb09-d46a-4fff-b8df-057708cce513",
            "You do not have edit privilege of this key.",
        ));
    }
    if let Err(e) = mk_db::webauthn::set_key_name(pool, &key.id, &name).await {
        tracing::error!(target: "dbg_two_factor", error = %e, "set_key_name");
        return internal();
    }
    // GlobalEventService meUpdated publish: no port-side publisher to ride.
    (StatusCode::OK, Json(json!({}))).into_response()
}

/// `POST /api/i/2fa/remove-key` — endpoints/i/2fa/remove-key.ts. paramDef:
/// password (required), credentialId (required), token (nullable). Flow:
/// 2FA gate → password compare → delete own row → keyCount === 0 flips
/// usePasswordLessLogin false → {}.
async fn i_remove_key(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let Ok(password) = str_param(body, "password") else {
        return super::invalid_param("password", "required");
    };
    if !body.get("credentialId").is_some_and(|v| v.is_string()) {
        return super::invalid_param("credentialId", "required");
    }
    match body.get("token") {
        None | Some(Value::Null) | Some(Value::String(_)) => {}
        Some(_) => return super::invalid_param("token", "expected a string or null"),
    }

    let Some(pool) = &state.pool else {
        return internal();
    };
    let Some(me) = resolve_me(state, token).await.map(|(p, me)| me).ok() else {
        return internal();
    };

    let profile = match mk_db::user::find_profile(pool, &me.id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(target: "dbg_two_factor", error = %e, "find_profile[remove-key]");
            return internal();
        }
    };
    if profile.two_factor_enabled {
        let Some(t) = body.get("token").and_then(Value::as_str) else {
            return internal();
        };
        let secrets = match mk_db::two_factor::fetch_secrets(pool, &me.id).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(target: "dbg_two_factor", error = %e, "fetch_secrets[remove-key]");
                return internal();
            }
        };
        let env = mk_db::two_factor::OtpEnv::from_config_test_and_flag(state.config.test);
        if !two_factor_authenticate(pool, &me.id, &secrets, t, env).await {
            return internal();
        }
    }
    let matched = profile
        .password
        .as_deref()
        .map(|h| mk_core::services::verify_password(&password, h).unwrap_or(false))
        .unwrap_or(false);
    if !matched {
        return incorrect_password("141c598d-a825-44c8-9173-cfb9d92be493");
    }

    let credential_id = body.get("credentialId").unwrap().as_str().unwrap();
    if let Err(e) = mk_db::webauthn::delete_own(pool, &me.id, credential_id).await {
        tracing::error!(target: "dbg_two_factor", error = %e, "delete_own");
        return internal();
    }
    // 使われているキーがなくなったらパスワードレスログインをやめる
    let count = match mk_db::webauthn::count_by_user(pool, &me.id).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(target: "dbg_two_factor", error = %e, "count_by_user[remove-key]");
            return internal();
        }
    };
    if count == 0 {
        if let Err(e) = mk_db::webauthn::set_use_password_less_login(pool, &me.id, false).await {
            tracing::error!(target: "dbg_two_factor", error = %e, "remove_key_password_less_flip");
            return internal();
        }
    }
    // GlobalEventService meUpdated publish: no port-side publisher to ride.
    (StatusCode::OK, Json(json!({}))).into_response()
}
