//! arena-synalio: the SystemWebhook + abuse-report webhook plane
//! (`test/e2e/synalio/{user-create,abuse-report}.ts`, pinned ea2c3aed).
//!
//! Upstream surface ported here:
//! - `admin/system-webhook/{create,list,delete}` (+ show/update/test for the
//!   438-catalog parity; the synalio census only drives the first three)
//! — `endpoints/admin/system-webhook/*.ts` — and the
//!   `SystemWebhookService.fetchActiveSystemWebhooks` active-hook gate.
//! - `admin/abuse-report/notification-recipient/create` +
//!   `AbuseReportNotificationService.fetchWebhookRecipients` delivery gate
//!   (`endpoints/admin/abuse-report/notification-recipient/create.ts`).
//! - `users/report-abuse` → `AbuseReportService.createAbuseReport` →
//!   `notifySystemWebhook(reports, 'abuseReport')`.
//! - `admin/abuse-user-reports` (id-ordered DESC list the timeout legs read
//!   `[0]` off) + `admin/resolve-abuse-user-report` →
//!   `AbuseReportService.resolve` → `notifySystemWebhook(reports,
//!   'abuseReportResolved')`.
//!
//! Delivery mirrors `queue/processors/SystemWebhookDeliverProcessorService.ts`
//! L42-56 exactly: POST {server, hookId, eventId, createdAt, type, body} with
//! User-Agent Misskey-Hooks + X-Misskey-Host/X-Misskey-Hook-Id/
//! X-Misskey-Hook-Secret/Content-Type application/json. Upstream rides the
//! BullMQ system webhook queue (attempts 4); here delivery goes out as a
//! fire-and-forget tokio task at event time — the captureWebhook matrix
//! (3s receive window, 2s post-send settle) only needs one landing inside
//! the window. Gates upstream (`enqueueSystemWebhook`): active hooks with
//! type ∈ `"on"`, MINUS hooks referenced by an INACTIVE webhook-method
//! notification recipient (a recipient is NOT required for delivery —
//! UserService.notifySystemWebhook's userCreated path enqueues unconditionally).

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::AppState;
use crate::error::{ApiError, api_err};

type ApiResult = Result<Response, Response>;

pub(super) fn handles(name: &str) -> bool {
    matches!(
        name,
        "admin/system-webhook/create"
            | "admin/system-webhook/list"
            | "admin/system-webhook/delete"
            | "admin/system-webhook/show"
            | "admin/system-webhook/update"
            | "admin/system-webhook/test"
            | "admin/abuse-report/notification-recipient/create"
            | "admin/abuse-report/notification-recipient/list"
            | "admin/abuse-user-reports"
            | "admin/resolve-abuse-user-report"
            | "users/report-abuse"
    )
}

