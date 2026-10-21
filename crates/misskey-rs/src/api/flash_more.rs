//! Flash (Misskey Play) endpoints — ports of upstream
//! `endpoints/flash/{create,update,delete,show,my,my-likes,like,unlike,
//! featured,search}.ts` + `core/FlashService.ts` and
//! `core/entities/{Flash,FlashLike}EntityService.ts`.
//!
//! Tables ship in the 0001 baseline (`flash`, `flash_like`); db paths live in
//! `mk_db::flash`. The `// flash-agent` dispatch arm in mod.rs parses the
//! body, extracts the token and probes a declared credential; this module
//! owns the requireCredential gates and the endpoint bodies.
//!
//! Guest-ok: flash/show, flash/featured, flash/search (optional me).
//! requireCredential: create/update/delete/my/my-likes/like/unlike.
//!
//! PACK (FlashEntityService.pack): { id, createdAt (from the aid),
//! updatedAt, userId, user (UserLite), title, summary, script, visibility,
//! likedCount } + `isLiked` only when me is present (upstream returns
//! `undefined`, which JSON.stringify drops). create and flash/my call
//! packMany WITHOUT me, so their responses omit isLiked too.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// Endpoint names routed into this module (single source of truth for the
/// `// flash-agent` dispatch arm in mod.rs).
pub(crate) const FLASH_ENDPOINTS: &[&str] = &[
    "flash/create",
    "flash/update",
    "flash/delete",
    "flash/show",
    "flash/my",
    "flash/my-likes",
    "flash/like",
    "flash/unlike",
    "flash/featured",
    "flash/search",
];

pub(crate) fn handles(name: &str) -> bool {
    FLASH_ENDPOINTS.contains(&name)
}

/// requireCredential endpoints (upstream meta.requireCredential).
const REQUIRES_CREDENTIAL: &[&str] = &[
    "flash/create",
    "flash/update",
    "flash/delete",
    "flash/my",
    "flash/my-likes",
    "flash/like",
    "flash/unlike",
];

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_flash", error = %e, "db error: {context}");
    internal()
}

/// Meta error without httpStatusCode: kind=client → default 400.
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

fn ok_empty() -> Response {
    // upstream `send(reply, undefined)` → 204
    StatusCode::NO_CONTENT.into_response()
}

fn ok_json(v: Value) -> Response {
    (StatusCode::OK, Json(v)).into_response()
}

fn required_str(body: &Value, key: &str) -> Result<String, Response> {
    match body.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(invalid_param(key, "must be a string")),
        None => Err(invalid_param(key, "required")),
    }
}

fn opt_str(body: &Value, key: &str) -> Result<Option<String>, Response> {
    match body.get(key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(invalid_param(key, "must be a string")),
    }
}

fn permissions_param(body: &Value, required: bool) -> Result<Option<Vec<String>>, Response> {
    match body.get("permissions") {
        None if required => Err(invalid_param("permissions", "required")),
        None => Ok(None),
        Some(Value::Array(a)) => {
            let mut out = Vec::with_capacity(a.len());
            for v in a {
                let Some(s) = v.as_str() else {
                    return Err(invalid_param("permissions", "must be an array of strings"));
                };
                out.push(s.to_string());
            }
            Ok(Some(out))
        }
        Some(_) => Err(invalid_param("permissions", "must be an array of strings")),
    }
}

fn visibility_param(body: &Value, default: Option<&str>) -> Result<Option<String>, Response> {
    match body.get("visibility") {
        None => Ok(default.map(str::to_string)),
        Some(Value::String(s)) if s == "public" || s == "private" => Ok(Some(s.clone())),
        Some(_) => Err(invalid_param("visibility", "must be public or private")),
    }
}

