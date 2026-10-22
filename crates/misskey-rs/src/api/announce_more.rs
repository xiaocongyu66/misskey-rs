//! announce-more — the announcements read side companion + the remaining
//! small `i/*` endpoints: `endpoints/i/update-email.ts` and
//! `endpoints/i/revoke-token.ts`.
//!
//! The announcements family itself (`announcements`, `announcements/show`,
//! `i/read-announcement`, `admin/announcements/*`) is already served by
//! `api::announcement` (db paths in `mk-db/src/announcement.rs`), so this
//! module only carries the two leftover `i/*` names.
//!
//! `i/update-email` — requireCredential (`secure: true`, `write:account`).
//! Flow per upstream: profile load → 2FA gate (`token` when enabled) →
//! bcrypt compare (`INCORRECT_PASSWORD`) → `emailRequiredForSignup` guard
//! when clearing the address (`EMAIL_REQUIRED`) → write
//! `email`/`emailVerified=false`/`emailVerifyCode=null` → pack MeDetailed →
//! fresh 16-char code + verification mail (the existing `verify-email`
//! lane consumes it). DEVIATION: upstream `EmailService.validateEmailForAccount`
//! (MX/disposable-address probe → `UNAVAILABLE`) is not ported — the repo's
//! email seam is delivery-only (`mk-core/services/email.rs`).
//!
//! `i/revoke-token` — no `requireCredential` upstream (the handler does the
//! gate itself so an access token can revoke itself): missing credential →
//! `CREDENTIAL_REQUIRED` 401; a native token (16 chars) may revoke any of the
//! caller's access tokens; an app/access token (32 chars, `access_token`
//! hash/token row) may only revoke itself (`PERMISSION_DENIED` 403).
//! paramDef is the upstream anyOf `{ tokenId }` | `{ token: string|null }`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// Endpoint names routed into this module (single source of truth for the
/// `// announce-more` dispatch arm in mod.rs).
pub(crate) const ENDPOINTS: &[&str] = &["i/update-email", "i/revoke-token"];

pub(crate) fn handles(name: &str) -> bool {
    ENDPOINTS.contains(&name)
}

/// Entry from the `// announce-more` dispatch arm in mod.rs (body pre-parsed,
/// token + credential-declared probe pre-extracted).
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
    match name {
        "i/update-email" => {
            update_email(state, pool, body, token.as_deref(), credential_declared).await
        }
        "i/revoke-token" => revoke_token(pool, body, token.as_deref()).await,
        _ => api_err(ApiError::unknown_endpoint()),
    }
}

// --- shared shapes ---

type Check<T = Response> = Result<T, Response>;

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_announce_more", error = %e, "db error: {context}");
    internal()
}

/// Meta error without httpStatusCode: kind 'client' → default 400.
fn meta_err(code: &str, id: &str, message: &str) -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        code,
        id,
        message,
    ))
}

fn invalid_param(param: &str, reason: &str) -> Response {
    api_err(ApiError::invalid_param(json!({
        "param": param,
        "reason": reason,
    })))
}

fn credential_required(id: &str) -> Response {
    api_err(ApiError::new(
        401,
        mk_types::api_error::ApiErrorKind::Client,
        "CREDENTIAL_REQUIRED",
        id,
        "Credential required.",
    ))
}