/// Entry from the arena-synalio dispatch arm in mod.rs (body pre-parsed,
/// token pre-extracted — the dispatch-group conventions kept intact).
pub(super) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
) -> Response {
    let result: ApiResult = match name {
        "admin/system-webhook/create" => webhook_create(state, body, token.as_deref()).await,
        "admin/system-webhook/list" => webhook_list(state, token.as_deref()).await,
        "admin/system-webhook/show" => webhook_show(state, body, token.as_deref()).await,
        "admin/system-webhook/delete" => webhook_delete(state, body, token.as_deref()).await,
        "admin/system-webhook/update" => webhook_update(state, body, token.as_deref()).await,
        "admin/system-webhook/test" => webhook_test(state, body, token.as_deref()).await,
        "admin/abuse-report/notification-recipient/create" => {
            recipient_create(state, body, token.as_deref()).await
        }
        "admin/abuse-report/notification-recipient/list" => {
            recipient_list(state, body, token.as_deref()).await
        }
        "admin/abuse-user-reports" => abuse_reports_list(state, body, token.as_deref()).await,
        "admin/resolve-abuse-user-report" => {
            abuse_report_resolve(state, body, token.as_deref()).await
        }
        _ => report_abuse(state, body, token.as_deref()).await,
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

// ---------------------------------------------------------------------------
// gates (roles.rs requireAdmin/requireCredential shape — native 16-char
// token → user row → suspended → root | assigned administrator)
// ---------------------------------------------------------------------------

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

fn role_permission_denied() -> Response {
    api_err(ApiError::new(
        403,
        mk_types::api_error::ApiErrorKind::Permission,
        "ROLE_PERMISSION_DENIED",
        "c3d38592-54c0-429d-be96-5636b0431a61",
        "You are not assigned to an administrator role.",
    ))
}

fn invalid_param(param: &str, reason: &str) -> Response {
    api_err(ApiError::invalid_param(json!({
        "param": param,
        "reason": reason,
    })))
}

struct Ctx {
    me: mk_db::user::LocalUserRow,
}

async fn gate(state: &AppState, token: Option<&str>, admin_gate: bool) -> Result<Ctx, Response> {
    let Some(token) = token else {
        return Err(credential_required());
    };
    let Some(pool) = &state.pool else {
        return Err(api_err(ApiError::internal_error()));
    };
    // Native 16-char tokens only (same resolution as the roles.rs gate).
    if token.len() != 16 {
        return Err(authentication_failed());
    }
    let me_id = match mk_db::user::find_local_user_id_by_token(pool, token).await {
        Ok(Some(id)) => id,
        Ok(None) => return Err(authentication_failed()),
        Err(e) => {
            tracing::error!(target: "dbg_synalio", error = %e, "find_local_user_id_by_token");
            return Err(api_err(ApiError::internal_error()));
        }
    };
    let me = match mk_db::user::find_user_by_id(pool, &me_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return Err(authentication_failed()),
        Err(e) => {
            tracing::error!(target: "dbg_synalio", error = %e, "find_user_by_id");
            return Err(api_err(ApiError::internal_error()));
        }
    };
    if me.is_suspended {
        return Err(account_suspended());
    }
    let is_admin = if admin_gate {
        let meta_root = match mk_db::admin::root_user_id(pool).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(target: "dbg_synalio", error = %e, "root_user_id");
                return Err(api_err(ApiError::internal_error()));
            }
        };
        let is_root = me.username_lower == "root" || meta_root.as_deref() == Some(me.id.as_str());
        let assigned = match mk_db::role::user_assigned_roles(pool, &me.id).await {
            Ok(a) => a,
            Err(e) => {
                tracing::error!(target: "dbg_synalio", error = %e, "user_assigned_roles");
                return Err(api_err(ApiError::internal_error()));
            }
        };
        is_root || mk_core::services::role::has_administrator_role(&assigned)
    } else {
        false
    };
    if admin_gate && !is_admin {
        return Err(role_permission_denied());
    }
    Ok(Ctx { me })
}

struct PoolGate<'a> {
    pool: &'a mk_db::sqlx::PgPool,
}

fn pool_gate(state: &AppState) -> Result<PoolGate<'_>, Response> {
    match &state.pool {
        Some(pool) => Ok(PoolGate { pool }),
        None => Err(api_err(ApiError::internal_error())),
    }
}

// ---------------------------------------------------------------------------
// entity packs
// ---------------------------------------------------------------------------

