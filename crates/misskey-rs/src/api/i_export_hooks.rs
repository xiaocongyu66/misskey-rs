//! `i/export-{antennas,notes}` + the `i/webhooks/*` family — ports of
//! `endpoints/i/export-{antennas,notes}.ts` and
//! `endpoints/i/webhooks/{create,delete,list,show,update,test}.ts`.
//!
//! UPSTREAM DIVERGENCE (exports): both export endpoints only enqueue a
//! `dbQueue` job (`createExportAntennasJob` / `createExportNotesJob`) whose
//! processor streams a ZIP (`antennas-*.zip` / `notes-*.zip`) to a drive
//! file; the endpoint itself returns 204 and there is no synchronous body.
//! This rewrite has no export queue/zip processor for these two families, so
//! the handlers return the caller's rows as a JSON array instead (antennas
//! packed like `antennas/list`; notes as the raw `to_jsonb(note)` rows).
//! Everything else (auth, ownership, response field names) matches upstream.
//!
//! WEBHOOKS (0001 `webhook`): every endpoint is `requireCredential` and
//! owner-scoped (`findOneBy({ id, userId })`), so another user's webhookId
//! resolves the endpoint's own NO_SUCH_WEBHOOK id (never 403). `create`
//! enforces the role-policy `webhookLimit` (RoleService default 3; the role
//! policy overlay is not ported, so the default is used). `update` treats an
//! absent field as "keep" and `secret: null` as `''` (TypeORM `update`
//! skips undefined). `test` performs the upstream dummy delivery: it merges
//! `override.{url,secret}`, ignores `active`/`on`, and fires the POST in the
//! background (upstream enqueues it with `attempts: 1`), answering 204
//! immediately. The `webhookEventTypes` enum is the upstream list.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};
// sqlx is a mk-db dependency, not a direct one here: import the re-export so
// the `sqlx::FromRow` derive and its `#[sqlx(...)]` helper attributes resolve.
use mk_db::sqlx;

pub(crate) const ENDPOINTS: &[&str] = &[
    "i/export-antennas",
    "i/export-notes",
    "i/webhooks/create",
    "i/webhooks/delete",
    "i/webhooks/list",
    "i/webhooks/show",
    "i/webhooks/update",
    "i/webhooks/test",
];

pub(crate) fn handles(name: &str) -> bool {
    ENDPOINTS.contains(&name)
}

/// upstream `models/Webhook.ts` `webhookEventTypes`.
const WEBHOOK_EVENT_TYPES: &[&str] = &[
    "mention", "unfollow", "follow", "followed", "note", "reply", "renote", "reaction",
];

/// RoleService `getDefaultPolicies().webhookLimit`.
const DEFAULT_WEBHOOK_LIMIT: i64 = 3;

/// Entry from the `// i-export-hooks` dispatch arm in mod.rs.
pub(crate) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
    credential_declared: bool,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = match super::social::require_checked_user(
        pool,
        token.as_deref(),
        credential_declared,
    )
    .await
    {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let result: Check<Response> = match name {
        "i/export-antennas" => export_antennas(pool, &me_id).await,
        "i/export-notes" => export_notes(pool, &me_id).await,
        "i/webhooks/create" => webhook_create(pool, &me_id, body).await,
        "i/webhooks/delete" => webhook_delete(pool, &me_id, body).await,
        "i/webhooks/list" => webhook_list(pool, &me_id).await,
        "i/webhooks/show" => webhook_show(pool, &me_id, body).await,
        "i/webhooks/update" => webhook_update(pool, &me_id, body).await,
        "i/webhooks/test" => webhook_test(pool, &me_id, body).await,
        _ => Ok(api_err(ApiError::unknown_endpoint())),
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

type Check<T = Response> = Result<T, Response>;

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_i_export_hooks", error = %e, "db error: {context}");
    internal()
}

fn invalid_param(param: &str, reason: &str) -> Response {
    api_err(ApiError::invalid_param(json!({
        "param": param,
        "reason": reason,
    })))
}

fn meta_err(code: &str, id: &str, message: &str) -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        code,
        id,
        message,
    ))
}

fn no_such_webhook(id: &str) -> Response {
    meta_err("NO_SUCH_WEBHOOK", id, "No such webhook.")
}

