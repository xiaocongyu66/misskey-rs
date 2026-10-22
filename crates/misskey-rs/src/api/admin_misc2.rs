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
            | "admin/unset-mfa"
            | "admin/send-email"
            | "admin/update-proxy-account"
            | "admin/update-abuse-user-report"
            | "admin/show-moderation-logs"
            | "admin/roles/update-default-policies"
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
    let g = if name.starts_with("admin/captcha/")
        || name == "admin/show-moderation-logs"
        || name == "admin/roles/update-default-policies"
    {
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
        "admin/unset-mfa" => unset_mfa(pool, body, &ctx.me.id).await,
        "admin/send-email" => send_email(pool, body).await,
        "admin/update-proxy-account" => update_proxy_account(state, pool, body, &ctx.me.id).await,
        "admin/update-abuse-user-report" => {
            update_abuse_user_report(pool, body, &ctx.me.id).await
        }
        "admin/show-moderation-logs" => show_moderation_logs(state, pool, body).await,
        "admin/roles/update-default-policies" => {
            update_default_policies(pool, body, &ctx.me.id).await
        }
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

fn req_str<'a>(body: &'a Value, key: &str) -> Check<&'a str> {
    body.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| super::invalid_param(key, "required"))
}

async fn find_target(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
) -> Check<Option<mk_db::user::LocalUserRow>> {
    let user_id = req_str(body, "userId")?;
    mk_db::user::find_user_by_id(pool, user_id).await.map_err(db_err)
}

async fn load_user(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
) -> Check<mk_db::user::LocalUserRow> {
    match find_target(pool, body).await? {
        Some(user) => Ok(user),
        None => Err(internal()), // upstream `throw new Error('user not found')`
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

// ---------------------------------------------------------------------------
// slice 2: unset-mfa / send-email / update-proxy-account /
//          update-abuse-user-report / show-moderation-logs /
//          roles/update-default-policies
// ---------------------------------------------------------------------------

fn no_such_user() -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "NO_SUCH_USER",
        "ccafc7fe-5074-4edd-9dc0-8ef9ef6a701d",
        "No such user.",
    ))
}

fn access_denied() -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "ACCESS_DENIED",
        "cda8f8ce-89a6-4f92-8055-33bbe0c1464d",
        "Access denied.",
    ))
}

fn no_such_report() -> Response {
    api_err(ApiError::new(
        404,
        mk_types::api_error::ApiErrorKind::Server,
        "NO_SUCH_ABUSE_REPORT",
        "15f51cf5-46d1-4b1d-a618-b35bcbed0662",
        "No such abuse report.",
    ))
}

