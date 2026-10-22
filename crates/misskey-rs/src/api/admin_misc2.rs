//! `admin/*` misc family — ports of upstream
//! `endpoints/admin/{captcha/current,captcha/save,unset-user-avatar,
//! unset-user-banner,update-user-note}.ts` (pinned ea2c3aed).
//!
//! Gate: captcha metas are `requireAdmin` (kind read/write:admin:meta); the
//! user avatar/banner/note metas are `requireModerator`. Both ride the shared
//! `admin::gate` chain (credential → suspended → ROLE_PERMISSION_DENIED with
//! the root bypass).
//!
//! Captcha settings live on the single `meta` row (`enableHcaptcha`,
//! `hcaptchaSiteKey`, …). `save` mirrors `CaptchaService.save`:
//! - `none` stores immediately;
//! - `testcaptcha` accepts only `captchaResult == "testcaptcha-passed"`;
//! - hcaptcha/recaptcha/turnstile/mcaptcha require secret (+ sitekey /
//!   instanceUrl for mcaptcha) and perform the upstream siteverify HTTP call.
//!
//! DOCUMENTED DEVIATION: upstream `save` returns `undefined` (204); we match.
//! Moderation-log writes mirror `ModerationLogService.log` and, like upstream,
//! do not fail the request when the log insert fails.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

use super::admin::{Gate, gate};

/// Endpoint names owned by this module (all POST-only).
pub(super) fn handles(name: &str) -> bool {
    matches!(
        name,
        "admin/captcha/current"
            | "admin/captcha/save"
            | "admin/unset-user-avatar"
            | "admin/unset-user-banner"
            | "admin/update-user-note"
    )
}

type Check<T = Response> = Result<T, Response>;

const PROVIDERS: &[&str] = &[
    "none",
    "hcaptcha",
    "mcaptcha",
    "recaptcha",
    "turnstile",
    "testcaptcha",
];