fn ok_empty() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

fn ok_json(v: Value) -> Response {
    (StatusCode::OK, Json(v)).into_response()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Exports (upstream queues a zip; this rewrite returns the rows as JSON)
// ---------------------------------------------------------------------------

async fn export_antennas(pool: &mk_db::sqlx::PgPool, me_id: &str) -> Check<Response> {
    let rows = mk_db::antennas::list_antennas(pool, me_id)
        .await
        .map_err(|e| db_err("i/export-antennas", e))?;
    let out: Vec<Value> = rows.iter().map(super::antennas::pack_antenna).collect();
    Ok(ok_json(json!(out)))
}

async fn export_notes(pool: &mk_db::sqlx::PgPool, me_id: &str) -> Check<Response> {
    let rows = mk_db::sqlx::query_scalar::<_, Value>(
        r#"SELECT to_jsonb(n) FROM "note" n WHERE n."userId" = $1 ORDER BY n."id" ASC"#,
    )
    .bind(me_id)
    .fetch_all(pool)
    .await
    .map_err(|e| db_err("i/export-notes", e))?;
    Ok(ok_json(json!(rows)))
}

// ---------------------------------------------------------------------------
// Webhooks
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
struct WebhookRow {
    id: String,
    #[sqlx(rename = "userId")]
    user_id: String,
    name: String,
    #[sqlx(rename = "on")]
    on: Vec<String>,
    url: String,
    secret: String,
    active: bool,
    latest_sent_at_ms: Option<i64>,
    #[sqlx(rename = "latestStatus")]
    latest_status: Option<i32>,
}

const WEBHOOK_COLS: &str = r#""id", "userId", "name", "on", "url", "secret", "active",
    (EXTRACT(EPOCH FROM "latestSentAt") * 1000)::bigint AS "latest_sent_at_ms",
    "latestStatus""#;

fn pack_webhook(row: &WebhookRow) -> Value {
    json!({
        "id": row.id,
        "userId": row.user_id,
        "name": row.name,
        "on": row.on,
        "url": row.url,
        "secret": row.secret,
        "active": row.active,
        "latestSentAt": row
            .latest_sent_at_ms
            .map(mk_core::services::signup::ms_to_iso),
        "latestStatus": row.latest_status,
    })
}

async fn find_webhook(
    pool: &mk_db::sqlx::PgPool,
    webhook_id: &str,
    user_id: &str,
) -> Result<Option<WebhookRow>, mk_db::sqlx::Error> {
    let sql = format!(r#"SELECT {WEBHOOK_COLS} FROM "webhook" WHERE "id" = $1 AND "userId" = $2"#);
    mk_db::sqlx::query_as::<_, WebhookRow>(&sql)
        .bind(webhook_id)
        .bind(user_id)
        .fetch_optional(pool)
        .await
}

fn required_webhook_id(body: &Value) -> Result<String, Response> {
    match body.get("webhookId").and_then(|v| v.as_str()) {
        Some(s) => Ok(s.to_string()),
        None => Err(invalid_param("webhookId", "required (misskey:id format)")),
    }
}

fn parse_name(body: &Value) -> Result<String, Response> {
    match body.get("name") {
        Some(Value::String(s)) if !s.is_empty() && s.chars().count() <= 100 => Ok(s.clone()),
        Some(Value::String(_)) => Err(invalid_param("name", "must be 1..100 characters")),
        _ => Err(invalid_param("name", "required (string 1..100 characters)")),
    }
}

fn parse_url(body: &Value) -> Result<String, Response> {
    match body.get("url") {
        Some(Value::String(s)) if !s.is_empty() && s.chars().count() <= 1024 => Ok(s.clone()),
        Some(Value::String(_)) => Err(invalid_param("url", "must be 1..1024 characters")),
        _ => Err(invalid_param("url", "required (string 1..1024 characters)")),
    }
}

fn parse_secret(body: &Value, required: bool) -> Result<Option<String>, Response> {
    match body.get("secret") {
        None => {
            if required {
                Ok(Some(String::new()))
            } else {
                Ok(None)
            }
        }
        Some(Value::Null) => Ok(Some(String::new())),
        Some(Value::String(s)) if s.chars().count() <= 1024 => Ok(Some(s.clone())),
        Some(Value::String(_)) => Err(invalid_param("secret", "must be at most 1024 characters")),
        Some(_) => Err(invalid_param("secret", "expected a string")),
    }
}

fn parse_on(body: &Value, required: bool) -> Result<Option<Vec<String>>, Response> {
    match body.get("on") {
        None => {
            if required {
                Err(invalid_param("on", "required"))
            } else {
                Ok(None)
            }
        }
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let Some(s) = item.as_str() else {
                    return Err(invalid_param("on", "expected a string array"));
                };
                if !WEBHOOK_EVENT_TYPES.contains(&s) {
                    return Err(invalid_param("on", "contains an unknown event type"));
                }
                out.push(s.to_string());
            }
            Ok(Some(out))
        }
        Some(_) => Err(invalid_param("on", "expected a string array")),
    }
}

