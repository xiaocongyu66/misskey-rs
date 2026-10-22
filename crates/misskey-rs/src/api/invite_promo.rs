//! invite + promo — ports of upstream `endpoints/invite/{create,list,limit}.ts`
//! + `endpoints/admin/invite/{create,list}.ts`, `endpoints/promo/read.ts` and
//! `endpoints/admin/promo/create.ts`, plus the repo `promo/show` picker.
//!
//! Tables ship in the 0001 baseline (`registration_ticket`, `promo_note`,
//! `promo_read`); db paths live in `mk_db::invite` / `mk_db::promo`. The
//! `// invite-promo` dispatch arm in mod.rs parses the body, extracts the
//! token and probes a declared credential; this module owns the gates and
//! the endpoint bodies.
//!
//! Gates: invite/* and promo/* are requireCredential (upstream also sets
//! requiredRolePolicy: 'canInvite' on the invite family; the repo has no
//! role-policy endpoint engine, so the default-role policy applies — see
//! INVITE_LIMIT below). admin/invite/* and admin/promo/* are
//! requireModerator and ride the shared admin::gate(Gate::Moderator) chain.
//!
//! Divergences (documented): upstream has no `promo/show`, no
//! `admin/promo/list` and no `admin/promo/delete` at the pinned ea2c3aed
//! (see /tmp/up.txt). `promo/show` picks a random unexpired promo note the
//! caller has not read and marks it read; `admin/promo/list` returns the
//! promo_note rows. `admin/promo/delete` is intentionally absent.
//!
//! invite/limit: upstream reads `policies.inviteLimit` (RoleService default
//! 0 = unlimited). The 0001 `meta` table has no inviteLimit column (upstream
//! keeps it on role policies, not meta), so the default-role constant is the
//! truthful instance value: remaining is null.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// Endpoint names routed into this module (single source of truth for the
/// `// invite-promo` dispatch arm in mod.rs).
pub(crate) const INVITE_PROMO_ENDPOINTS: &[&str] = &[
    "invite/create",
    "invite/list",
    "invite/limit",
    "admin/invite/create",
    "admin/invite/list",
    "promo/show",
    "promo/read",
    "admin/promo/create",
    "admin/promo/list",
];

pub(crate) fn handles(name: &str) -> bool {
    INVITE_PROMO_ENDPOINTS.contains(&name)
}

/// RoleService.ts L84-85 default-role policies. 0 = unlimited.
const INVITE_LIMIT: i64 = 0;
const INVITE_LIMIT_CYCLE_MIN: i64 = 60 * 24 * 7;
const INVITE_EXPIRATION_MIN: i64 = 0;

/// InviteCodeEntityService error ids (upstream meta).
const EXCEEDED_LIMIT_ID: &str = "8b165dd3-6f37-4557-8db1-73175d63c641";
const INVALID_DATETIME_ID: &str = "f1380b15-3760-4c6c-a1db-5c3aaf1cbd49";
const NO_SUCH_NOTE_READ_ID: &str = "d785b897-fcd3-4fe9-8fc3-b85c26e6c932";
const NO_SUCH_NOTE_PROMO_ID: &str = "ee449fbe-af2a-453b-9cae-cf2fe7c895fc";
const ALREADY_PROMOTED_ID: &str = "ae427aa2-7a41-484f-a18c-2c1104051604";

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_invite_promo", error = %e, "db error: {context}");
    internal()
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

fn exceeded_limit() -> Response {
    meta_err(
        "EXCEEDED_LIMIT_OF_CREATE_INVITE_CODE",
        EXCEEDED_LIMIT_ID,
        "You have exceeded the limit for creating an invitation code.",
    )
}

fn invalid_datetime() -> Response {
    meta_err(
        "INVALID_DATE_TIME",
        INVALID_DATETIME_ID,
        "Invalid date-time format",
    )
}

fn invalid_param(param: &str, reason: &str) -> Response {
    api_err(ApiError::invalid_param(json!({
        "param": param,
        "reason": reason,
    })))
}

fn ms_iso(ms: i64) -> String {
    mk_core::services::signup::ms_to_iso(ms)
}

// --- params ---

fn require_str(body: &Value, key: &str) -> Result<String, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Err(invalid_param(
            key,
            &format!("must have required property '{key}'"),
        )),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(invalid_param(key, "must be a string")),
    }
}

fn require_i64(body: &Value, key: &str) -> Result<i64, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Err(invalid_param(
            key,
            &format!("must have required property '{key}'"),
        )),
        Some(v) => v
            .as_i64()
            .ok_or_else(|| invalid_param(key, "must be an integer")),
    }
}