fn limit_param(body: &Value, dflt: i64) -> Result<i64, Response> {
    match body.get("limit") {
        None => Ok(dflt),
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

/// makePaginationQuery id-or-date fallback: the id form wins; a date falls to
/// `idService.gen(date)` — a 0 date is falsy upstream and resolves to none.
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

fn search_words(search: &str) -> Vec<String> {
    search.trim().split(' ').map(str::to_string).collect()
}

/// `POST /api/flash/...` entry (body pre-parsed, token + credential-declared
/// pre-extracted).
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
    let me_id: Option<String> = if REQUIRES_CREDENTIAL.contains(&name) {
        match super::social::require_checked_user(pool, token.as_deref(), credential_declared).await
        {
            Ok(id) => Some(id),
            Err(resp) => return resp,
        }
    } else if token.is_some() || credential_declared {
        super::social::require_checked_user(pool, token.as_deref(), credential_declared)
            .await
            .ok()
    } else {
        None
    };

    let result: Result<Response, Response> = match name {
        "flash/create" => flash_create(state, pool, body, me_id.as_deref().unwrap()).await,
        "flash/update" => flash_update(pool, body, me_id.as_deref().unwrap()).await,
        "flash/delete" => flash_delete(pool, body, me_id.as_deref().unwrap()).await,
        "flash/show" => flash_show(state, pool, body, me_id.as_deref()).await,
        "flash/my" => flash_my(state, pool, body, me_id.as_deref().unwrap()).await,
        "flash/my-likes" => flash_my_likes(state, pool, body, me_id.as_deref().unwrap()).await,
        "flash/like" => flash_like(pool, body, me_id.as_deref().unwrap()).await,
        "flash/unlike" => flash_unlike(pool, body, me_id.as_deref().unwrap()).await,
        "flash/featured" => flash_featured(state, pool, body, me_id.as_deref()).await,
        "flash/search" => flash_search(state, pool, body, me_id.as_deref()).await,
        _ => Ok(api_err(ApiError::unknown_endpoint())),
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

// ---------------------------------------------------------------------------
// FlashEntityService.pack
// ---------------------------------------------------------------------------

async fn pack_flash(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    row: &mk_db::flash::FlashRow,
    me_id: Option<&str>,
) -> Result<Value, Response> {
    let user_lite = super::note_user_lite(state, pool, &row.user_id).await?;
    let created_at = mk_types::id::parse_aid_date(&row.id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default();
    let mut out = json!({
        "id": row.id,
        "createdAt": created_at,
        "updatedAt": mk_core::services::signup::ms_to_iso(row.updated_at_ms),
        "userId": row.user_id,
        "user": user_lite,
        "title": row.title,
        "summary": row.summary,
        "script": row.script,
        "visibility": row.visibility,
        "likedCount": row.liked_count,
    });
    if let Some(me) = me_id {
        let liked = mk_db::flash::is_liked(pool, &row.id, me)
            .await
            .map_err(|e| db_err("pack_flash is_liked", e))?;
        out.as_object_mut()
            .unwrap()
            .insert("isLiked".into(), json!(liked));
    }
    Ok(out)
}

async fn pack_many(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    rows: &[mk_db::flash::FlashRow],
    me_id: Option<&str>,
) -> Result<Value, Response> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(pack_flash(state, pool, row, me_id).await?);
    }
    Ok(json!(out))
}

async fn load_flash(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    no_such_id: &str,
) -> Result<mk_db::flash::FlashRow, Response> {
    let flash_id = required_str(body, "flashId")?;
    match mk_db::flash::find_flash(pool, &flash_id).await {
        Ok(Some(row)) => Ok(row),
        Ok(None) => Err(meta_err("NO_SUCH_FLASH", no_such_id, "No such flash.")),
        Err(e) => Err(db_err("find_flash", e)),
    }
}

// ---------------------------------------------------------------------------
// endpoints
// ---------------------------------------------------------------------------

/// `POST /api/flash/create` — title/summary/script/permissions required.
async fn flash_create(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let title = required_str(body, "title")?;
    let summary = required_str(body, "summary")?;
    let script = required_str(body, "script")?;
    let permissions = permissions_param(body, true)?.expect("required permissions");
    let visibility = visibility_param(body, Some("public"))?.expect("default visibility");
    let id = mk_types::id::gen_aid_now();
    mk_db::flash::insert_flash(
        pool,
        &mk_db::flash::NewFlash {
            id: &id,
            user_id: me_id,
            title: &title,
            summary: &summary,
            script: &script,
            permissions: &permissions,
            visibility: &visibility,
        },
    )
    .await
    .map_err(|e| {
        // diagnostic: the Debug form carries the FK detail (key + table)
        tracing::error!(target: "dbg_flash", error = ?e, user = %me_id, "flash_create insert");
        internal()
    })?;
    let row = mk_db::flash::find_flash(pool, &id)
        .await
        .map_err(|e| db_err("flash_create refetch", e))?
        .ok_or_else(internal)?;
    // upstream `pack(flash)` (no me) → no isLiked key
    Ok(ok_json(pack_flash(state, pool, &row, None).await?))
}

/// `POST /api/flash/update` — owner only; returns 204.
async fn flash_update(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let row = load_flash(pool, body, "611e13d2-309e-419a-a5e4-e0422da39b02").await?;
    if row.user_id != me_id {
        return Err(meta_err(
            "ACCESS_DENIED",
            "08e60c88-5948-478e-a132-02ec701d67b2",
            "Access denied.",
        ));
    }
    let title = opt_str(body, "title")?;
    let summary = opt_str(body, "summary")?;
    let script = opt_str(body, "script")?;
    let permissions = permissions_param(body, false)?;
    let visibility = visibility_param(body, None)?;
    mk_db::flash::update_flash(
        pool,
        &row.id,
        &mk_db::flash::UpdateFlash {
            title: title.as_deref(),
            summary: summary.as_deref(),
            script: script.as_deref(),
            permissions: permissions.as_deref(),
            visibility: visibility.as_deref(),
        },
    )
    .await
    .map_err(|e| db_err("flash_update", e))?;
    Ok(ok_empty())
}

/// `POST /api/flash/delete` — owner or moderator; returns 204.
async fn flash_delete(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let row = load_flash(pool, body, "de1623ef-bbb3-4289-a71e-14cfa83d9740").await?;
    if row.user_id != me_id && !super::viewer_is_moderator(pool, Some(me_id)).await {
        return Err(meta_err(
            "ACCESS_DENIED",
            "1036ad7b-9f92-4fff-89c3-0e50dc941704",
            "Access denied.",
        ));
    }
    mk_db::flash::delete_flash(pool, &row.id)
        .await
        .map_err(|e| db_err("flash_delete", e))?;
    Ok(ok_empty())
}

/// `POST /api/flash/show` — guest-ok.
async fn flash_show(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: Option<&str>,
) -> Result<Response, Response> {
    let row = load_flash(pool, body, "f0d34a1a-d29a-401d-90ba-1982122b5630").await?;
    Ok(ok_json(pack_flash(state, pool, &row, me_id).await?))
}

/// `POST /api/flash/my` — my flashes, packed without me (upstream packMany).
async fn flash_my(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let limit = limit_param(body, 10)?;
    let (since_id, until_id) = pager_ids(body)?;
    let rows = mk_db::flash::list_my(pool, me_id, since_id.as_deref(), until_id.as_deref(), limit)
        .await
        .map_err(|e| db_err("flash_my list_my", e))?;
    pack_many(state, pool, &rows, None).await.map(ok_json)
}

/// `POST /api/flash/my-likes` — [{ id (like), flash }].
async fn flash_my_likes(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let limit = limit_param(body, 10)?;
    let (since_id, until_id) = pager_ids(body)?;
    let search = match body.get("search") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            if s.chars().count() > 100 {
                return Err(invalid_param("search", "must be up to 100 characters"));
            }
            Some(s.as_str())
        }
        Some(_) => return Err(invalid_param("search", "must be a string or null")),
    };
    let words = search.map(search_words).unwrap_or_default();
    let likes = mk_db::flash::list_my_likes(
        pool,
        me_id,
        &words,
        since_id.as_deref(),
        until_id.as_deref(),
        limit,
    )
    .await
    .map_err(|e| db_err("flash_my_likes list", e))?;
    let mut out = Vec::with_capacity(likes.len());
    for like in &likes {
        let row = mk_db::flash::find_flash(pool, &like.flash_id)
            .await
            .map_err(|e| db_err("flash_my_likes find_flash", e))?;
        let Some(row) = row else { continue };
        out.push(json!({
            "id": like.id,
            "flash": pack_flash(state, pool, &row, Some(me_id)).await?,
        }));
    }
    Ok(ok_json(json!(out)))
}