async fn webhook_create(pool: &mk_db::sqlx::PgPool, me_id: &str, body: &Value) -> Check<Response> {
    let name = parse_name(body)?;
    let url = parse_url(body)?;
    let secret = parse_secret(body, true)?.unwrap_or_default();
    let on = parse_on(body, true)?.unwrap_or_default();

    let count = mk_db::sqlx::query_scalar::<_, i64>(
        r#"SELECT COUNT(*) FROM "webhook" WHERE "userId" = $1"#,
    )
    .bind(me_id)
    .fetch_one(pool)
    .await
    .map_err(|e| db_err("i/webhooks/create count", e))?;
    if count >= DEFAULT_WEBHOOK_LIMIT {
        return Err(meta_err(
            "TOO_MANY_WEBHOOKS",
            "87a9bb19-111e-4e37-81d3-a3e7426453b0",
            "You cannot create webhook any more.",
        ));
    }

    let id = mk_types::id::gen_aid_now();
    mk_db::sqlx::query(
        r#"INSERT INTO "webhook" ("id","userId","name","on","url","secret","active")
           VALUES ($1,$2,$3,$4,$5,$6,true)"#,
    )
    .bind(&id)
    .bind(me_id)
    .bind(&name)
    .bind(&on)
    .bind(&url)
    .bind(&secret)
    .execute(pool)
    .await
    .map_err(|e| db_err("i/webhooks/create insert", e))?;

    let row = find_webhook(pool, &id, me_id)
        .await
        .map_err(|e| db_err("i/webhooks/create refetch", e))?
        .ok_or_else(internal)?;
    Ok(ok_json(pack_webhook(&row)))
}