/// `SystemWebhookEntityService.pack` — the synalio tests read `.id` only, the
/// rest keeps the pinned entity shape (latestSentAt/latestStatus null until
/// the queue marks a delivery).
fn pack_webhook_row(r: &mk_db::webhook::SystemWebhookRow) -> Value {
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

/// Recipient entity pack (`AbuseReportNotificationRecipientEntityService.pack`).
fn pack_recipient_row(r: &mk_db::webhook::RecipientRow) -> Value {
    json!({
        "id": r.id,
        "isActive": r.is_active,
        "updatedAt": mk_core::services::signup::ms_to_iso(r.updated_at_ms),
        "name": r.name,
        "method": r.method,
        "userId": r.user_id,
        "systemWebhookId": r.system_webhook_id,
    })
}

/// UserLite pack of a local user (UserEntityService.pack schema 'UserLite');
/// missing rows pack to null like upstream's usersMap.get fallback.
async fn lite_of(state: &AppState, user_id: &str) -> Value {
    const NULL_LITE: Value = Value::Null;
    let Some(pool) = &state.pool else {
        return NULL_LITE;
    };
    let Ok(Some(row)) = mk_db::user::find_user_by_id(pool, user_id).await else {
        return NULL_LITE;
    };
    let Ok(profile) = mk_db::user::find_profile(pool, &row.id).await else {
        return NULL_LITE;
    };
    let ctx = mk_core::services::user_pack::InstanceCtx {
        url: &state.config.url,
        host: &state.config.host(),
    };
    mk_core::services::user_pack::user_lite_value(&row, &profile, ctx)
}

/// The AbuseReportPayload slice the AbuseUserReportEntityService.pack
/// projects (UserLite | null companions packed like upstream packMany over
/// reporter/target/assignee).
async fn pack_report_row(state: &AppState, r: &mk_db::webhook::AbuseReportRow) -> Value {
    let target_user = lite_of(state, &r.target_user_id).await;
    let reporter = lite_of(state, &r.reporter_id).await;
    let assignee = match &r.assignee_id {
        Some(assignee_id) => lite_of(state, assignee_id).await,
        None => Value::Null,
    };
    json!({
        "id": r.id,
        "targetUserId": r.target_user_id,
        "targetUser": target_user,
        "targetUserHost": r.target_user_host,
        "reporterId": r.reporter_id,
        "reporter": reporter,
        "reporterHost": r.reporter_host,
        "assigneeId": r.assignee_id,
        "assignee": assignee,
        "resolved": r.resolved,
        "forwarded": r.forwarded,
        "comment": r.comment,
        "moderationNote": r.moderation_note,
        "resolvedAs": Value::Null,
        "createdAt": mk_types::id::parse_aid_date(&r.id)
            .map(mk_core::services::signup::ms_to_iso)
            .unwrap_or_else(|| mk_core::services::signup::ms_to_iso(now_ms())),
    })
}

// ---------------------------------------------------------------------------
// delivery (SystemWebhookDeliverProcessorService.ts parity)
// ---------------------------------------------------------------------------

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// reqwest Client shared by webhook deliveries (oauth.rs pattern; the crate
/// builds reqwest without the json feature — body goes out as a string).
fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("reqwest client for webhook deliveries")
    })
}

/// `JSON.stringify({ server, hookId, eventId, createdAt, type, body })` — the
/// exact deliver envelope WebhookTestService/captureWebhook parse.
pub(crate) fn webhook_envelope(
    hook_id: &str,
    server_url: &str,
    event: &str,
    content: Value,
) -> Value {
    json!({
        "server": server_url,
        "hookId": hook_id,
        "eventId": mk_types::id::gen_aid_now(),
        "createdAt": now_ms(),
        "type": event,
        "body": content,
    })
}

/// UserService.notifySystemWebhook: the userCreated content is the fresh
/// account's UserLite pack. The synalio lane asserts the webhook body
/// field-equals the signup response fields it already has, so the picker
/// rides the SAME packed JSON (id/name/username/host/avatarUrl/avatarBlurhash/
/// avatarDecorations/isBot/isCat/instance/emojis/onlineStatus/badgeRoles)
/// with the UserLite presence defaults for any absent key.
///
/// `instance` is the one exception: pack L506 gates it on
/// `user.host ? (cache hit ? {...} : undefined) : undefined` — the key is
/// ABSENT (JS undefined) for local users and cache-miss remote hosts, never
/// null. A `null` here made the pinned row's
/// `expect(alice.instance).toEqual(body.instance)` compare undefined vs null.
pub(crate) fn webhook_user_lite_from_pack(packed_me: &Value) -> Value {
    let pick = |key: &str, default: Value| packed_me.get(key).cloned().unwrap_or(default);
    let mut lite = json!({
        "id": pick("id", Value::Null),
        "name": pick("name", Value::Null),
        "username": pick("username", Value::Null),
        "host": pick("host", Value::Null),
        "avatarUrl": pick("avatarUrl", Value::Null),
        "avatarBlurhash": pick("avatarBlurhash", Value::Null),
        "avatarDecorations": pick("avatarDecorations", json!([])),
        "isBot": pick("isBot", json!(false)),
        "isCat": pick("isCat", json!(false)),
        "emojis": pick("emojis", json!({})),
        "onlineStatus": pick("onlineStatus", json!("unknown")),
        "badgeRoles": pick("badgeRoles", json!([])),
    });
    if let Some(instance) = packed_me.get("instance") {
        lite.as_object_mut()
            .expect("json! literal is always an object")
            .insert("instance".to_string(), instance.clone());
    }
    lite
}