/// `POST /api/flash/like`.
async fn flash_like(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let row = load_flash(pool, body, "c07c1491-9161-4c5c-9d75-01906f911f73").await?;
    if row.user_id == me_id {
        return Err(meta_err(
            "YOUR_FLASH",
            "3fd8a0e7-5955-4ba9-85bb-bf3e0c30e13b",
            "You cannot like your flash.",
        ));
    }
    if mk_db::flash::is_liked(pool, &row.id, me_id)
        .await
        .map_err(|e| db_err("flash_like is_liked", e))?
    {
        return Err(meta_err(
            "ALREADY_LIKED",
            "010065cf-ad43-40df-8067-abff9f4686e3",
            "The flash has already been liked.",
        ));
    }
    let duplicate = mk_db::flash::insert_like(pool, &mk_types::id::gen_aid_now(), &row.id, me_id)
        .await
        .map_err(|e| db_err("flash_like insert", e))?;
    if duplicate {
        return Err(meta_err(
            "ALREADY_LIKED",
            "010065cf-ad43-40df-8067-abff9f4686e3",
            "The flash has already been liked.",
        ));
    }
    mk_db::flash::increment_liked_count(pool, &row.id)
        .await
        .map_err(|e| db_err("flash_like increment", e))?;
    Ok(ok_empty())
}