async fn webhook_delete(pool: &mk_db::sqlx::PgPool, me_id: &str, body: &Value) -> Check<Response> {
    let webhook_id = required_webhook_id(body)?;
    let Some(row) = find_webhook(pool, &webhook_id, me_id)
        .await
        .map_err(|e| db_err("i/webhooks/delete find", e))?
    else {
        return Err(no_such_webhook("bae73e5a-5522-4965-ae19-3a8688e71d82"));
    };
    mk_db::sqlx::query(r#"DELETE FROM "webhook" WHERE "id" = $1"#)
        .bind(&row.id)
        .execute(pool)
        .await
        .map_err(|e| db_err("i/webhooks/delete", e))?;
    Ok(ok_empty())
}

async fn webhook_list(pool: &mk_db::sqlx::PgPool, me_id: &str) -> Check<Response> {
    let sql =
        format!(r#"SELECT {WEBHOOK_COLS} FROM "webhook" WHERE "userId" = $1 ORDER BY "id" ASC"#);
    let rows = mk_db::sqlx::query_as::<_, WebhookRow>(&sql)
        .bind(me_id)
        .fetch_all(pool)
        .await
        .map_err(|e| db_err("i/webhooks/list", e))?;
    let out: Vec<Value> = rows.iter().map(pack_webhook).collect();
    Ok(ok_json(json!(out)))
}

async fn webhook_show(pool: &mk_db::sqlx::PgPool, me_id: &str, body: &Value) -> Check<Response> {
    let webhook_id = required_webhook_id(body)?;
    let Some(row) = find_webhook(pool, &webhook_id, me_id)
        .await
        .map_err(|e| db_err("i/webhooks/show", e))?
    else {
        return Err(no_such_webhook("50f614d9-3047-4f7e-90d8-ad6b2d5fb098"));
    };
    Ok(ok_json(pack_webhook(&row)))
}

async fn webhook_update(pool: &mk_db::sqlx::PgPool, me_id: &str, body: &Value) -> Check<Response> {
    let webhook_id = required_webhook_id(body)?;
    let Some(row) = find_webhook(pool, &webhook_id, me_id)
        .await
        .map_err(|e| db_err("i/webhooks/update find", e))?
    else {
        return Err(no_such_webhook("fb0fea69-da18-45b1-828d-bd4fd1612518"));
    };

    let name = match body.get("name") {
        None => row.name,
        Some(_) => parse_name(body)?,
    };
    let url = match body.get("url") {
        None => row.url,
        Some(_) => parse_url(body)?,
    };
    let secret = parse_secret(body, false)?.unwrap_or(row.secret);
    let on = parse_on(body, false)?.unwrap_or(row.on);
    let active = match body.get("active") {
        None | Some(Value::Null) => row.active,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(invalid_param("active", "expected a boolean")),
    };

    mk_db::sqlx::query(
        r#"UPDATE "webhook" SET "name" = $2, "url" = $3, "secret" = $4,
                  "on" = $5, "active" = $6 WHERE "id" = $1"#,
    )
    .bind(&row.id)
    .bind(&name)
    .bind(&url)
    .bind(&secret)
    .bind(&on)
    .bind(active)
    .execute(pool)
    .await
    .map_err(|e| db_err("i/webhooks/update", e))?;
    Ok(ok_empty())
}

async fn webhook_test(pool: &mk_db::sqlx::PgPool, me_id: &str, body: &Value) -> Check<Response> {
    let webhook_id = required_webhook_id(body)?;
    let event_type = match body.get("type").and_then(|v| v.as_str()) {
        Some(t) if WEBHOOK_EVENT_TYPES.contains(&t) => t.to_string(),
        _ => return Err(invalid_param("type", "required (a webhook event type)")),
    };
    let Some(row) = find_webhook(pool, &webhook_id, me_id)
        .await
        .map_err(|e| db_err("i/webhooks/test find", e))?
    else {
        return Err(no_such_webhook("0c52149c-e913-18f8-5dc7-74870bfe0cf9"));
    };

    // override.{url,secret} — unknown members are ignored (paramDef shape).
    let (to, secret) = match body.get("override") {
        Some(Value::Object(o)) => (
            o.get("url")
                .and_then(|v| v.as_str())
                .unwrap_or(&row.url)
                .to_string(),
            o.get("secret")
                .and_then(|v| v.as_str())
                .unwrap_or(&row.secret)
                .to_string(),
        ),
        _ => (row.url.clone(), row.secret.clone()),
    };

    let content = dummy_content(&event_type);
    let payload = json!({
        "type": event_type,
        "content": content,
        "webhookId": row.id,
        "userId": me_id,
        "to": to,
        "secret": secret,
        "createdAt": now_ms(),
        "eventId": mk_types::id::gen_aid_now(),
    });

    // Upstream queues `userWebhookDeliver(..., { attempts: 1 })`; fire and
    // forget so the endpoint answers 204 immediately even if the receiver is
    // unreachable.
    tokio::spawn(async move {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build();
        if let Ok(client) = client {
            if let Err(e) = client
                .post(&to)
                .header("content-type", "application/json")
                .body(payload.to_string())
                .send()
                .await
            {
                tracing::warn!(target: "dbg_i_export_hooks", error = %e, "webhook test delivery failed");
            }
        }
    });

    Ok(ok_empty())
}

/// `WebhookTestService` dummy payloads, trimmed to the identifying fields
/// (the real service ships full packed Note/User objects).
fn dummy_content(event_type: &str) -> Value {
    match event_type {
        "note" | "reply" | "renote" | "mention" => json!({
            "note": {
                "id": "dummy-note-1",
                "text": "This is a dummy note for testing purposes.",
                "userId": "dummy-user-1",
                "visibility": "public",
            }
        }),
        "follow" | "unfollow" | "followed" => json!({
            "user": {
                "id": "dummy-user-1",
                "username": "dummy1",
                "name": "DummyUser1",
            }
        }),
        _ => json!({}),
    }
}
