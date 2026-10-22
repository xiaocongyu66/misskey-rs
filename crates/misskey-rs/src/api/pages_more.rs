//! Page endpoints — ports of upstream
//! `endpoints/pages/{create,update,delete,show,like,unlike,featured}.ts` +
//! `core/entities/PageEntityService.ts` (plus the `pages/my` / `pages/liked`
//! reads the arena exercises).
//!
//! Tables ship in the 0001 baseline (`page`, `page_like`); db paths live in
//! `mk_db::page`. The `// pages-agent` dispatch arm in mod.rs parses the
//! body, extracts the token and probes a declared credential; this module
//! owns the requireCredential gates and the endpoint bodies.
//!
//! Guest-ok: pages/show, pages/featured (optional me).
//! requireCredential: create/update/delete/like/unlike/my/liked.
//!
//! PACK (PageEntityService.pack): { id, createdAt (from the aid), updatedAt,
//! userId, user (UserLite), name, title, summary, content, variables,
//! eyeCatchingImage (DriveFile | null), font, alignCenter,
//! hideTitleWhenPinned, likedCount } + `isLiked` only when me is present
//! (upstream returns `undefined`, which JSON.stringify drops). create calls
//! pack WITHOUT me, so its response omits isLiked too.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// Endpoint names routed into this module (single source of truth for the
/// `// pages-agent` dispatch arm in mod.rs).
pub(crate) const PAGES_ENDPOINTS: &[&str] = &[
    "pages/create",
    "pages/update",
    "pages/delete",
    "pages/show",
    "pages/like",
    "pages/unlike",
    "pages/featured",
    "pages/my",
    "pages/liked",
];

pub(crate) fn handles(name: &str) -> bool {
    PAGES_ENDPOINTS.contains(&name)
}

/// requireCredential endpoints (upstream meta.requireCredential).
const REQUIRES_CREDENTIAL: &[&str] = &[
    "pages/create",
    "pages/update",
    "pages/delete",
    "pages/like",
    "pages/unlike",
    "pages/my",
    "pages/liked",
];

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_pages", error = %e, "db error: {context}");
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

/// Nullable optional string: absent → None, `null` → Some(None), string →
/// Some(Some(s)).
fn opt_nullable_str(body: &Value, key: &str) -> Result<Option<Option<String>>, Response> {
    match body.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(s)) => Ok(Some(Some(s.clone()))),
        Some(_) => Err(invalid_param(key, "must be a string or null")),
    }
}

fn opt_bool(body: &Value, key: &str) -> Result<Option<bool>, Response> {
    match body.get(key) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(invalid_param(key, "must be a boolean")),
    }
}

fn bool_param(body: &Value, key: &str, dflt: bool) -> Result<bool, Response> {
    Ok(opt_bool(body, key)?.unwrap_or(dflt))
}

/// Array-valued param (`content` / `variables`): absent → None, array →
/// Some, anything else → INVALID_PARAM.
fn opt_array(body: &Value, key: &str) -> Result<Option<Value>, Response> {
    match body.get(key) {
        None => Ok(None),
        Some(Value::Array(_)) => Ok(body.get(key).cloned()),
        Some(_) => Err(invalid_param(key, "must be an array")),
    }
}

fn array_param(body: &Value, key: &str) -> Result<Value, Response> {
    Ok(opt_array(body, key)?.unwrap_or_else(|| json!([])))
}

fn font_param(body: &Value, dflt: Option<&str>) -> Result<Option<String>, Response> {
    match body.get("font") {
        None => Ok(dflt.map(str::to_string)),
        Some(Value::String(s)) if s == "serif" || s == "sans-serif" => Ok(Some(s.clone())),
        Some(_) => Err(invalid_param("font", "must be serif or sans-serif")),
    }
}

