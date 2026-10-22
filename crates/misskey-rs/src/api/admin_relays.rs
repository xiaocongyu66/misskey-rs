//! `admin/relays/*` — ports of upstream
//! `endpoints/admin/relays/{list,add,remove}.ts` through
//! `core/RelayService.ts`. Db paths live in `mk_db::relay` (the `relay`
//! table ships in the 0001 baseline: `id`, `inbox`, `status`
//! enum(requesting|accepted|rejected), UNIQUE btree on `inbox`; no
//! migration).
//!
//! Gate: upstream `requireCredential` + `requireModerator` (all three).
//! The shared `admin::gate` runs the credential → suspended → moderator-role
//! chain, so a plain user gets the same ROLE_PERMISSION_DENIED (moderator id
//! d33d5333-…) shape as the other admin arms.
//!
//! DOCUMENTED SKIP — ActivityPub handshake: upstream `addRelay` also renders
//! a Follow activity from the `relay` system account and queues delivery to
//! `relay.inbox`; `removeRelay` sends the matching Undo(Follow). This tree
//! has no relay system-account / outbound AP delivery lane, so these
//! endpoints are DB-row-only, best-effort: add inserts the `requesting` row,
//! remove deletes it. No activity is sent (and no `accepted`/`rejected`
//! transition can be observed without the inbound Accept handler).
//!
//! Status mapping: `add` returns the inserted relay entity
//! (`RelayEntityService.pack` = `{ id, inbox, status }`); a non-https inbox
//! is the declared INVALID_URL 400 (fb8c92d3); a duplicate inbox hits the
//! UNIQUE btree like upstream's uncaught insert error → 500
//! INTERNAL_ERROR; `remove` on an unknown inbox is upstream's plain
//! `throw new Error('relay not found')` → 500 INTERNAL_ERROR; `remove`
//! success and all void paths answer 204 NO_CONTENT; `list` returns the
//! entity array.

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
        "admin/relays/add" | "admin/relays/list" | "admin/relays/remove"
    )
}

type Check<T = Response> = Result<T, Response>;

/// Entry from the admin-relays dispatch arm in mod.rs (body pre-parsed,
/// token pre-extracted).
pub(super) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<&str>,
) -> Response {
    // Shared requireModerator gate (root bypass + role check) — upstream all
    // three endpoints declare requireModerator, not requireAdmin.
    if let Err(resp) = gate(state, token, Gate::Moderator).await {
        return resp;
    }
    let Some(pool) = &state.pool else {
        return internal();
    };
    let result: Check<Response> = match name {
        "admin/relays/add" => add(pool, body).await,
        "admin/relays/list" => list(pool).await,
        _ => remove(pool, body).await,
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_admin_relays", error = %e, "db error: {context}");
    internal()
}

/// add.ts meta.errors.invalidUrl (fb8c92d3-d4e5-44e7-b3d4-800d5cef8b2c).
fn invalid_url() -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "INVALID_URL",
        "fb8c92d3-d4e5-44e7-b3d4-800d5cef8b2c",
        "Invalid URL",
    ))
}

/// `inbox` param (`{ type: 'string' }`, required).
fn require_inbox(body: &Value) -> Check<String> {
    match body.get("inbox") {
        None | Some(Value::Null) => Err(super::invalid_param(
            "inbox",
            "must have required property 'inbox'",
        )),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(super::invalid_param("inbox", "must be string")),
    }
}

/// `RelayEntityService.pack` — exactly `{ id, inbox, status }`.
fn pack(row: &mk_db::relay::RelayRow) -> Value {
    json!({
        "id": row.id,
        "inbox": row.inbox,
        "status": row.status,
    })
}

/// `RelayService.listRelay` — the raw entity array (200).
async fn list(pool: &mk_db::sqlx::PgPool) -> Check<Response> {
    let rows = mk_db::relay::list(pool)
        .await
        .map_err(|e| db_err("relay list", e))?;
    Ok(Json(Value::Array(rows.iter().map(pack).collect())).into_response())
}

/// `RelayService.addRelay` local slice: validate https, insert the
/// `requesting` row, echo the entity. The Follow delivery is a documented
/// skip (no outbound AP lane).
async fn add(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let inbox = require_inbox(body)?;
    // upstream: `new URL(ps.inbox).protocol !== 'https:'` → invalidUrl.
    match url::Url::parse(&inbox) {
        Ok(u) if u.scheme() == "https" => {}
        _ => return Err(invalid_url()),
    }
    let id = mk_types::id::gen_aid_now();
    // Duplicate inbox → UNIQUE-btree violation, surfaced as 500 exactly like
    // upstream's uncaught insertOne error (no pre-check upstream either).
    mk_db::relay::insert(pool, &id, &inbox, "requesting")
        .await
        .map_err(|e| db_err("relay insert", e))?;
    let row = mk_db::relay::find_by_inbox(pool, &inbox)
        .await
        .map_err(|e| db_err("relay find after insert", e))?;
    match row {
        Some(row) => Ok(Json(pack(&row)).into_response()),
        None => Err(internal()),
    }
}

/// `RelayService.removeRelay` local slice: delete by inbox; unknown inbox is
/// upstream's plain thrown Error → 500. Undo(Follow) delivery is a documented
/// skip.
async fn remove(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let inbox = require_inbox(body)?;
    let row = mk_db::relay::find_by_inbox(pool, &inbox)
        .await
        .map_err(|e| db_err("relay find", e))?;
    let Some(row) = row else {
        // upstream: `if (relay == null) throw new Error('relay not found')`.
        return Err(internal());
    };
    mk_db::relay::delete_by_id(pool, &row.id)
        .await
        .map_err(|e| db_err("relay delete", e))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