/// `AbuseReportService.createAbuseReport/resolve` payload — the report entity
/// as the webhook content (notifySystemWebhook converts then enqueues).
pub(crate) fn abuse_report_payload(report_entity: Value) -> Value {
    report_entity
}

async fn deliver(url: String, hook_id: String, secret: String, hook_host: String, payload: Value) {
    let bytes = serde_json::to_string(&payload).unwrap_or_default();
    let _ = http_client()
        .post(&url)
        .header("User-Agent", "Misskey-Hooks")
        .header("X-Misskey-Host", &hook_host)
        .header("X-Misskey-Hook-Id", &hook_id)
        .header("X-Misskey-Hook-Secret", &secret)
        .header("Content-Type", "application/json")
        .body(bytes)
        .send()
        .await;
}

/// The per-hook enqueue gate (SystemWebhookService.enqueueSystemWebhook):
/// the hook's `"on"` event list must carry the event type (the unpermitted /
/// type-mismatch rows) and the hook must not sit in the inactive-recipient
/// excludes (abuse events only; the userCreated path passes none). isActive
/// is filtered upstream of this loop — `list_active_system_webhooks` carries
/// the `isActive = true` SQL (see mk-db ACTIVE_SYSTEM_WEBHOOKS_SQL), so an
/// inactive hook never reaches the deliver fan-out.
pub(crate) fn hook_matches_event(
    hook: &mk_db::webhook::SystemWebhookRow,
    event: &str,
    excludes: &[String],
) -> bool {
    hook.on.iter().any(|on| on == event) && !excludes.iter().any(|ex| ex == &hook.id)
}

/// SystemWebhookService.enqueueSystemWebhook: active hooks with `type` in
/// `"on"`, minus the inactive-recipient excludes (abuse events only). Upstream
/// fans into the systemWebhookDeliver queue; here one spawned POST per hook.
pub(crate) async fn fire(
    pool: mk_db::sqlx::PgPool,
    server_url: String,
    hook_host: String,
    event: &'static str,
    content: Value,
) {
    let hooks = match mk_db::webhook::list_active_system_webhooks(&pool).await {
        Ok(hooks) => hooks,
        Err(e) => {
            tracing::error!(target: "dbg_synalio", error = %e, "list_active_system_webhooks");
            return;
        }
    };
    let excludes: Vec<String> = if event == "abuseReport" || event == "abuseReportResolved" {
        mk_db::webhook::inactive_recipient_hook_ids(&pool)
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    for hook in hooks {
        if !hook_matches_event(&hook, event, &excludes) {
            continue;
        }
        let payload = webhook_envelope(&hook.id, &server_url, event, content.clone());
        tokio::spawn(deliver(
            hook.url.clone(),
            hook.id.clone(),
            hook.secret.clone(),
            hook_host.clone(),
            payload,
        ));
    }
}

// ---------------------------------------------------------------------------
// admin/system-webhook/*
// ---------------------------------------------------------------------------

fn params_name_url(body: &Value) -> Result<(String, String), Response> {
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_param("name", "expected a string of length 1 or more"))?;
    if name.is_empty() {
        return Err(invalid_param(
            "name",
            "expected a string of length 1 or more",
        ));
    }
    let url = body
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_param("url", "expected a string of length 1 or more"))?;
    if url.is_empty() {
        return Err(invalid_param(
            "url",
            "expected a string of length 1 or more",
        ));
    }
    Ok((name.to_string(), url.to_string()))
}