/// paramDef limit: integer 1..=100.
fn optional_limit(body: &Value, dflt: i64) -> Result<i64, Response> {
    match body.get("limit") {
        None | Some(Value::Null) => Ok(dflt),
        Some(v) => {
            let n = v
                .as_i64()
                .ok_or_else(|| invalid_param("limit", "must be an integer"))?;
            if !(1..=100).contains(&n) {
                return Err(invalid_param("limit", "must be >= 1 and <= 100"));
            }
            Ok(n)
        }
    }
}

/// paramDef offset: integer (no minimum upstream).
fn optional_offset(body: &Value) -> Result<i64, Response> {
    match body.get("offset") {
        None | Some(Value::Null) => Ok(0),
        Some(v) => v
            .as_i64()
            .ok_or_else(|| invalid_param("offset", "must be an integer")),
    }
}

fn optional_enum(
    body: &Value,
    key: &str,
    allowed: &[&str],
    dflt: &str,
) -> Result<String, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(dflt.to_string()),
        Some(Value::String(s)) if allowed.contains(&s.as_str()) => Ok(s.clone()),
        Some(_) => Err(invalid_param(
            key,
            &format!("must be one of: {}", allowed.join(", ")),
        )),
    }
}

/// makePaginationQuery id-or-date fallback (same rule as gallery_more).
fn pager_ids(body: &Value) -> Result<(Option<String>, Option<String>), Response> {
    let id_or_date = |key: &str, date_key: &str| -> Result<Option<String>, Response> {
        match body.get(key) {
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(invalid_param(key, "must be a misskey:id")),
            None => Ok(body
                .get(date_key)
                .and_then(|v| v.as_i64())
                .filter(|&ms| ms != 0)
                .map(mk_types::id::gen_aid)),
        }
    };
    Ok((
        id_or_date("sinceId", "sinceDate")?,
        id_or_date("untilId", "untilDate")?,
    ))
}

// --- invite code generation ---

/// Port of `misc/generate-invite-code.ts`: 8 random chars from the
/// [0-9A-Z] minus [01IO] alphabet + the base-32 minute suffix.
fn generate_invite_code() -> String {
    const CHARS: &[u8] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
    let mut code = String::with_capacity(13);
    for b in mk_core::services::signup::secure_rndstr(8).bytes() {
        code.push(CHARS[(b as usize) % CHARS.len()] as char);
    }
    let mut n = (super::now_ms() / 1000 / 60) as u64;
    let mut suffix = Vec::new();
    loop {
        suffix.push(CHARS[(n as usize) % CHARS.len()] as char);
        n /= CHARS.len() as u64;
        if n == 0 {
            break;
        }
    }
    suffix.reverse();
    code.extend(suffix);
    code
}

// --- packing ---

async fn pack_ticket(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    row: &mk_db::invite::TicketRow,
) -> Result<Value, Response> {
    let created_by = match &row.created_by_id {
        Some(id) => Some(super::note_user_lite(state, pool, id).await?),
        None => None,
    };
    let used_by = match &row.used_by_id {
        Some(id) => Some(super::note_user_lite(state, pool, id).await?),
        None => None,
    };
    Ok(json!({
        "id": row.id,
        "code": row.code,
        "expiresAt": row.expires_at_ms.map(ms_iso),
        "createdAt": mk_types::id::parse_aid_date(&row.id).map(ms_iso).unwrap_or_default(),
        "createdBy": created_by,
        "usedBy": used_by,
        "usedAt": row.used_at_ms.map(ms_iso),
        "used": row.used_at_ms.is_some(),
    }))
}

async fn pack_tickets(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    rows: &[mk_db::invite::TicketRow],
) -> Result<Value, Response> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(pack_ticket(state, pool, row).await?);
    }
    Ok(Value::Array(out))
}

fn promo_note_json(row: &mk_db::promo::PromoNoteRow) -> Value {
    json!({
        "noteId": row.note_id,
        "expiresAt": ms_iso(row.expires_at_ms),
        "userId": row.user_id,
    })
}

