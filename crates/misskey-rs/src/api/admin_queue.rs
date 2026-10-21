//! `admin/queue/*` endpoint family — ports of
//! `endpoints/admin/queue/{queues,queue-stats,jobs,show-job,show-job-logs,
//! remove-job,retry-job,promote-jobs,clear,stats,inbox-delayed,
//! deliver-delayed}.ts` over the in-tree PG queue (`mk_queue_jobs`, owned by
//! the `mk-queue` crate).
//!
//! # Divergence from upstream (documented, intentional)
//!
//! Upstream brokers through Redis + BullMQ: one named queue per domain
//! (`QUEUE_TYPES` = system/deliver/inbox/db/...) with per-queue pause state,
//! completed/failed sets, metrics buckets, job logs and a Redis `INFO` block.
//! `mk-queue` is a **single** Postgres table keyed by `kind`; its public API
//! is enqueue/claim/complete/retry only. This module therefore reads/writes
//! that table directly (the same exception mk-queue itself makes for the
//! table it owns) and treats each distinct `kind` as a "queue" name — the
//! closest analog to BullMQ's per-domain queues.
//!
//! Consequences, all surfaced honestly in the response shapes:
//! - `counts.active` / `counts.completed` are always 0: a claimed job has no
//!   in-flight marker (claim is `FOR UPDATE SKIP LOCKED`, the row stays
//!   pending) and a completed job is deleted.
//! - `counts.failed` = rows with `attempts > 0` (the job failed at least
//!   once and is waiting out its backoff). `counts.delayed` = `run_after`
//!   in the future; `counts.waiting` = due now.
//! - `metrics` (BullMQ's one-week completed/failed buckets) have no store
//!   and are zeroed.
//! - `db` (Redis INFO) is a PG placeholder block: `version` "postgres",
//!   mode "standalone", zeroed counters.
//! - `show-job-logs`: `mk-queue` records no per-job logs, so the endpoint
//!   returns `[]` rather than inventing entries.
//! - `pause` / `resume`: `mk-queue` has no pause primitive (and the worker
//!   has no pause check), so these two names are deliberately **not**
//!   registered and fall through to the admin catch-all (404
//!   UNKNOWN_API_ENDPOINT). Implementing them as no-ops would lie to the
//!   operator.
//! - `jobs` gains optional `limit`/`offset` (the slice asks for a paged
//!   list); upstream returns at most 100 jobs with no paging params.
//! - `queue` is not validated against `QUEUE_TYPES`: our names are job
//!   kinds, so any non-empty kind is accepted (unknown kinds simply have
//!   zero counts / no jobs).
//!
//! `inbox-delayed` / `deliver-delayed` read delayed rows of the kinds
//! `inbox` / `deliver` and group them by the payload host (`signature.keyId`
//! for inbox, `to` for deliver), matching upstream. Nothing in-tree enqueues
//! those kinds today, so both answer `[]` until an inbox/deliver worker
//! lands.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::Request;
use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

use super::admin::{self, Gate};
use super::invalid_param;

/// Endpoint names owned by this module (all POST-only, requireModerator,
/// kind `read:admin:queue` / `write:admin:queue`).
const ENDPOINTS: &[&str] = &[
    "admin/queue/queues",
    "admin/queue/queue-stats",
    "admin/queue/jobs",
    "admin/queue/show-job",
    "admin/queue/show-job-logs",
    "admin/queue/remove-job",
    "admin/queue/retry-job",
    "admin/queue/promote-jobs",
    "admin/queue/clear",
    "admin/queue/stats",
    "admin/queue/inbox-delayed",
    "admin/queue/deliver-delayed",
];