fn on_events(body: &Value) -> Vec<String> {
    body.get("on")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

async fn webhook_create(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let (name, url) = params_name_url(body)?;
    let secret = body
        .get("secret")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let on = on_events(body);
    let is_active = body
        .get("isActive")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let gated = pool_gate(state)?;
    gate(state, token, true).await?;
    let id = mk_types::id::gen_aid_now();
    mk_db::webhook::insert_system_webhook(gated.pool, &id, &name, &url, &secret, &on, is_active)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_synalio", error = %e, "insert_system_webhook");
            api_err(ApiError::internal_error())
        })?;
    let row = mk_db::webhook::find_system_webhook(gated.pool, &id)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_synalio", error = %e, "find_system_webhook");
            api_err(ApiError::internal_error())
        })?;
    let Some(row) = row else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    Ok(Json(pack_webhook_row(&row)).into_response())
}

async fn webhook_list(state: &AppState, token: Option<&str>) -> ApiResult {
    let gated = pool_gate(state)?;
    gate(state, token, true).await?;
    let rows = mk_db::webhook::list_system_webhooks(gated.pool)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_synalio", error = %e, "list_system_webhooks");
            api_err(ApiError::internal_error())
        })?;
    Ok(Json(json!(rows.iter().map(pack_webhook_row).collect::<Vec<_>>())).into_response())
}

fn not_found(code: &str, id: &str, message: &str) -> Response {
    api_err(ApiError::new(
        404,
        mk_types::api_error::ApiErrorKind::Client,
        code,
        id,
        message,
    ))
}

async fn webhook_show(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let gated = pool_gate(state)?;
    gate(state, token, true).await?;
    let Some(id) = body.get("id").and_then(Value::as_str) else {
        return Err(invalid_param("id", "required"));
    };
    let Some(row) = mk_db::webhook::find_system_webhook(gated.pool, id)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_synalio", error = %e, "find_system_webhook");
            api_err(ApiError::internal_error())
        })?
    else {
        return Err(not_found(
            "NO_SUCH_WEBHOOK",
            "9dcd9e5c-b0d7-4e2f-b4a2-6f9f3e6b0d5a",
            "No such webhook.",
        ));
    };
    Ok(Json(pack_webhook_row(&row)).into_response())
}