/// `admin/unset-mfa.ts`: delete passkeys + disable TOTP/passwordless. Refuses
/// to strip an administrator's MFA unless the caller is that administrator.
async fn unset_mfa(pool: &mk_db::sqlx::PgPool, body: &Value, me_id: &str) -> Check {
    let Some(user) = find_target(pool, body).await? else {
        return Err(no_such_user());
    };
    let root = mk_db::admin::root_user_id(pool).await.map_err(db_err)?;
    let assigned = mk_db::role::user_assigned_roles(pool, &user.id)
        .await
        .map_err(db_err)?;
    let target_is_admin = root.as_deref() == Some(user.id.as_str())
        || mk_core::services::role::has_administrator_role(&assigned);
    if target_is_admin && me_id != user.id {
        return Err(access_denied());
    }
    mk_db::admin::unset_mfa(pool, &user.id).await.map_err(db_err)?;
    log(
        pool,
        me_id,
        "unsetMfa",
        json!({
            "userId": user.id,
            "userUsername": user.username,
            "userHost": Value::Null,
        }),
    );
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `admin/send-email.ts` → `EmailService.sendEmail(to, subject, text, text)`
/// over the shared `SmtpSender` lane. No `meta.smtpHost` → upstream no-op.
async fn send_email(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check {
    let to = req_str(body, "to")?;
    let subject = req_str(body, "subject")?;
    let text = req_str(body, "text")?;
    let cfg = mk_db::admin::smtp_config(pool).await.map_err(db_err)?;
    let Some(host) = cfg.host.filter(|h| !h.is_empty()) else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    let sender = mk_core::services::email::SmtpSender {
        host,
        port: cfg.port.unwrap_or(25).clamp(1, 65535) as u16,
        pool: Some(pool.clone()),
        username: cfg.user,
        password: cfg.pass,
        use_tls: cfg.secure,
    };
    mk_core::services::email::EmailSender::send_email(&sender, to, subject, text, text)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_admin_misc2", error = %e, "send_email");
            internal()
        })?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `SystemAccountService.fetch('proxy')` create leg: the upstream service
/// creates `system.proxy` on demand with a generated keypair + profile.
async fn ensure_proxy_account(pool: &mk_db::sqlx::PgPool) -> Check<String> {
    if let Some(id) = mk_db::admin::system_account_user_id(pool, "proxy")
        .await
        .map_err(db_err)?
    {
        return Ok(id);
    }
    let outcome =
        mk_core::services::build_signup("system.proxy", &mk_types::id::gen_aid_now(), None, None)
            .map_err(|_| internal())?;
    let new_user = mk_db::user::NewUser {
        id: &outcome.user_id,
        username: &outcome.username,
        username_lower: &outcome.username_lower,
        name: outcome.name.as_deref(),
        host: outcome.host.as_deref(),
        token: &outcome.token,
        password_hash: &outcome.password_hash,
        public_key: &outcome.public_key,
        private_key: &outcome.private_key,
    };
    mk_db::user::insert_new_user(pool, &new_user)
        .await
        .map_err(db_err)?;
    mk_db::admin::insert_system_account(
        pool,
        &mk_types::id::gen_aid_now(),
        &outcome.user_id,
        "proxy",
    )
    .await
    .map_err(db_err)?;
    Ok(outcome.user_id)
}

/// `admin/update-proxy-account.ts` → `SystemAccountService
/// .updateCorrespondingUserProfile('proxy', { description })`, packed as
/// MeDetailed.
async fn update_proxy_account(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Check {
    let description = super::opt_nullable_str(body, "description")?;
    let proxy_id = ensure_proxy_account(pool).await?;
    if let Some(desc) = description {
        mk_db::admin::set_profile_description(pool, &proxy_id, desc.as_deref())
            .await
            .map_err(db_err)?;
        log(
            pool,
            me_id,
            "updateProxyAccountDescription",
            json!({ "before": Value::Null, "after": desc }),
        );
    }
    let user = mk_db::user::find_user_by_id(pool, &proxy_id)
        .await
        .map_err(db_err)?
        .ok_or_else(internal)?;
    let profile = mk_db::user::find_profile(pool, &proxy_id)
        .await
        .map_err(db_err)?;
    let packed = mk_core::services::user_pack::pack_inline(
        &user,
        &profile,
        true,
        None,
        mk_core::services::user_pack::InstanceCtx {
            url: &state.config.url,
            host: &state.config.host(),
        },
    );
    Ok((StatusCode::OK, Json(packed)).into_response())
}

/// `admin/update-abuse-user-report.ts` → `AbuseReportService.update`
/// (moderationNote only; the log fires only for a non-null changed note).
async fn update_abuse_user_report(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Check {
    let report_id = req_str(body, "reportId")?;
    let note = super::opt_nullable_str(body, "moderationNote")?;
    let Some(report) = mk_db::abuse::find_report(pool, report_id)
        .await
        .map_err(db_err)?
    else {
        return Err(no_such_report());
    };
    if let Some(note) = note {
        mk_db::admin::update_report_note(pool, &report.id, note.as_deref())
            .await
            .map_err(db_err)?;
        if note.is_some() && note.as_deref() != Some(report.moderation_note.as_str()) {
            log(
                pool,
                me_id,
                "updateAbuseReportNote",
                json!({
                    "reportId": report.id,
                    "before": report.moderation_note,
                    "after": note,
                }),
            );
        }
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `admin/show-moderation-logs.ts` — pagination + type/userId/search filters,
/// `ModerationLogEntityService.pack` (createdAt from the AID, packed user).
async fn show_moderation_logs(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
) -> Check {
    let limit = body
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(10)
        .clamp(1, 100);
    let raw_since_id = body.get("sinceId").and_then(|v| v.as_str());
    let raw_until_id = body.get("untilId").and_then(|v| v.as_str());
    let since_date = body.get("sinceDate").and_then(|v| v.as_i64());
    let until_date = body.get("untilDate").and_then(|v| v.as_i64());
    // QueryService.makePaginationQuery: id bounds win over date bounds; dates
    // are converted to AID bounds at the api layer (mk-db has no mk-types dep).
    let (since_id, until_id) = if raw_since_id.is_some() || raw_until_id.is_some() {
        (
            raw_since_id.map(str::to_string),
            raw_until_id.map(str::to_string),
        )
    } else {
        (
            since_date.map(mk_types::id::gen_aid),
            until_date.map(mk_types::id::gen_aid),
        )
    };
    let order_asc = if raw_since_id.is_some() {
        raw_until_id.is_none()
    } else if raw_until_id.is_some() {
        false
    } else {
        since_date.is_some() && until_date.is_none()
    };
    let q = mk_db::admin::ModerationLogQuery {
        limit,
        since_id: since_id.as_deref(),
        until_id: until_id.as_deref(),
        order_asc,
        log_type: body.get("type").and_then(|v| v.as_str()),
        user_id: body.get("userId").and_then(|v| v.as_str()),
        search: body.get("search").and_then(|v| v.as_str()),
    };
    let rows = mk_db::admin::list_moderation_logs(pool, &q)
        .await
        .map_err(db_err)?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let created = mk_types::id::parse_aid_date(&r.id)
            .map(mk_core::services::signup::ms_to_iso)
            .map(Value::String)
            .unwrap_or(Value::Null);
        let user = mk_db::user::find_user_by_id(pool, &r.user_id)
            .await
            .map_err(db_err)?
            .ok_or_else(internal)?;
        let profile = mk_db::user::find_profile(pool, &r.user_id)
            .await
            .map_err(db_err)?;
        let packed = mk_core::services::user_pack::pack_inline(
            &user,
            &profile,
            false,
            None,
            mk_core::services::user_pack::InstanceCtx {
                url: &state.config.url,
                host: &state.config.host(),
            },
        );
        out.push(json!({
            "id": r.id,
            "createdAt": created,
            "type": r.log_type,
            "info": r.info,
            "userId": r.user_id,
            "user": packed,
        }));
    }
    Ok(Json(out).into_response())
}

/// `admin/roles/update-default-policies.ts` → `metaService.update({policies})`
/// + `updateServerSettings` moderation log (the internal-event publish has no
/// local bus; documented skip).
async fn update_default_policies(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Check {
    let Some(policies) = body.get("policies") else {
        return Err(super::invalid_param("policies", "required"));
    };
    if !policies.is_object() {
        return Err(super::invalid_param("policies", "object"));
    }
    let before = mk_db::admin::meta_policies(pool)
        .await
        .map_err(db_err)?
        .unwrap_or(Value::Null);
    mk_db::admin::set_meta_policies(pool, policies)
        .await
        .map_err(db_err)?;
    let after = mk_db::admin::meta_policies(pool)
        .await
        .map_err(db_err)?
        .unwrap_or(Value::Null);
    log(
        pool,
        me_id,
        "updateServerSettings",
        json!({ "before": before, "after": after }),
    );
    Ok(StatusCode::NO_CONTENT.into_response())
}