/// In-tree job kinds (the enqueue sites: delete_account.rs, exports.rs,
/// move_account.rs, queue_processors.rs and the notes/create poll branch).
/// `queues` returns these even before the first job lands so the admin UI
/// sees the roster; any other kind present in the table is appended too.
const KNOWN_KINDS: &[&str] = &[
    "deleteAccount",
    "endedPollNotification",
    "exportBlocking",
    "exportClips",
    "exportFavorites",
    "exportFollowing",
    "exportMuting",
    "exportUserLists",
    "relationship:block",
    "relationship:follow",
    "relationship:unfollow",
];

/// Upstream `stats.db` queue domain: the db-queue processors we ported
/// (DeleteAccount + the export family).
const DB_KINDS: &[&str] = &[
    "deleteAccount",
    "exportBlocking",
    "exportClips",
    "exportFavorites",
    "exportFollowing",
    "exportMuting",
    "exportUserLists",
];

type ApiResult = Result<Response, Response>;

/// True for the names this module owns (the mod.rs arm predicate).
pub(super) fn handles(name: &str) -> bool {
    ENDPOINTS.contains(&name)
}

/// Single entry point from the mod.rs dispatch arm (nosed in ahead of the
/// admin/* catch-all).
pub(super) async fn handle(state: &AppState, name: &str, req: Request) -> Response {
    if !ENDPOINTS.contains(&name) {
        return api_err(ApiError::unknown_endpoint());
    }
    if req.method() != Method::POST {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }

    let headers = req.headers().clone();
    let body = match super::parse_json_body(req).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let token = match headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
    {
        Some(t) => Some(t.to_string()),
        None => match body.get("i") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => return StatusCode::BAD_REQUEST.into_response(),
        },
    };

    let result = match name {
        "admin/queue/queues" => queues(state, token.as_deref()).await,
        "admin/queue/queue-stats" => queue_stats(state, &body, token.as_deref()).await,
        "admin/queue/jobs" => jobs(state, &body, token.as_deref()).await,
        "admin/queue/show-job" => show_job(state, &body, token.as_deref()).await,
        "admin/queue/show-job-logs" => show_job_logs(state, &body, token.as_deref()).await,
        "admin/queue/remove-job" => remove_job(state, &body, token.as_deref()).await,
        "admin/queue/retry-job" => retry_job(state, &body, token.as_deref()).await,
        "admin/queue/promote-jobs" => promote_jobs(state, &body, token.as_deref()).await,
        "admin/queue/clear" => clear(state, &body, token.as_deref()).await,
        "admin/queue/stats" => stats(state, token.as_deref()).await,
        "admin/queue/inbox-delayed" => delayed_hosts(state, "inbox", token.as_deref()).await,
        "admin/queue/deliver-delayed" => delayed_hosts(state, "deliver", token.as_deref()).await,
        _ => unreachable!("checked against ENDPOINTS above"),
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

// --- errors / gates ---

fn handler_internal_error() -> Response {
    api_err(ApiError::internal_error())
}

fn db_err(e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_admin", error = %e, "admin/queue db");
    api_err(ApiError::internal_error())
}

/// ApiCallService.call ep.meta.kind gate (same shape as
/// admin/get-index-stats): app/miauth tokens must carry the endpoint's kind;
/// native user tokens ride the `token == null` path and skip it.
fn require_scope(ctx: &admin::AdminCtx, kind: &str) -> Result<(), Response> {
    if let Some(perms) = &ctx.token_permission {
        if !perms.iter().any(|p| p == kind) {
            return Err(api_err(ApiError::new(
                403,
                mk_types::api_error::ApiErrorKind::Permission,
                "PERMISSION_DENIED",
                "1370e5b7-d4eb-4566-bb1d-7748ee6a1838",
                "Your app does not have the necessary permissions to use this endpoint.",
            )));
        }
    }
    Ok(())
}

/// requireModerator + the read/write scope for the endpoint name.
async fn gate_read(
    state: &AppState,
    token: Option<&str>,
) -> Result<admin::AdminCtx, Response> {
    let ctx = admin::gate(state, token, Gate::Moderator).await?;
    require_scope(&ctx, "read:admin:queue")?;
    Ok(ctx)
}

async fn gate_write(
    state: &AppState,
    token: Option<&str>,
) -> Result<admin::AdminCtx, Response> {
    let ctx = admin::gate(state, token, Gate::Moderator).await?;
    require_scope(&ctx, "write:admin:queue")?;
    Ok(ctx)
}

// --- params ---

fn require_queue(body: &Value) -> Result<String, Response> {
    match body.get("queue") {
        None | Some(Value::Null) => Err(invalid_param(
            "queue",
            "must have required property 'queue'",
        )),
        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        Some(_) => Err(invalid_param("queue", "must be string")),
    }
}

fn require_job_id(body: &Value) -> Result<String, Response> {
    match body.get("jobId") {
        None | Some(Value::Null) => Err(invalid_param(
            "jobId",
            "must have required property 'jobId'",
        )),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(invalid_param("jobId", "must be string")),
    }
}

fn parse_states(body: &Value) -> Result<Vec<String>, Response> {
    const ALLOWED: &[&str] = &["active", "wait", "delayed", "completed", "failed"];
    match body.get("state") {
        None | Some(Value::Null) => Err(invalid_param(
            "state",
            "must have required property 'state'",
        )),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(s) if ALLOWED.contains(&s) => out.push(s.to_string()),
                    _ => {
                        return Err(invalid_param(
                            "state",
                            "must be an array of active|wait|delayed|completed|failed",
                        ));
                    }
                }
            }
            Ok(out)
        }
        Some(_) => Err(invalid_param("state", "must be array")),
    }
}