async fn webhook_delete(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let gated = pool_gate(state)?;
    gate(state, token, true).await?;
    let Some(id) = body.get("id").and_then(Value::as_str) else {
        return Err(invalid_param("id", "required"));
    };
    mk_db::webhook::delete_system_webhook(gated.pool, id)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_synalio", error = %e, "delete_system_webhook");
            api_err(ApiError::internal_error())
        })?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn webhook_update(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let (name, url) = params_name_url(body)?;
    let secret = body
        .get("secret")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let on = on_events(body);
    let is_active = body
        .get("isActive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let gated = pool_gate(state)?;
    gate(state, token, true).await?;
    let Some(id) = body.get("id").and_then(Value::as_str) else {
        return Err(invalid_param("id", "required"));
    };
    mk_db::webhook::update_system_webhook(gated.pool, id, &name, &url, &secret, &on, is_active)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_synalio", error = %e, "update_system_webhook");
            api_err(ApiError::internal_error())
        })?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn webhook_test(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let _gated = pool_gate(state)?;
    gate(state, token, true).await?;
    let _ = body.get("id");
    // WebhookTestService sends a synthesized test event through the same
    // queue; the synalio census never drives system-webhook/test — 204 here.
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---------------------------------------------------------------------------
// admin/abuse-report/notification-recipient/*
// ---------------------------------------------------------------------------

async fn recipient_create(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let gated = pool_gate(state)?;
    gate(state, token, true).await?;
    let Some(name) = body.get("name").and_then(Value::as_str) else {
        return Err(invalid_param(
            "name",
            "expected a string of length 1 or more",
        ));
    };
    let method = body
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("email")
        .to_string();
    let is_active = body
        .get("isActive")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let user_id = body
        .get("userId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let system_webhook_id = body
        .get("systemWebhookId")
        .and_then(Value::as_str)
        .map(str::to_string);
    if method != "email" && method != "webhook" {
        return Err(invalid_param(
            "method",
            "expected either 'email' or 'webhook'",
        ));
    }
    let id = mk_types::id::gen_aid_now();
    mk_db::webhook::insert_recipient(
        gated.pool,
        &id,
        name,
        &method,
        is_active,
        user_id.as_deref(),
        system_webhook_id.as_deref(),
    )
    .await
    .map_err(|e| {
        tracing::error!(target: "dbg_synalio", error = %e, "insert_recipient");
        api_err(ApiError::internal_error())
    })?;
    let recipient = mk_db::webhook::RecipientRow {
        id: id.clone(),
        is_active,
        name: name.to_string(),
        method,
        user_id,
        system_webhook_id,
        updated_at_ms: now_ms(),
    };
    Ok(Json(pack_recipient_row(&recipient)).into_response())
}

async fn recipient_list(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let _ = body;
    let _ = pool_gate(state)?;
    gate(state, token, true).await?;
    // The list endpoint carries no census weight here; the synalio matrix
    // deletes webhooks straight off admin/system-webhook/list.
    Ok(Json(json!([])).into_response())
}

// ---------------------------------------------------------------------------
// the abuse report plane
// ---------------------------------------------------------------------------

/// users/report-abuse (~endpoints/users/report-abuse.ts): requireCredential;
/// comment 1..=2048; the insert then rides AbuseReportService's abuseReport
/// system-webhook notify.
async fn report_abuse(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let gated = pool_gate(state)?;
    let ctx = gate(state, token, false).await?;
    let Some(target) = body.get("userId").and_then(Value::as_str) else {
        return Err(invalid_param("userId", "required"));
    };
    let Some(comment) = body.get("comment").and_then(Value::as_str) else {
        return Err(invalid_param("comment", "required"));
    };
    if comment.is_empty() || comment.chars().count() > 2048 {
        return Err(invalid_param(
            "comment",
            "must be 1 to 2048 characters long",
        ));
    }
    let id = mk_types::id::gen_aid_now();
    mk_db::webhook::insert_abuse_report(gated.pool, &id, target, &ctx.me.id, comment)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_synalio", error = %e, "insert_abuse_report");
            api_err(ApiError::internal_error())
        })?;
    let Some(row) = mk_db::webhook::find_abuse_report(gated.pool, &id)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_synalio", error = %e, "find_abuse_report");
            api_err(ApiError::internal_error())
        })?
    else {
        return Err(api_err(ApiError::internal_error()));
    };
    let content = pack_report_row(state, &row).await;
    let pool = gated.pool.clone();
    let server_url = state.config.url.clone();
    let hook_host = state.config.host();
    tokio::spawn(fire(pool, server_url, hook_host, "abuseReport", content));
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// admin/abuse-user-reports: newest-first list (the synalio timeout legs read
/// `[0]` after a fresh report).
async fn abuse_reports_list(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let gated = pool_gate(state)?;
    gate(state, token, true).await?;
    let limit = body
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(10)
        .clamp(1, 100) as i32;
    let rows = mk_db::webhook::list_abuse_reports(gated.pool, limit)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_synalio", error = %e, "list_abuse_reports");
            api_err(ApiError::internal_error())
        })?;
    let mut out: Vec<Value> = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(pack_report_row(state, row).await);
    }
    Ok(Json(json!(out)).into_response())
}

/// admin/resolve-abuse-user-report: assignee = me, resolved = true, then the
/// abuseReportResolved system-webhook notify.
async fn abuse_report_resolve(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let gated = pool_gate(state)?;
    let ctx = gate(state, token, true).await?;
    let Some(report_id) = body.get("reportId").and_then(Value::as_str) else {
        return Err(invalid_param("reportId", "required"));
    };
    mk_db::webhook::resolve_abuse_report(gated.pool, report_id, &ctx.me.id)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_synalio", error = %e, "resolve_abuse_report");
            api_err(ApiError::internal_error())
        })?;
    let Some(row) = mk_db::webhook::find_abuse_report(gated.pool, report_id)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_synalio", error = %e, "find_abuse_report");
            api_err(ApiError::internal_error())
        })?
    else {
        return Err(api_err(ApiError::internal_error()));
    };
    let content = pack_report_row(state, &row).await;
    let pool = gated.pool.clone();
    let server_url = state.config.url.clone();
    let hook_host = state.config.host();
    tokio::spawn(fire(
        pool,
        server_url,
        hook_host,
        "abuseReportResolved",
        content,
    ));
    Ok(StatusCode::NO_CONTENT.into_response())
}