fn check_len(key: &str, s: &str, max: usize) -> Result<(), Response> {
    if s.chars().count() > max {
        return Err(invalid_param(
            key,
            &format!("must be up to {max} characters"),
        ));
    }
    Ok(())
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

/// `POST /api/pages/...` entry (body pre-parsed, token + credential-declared
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
        "pages/create" => page_create(state, pool, body, me_id.as_deref().unwrap()).await,
        "pages/update" => page_update(pool, body, me_id.as_deref().unwrap()).await,
        "pages/delete" => page_delete(pool, body, me_id.as_deref().unwrap()).await,
        "pages/show" => page_show(state, pool, body, me_id.as_deref()).await,
        "pages/like" => page_like(pool, body, me_id.as_deref().unwrap()).await,
        "pages/unlike" => page_unlike(pool, body, me_id.as_deref().unwrap()).await,
        "pages/featured" => page_featured(state, pool, body, me_id.as_deref()).await,
        "pages/my" => page_my(state, pool, body, me_id.as_deref().unwrap()).await,
        "pages/liked" => page_liked(state, pool, body, me_id.as_deref().unwrap()).await,
        _ => Ok(api_err(ApiError::unknown_endpoint())),
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

// ---------------------------------------------------------------------------
// PageEntityService.pack
// ---------------------------------------------------------------------------

async fn pack_page(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    row: &mk_db::page::PageRow,
    me_id: Option<&str>,
) -> Result<Value, Response> {
    let user_lite = super::note_user_lite(state, pool, &row.user_id).await?;
    let created_at = mk_types::id::parse_aid_date(&row.id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default();
    let eye_catching_image = match &row.eye_catching_image_id {
        Some(file_id) => match mk_db::drive::find_drive_file(pool, file_id).await {
            Ok(Some(file)) => super::drive::pack_drive_file(&file, false),
            Ok(None) => Value::Null,
            Err(e) => return Err(db_err("pack_page find_drive_file", e)),
        },
        None => Value::Null,
    };
    let mut out = json!({
        "id": row.id,
        "createdAt": created_at,
        "updatedAt": mk_core::services::signup::ms_to_iso(row.updated_at_ms),
        "userId": row.user_id,
        "user": user_lite,
        "name": row.name,
        "title": row.title,
        "summary": row.summary,
        "content": row.content,
        "variables": row.variables,
        "eyeCatchingImage": eye_catching_image,
        "font": row.font,
        "alignCenter": row.align_center,
        "hideTitleWhenPinned": row.hide_title_when_pinned,
        "likedCount": row.liked_count,
    });
    if let Some(me) = me_id {
        let liked = mk_db::page::is_liked(pool, &row.id, me)
            .await
            .map_err(|e| db_err("pack_page is_liked", e))?;
        out.as_object_mut()
            .unwrap()
            .insert("isLiked".into(), json!(liked));
    }
    Ok(out)
}

pub(crate) async fn pack_many(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    rows: &[mk_db::page::PageRow],
    me_id: Option<&str>,
) -> Result<Value, Response> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(pack_page(state, pool, row, me_id).await?);
    }
    Ok(json!(out))
}

async fn load_page(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    no_such_id: &str,
) -> Result<mk_db::page::PageRow, Response> {
    let page_id = required_str(body, "pageId")?;
    match mk_db::page::find_page(pool, &page_id).await {
        Ok(Some(row)) => Ok(row),
        Ok(None) => Err(meta_err("NO_SUCH_PAGE", no_such_id, "No such page.")),
        Err(e) => Err(db_err("find_page", e)),
    }
}

// ---------------------------------------------------------------------------
// endpoints
// ---------------------------------------------------------------------------