fn parse_limit(body: &Value) -> Result<i64, Response> {
    match body.get("limit") {
        None | Some(Value::Null) => Ok(100),
        Some(v) => match v.as_i64() {
            Some(n) if (1..=100).contains(&n) => Ok(n),
            _ => Err(invalid_param(
                "limit",
                "must be an integer between 1 and 100",
            )),
        },
    }
}

fn parse_offset(body: &Value) -> Result<i64, Response> {
    match body.get("offset") {
        None | Some(Value::Null) => Ok(0),
        Some(v) => match v.as_i64() {
            Some(n) if n >= 0 => Ok(n),
            _ => Err(invalid_param("offset", "must be a non-negative integer")),
        },
    }
}

// --- row / packing helpers ---

struct JobRow {
    id: i64,
    kind: String,
    payload: Value,
    attempts: i32,
    created_ms: i64,
    run_after_ms: i64,
}

impl JobRow {
    /// `QueueService.packJobData` over the columns mk-queue stores. Optional
    /// BullMQ fields that are always null here (processedOn/processedBy/
    /// finishedOn) are omitted, matching the upstream JSON serialization of
    /// `undefined`.
    fn pack(&self) -> Value {
        json!({
            "id": self.id.to_string(),
            "name": self.kind,
            "data": self.payload,
            "opts": {},
            "timestamp": self.created_ms,
            "progress": {},
            // BullMQ attemptsMade counts finished attempts; our `attempts`
            // counts claims, which is the same number after a failure.
            "attempts": self.attempts,
            // run_after - created_at is the enqueue delay (constant for a
            // delayed job; grows on retries).
            "delay": (self.run_after_ms - self.created_ms).max(0),
            "failedReason": "",
            "stacktrace": [],
            "returnValue": {},
            "isFailed": self.attempts > 0,
        })
    }
}

fn zero_counts() -> Value {
    json!({
        "waiting": 0,
        "active": 0,
        "completed": 0,
        "failed": 0,
        "delayed": 0,
    })
}

fn counts_value(waiting: i64, delayed: i64, failed: i64) -> Value {
    json!({
        "waiting": waiting,
        "active": 0,
        "completed": 0,
        "failed": failed,
        "delayed": delayed,
    })
}

fn zero_metrics() -> Value {
    json!({
        "meta": { "count": 0, "prevTS": 0, "prevCount": 0 },
        "data": [],
        "count": 0,
    })
}

