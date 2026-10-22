//! `admin/abuse-*` family — ports of upstream
//! `endpoints/admin/{abuse-user-reports,resolve-abuse-user-report,
//! forward-abuse-user-report}.ts` and
//! `endpoints/admin/abuse-report/notification-recipient/{list,create,delete,
//! show,update}.ts` (pinned ea2c3aed).
//!
//! Gate: upstream `requireCredential` + `requireModerator` — the shared
//! `admin::gate(Gate::Moderator)` chain (credential → suspended →
//! ROLE_PERMISSION_DENIED with the root bypass). The `secure: true` recipient
//! metas ride the same native/app-token resolution.
//!
//! The resolve handler keeps the synalio `abuseReportResolved` system-webhook
//! fan-out (`system_webhook::fire`); list/forward/recipient CRUD are pure db
//! paths in `mk_db::abuse` (tables ship in the 0001 baseline).
//!
//! DOCUMENTED SKIPS:
//! - forward's outbound AP `Flag` delivery (SystemAccount + inbox renderer)
//!   is absent; the `forwarded` flag is what the admin list reads.
//! - sinceDate/untilDate pagination bounds are not translated to AID ranges.

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
        "admin/abuse-user-reports"
            | "admin/resolve-abuse-user-report"
            | "admin/forward-abuse-user-report"
            | "admin/abuse-report/notification-recipient/list"
            | "admin/abuse-report/notification-recipient/create"
            | "admin/abuse-report/notification-recipient/delete"
            | "admin/abuse-report/notification-recipient/show"
            | "admin/abuse-report/notification-recipient/update"
    )
}

type Check<T = Response> = Result<T, Response>;

/// Entry from the admin-abuse dispatch arm in mod.rs (body pre-parsed, token
/// pre-extracted).
pub(super) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<&str>,
) -> Response {
    let ctx = match gate(state, token, Gate::Moderator).await {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    let Some(pool) = &state.pool else {
        return internal();
    };
    let result: Check = match name {
        "admin/abuse-user-reports" => reports_list(state, pool, body).await,
        "admin/resolve-abuse-user-report" => report_resolve(state, pool, body, &ctx.me.id).await,
        "admin/forward-abuse-user-report" => report_forward(pool, body).await,
        "admin/abuse-report/notification-recipient/list" => recipient_list(state, pool, body).await,
        "admin/abuse-report/notification-recipient/create" => {
            recipient_create(state, pool, body).await
        }
        "admin/abuse-report/notification-recipient/delete" => recipient_delete(pool, body).await,
        "admin/abuse-report/notification-recipient/show" => recipient_show(state, pool, body).await,
        "admin/abuse-report/notification-recipient/update" => {
            recipient_update(state, pool, body).await
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

fn no_such_report() -> Response {
    api_err(ApiError::new(
        404,
        mk_types::api_error::ApiErrorKind::Server,
        "NO_SUCH_ABUSE_REPORT",
        "ac3794dd-2ce4-d878-e546-73c60c06b398",
        "No such abuse report.",
    ))
}

fn no_such_recipient() -> Response {
    api_err(ApiError::new(
        404,
        mk_types::api_error::ApiErrorKind::Server,
        "NO_SUCH_RECIPIENT",
        "013de6a8-f757-04cb-4d73-cc2a7e3368e4",
        "No such recipient.",
    ))
}

fn correlation_check_email() -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "CORRELATION_CHECK_EMAIL",
        "348bb8ae-575a-6fe9-4327-5811999def8f",
        "If \"method\" is email, \"userId\" must be set.",
    ))
}

fn correlation_check_webhook() -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "CORRELATION_CHECK_WEBHOOK",
        "b0c15051-de2d-29ef-260c-9585cddd701a",
        "If \"method\" is webhook, \"systemWebhookId\" must be set.",
    ))
}

fn email_address_not_set() -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "EMAIL_ADDRESS_NOT_SET",
        "7cc1d85e-2f58-fc31-b644-3de8d0d3421f",
        "Email address is not set.",
    ))
}