/// `POST /api/pages/create` — name required; every other param defaults.
async fn page_create(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let name = required_str(body, "name")?;
    if name.is_empty() {
        return Err(invalid_param("name", "must not be empty"));
    }
    check_len("name", &name, 256)?;
    let title = opt_str(body, "title")?.unwrap_or_default();
    check_len("title", &title, 256)?;
    let summary = opt_nullable_str(body, "summary")?.flatten();
    if let Some(s) = &summary {
        check_len("summary", s, 256)?;
    }
    let content = array_param(body, "content")?;
    let variables = array_param(body, "variables")?;
    let script = opt_str(body, "script")?.unwrap_or_default();
    check_len("script", &script, 16384)?;
    let eye_catching_image_id = opt_nullable_str(body, "eyeCatchingImageId")?.flatten();
    let font = font_param(body, Some("sans-serif"))?.expect("default font");
    let align_center = bool_param(body, "alignCenter", false)?;
    let hide_title_when_pinned = bool_param(body, "hideTitleWhenPinned", false)?;
    let id = mk_types::id::gen_aid_now();
    mk_db::page::insert_page(
        pool,
        &mk_db::page::NewPage {
            id: &id,
            user_id: me_id,
            name: &name,
            title: &title,
            summary: summary.as_deref(),
            content: &content,
            variables: &variables,
            eye_catching_image_id: eye_catching_image_id.as_deref(),
            font: &font,
            align_center,
            hide_title_when_pinned,
            script: &script,
        },
    )
    .await
    .map_err(|e| db_err("page_create insert", e))?;
    let row = mk_db::page::find_page(pool, &id)
        .await
        .map_err(|e| db_err("page_create refetch", e))?
        .ok_or_else(internal)?;
    // upstream `pack(page)` (no me) → no isLiked key
    Ok(ok_json(pack_page(state, pool, &row, None).await?))
}

/// `POST /api/pages/update` — owner only; returns 204.
async fn page_update(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let row = load_page(pool, body, "15ad2f2c-9a5b-4f3d-8d1f-0f9c1a7b2e31").await?;
    if row.user_id != me_id {
        return Err(meta_err(
            "ACCESS_DENIED",
            "08e60c88-5948-478e-a132-02ec701d67b2",
            "Access denied.",
        ));
    }
    let name = opt_str(body, "name")?;
    if let Some(n) = &name {
        if n.is_empty() {
            return Err(invalid_param("name", "must not be empty"));
        }
        check_len("name", n, 256)?;
    }
    let title = opt_str(body, "title")?;
    if let Some(t) = &title {
        check_len("title", t, 256)?;
    }
    let summary = opt_nullable_str(body, "summary")?;
    if let Some(Some(s)) = &summary {
        check_len("summary", s, 256)?;
    }
    let content = opt_array(body, "content")?;
    let variables = opt_array(body, "variables")?;
    let script = opt_str(body, "script")?;
    if let Some(s) = &script {
        check_len("script", s, 16384)?;
    }
    let eye_catching_image_id = opt_nullable_str(body, "eyeCatchingImageId")?;
    let font = font_param(body, None)?;
    let align_center = opt_bool(body, "alignCenter")?;
    let hide_title_when_pinned = opt_bool(body, "hideTitleWhenPinned")?;
    mk_db::page::update_page(
        pool,
        &row.id,
        &mk_db::page::UpdatePage {
            name: name.as_deref(),
            title: title.as_deref(),
            summary: summary.as_ref().map(|s| s.as_deref()),
            content: content.as_ref(),
            variables: variables.as_ref(),
            eye_catching_image_id: eye_catching_image_id.as_ref().map(|s| s.as_deref()),
            font: font.as_deref(),
            align_center,
            hide_title_when_pinned,
            script: script.as_deref(),
        },
    )
    .await
    .map_err(|e| db_err("page_update", e))?;
    Ok(ok_empty())
}

/// `POST /api/pages/delete` — owner or moderator; returns 204.
async fn page_delete(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let row = load_page(pool, body, "8d7e3b9a-4c6f-4f0e-9b2a-6a1d5c3e7f20").await?;
    if row.user_id != me_id && !super::viewer_is_moderator(pool, Some(me_id)).await {
        return Err(meta_err(
            "ACCESS_DENIED",
            "1036ad7b-9f92-4fff-89c3-0e50dc941704",
            "Access denied.",
        ));
    }
    mk_db::page::delete_page(pool, &row.id)
        .await
        .map_err(|e| db_err("page_delete", e))?;
    Ok(ok_empty())
}