// --- dispatch ---

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

    let me_id = if name.starts_with("admin/") {
        match super::admin::gate(state, token.as_deref(), super::admin::Gate::Moderator).await {
            Ok(ctx) => ctx.me.id,
            Err(resp) => return resp,
        }
    } else {
        match super::social::require_checked_user(pool, token.as_deref(), credential_declared).await
        {
            Ok(id) => id,
            Err(resp) => return resp,
        }
    };

    let result: Result<Response, Response> = match name {
        "invite/create" => invite_create(state, pool, &me_id).await,
        "invite/list" => invite_list(state, pool, &me_id, body).await,
        "invite/limit" => invite_limit(pool, &me_id).await,
        "admin/invite/create" => admin_invite_create(state, pool, &me_id, body).await,
        "admin/invite/list" => admin_invite_list(state, pool, body).await,
        "promo/show" => promo_show(pool, &me_id).await,
        "promo/read" => promo_read(pool, &me_id, body).await,
        "admin/promo/create" => admin_promo_create(pool, body).await,
        "admin/promo/list" => admin_promo_list(pool).await,
        _ => Ok(api_err(ApiError::unknown_endpoint())),
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

// --- invite/* ---

async fn invite_create(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    me_id: &str,
) -> Result<Response, Response> {
    if INVITE_LIMIT > 0 {
        let since =
            mk_types::id::gen_aid(super::now_ms() as i64 - INVITE_LIMIT_CYCLE_MIN * 60 * 1000);
        let count = mk_db::invite::count_created_since(pool, me_id, &since)
            .await
            .map_err(|e| db_err("count_created_since[invite/create]", e))?;
        if count >= INVITE_LIMIT {
            return Err(exceeded_limit());
        }
    }

    let expires_at = if INVITE_EXPIRATION_MIN > 0 {
        Some(super::now_ms() as i64 + INVITE_EXPIRATION_MIN * 60 * 1000)
    } else {
        None
    };
    let id = mk_types::id::gen_aid_now();
    let code = generate_invite_code();
    mk_db::invite::insert(pool, &id, &code, expires_at, me_id)
        .await
        .map_err(|e| db_err("insert[invite/create]", e))?;

    let row = mk_db::invite::TicketRow {
        id,
        code,
        expires_at_ms: expires_at,
        used_at_ms: None,
        pending_user_id: None,
        created_by_id: Some(me_id.to_string()),
        used_by_id: None,
    };
    Ok(Json(pack_ticket(state, pool, &row).await?).into_response())
}

async fn invite_list(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    me_id: &str,
    body: &Value,
) -> Result<Response, Response> {
    let limit = optional_limit(body, 30)?;
    let (since_id, until_id) = pager_ids(body)?;
    let rows = mk_db::invite::list_by_creator(
        pool,
        me_id,
        limit,
        since_id.as_deref(),
        until_id.as_deref(),
    )
    .await
    .map_err(|e| db_err("list_by_creator[invite/list]", e))?;
    Ok(Json(pack_tickets(state, pool, &rows).await?).into_response())
}

async fn invite_limit(pool: &mk_db::sqlx::PgPool, me_id: &str) -> Result<Response, Response> {
    let remaining = if INVITE_LIMIT > 0 {
        let since =
            mk_types::id::gen_aid(super::now_ms() as i64 - INVITE_LIMIT_CYCLE_MIN * 60 * 1000);
        let count = mk_db::invite::count_created_since(pool, me_id, &since)
            .await
            .map_err(|e| db_err("count_created_since[invite/limit]", e))?;
        Some((INVITE_LIMIT - count).max(0))
    } else {
        None
    };
    Ok(Json(json!({ "remaining": remaining })).into_response())
}

// --- admin/invite/* ---

async fn admin_invite_create(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    me_id: &str,
    body: &Value,
) -> Result<Response, Response> {
    let count = match body.get("count") {
        None | Some(Value::Null) => 1,
        Some(v) => {
            let n = v
                .as_i64()
                .ok_or_else(|| invalid_param("count", "must be an integer"))?;
            if !(1..=100).contains(&n) {
                return Err(invalid_param("count", "must be >= 1 and <= 100"));
            }
            n
        }
    };
    let expires_at = match body.get("expiresAt") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => mk_db::invite::parse_timestamptz_ms(pool, s)
            .await
            .map_err(|_| invalid_datetime())?,
        Some(_) => return Err(invalid_param("expiresAt", "must be a string or null")),
    };

    let mut rows = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let id = mk_types::id::gen_aid_now();
        let code = generate_invite_code();
        mk_db::invite::insert(pool, &id, &code, expires_at, me_id)
            .await
            .map_err(|e| db_err("insert[admin/invite/create]", e))?;
        rows.push(mk_db::invite::TicketRow {
            id,
            code,
            expires_at_ms: expires_at,
            used_at_ms: None,
            pending_user_id: None,
            created_by_id: Some(me_id.to_string()),
            used_by_id: None,
        });
    }
    Ok(Json(pack_tickets(state, pool, &rows).await?).into_response())
}