// ---------------------------------------------------------------------------
// entity packs
// ---------------------------------------------------------------------------

async fn detailed(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    user_id: &str,
) -> Result<Value, Response> {
    Ok(super::freq::pack_user_detailed(state, pool, user_id, None)
        .await?
        .unwrap_or(Value::Null))
}

/// `AbuseUserReportEntityService.pack` (admin schema: reporter/targetUser/
/// assignee are UserDetailedNotMe).
async fn pack_report(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    r: &mk_db::abuse::AdminReportRow,
) -> Result<Value, Response> {
    let target_user = detailed(state, pool, &r.target_user_id).await?;
    let reporter = detailed(state, pool, &r.reporter_id).await?;
    let assignee = match &r.assignee_id {
        Some(id) => detailed(state, pool, id).await?,
        None => Value::Null,
    };
    Ok(json!({
        "id": r.id,
        "createdAt": mk_types::id::parse_aid_date(&r.id)
            .map(mk_core::services::signup::ms_to_iso)
            .unwrap_or_default(),
        "comment": r.comment,
        "resolved": r.resolved,
        "reporterId": r.reporter_id,
        "targetUserId": r.target_user_id,
        "targetUserHost": r.target_user_host,
        "reporterHost": r.reporter_host,
        "assigneeId": r.assignee_id,
        "reporter": reporter,
        "targetUser": target_user,
        "assignee": assignee,
        "forwarded": r.forwarded,
        "resolvedAs": r.resolved_as,
        "moderationNote": r.moderation_note,
    }))
}

/// UserLite pack (`UserEntityService.pack` default schema) for the recipient
/// companion field.
async fn lite_of(state: &AppState, pool: &mk_db::sqlx::PgPool, user_id: &str) -> Value {
    let Ok(Some(row)) = mk_db::user::find_user_by_id(pool, user_id).await else {
        return Value::Null;
    };
    let Ok(profile) = mk_db::user::find_profile(pool, &row.id).await else {
        return Value::Null;
    };
    let ctx = mk_core::services::user_pack::InstanceCtx {
        url: &state.config.url,
        host: &state.config.host(),
    };
    mk_core::services::user_pack::user_lite_value(&row, &profile, ctx)
}

fn pack_webhook(r: &mk_db::webhook::SystemWebhookRow) -> Value {
    json!({
        "id": r.id,
        "isActive": r.is_active,
        "updatedAt": mk_core::services::signup::ms_to_iso(r.updated_at_ms),
        "latestSentAt": Value::Null,
        "latestStatus": Value::Null,
        "name": r.name,
        "on": r.on,
        "url": r.url,
        "secret": r.secret,
    })
}

/// `AbuseReportNotificationRecipientEntityService.pack`.
async fn pack_recipient(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    r: &mk_db::webhook::RecipientRow,
) -> Value {
    let user = match &r.user_id {
        Some(id) => lite_of(state, pool, id).await,
        None => Value::Null,
    };
    let system_webhook = match &r.system_webhook_id {
        Some(id) => match mk_db::webhook::find_system_webhook(pool, id).await {
            Ok(Some(w)) => pack_webhook(&w),
            _ => Value::Null,
        },
        None => Value::Null,
    };
    json!({
        "id": r.id,
        "isActive": r.is_active,
        "updatedAt": mk_core::services::signup::ms_to_iso(r.updated_at_ms),
        "name": r.name,
        "method": r.method,
        "userId": r.user_id,
        "user": user,
        "systemWebhookId": r.system_webhook_id,
        "systemWebhook": system_webhook,
    })
}

// ---------------------------------------------------------------------------
// admin/abuse-user-reports + resolve + forward
// ---------------------------------------------------------------------------