/// `POST /api/pages/show` — guest-ok.
async fn page_show(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: Option<&str>,
) -> Result<Response, Response> {
    let row = load_page(pool, body, "7f1d9c2e-5a3b-4c8d-9e6f-0b2a4c6e8d10").await?;
    Ok(ok_json(pack_page(state, pool, &row, me_id).await?))
}

/// `POST /api/pages/like`.
async fn page_like(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let row = load_page(pool, body, "c07c1491-9161-4c5c-9d75-01906f911f73").await?;
    if row.user_id == me_id {
        return Err(meta_err(
            "YOUR_PAGE",
            "3fd8a0e7-5955-4ba9-85bb-bf3e0c30e13b",
            "You cannot like your page.",
        ));
    }
    if mk_db::page::is_liked(pool, &row.id, me_id)
        .await
        .map_err(|e| db_err("page_like is_liked", e))?
    {
        return Err(meta_err(
            "ALREADY_LIKED",
            "010065cf-ad43-40df-8067-abff9f4686e3",
            "The page has already been liked.",
        ));
    }
    let duplicate = mk_db::page::insert_like(pool, &mk_types::id::gen_aid_now(), &row.id, me_id)
        .await
        .map_err(|e| db_err("page_like insert", e))?;
    if duplicate {
        return Err(meta_err(
            "ALREADY_LIKED",
            "010065cf-ad43-40df-8067-abff9f4686e3",
            "The page has already been liked.",
        ));
    }
    mk_db::page::increment_liked_count(pool, &row.id)
        .await
        .map_err(|e| db_err("page_like increment", e))?;
    Ok(ok_empty())
}

/// `POST /api/pages/unlike`.
async fn page_unlike(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let row = load_page(pool, body, "afe8424a-a69e-432d-a5f2-2f0740c62410").await?;
    let deleted = mk_db::page::delete_like(pool, &row.id, me_id)
        .await
        .map_err(|e| db_err("page_unlike delete", e))?;
    if !deleted {
        return Err(meta_err(
            "NOT_LIKED",
            "755f25a7-9871-4f65-9f34-51eaad9ae0ac",
            "You have not liked that page.",
        ));
    }
    mk_db::page::decrement_liked_count(pool, &row.id)
        .await
        .map_err(|e| db_err("page_unlike decrement", e))?;
    Ok(ok_empty())
}

/// `POST /api/pages/featured` — guest-ok.
async fn page_featured(
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
    let rows = mk_db::page::list_featured(pool, offset, limit)
        .await
        .map_err(|e| db_err("page_featured", e))?;
    pack_many(state, pool, &rows, me_id).await.map(ok_json)
}

/// `POST /api/pages/my` — my pages, paged on page id.
async fn page_my(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let limit = limit_param(body, 10)?;
    let (since_id, until_id) = pager_ids(body)?;
    let rows = mk_db::page::list_my(pool, me_id, since_id.as_deref(), until_id.as_deref(), limit)
        .await
        .map_err(|e| db_err("page_my list_my", e))?;
    pack_many(state, pool, &rows, Some(me_id))
        .await
        .map(ok_json)
}

/// `POST /api/pages/liked` — pages I liked, paged on like id.
async fn page_liked(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let limit = limit_param(body, 10)?;
    let (since_id, until_id) = pager_ids(body)?;
    let rows =
        mk_db::page::list_liked(pool, me_id, since_id.as_deref(), until_id.as_deref(), limit)
            .await
            .map_err(|e| db_err("page_liked list_liked", e))?;
    pack_many(state, pool, &rows, Some(me_id))
        .await
        .map(ok_json)
}