async fn admin_invite_list(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
) -> Result<Response, Response> {
    let limit = optional_limit(body, 30)?;
    let offset = optional_offset(body)?;
    let ty = optional_enum(body, "type", &["unused", "used", "expired", "all"], "all")?;
    let sort = match body.get("sort") {
        None | Some(Value::Null) => None,
        Some(Value::String(s))
            if ["+createdAt", "-createdAt", "+usedAt", "-usedAt"].contains(&s.as_str()) =>
        {
            Some(s.clone())
        }
        Some(_) => {
            return Err(invalid_param(
                "sort",
                "must be one of: +createdAt, -createdAt, +usedAt, -usedAt",
            ));
        }
    };
    let rows = mk_db::invite::list_admin(pool, limit, offset, &ty, sort.as_deref())
        .await
        .map_err(|e| db_err("list_admin[admin/invite/list]", e))?;
    Ok(Json(pack_tickets(state, pool, &rows).await?).into_response())
}

// --- promo/* ---

/// Repo picker (no upstream counterpart at ea2c3aed): return a random
/// unexpired promo note the caller has not read, then mark it read. Null when
/// nothing is left.
async fn promo_show(pool: &mk_db::sqlx::PgPool, me_id: &str) -> Result<Response, Response> {
    let Some(row) = mk_db::promo::random_unread(pool, me_id, super::now_ms() as i64)
        .await
        .map_err(|e| db_err("random_unread[promo/show]", e))?
    else {
        return Ok(Json(Value::Null).into_response());
    };
    mk_db::promo::mark_read(pool, &mk_types::id::gen_aid_now(), me_id, &row.note_id)
        .await
        .map_err(|e| db_err("mark_read[promo/show]", e))?;
    Ok(Json(promo_note_json(&row)).into_response())
}

/// Upstream `endpoints/promo/read.ts`: mark one note read (idempotent).
async fn promo_read(
    pool: &mk_db::sqlx::PgPool,
    me_id: &str,
    body: &Value,
) -> Result<Response, Response> {
    let note_id = require_str(body, "noteId")?;
    let note = mk_db::note::find_note_by_id(pool, &note_id)
        .await
        .map_err(|e| db_err("find_note_by_id[promo/read]", e))?;
    if note.is_none() {
        return Err(meta_err(
            "NO_SUCH_NOTE",
            NO_SUCH_NOTE_READ_ID,
            "No such note.",
        ));
    }
    let exists = mk_db::promo::is_read(pool, me_id, &note_id)
        .await
        .map_err(|e| db_err("is_read[promo/read]", e))?;
    if !exists {
        mk_db::promo::mark_read(pool, &mk_types::id::gen_aid_now(), me_id, &note_id)
            .await
            .map_err(|e| db_err("mark_read[promo/read]", e))?;
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

// --- admin/promo/* ---

async fn admin_promo_create(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
) -> Result<Response, Response> {
    let note_id = require_str(body, "noteId")?;
    let expires_at = require_i64(body, "expiresAt")?;
    let note = mk_db::note::find_note_by_id(pool, &note_id)
        .await
        .map_err(|e| db_err("find_note_by_id[admin/promo/create]", e))?;
    let Some(note) = note else {
        return Err(meta_err(
            "NO_SUCH_NOTE",
            NO_SUCH_NOTE_PROMO_ID,
            "No such note.",
        ));
    };
    if mk_db::promo::note_exists(pool, &note_id)
        .await
        .map_err(|e| db_err("note_exists[admin/promo/create]", e))?
    {
        return Err(meta_err(
            "ALREADY_PROMOTED",
            ALREADY_PROMOTED_ID,
            "The note has already promoted.",
        ));
    }
    mk_db::promo::insert_note(pool, &note_id, expires_at, &note.user_id)
        .await
        .map_err(|e| db_err("insert_note[admin/promo/create]", e))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Repo list (no upstream counterpart at ea2c3aed): all promo_note rows.
async fn admin_promo_list(pool: &mk_db::sqlx::PgPool) -> Result<Response, Response> {
    let rows = mk_db::promo::list_notes(pool)
        .await
        .map_err(|e| db_err("list_notes[admin/promo/list]", e))?;
    let out: Vec<Value> = rows.iter().map(promo_note_json).collect();
    Ok(Json(Value::Array(out)).into_response())
}