async fn reports_list(state: &AppState, pool: &mk_db::sqlx::PgPool, body: &Value) -> Check {
    let limit = body
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(10)
        .clamp(1, 100);
    let since_id = body.get("sinceId").and_then(Value::as_str);
    let until_id = body.get("untilId").and_then(Value::as_str);
    let state_filter = body.get("state").and_then(Value::as_str);
    let reporter_origin = body
        .get("reporterOrigin")
        .and_then(Value::as_str)
        .unwrap_or("combined");
    let target_user_origin = body
        .get("targetUserOrigin")
        .and_then(Value::as_str)
        .unwrap_or("combined");
    let rows = mk_db::abuse::list_reports(
        pool,
        limit,
        since_id,
        until_id,
        state_filter,
        reporter_origin,
        target_user_origin,
    )
    .await
    .map_err(|e| {
        tracing::error!(target: "dbg_abuse", error = %e, "list_reports");
        internal()
    })?;
    let mut out: Vec<Value> = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(pack_report(state, pool, row).await?);
    }
    Ok(Json(json!(out)).into_response())
}

async fn report_resolve(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Check {
    let Some(report_id) = body.get("reportId").and_then(Value::as_str) else {
        return Err(super::invalid_param("reportId", "required"));
    };
    let resolved_as = match body.get("resolvedAs") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s == "accept" || s == "reject" => Some(s.clone()),
        Some(_) => {
            return Err(super::invalid_param(
                "resolvedAs",
                "expected one of: accept, reject, null",
            ));
        }
    };
    match mk_db::abuse::find_report(pool, report_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return Err(no_such_report()),
        Err(e) => {
            tracing::error!(target: "dbg_abuse", error = %e, "find_report");
            return Err(internal());
        }
    }
    mk_db::abuse::resolve_report(pool, report_id, me_id, resolved_as.as_deref())
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_abuse", error = %e, "resolve_report");
            internal()
        })?;
    let row = mk_db::abuse::find_report(pool, report_id)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_abuse", error = %e, "find_report");
            internal()
        })?
        .ok_or_else(internal)?;
    let content = pack_report(state, pool, &row).await?;
    tokio::spawn(super::system_webhook::fire(
        pool.clone(),
        state.config.url.clone(),
        state.config.host(),
        "abuseReportResolved",
        content,
    ));
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn report_forward(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check {
    let Some(report_id) = body.get("reportId").and_then(Value::as_str) else {
        return Err(super::invalid_param("reportId", "required"));
    };
    let row = match mk_db::abuse::find_report(pool, report_id).await {
        Ok(Some(row)) => row,
        Ok(None) => return Err(no_such_report()),
        Err(e) => {
            tracing::error!(target: "dbg_abuse", error = %e, "find_report");
            return Err(internal());
        }
    };
    // AbuseReportService.forward: target must be remote and not yet forwarded
    // (upstream throws plain Errors → 500 INTERNAL_ERROR).
    if row.target_user_host.is_none() || row.forwarded {
        return Err(internal());
    }
    mk_db::abuse::forward_report(pool, report_id)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_abuse", error = %e, "forward_report");
            internal()
        })?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---------------------------------------------------------------------------
// admin/abuse-report/notification-recipient/*
// ---------------------------------------------------------------------------

fn method_array(body: &Value) -> Result<Vec<String>, Response> {
    match body.get("method") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some("email") | Some("webhook") => out.push(item.as_str().unwrap().to_string()),
                    _ => {
                        return Err(super::invalid_param(
                            "method",
                            "expected an array of 'email' or 'webhook'",
                        ));
                    }
                }
            }
            Ok(out)
        }
        Some(_) => Err(super::invalid_param("method", "expected an array")),
    }
}

async fn recipient_list(state: &AppState, pool: &mk_db::sqlx::PgPool, body: &Value) -> Check {
    let methods = method_array(body)?;
    let rows = mk_db::abuse::list_recipients(pool, &methods)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_abuse", error = %e, "list_recipients");
            internal()
        })?;
    let mut out: Vec<Value> = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(pack_recipient(state, pool, row).await);
    }
    Ok(Json(json!(out)).into_response())
}