/// PG placeholder for BullMQ's Redis INFO block (no Redis in this rewrite).
fn db_block() -> Value {
    json!({
        "version": "postgres",
        "mode": "standalone",
        "runId": "",
        "processId": "",
        "port": 0,
        "os": std::env::consts::OS,
        "uptime": 0,
        "memory": {
            "total": 0,
            "used": 0,
            "fragmentationRatio": 0,
            "peak": 0,
        },
        "clients": { "blocked": 0, "connected": 0 },
    })
}

fn queue_summary(name: &str, waiting: i64, delayed: i64, failed: i64) -> Value {
    json!({
        "name": name,
        "counts": counts_value(waiting, delayed, failed),
        "isPaused": false,
        "metrics": {
            "completed": zero_metrics(),
            "failed": zero_metrics(),
        },
    })
}

async fn kind_counts(
    pool: &mk_db::sqlx::PgPool,
    kind: &str,
) -> Result<(i64, i64, i64), mk_db::sqlx::Error> {
    mk_db::sqlx::query_as::<_, (i64, i64, i64)>(
        r#"SELECT
               count(*) FILTER (WHERE "run_after" <= now())::bigint,
               count(*) FILTER (WHERE "run_after" > now())::bigint,
               count(*) FILTER (WHERE "attempts" > 0)::bigint
           FROM "mk_queue_jobs" WHERE "kind" = $1"#,
    )
    .bind(kind)
    .fetch_one(pool)
    .await
}

async fn kinds_counts(
    pool: &mk_db::sqlx::PgPool,
    kinds: &[&str],
) -> Result<(i64, i64, i64), mk_db::sqlx::Error> {
    let kinds: Vec<String> = kinds.iter().map(|k| k.to_string()).collect();
    mk_db::sqlx::query_as::<_, (i64, i64, i64)>(
        r#"SELECT
               count(*) FILTER (WHERE "run_after" <= now())::bigint,
               count(*) FILTER (WHERE "run_after" > now())::bigint,
               count(*) FILTER (WHERE "attempts" > 0)::bigint
           FROM "mk_queue_jobs" WHERE "kind" = ANY($1)"#,
    )
    .bind(kinds)
    .fetch_one(pool)
    .await
}

/// SQL predicate for the upstream `state` array over our single-table model.
fn state_predicate(states: &[String]) -> String {
    if states.is_empty() {
        return "FALSE".to_string();
    }
    let parts: Vec<&str> = states
        .iter()
        .map(|s| match s.as_str() {
            "wait" => r#""run_after" <= now()"#,
            "delayed" => r#""run_after" > now()"#,
            "failed" => r#""attempts" > 0"#,
            // active: no in-flight marker; completed: rows are deleted.
            _ => "FALSE",
        })
        .collect();
    format!("({})", parts.join(" OR "))
}

async fn fetch_jobs(
    pool: &mk_db::sqlx::PgPool,
    kind: &str,
    states: &[String],
    limit: i64,
    offset: i64,
) -> Result<Vec<JobRow>, mk_db::sqlx::Error> {
    let sql = format!(
        r#"SELECT "id", "kind", "payload", "attempts",
                  (extract(epoch FROM "created_at") * 1000)::bigint,
                  (extract(epoch FROM "run_after") * 1000)::bigint
           FROM "mk_queue_jobs"
           WHERE "kind" = $1 AND {}
           ORDER BY "id" LIMIT $2 OFFSET $3"#,
        state_predicate(states)
    );
    let rows = mk_db::sqlx::query_as::<_, (i64, String, Value, i32, i64, i64)>(&sql)
        .bind(kind)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(
            |(id, kind, payload, attempts, created_ms, run_after_ms)| JobRow {
                id,
                kind,
                payload,
                attempts,
                created_ms,
                run_after_ms,
            },
        )
        .collect())
}