/// Entry from the admin-misc2 dispatch arm in mod.rs (body pre-parsed, token
/// pre-extracted).
pub(super) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<&str>,
) -> Response {
    let g = if name.starts_with("admin/captcha/") {
        Gate::Admin
    } else {
        Gate::Moderator
    };
    let ctx = match gate(state, token, g).await {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    let Some(pool) = &state.pool else {
        return internal();
    };
    let result: Check = match name {
        "admin/captcha/current" => captcha_current(pool).await,
        "admin/captcha/save" => captcha_save(pool, body).await,
        "admin/unset-user-avatar" => unset_avatar(pool, body, &ctx.me.id).await,
        "admin/unset-user-banner" => unset_banner(pool, body, &ctx.me.id).await,
        "admin/update-user-note" => update_note(pool, body, &ctx.me.id).await,
        _ => unreachable!("checked by handles()"),
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

// ---------------------------------------------------------------------------
// error shapes (upstream meta.errors)
// ---------------------------------------------------------------------------

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn db_err(e: sqlx::Error) -> Response {
    tracing::error!(target: "dbg_admin_misc2", error = %e, "db");
    internal()
}

fn captcha_err(code: &str, id: &str, status: u16, msg: &str) -> Response {
    api_err(ApiError::new(
        status,
        if status >= 500 {
            mk_types::api_error::ApiErrorKind::Server
        } else {
            mk_types::api_error::ApiErrorKind::Client
        },
        code,
        id,
        msg,
    ))
}

fn invalid_provider() -> Response {
    captcha_err(
        "INVALID_PROVIDER",
        "14bf7ae1-80cc-4363-acb2-4fd61d086af0",
        400,
        "Invalid provider.",
    )
}

fn invalid_parameters(msg: &str) -> Response {
    captcha_err(
        "INVALID_PARAMETERS",
        "26654194-410e-44e2-b42e-460ff6f92476",
        400,
        msg,
    )
}

fn no_response_provided() -> Response {
    captcha_err(
        "NO_RESPONSE_PROVIDED",
        "40acbba8-0937-41fb-bb3f-474514d40afe",
        400,
        "No response provided.",
    )
}

fn request_failed() -> Response {
    captcha_err(
        "REQUEST_FAILED",
        "0f4fe2f1-2c15-4d6e-b714-efbfcde231cd",
        500,
        "Request failed.",
    )
}

fn verification_failed() -> Response {
    captcha_err(
        "VERIFICATION_FAILED",
        "c41c067f-24f3-4150-84b2-b5a3ae8c2214",
        400,
        "Verification failed.",
    )
}

// ---------------------------------------------------------------------------
// captcha/current + captcha/save
// ---------------------------------------------------------------------------

async fn captcha_current(pool: &mk_db::sqlx::PgPool) -> Check {
    let s = mk_db::admin::captcha_settings(pool).await.map_err(db_err)?;
    Ok(Json(json!({
        "provider": s.provider,
        "hcaptcha": { "siteKey": s.hcaptcha_site_key, "secretKey": s.hcaptcha_secret_key },
        "mcaptcha": {
            "siteKey": s.mcaptcha_site_key,
            "secretKey": s.mcaptcha_secret_key,
            "instanceUrl": s.mcaptcha_instance_url,
        },
        "recaptcha": { "siteKey": s.recaptcha_site_key, "secretKey": s.recaptcha_secret_key },
        "turnstile": { "siteKey": s.turnstile_site_key, "secretKey": s.turnstile_secret_key },
    }))
    .into_response())
}

/// Upstream paramDef: absent → undefined (skip), null → write NULL.
fn nullable(body: &Value, key: &str) -> Option<Option<String>> {
    match body.get(key) {
        None => None,
        Some(Value::Null) => Some(None),
        Some(v) => v.as_str().map(|s| Some(s.to_string())),
    }
}

async fn captcha_save(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check {
    let provider = body
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if !PROVIDERS.contains(&provider) {
        return Err(invalid_provider());
    }
    let sitekey = nullable(body, "sitekey");
    let secret = nullable(body, "secret");
    let instance_url = nullable(body, "instanceUrl");
    let result = body.get("captchaResult").and_then(|v| v.as_str());

    match provider {
        "none" => {}
        "testcaptcha" => {
            let Some(result) = result else {
                return Err(no_response_provided());
            };
            if result != "testcaptcha-passed" {
                return Err(verification_failed());
            }
        }
        "hcaptcha" | "recaptcha" | "turnstile" => {
            let (Some(secret), Some(result)) = (secret.as_ref().and_then(|v| v.as_deref()), result)
            else {
                return Err(invalid_parameters(
                    "secret and captureResult are required",
                ));
            };
            let url = match provider {
                "hcaptcha" => "https://hcaptcha.com/siteverify",
                "recaptcha" => "https://www.recaptcha.net/recaptcha/api/siteverify",
                _ => "https://challenges.cloudflare.com/turnstile/v0/siteverify",
            };
            verify_siteverify(url, secret, result).await?;
        }
        "mcaptcha" => {
            let (Some(secret), Some(result)) = (secret.as_ref().and_then(|v| v.as_deref()), result)
            else {
                return Err(invalid_parameters(
                    "secret, sitekey, instanceUrl and captureResult are required",
                ));
            };
            let (Some(sitekey), Some(instance_url)) = (
                sitekey.as_ref().and_then(|v| v.as_deref()),
                instance_url.as_ref().and_then(|v| v.as_deref()),
            ) else {
                return Err(invalid_parameters(
                    "secret, sitekey, instanceUrl and captureResult are required",
                ));
            };
            verify_mcaptcha(instance_url, sitekey, secret, result).await?;
        }
        _ => unreachable!("provider checked above"),
    }

    mk_db::admin::update_captcha_meta(
        pool,
        provider,
        sitekey.as_ref().map(|v| v.as_deref()),
        secret.as_ref().map(|v| v.as_deref()),
        instance_url.as_ref().map(|v| v.as_deref()),
    )
    .await
    .map_err(db_err)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn verify_siteverify(url: &str, secret: &str, response: &str) -> Check {
    let resp = reqwest::Client::new()
        .post(url)
        .form(&[("secret", secret), ("response", response)])
        .send()
        .await
        .map_err(|e| {
            tracing::warn!(target: "dbg_admin_misc2", error = %e, "captcha request");
            request_failed()
        })?;
    if !resp.status().is_success() {
        return Err(request_failed());
    }
    let v: Value = resp.json().await.map_err(|_| request_failed())?;
    if v.get("success").and_then(|s| s.as_bool()) != Some(true) {
        return Err(verification_failed());
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn verify_mcaptcha(
    instance_url: &str,
    sitekey: &str,
    secret: &str,
    token: &str,
) -> Check {
    let url = format!(
        "{}/api/v1/pow/siteverify",
        instance_url.trim_end_matches('/')
    );
    let resp = reqwest::Client::new()
        .post(url)
        .json(&json!({ "key": sitekey, "secret": secret, "token": token }))
        .send()
        .await
        .map_err(|e| {
            tracing::warn!(target: "dbg_admin_misc2", error = %e, "mcaptcha request");
            request_failed()
        })?;
    if resp.status() != reqwest::StatusCode::OK {
        return Err(request_failed());
    }
    let v: Value = resp.json().await.map_err(|_| request_failed())?;
    if v.get("valid").and_then(|s| s.as_bool()) != Some(true) {
        return Err(verification_failed());
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---------------------------------------------------------------------------
// unset-user-avatar / unset-user-banner / update-user-note
// ---------------------------------------------------------------------------

async fn load_user(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
) -> Check<mk_db::user::LocalUserRow> {
    let Some(user_id) = body.get("userId").and_then(|v| v.as_str()) else {
        return Err(super::invalid_param("userId", "required"));
    };
    match mk_db::user::find_user_by_id(pool, user_id).await {
        Ok(Some(user)) => Ok(user),
        Ok(None) => Err(internal()), // upstream `throw new Error('user not found')`
        Err(e) => Err(db_err(e)),
    }
}

fn log(pool: &mk_db::sqlx::PgPool, moderator_id: &str, kind: &str, info: Value) {
    let id = mk_types::id::gen_aid_now();
    let pool = pool.clone();
    let kind = kind.to_string();
    let moderator_id = moderator_id.to_string();
    tokio::spawn(async move {
        if let Err(e) =
            mk_db::admin::insert_moderation_log(&pool, &id, &moderator_id, &kind, &info).await
        {
            tracing::error!(target: "dbg_admin_misc2", error = %e, "insert_moderation_log[{kind}]");
        }
    });
}

async fn unset_avatar(pool: &mk_db::sqlx::PgPool, body: &Value, me_id: &str) -> Check {
    let user = load_user(pool, body).await?;
    let Some(file_id) = user.avatar_id.clone() else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    mk_db::user::update_avatar_banner(
        pool,
        &user.id,
        mk_db::user::AvatarBannerOp::Clear,
        mk_db::user::AvatarBannerOp::Untouched,
    )
    .await
    .map_err(db_err)?;
    log(
        pool,
        me_id,
        "unsetUserAvatar",
        json!({
            "userId": user.id,
            "userUsername": user.username,
            "userHost": Value::Null,
            "fileId": file_id,
        }),
    );
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn unset_banner(pool: &mk_db::sqlx::PgPool, body: &Value, me_id: &str) -> Check {
    let user = load_user(pool, body).await?;
    let Some(file_id) = user.banner_id.clone() else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    mk_db::user::update_avatar_banner(
        pool,
        &user.id,
        mk_db::user::AvatarBannerOp::Untouched,
        mk_db::user::AvatarBannerOp::Clear,
    )
    .await
    .map_err(db_err)?;
    log(
        pool,
        me_id,
        "unsetUserBanner",
        json!({
            "userId": user.id,
            "userUsername": user.username,
            "userHost": Value::Null,
            "fileId": file_id,
        }),
    );
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn update_note(pool: &mk_db::sqlx::PgPool, body: &Value, me_id: &str) -> Check {
    let Some(text) = body.get("text").and_then(|v| v.as_str()) else {
        return Err(super::invalid_param("text", "required"));
    };
    let user = load_user(pool, body).await?;
    let before = mk_db::admin::moderation_note(pool, &user.id)
        .await
        .map_err(db_err)?
        .unwrap_or_default();
    mk_db::admin::set_moderation_note(pool, &user.id, text)
        .await
        .map_err(db_err)?;
    log(
        pool,
        me_id,
        "updateUserNote",
        json!({
            "userId": user.id,
            "userUsername": user.username,
            "userHost": Value::Null,
            "before": before,
            "after": text,
        }),
    );
    Ok(StatusCode::NO_CONTENT.into_response())
}