/// The shared create/update body validation (`isActive`, `name`, `method` +
/// the email/webhook correlation checks).
async fn recipient_params(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
) -> Result<(bool, String, String, Option<String>, Option<String>), Response> {
    let Some(is_active) = body.get("isActive").and_then(Value::as_bool) else {
        return Err(super::invalid_param("isActive", "required"));
    };
    let Some(name) = body.get("name").and_then(Value::as_str) else {
        return Err(super::invalid_param("name", "required"));
    };
    if name.is_empty() || name.chars().count() > 255 {
        return Err(super::invalid_param(
            "name",
            "must be 1 to 255 characters long",
        ));
    }
    let Some(method) = body.get("method").and_then(Value::as_str) else {
        return Err(super::invalid_param("method", "required"));
    };
    if method != "email" && method != "webhook" {
        return Err(super::invalid_param(
            "method",
            "expected either 'email' or 'webhook'",
        ));
    }
    let user_id = body.get("userId").and_then(Value::as_str);
    let system_webhook_id = body.get("systemWebhookId").and_then(Value::as_str);
    if method == "email" {
        let Some(user_id) = user_id else {
            return Err(correlation_check_email());
        };
        let profile = match mk_db::user::find_profile(pool, user_id).await {
            Ok(profile) => profile,
            Err(_) => return Err(correlation_check_email()),
        };
        if profile.email.as_deref().map_or(true, str::is_empty) || !profile.email_verified {
            return Err(email_address_not_set());
        }
    } else if system_webhook_id.is_none() {
        return Err(correlation_check_webhook());
    }
    Ok((
        is_active,
        name.to_string(),
        method.to_string(),
        user_id.map(str::to_string),
        system_webhook_id.map(str::to_string),
    ))
}

async fn recipient_create(state: &AppState, pool: &mk_db::sqlx::PgPool, body: &Value) -> Check {
    let (is_active, name, method, user_id, system_webhook_id) =
        recipient_params(pool, body).await?;
    let id = mk_types::id::gen_aid_now();
    mk_db::webhook::insert_recipient(
        pool,
        &id,
        &name,
        &method,
        is_active,
        user_id.as_deref(),
        system_webhook_id.as_deref(),
    )
    .await
    .map_err(|e| {
        tracing::error!(target: "dbg_abuse", error = %e, "insert_recipient");
        internal()
    })?;
    let row = mk_db::abuse::find_recipient(pool, &id)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_abuse", error = %e, "find_recipient");
            internal()
        })?
        .ok_or_else(internal)?;
    Ok(Json(pack_recipient(state, pool, &row).await).into_response())
}

async fn recipient_show(state: &AppState, pool: &mk_db::sqlx::PgPool, body: &Value) -> Check {
    let Some(id) = body.get("id").and_then(Value::as_str) else {
        return Err(super::invalid_param("id", "required"));
    };
    let row = mk_db::abuse::find_recipient(pool, id)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_abuse", error = %e, "find_recipient");
            internal()
        })?
        .ok_or_else(no_such_recipient)?;
    Ok(Json(pack_recipient(state, pool, &row).await).into_response())
}

async fn recipient_update(state: &AppState, pool: &mk_db::sqlx::PgPool, body: &Value) -> Check {
    let Some(id) = body.get("id").and_then(Value::as_str) else {
        return Err(super::invalid_param("id", "required"));
    };
    let (is_active, name, method, user_id, system_webhook_id) =
        recipient_params(pool, body).await?;
    let updated = mk_db::abuse::update_recipient(
        pool,
        id,
        is_active,
        &name,
        &method,
        user_id.as_deref(),
        system_webhook_id.as_deref(),
    )
    .await
    .map_err(|e| {
        tracing::error!(target: "dbg_abuse", error = %e, "update_recipient");
        internal()
    })?;
    if !updated {
        return Err(no_such_recipient());
    }
    let row = mk_db::abuse::find_recipient(pool, id)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_abuse", error = %e, "find_recipient");
            internal()
        })?
        .ok_or_else(no_such_recipient)?;
    Ok(Json(pack_recipient(state, pool, &row).await).into_response())
}

async fn recipient_delete(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check {
    let Some(id) = body.get("id").and_then(Value::as_str) else {
        return Err(super::invalid_param("id", "required"));
    };
    mk_db::abuse::delete_recipient(pool, id)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_abuse", error = %e, "delete_recipient");
            internal()
        })?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