// --- endpoints ---

/// `endpoints/admin/queue/queues.ts` → `QueueService.queueGetQueues`:
/// the roster with counts, pause flag and metrics.
async fn queues(state: &AppState, token: Option<&str>) -> ApiResult {
    let _ctx = gate_read(state, token).await?;
    let pool = state.pool.as_ref().expect("gated");

    let rows = mk_db::sqlx::query_as::<_, (String, i64, i64, i64)>(
        r#"SELECT "kind",
                  count(*) FILTER (WHERE "run_after" <= now())::bigint,
                  count(*) FILTER (WHERE "run_after" > now())::bigint,
                  count(*) FILTER (WHERE "attempts" > 0)::bigint
           FROM "mk_queue_jobs" GROUP BY "kind""#,
    )
    .fetch_all(pool)
    .await
    .map_err(db_err)?;

    let mut by_kind: BTreeMap<String, (i64, i64, i64)> = BTreeMap::new();
    for k in KNOWN_KINDS {
        by_kind.insert((*k).to_string(), (0, 0, 0));
    }
    for (kind, waiting, delayed, failed) in rows {
        by_kind.insert(kind, (waiting, delayed, failed));
    }
    let out: Vec<Value> = by_kind
        .into_iter()
        .map(|(name, (waiting, delayed, failed))| queue_summary(&name, waiting, delayed, failed))
        .collect();
    Ok((StatusCode::OK, Json(json!(out))).into_response())
}

/// `endpoints/admin/queue/queue-stats.ts` → `QueueService.queueGetQueue`.
async fn queue_stats(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let _ctx = gate_read(state, token).await?;
    let pool = state.pool.as_ref().expect("gated");
    let kind = require_queue(body)?;

    let (waiting, delayed, failed) = kind_counts(pool, &kind).await.map_err(db_err)?;
    let mut item = queue_summary(&kind, waiting, delayed, failed);
    if let Some(obj) = item.as_object_mut() {
        obj.insert("qualifiedName".to_string(), json!(kind));
        obj.insert("db".to_string(), db_block());
    }
    Ok((StatusCode::OK, Json(item)).into_response())
}

/// `endpoints/admin/queue/jobs.ts` → `QueueService.queueGetJobs`
/// (+ optional limit/offset paging and the upstream search semantics:
/// space-separated terms, all must occur in the packed job JSON).
async fn jobs(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let _ctx = gate_read(state, token).await?;
    let pool = state.pool.as_ref().expect("gated");
    let kind = require_queue(body)?;
    let states = parse_states(body)?;
    let limit = parse_limit(body)?;
    let offset = parse_offset(body)?;
    let search = match body.get("search") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return Err(invalid_param("search", "must be string")),
    };

    let rows = fetch_jobs(pool, &kind, &states, limit, offset)
        .await
        .map_err(db_err)?;
    let mut packed: Vec<Value> = rows.iter().map(JobRow::pack).collect();
    if let Some(search) = search {
        let terms: Vec<String> = search
            .to_lowercase()
            .split(' ')
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect();
        packed.retain(|job| {
            let haystack = serde_json::to_string(job)
                .unwrap_or_default()
                .to_lowercase();
            terms.iter().all(|term| haystack.contains(term))
        });
        packed.truncate(100);
    }
    Ok((StatusCode::OK, Json(json!(packed))).into_response())
}