/// `POST /api/flash/unlike`.
async fn flash_unlike(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let row = load_flash(pool, body, "afe8424a-a69e-432d-a5f2-2f0740c62410").await?;
    let deleted = mk_db::flash::delete_like(pool, &row.id, me_id)
        .await
        .map_err(|e| db_err("flash_unlike delete", e))?;
    if !deleted {
        return Err(meta_err(
            "NOT_LIKED",
            "755f25a7-9871-4f65-9f34-51eaad9ae0ac",
            "You have not liked that flash.",
        ));
    }
    mk_db::flash::decrement_liked_count(pool, &row.id)
        .await
        .map_err(|e| db_err("flash_unlike decrement", e))?;
    Ok(ok_empty())
}

/// `POST /api/flash/featured` — guest-ok.
async fn flash_featured(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: Option<&str>,
) -> Result<Response, Response> {
    let offset = match body.get("offset") {
        None => 0,
        Some(v) => {
            let n = v
                .as_i64()
                .ok_or_else(|| invalid_param("offset", "must be an integer"))?;
            if n < 0 {
                return Err(invalid_param("offset", "must be >= 0"));
            }
            n
        }
    };
    let limit = limit_param(body, 10)?;
    let rows = mk_db::flash::list_featured(pool, offset, limit)
        .await
        .map_err(|e| db_err("flash_featured", e))?;
    pack_many(state, pool, &rows, me_id).await.map(ok_json)
}

/// `POST /api/flash/search` — guest-ok; public flashes only.
async fn flash_search(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: Option<&str>,
) -> Result<Response, Response> {
    let query = match body.get("query") {
        Some(Value::String(s)) if !s.is_empty() && s.chars().count() <= 100 => s.clone(),
        Some(Value::String(_)) => {
            return Err(invalid_param("query", "must be 1..100 characters"));
        }
        Some(_) => return Err(invalid_param("query", "must be a string")),
        None => return Err(invalid_param("query", "required")),
    };
    let limit = limit_param(body, 5)?;
    let (since_id, until_id) = pager_ids(body)?;
    let words = search_words(&query);
    let rows = mk_db::flash::search(
        pool,
        &words,
        since_id.as_deref(),
        until_id.as_deref(),
        limit,
    )
    .await
    .map_err(|e| db_err("flash_search", e))?;
    pack_many(state, pool, &rows, me_id).await.map(ok_json)
}