fn auth_failed() -> Response {
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

// --- i/update-email ---

/// `POST /api/i/update-email` — endpoints/i/update-email.ts.
async fn update_email(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    token: Option<&str>,
    credential_declared: bool,
) -> Response {
    let result: Check<Response> = async {
        // requireCredential (native-token path, like every requireCredential
        // arm in this port).
        let me_id = super::social::require_checked_user(pool, token, credential_declared).await?;

        // paramDef: password required, email/token nullable.
        let Some(password) = body.get("password").and_then(Value::as_str) else {
            return Err(invalid_param("password", "must be a string"));
        };
        let email: Option<String> = match body.get("email") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => return Err(invalid_param("email", "must be a string or null")),
        };
        let two_factor_token = match body.get("token") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => return Err(invalid_param("token", "must be a string or null")),
        };

        let profile = mk_db::user::find_profile(pool, &me_id)
            .await
            .map_err(|e| db_err("find_profile[update-email]", e))?;

        // `if (profile.twoFactorEnabled) { if (token == null) throw ... }`
        if profile.two_factor_enabled {
            let Some(t) = two_factor_token.as_deref() else {
                return Err(internal()); // Error('authentication failed')
            };
            let secrets = mk_db::two_factor::fetch_secrets(pool, &me_id)
                .await
                .map_err(|e| db_err("fetch_secrets[update-email]", e))?;
            let env = mk_db::two_factor::OtpEnv::from_config_test_and_flag(state.config.test);
            if !super::two_factor::two_factor_authenticate(pool, &me_id, &secrets, t, env).await {
                return Err(internal()); // Error('authentication failed')
            }
        }

        // `bcrypt.compare(ps.password, profile.password!)` — a NULL hash fails
        // the compare upstream (bcrypt throws).
        let matched = profile
            .password
            .as_deref()
            .map(|h| mk_core::services::verify_password(password, h).unwrap_or(false))
            .unwrap_or(false);
        if !matched {
            return Err(meta_err(
                "INCORRECT_PASSWORD",
                "e54c1d7e-e7d6-4103-86b6-0a95069b4ad3",
                "Incorrect password.",
            ));
        }

        // `else if (meta.emailRequiredForSignup) throw EMAIL_REQUIRED`.
        if email.is_none() {
            let required: Option<bool> =
                mk_db::sqlx::query_scalar(r#"SELECT "emailRequiredForSignup" FROM "meta" LIMIT 1"#)
                    .fetch_optional(pool)
                    .await
                    .map_err(|e| db_err("meta.emailRequiredForSignup", e))?;
            if required.unwrap_or(false) {
                return Err(meta_err(
                    "EMAIL_REQUIRED",
                    "324c7a88-59f2-492f-903f-89134f93e47e",
                    "Email address is required.",
                ));
            }
        }

        // `userProfilesRepository.update(me.id, { email, emailVerified:
        // false, emailVerifyCode: null })`.
        mk_db::sqlx::query(
            r#"UPDATE "user_profile"
               SET "email" = $1, "emailVerified" = FALSE, "emailVerifyCode" = NULL
               WHERE "userId" = $2"#,
        )
        .bind(email.as_deref())
        .bind(&me_id)
        .execute(pool)
        .await
        .map_err(|e| db_err("update_email", e))?;

        // MeDetailed includeSecrets pack (same tail as i/update).
        let row = mk_db::user::find_user_by_id(pool, &me_id)
            .await
            .map_err(|e| db_err("find_user_by_id[update-email]", e))?
            .ok_or_else(internal)?;
        let profile = mk_db::user::find_profile(pool, &me_id)
            .await
            .map_err(|e| db_err("find_profile[fresh]", e))?;
        let ctx = mk_core::services::user_pack::InstanceCtx {
            url: &state.config.url,
            host: &state.config.host(),
        };
        let mut packed = mk_core::services::user_pack::pack_inline(&row, &profile, true, None, ctx);
        super::apply_me_unread(pool, &mut packed).await;
        super::apply_me_role_compat(&mut packed);
        super::apply_me_security_keys(pool, profile.two_factor_enabled, &row.id, &mut packed).await;

        // Fresh 16-char code + mail; the verify-email lane consumes it.
        if let Some(addr) = email.as_deref() {
            let code = mk_core::services::email::generate_code();
            mk_db::user::set_email_verify_code(pool, &me_id, addr, &code)
                .await
                .map_err(|e| db_err("set_email_verify_code[update-email]", e))?;
            let link = format!("{}/verify-email/{}", state.config.url, code);
            let html = format!(
                "To verify email, please click this link:<br><a href=\"{link}\">{link}</a>"
            );
            let text = format!("To verify email, please click this link: {link}");
            let sender = mk_core::services::email::NoopSender;
            let _ = mk_core::services::email::EmailSender::send_email(
                &sender,
                addr,
                "Email verification",
                &html,
                &text,
            )
            .await;
        }

        Ok(Json(packed).into_response())
    }
    .await;
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

// --- i/revoke-token ---

/// Resolved caller: user id + the access-token row id when the credential
/// itself is an app/access token (native tokens carry `None`).
async fn resolve_caller(
    pool: &mk_db::sqlx::PgPool,
    token: Option<&str>,
) -> Check<(String, Option<String>)> {
    let Some(t) = token else {
        // endpoints/i/revoke-token.ts `credentialRequired`.
        return Err(credential_required("6f1f0d3a-3d5b-4b1f-9c3e-2a6d1e5b8c47"));
    };
    let resolved = if t.len() == 16 {
        match mk_db::user::find_local_user_id_by_token(pool, t).await {
            Ok(Some(id)) => (id, None),
            Ok(None) => return Err(auth_failed()),
            Err(e) => return Err(db_err("find_local_user_id_by_token", e)),
        }
    } else {
        // AuthenticateService access-token branch: hash (lowercased) or the
        // raw token column.
        let row: Option<(String, String)> = mk_db::sqlx::query_as(
            r#"SELECT "id", "userId" FROM "access_token"
               WHERE "hash" = $1 OR "token" = $2 LIMIT 1"#,
        )
        .bind(t.to_lowercase())
        .bind(t)
        .fetch_optional(pool)
        .await
        .map_err(|e| db_err("access_token find", e))?;
        match row {
            Some((id, user_id)) => (user_id, Some(id)),
            None => return Err(auth_failed()),
        }
    };
    match mk_db::user::find_user_by_id(pool, &resolved.0).await {
        Ok(Some(u)) if u.is_suspended => return Err(account_suspended()),
        Ok(Some(_)) => {}
        Ok(None) => return Err(auth_failed()),
        Err(e) => return Err(db_err("find_user_by_id[revoke-token]", e)),
    }
    Ok(resolved)
}

/// `POST /api/i/revoke-token` — endpoints/i/revoke-token.ts.
async fn revoke_token(pool: &mk_db::sqlx::PgPool, body: &Value, token: Option<&str>) -> Response {
    let result: Check<Response> = async {
        // ApiCallService authenticates before paramDef validation.
        let (me_id, caller_token_id) = resolve_caller(pool, token).await?;

        // paramDef anyOf: `{ tokenId }` | `{ token: string|null }`.
        let token_id: Option<String> = match body.get("tokenId") {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            Some(_) => return Err(invalid_param("tokenId", "must be a string")),
            None => None,
        };
        let token_param: Option<Option<String>> = match body.get("token") {
            None => None,
            Some(Value::Null) => Some(None),
            Some(Value::String(s)) => Some(Some(s.clone())),
            Some(_) => return Err(invalid_param("token", "must be a string or null")),
        };

        let target: Option<String> = if let Some(id) = token_id {
            mk_db::sqlx::query_scalar(
                r#"SELECT "id" FROM "access_token" WHERE "id" = $1 AND "userId" = $2"#,
            )
            .bind(id)
            .bind(&me_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| db_err("access_token find by id", e))?
        } else {
            let Some(Some(t)) = token_param else {
                return Err(invalid_param(
                    "tokenId",
                    "must have required property 'tokenId'",
                ));
            };
            // `if (ps.token == null || ps.token === '') return;`
            if t.is_empty() {
                return Ok(StatusCode::NO_CONTENT.into_response());
            }
            mk_db::sqlx::query_scalar(
                r#"SELECT "id" FROM "access_token" WHERE "token" = $1 AND "userId" = $2"#,
            )
            .bind(&t)
            .bind(&me_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| db_err("access_token find by token", e))?
        };

        // `if (target == null) return;`
        let Some(target_id) = target else {
            return Ok(StatusCode::NO_CONTENT.into_response());
        };

        // Third-party app tokens may only revoke themselves.
        if let Some(caller_id) = caller_token_id {
            if caller_id != target_id {
                return Err(api_err(ApiError::new(
                    403,
                    mk_types::api_error::ApiErrorKind::Permission,
                    "PERMISSION_DENIED",
                    "fc20d118-5705-4462-b6c5-2b5b43092cf3",
                    "Permission denied.",
                )));
            }
        }

        mk_db::sqlx::query(r#"DELETE FROM "access_token" WHERE "id" = $1"#)
            .bind(&target_id)
            .execute(pool)
            .await
            .map_err(|e| db_err("access_token delete", e))?;
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}