/// `endpoints/admin/queue/show-job.ts` → `QueueService.queueGetJob` (a
/// missing job is an upstream `throw new Error` → 500 INTERNAL_ERROR).
async fn show_job(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let _ctx = gate_read(state, token).await?;
    let pool = state.pool.as_ref().expect("gated");
    let kind = require_queue(body)?;
    let job_id = require_job_id(body)?;
    let Some(id) = job_id.parse::<i64>().ok() else {
        return Err(handler_internal_error());
    };

    let row = mk_db::sqlx::query_as::<_, (i64, String, Value, i32, i64, i64)>(
        r#"SELECT "id", "kind", "payload", "attempts",
                  (extract(epoch FROM "created_at") * 1000)::bigint,
                  (extract(epoch FROM "run_after") * 1000)::bigint
           FROM "mk_queue_jobs" WHERE "kind" = $1 AND "id" = $2"#,
    )
    .bind(&kind)
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(db_err)?;
    let Some((id, kind, payload, attempts, created_ms, run_after_ms)) = row else {
        return Err(handler_internal_error());
    };
    let job = JobRow {
        id,
        kind,
        payload,
        attempts,
        created_ms,
        run_after_ms,
    };
    Ok((StatusCode::OK, Json(job.pack())).into_response())
}

/// `endpoints/admin/queue/show-job-logs.ts` — `mk-queue` keeps no per-job
/// log store (BullMQ's `getJobLogs`), so this is an honest empty list.
async fn show_job_logs(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let _ctx = gate_read(state, token).await?;
    let _kind = require_queue(body)?;
    let _job_id = require_job_id(body)?;
    Ok((StatusCode::OK, Json(json!([]))).into_response())
}

/// `endpoints/admin/queue/remove-job.ts` → `queueRemoveJob` (missing job is
/// a silent no-op; void handler → 204).
async fn remove_job(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let _ctx = gate_write(state, token).await?;
    let pool = state.pool.as_ref().expect("gated");
    let kind = require_queue(body)?;
    let job_id = require_job_id(body)?;
    if let Some(id) = job_id.parse::<i64>().ok() {
        mk_db::sqlx::query(r#"DELETE FROM "mk_queue_jobs" WHERE "kind" = $1 AND "id" = $2"#)
            .bind(&kind)
            .bind(id)
            .execute(pool)
            .await
            .map_err(db_err)?;
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `endpoints/admin/queue/retry-job.ts` → `queueRetryJob`: our retry
/// primitive is "make it due now" (a pending job is promoted; a failed one
/// skips the rest of its backoff). Missing job is a silent no-op.
async fn retry_job(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let _ctx = gate_write(state, token).await?;
    let pool = state.pool.as_ref().expect("gated");
    let kind = require_queue(body)?;
    let job_id = require_job_id(body)?;
    if let Some(id) = job_id.parse::<i64>().ok() {
        mk_db::sqlx::query(
            r#"UPDATE "mk_queue_jobs" SET "run_after" = now() WHERE "kind" = $1 AND "id" = $2"#,
        )
        .bind(&kind)
        .bind(id)
        .execute(pool)
        .await
        .map_err(db_err)?;
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `endpoints/admin/queue/promote-jobs.ts` → `queuePromoteJobs`: make every
/// delayed job of the queue due now.
async fn promote_jobs(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let _ctx = gate_write(state, token).await?;
    let pool = state.pool.as_ref().expect("gated");
    let kind = require_queue(body)?;

    let res = mk_db::sqlx::query(
        r#"UPDATE "mk_queue_jobs" SET "run_after" = now()
           WHERE "kind" = $1 AND "run_after" > now()"#,
    )
    .bind(&kind)
    .execute(pool)
    .await
    .map_err(db_err)?;
    tracing::info!(
        target: "dbg_admin",
        queue = %kind,
        promoted = res.rows_affected(),
        "admin/queue/promote-jobs"
    );
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `endpoints/admin/queue/clear.ts` → `queueClear`. `*` wipes the whole
/// kind; otherwise the upstream state maps onto our pending-row model
/// (completed/active/paused/prioritized have no rows to clean).
async fn clear(state: &AppState, body: &Value, token: Option<&str>) -> ApiResult {
    let _ctx = gate_write(state, token).await?;
    let pool = state.pool.as_ref().expect("gated");
    let kind = require_queue(body)?;
    let state_name = match body.get("state") {
        None | Some(Value::Null) => {
            return Err(invalid_param(
                "state",
                "must have required property 'state'",
            ));
        }
        Some(Value::String(s))
            if matches!(
                s.as_str(),
                "*" | "completed" | "wait" | "active" | "paused" | "prioritized" | "delayed"
                    | "failed"
            ) =>
        {
            s.clone()
        }
        Some(_) => {
            return Err(invalid_param(
                "state",
                "must be one of *, completed, wait, active, paused, prioritized, delayed, failed",
            ));
        }
    };

    let predicate = match state_name.as_str() {
        "*" => "TRUE".to_string(),
        "wait" => r#""run_after" <= now()"#.to_string(),
        "delayed" => r#""run_after" > now()"#.to_string(),
        "failed" => r#""attempts" > 0"#.to_string(),
        _ => "FALSE".to_string(),
    };
    let sql = format!(r#"DELETE FROM "mk_queue_jobs" WHERE "kind" = $1 AND {predicate}"#);
    let res = mk_db::sqlx::query(&sql)
        .bind(&kind)
        .execute(pool)
        .await
        .map_err(db_err)?;
    tracing::info!(
        target: "dbg_admin",
        queue = %kind,
        state = %state_name,
        removed = res.rows_affected(),
        "admin/queue/clear"
    );
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `endpoints/admin/queue/stats.ts` → the four BullMQ domain counters.
/// `db` aggregates the db-queue kinds we ported; the other three read the
/// kind of the same name (nothing enqueues them today → zero counts).
async fn stats(state: &AppState, token: Option<&str>) -> ApiResult {
    let _ctx = gate_read(state, token).await?;
    let pool = state.pool.as_ref().expect("gated");

    let (dw, dd, df) = kind_counts(pool, "deliver").await.map_err(db_err)?;
    let (iw, id, if_) = kind_counts(pool, "inbox").await.map_err(db_err)?;
    let (bw, bd, bf) = kinds_counts(pool, DB_KINDS).await.map_err(db_err)?;
    let (ow, od, of) = kind_counts(pool, "objectStorage").await.map_err(db_err)?;

    let resp = json!({
        "deliver": counts_value(dw, dd, df),
        "inbox": counts_value(iw, id, if_),
        "db": counts_value(bw, bd, bf),
        "objectStorage": counts_value(ow, od, of),
    });
    Ok((StatusCode::OK, Json(resp)).into_response())
}

/// `endpoints/admin/queue/inbox-delayed.ts` / `deliver-delayed.ts`: delayed
/// jobs of the kind grouped by payload host, sorted by count desc.
async fn delayed_hosts(
    state: &AppState,
    kind: &str,
    token: Option<&str>,
) -> ApiResult {
    let _ctx = gate_read(state, token).await?;
    let pool = state.pool.as_ref().expect("gated");

    let payloads: Vec<Value> = mk_db::sqlx::query_scalar(
        r#"SELECT "payload" FROM "mk_queue_jobs"
           WHERE "kind" = $1 AND "run_after" > now()"#,
    )
    .bind(kind)
    .fetch_all(pool)
    .await
    .map_err(db_err)?;

    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    for payload in payloads {
        let host = if kind == "inbox" {
            payload
                .get("signature")
                .and_then(|s| s.get("keyId"))
                .and_then(Value::as_str)
                .and_then(host_of)
        } else {
            payload.get("to").and_then(Value::as_str).and_then(host_of)
        };
        if let Some(host) = host {
            *counts.entry(host).or_insert(0) += 1;
        }
    }
    let mut out: Vec<(String, i64)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let res: Vec<Value> = out
        .into_iter()
        .map(|(host, n)| json!([host, n]))
        .collect();
    Ok((StatusCode::OK, Json(json!(res))).into_response())
}

/// `new URL(value).host` for the delayed-job grouping (invalid → skip, like
/// a payload the worker could not have produced).
fn host_of(raw: &str) -> Option<String> {
    url::Url::parse(raw)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
}
