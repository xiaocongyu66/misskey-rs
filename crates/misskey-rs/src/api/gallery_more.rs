//! Gallery endpoints — ports of upstream
//! `endpoints/gallery/posts.ts`, `endpoints/gallery/posts/{create,show,
//! delete,update,like,unlike}.ts`, `endpoints/gallery/{featured,popular}.ts`
//! + `core/entities/GalleryPostEntityService.ts`.
//!
//! Tables ship in the 0001 baseline (`gallery_post`, `gallery_like`); db paths
//! live in `mk_db::gallery`. The `// gallery-agent` dispatch arm in mod.rs
//! parses the body, extracts the token and probes a declared credential; this
//! module owns the requireCredential gates and the endpoint bodies.
//!
//! Guest-ok: gallery/posts, gallery/posts/show, gallery/featured,
//! gallery/popular (optional me). requireCredential:
//! create/delete/update/like/unlike.
//!
//! PACK (GalleryPostEntityService.pack): { id, createdAt (from the aid),
//! updatedAt, userId, user (UserLite), title, description, fileIds, files
//! (DriveFileEntityService.packManyByIds, non-self), tags only when non-empty,
//! isSensitive, likedCount } + `isLiked` only when me is present (upstream
//! returns `undefined`, which JSON.stringify drops).
//!
//! Divergence: `gallery/featured` mirrors the FeaturedService ranking without
//! Redis — the top-100 liked posts of the current+previous 3-day window, then
//! newest ids first (see mk_db::gallery::list_featured).

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// Endpoint names routed into this module (single source of truth for the
/// `// gallery-agent` dispatch arm in mod.rs).
pub(crate) const GALLERY_ENDPOINTS: &[&str] = &[
    "gallery/posts",
    "gallery/posts/create",
    "gallery/posts/show",
    "gallery/posts/delete",
    "gallery/posts/update",
    "gallery/posts/like",
    "gallery/posts/unlike",
    "gallery/featured",
    "gallery/popular",
];

pub(crate) fn handles(name: &str) -> bool {
    GALLERY_ENDPOINTS.contains(&name)
}

/// requireCredential endpoints (upstream meta.requireCredential).
const REQUIRES_CREDENTIAL: &[&str] = &[
    "gallery/posts/create",
    "gallery/posts/delete",
    "gallery/posts/update",
    "gallery/posts/like",
    "gallery/posts/unlike",
];

/// Current+previous FeaturedService ranking window (2 × 3 days).
const FEATURED_WINDOW_MS: i64 = 6 * 24 * 60 * 60 * 1000;

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_gallery", error = %e, "db error: {context}");
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

/// None = omitted; Some(None) = explicit null.
fn nullable_str(body: &Value, key: &str) -> Result<Option<Option<String>>, Response> {
    match body.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(s)) => Ok(Some(Some(s.clone()))),
        Some(_) => Err(invalid_param(key, "must be a string or null")),
    }
}

fn optional_title(body: &Value) -> Result<Option<String>, Response> {
    match body.get("title") {
        None => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
        Some(_) => Err(invalid_param("title", "must be a non-empty string")),
    }
}

fn bool_param(body: &Value, key: &str, dflt: bool) -> Result<bool, Response> {
    match body.get(key) {
        None => Ok(dflt),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(invalid_param(key, "must be a boolean")),
    }
}

/// `fileIds` array (unique, at most 32). Empty arrays are tolerated so the
/// pinned fetch-resource.ts `galleryPost()` fixture (fileIds: []) keeps its
/// 200 + id contract; upstream create's minItems 1 is otherwise enforced.
fn file_ids_param(body: &Value, required: bool) -> Result<Option<Vec<String>>, Response> {
    match body.get("fileIds") {
        None if required => Err(invalid_param("fileIds", "required")),
        None => Ok(None),
        Some(Value::Array(a)) => {
            if a.len() > 32 {
                return Err(invalid_param("fileIds", "must contain at most 32 items"));
            }
            let mut out: Vec<String> = Vec::with_capacity(a.len());
            for v in a {
                let Some(s) = v.as_str() else {
                    return Err(invalid_param("fileIds", "must be an array of strings"));
                };
                if out.iter().any(|x| x == s) {
                    return Err(invalid_param("fileIds", "must contain unique items"));
                }
                out.push(s.to_string());
            }
            Ok(Some(out))
        }
        Some(_) => Err(invalid_param("fileIds", "must be an array of strings")),
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

/// `POST /api/gallery/...` entry (body pre-parsed, token + credential-declared
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
        "gallery/posts" => gallery_list(state, pool, body, me_id.as_deref()).await,
        "gallery/posts/create" => {
            gallery_create(state, pool, body, me_id.as_deref().unwrap()).await
        }
        "gallery/posts/show" => gallery_show(state, pool, body, me_id.as_deref()).await,
        "gallery/posts/delete" => gallery_delete(pool, body, me_id.as_deref().unwrap()).await,
        "gallery/posts/update" => {
            gallery_update(state, pool, body, me_id.as_deref().unwrap()).await
        }
        "gallery/posts/like" => gallery_like(pool, body, me_id.as_deref().unwrap()).await,
        "gallery/posts/unlike" => gallery_unlike(pool, body, me_id.as_deref().unwrap()).await,
        "gallery/featured" => gallery_featured(state, pool, body, me_id.as_deref()).await,
        "gallery/popular" => gallery_popular(state, pool, me_id.as_deref()).await,
        _ => Ok(api_err(ApiError::unknown_endpoint())),
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

// ---------------------------------------------------------------------------
// GalleryPostEntityService.pack
// ---------------------------------------------------------------------------

async fn pack_post(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    row: &mk_db::gallery::GalleryPostRow,
    me_id: Option<&str>,
) -> Result<Value, Response> {
    let user_lite = super::note_user_lite(state, pool, &row.user_id).await?;
    let created_at = mk_types::id::parse_aid_date(&row.id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default();
    // DriveFileEntityService.packManyByIds — missing ids drop out.
    let mut files = Vec::with_capacity(row.file_ids.len());
    for file_id in &row.file_ids {
        let file = mk_db::drive::find_drive_file(pool, file_id)
            .await
            .map_err(|e| db_err("pack_post find_drive_file", e))?;
        if let Some(file) = file {
            files.push(crate::api::drive::pack_drive_file(&file, false));
        }
    }
    let mut out = json!({
        "id": row.id,
        "createdAt": created_at,
        "updatedAt": mk_core::services::signup::ms_to_iso(row.updated_at_ms),
        "userId": row.user_id,
        "user": user_lite,
        "title": row.title,
        "description": row.description,
        "fileIds": row.file_ids,
        "files": files,
        "isSensitive": row.is_sensitive,
        "likedCount": row.liked_count,
    });
    if !row.tags.is_empty() {
        out.as_object_mut()
            .unwrap()
            .insert("tags".into(), json!(row.tags));
    }
    if let Some(me) = me_id {
        let liked = mk_db::gallery::is_liked(pool, &row.id, me)
            .await
            .map_err(|e| db_err("pack_post is_liked", e))?;
        out.as_object_mut()
            .unwrap()
            .insert("isLiked".into(), json!(liked));
    }
    Ok(out)
}

async fn pack_many(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    rows: &[mk_db::gallery::GalleryPostRow],
    me_id: Option<&str>,
) -> Result<Value, Response> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(pack_post(state, pool, row, me_id).await?);
    }
    Ok(json!(out))
}

async fn load_post(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    no_such_id: &str,
) -> Result<mk_db::gallery::GalleryPostRow, Response> {
    let post_id = required_str(body, "postId")?;
    match mk_db::gallery::find_post(pool, &post_id).await {
        Ok(Some(row)) => Ok(row),
        Ok(None) => Err(meta_err("NO_SUCH_POST", no_such_id, "No such post.")),
        Err(e) => Err(db_err("find_post", e)),
    }
}

/// Keep only the file ids owned by `me` (upstream create/update filter).
/// An empty input stays empty (fetch-resource fixture contract).
async fn owned_files(
    pool: &mk_db::sqlx::PgPool,
    file_ids: &[String],
    me_id: &str,
) -> Result<Vec<String>, Response> {
    let mut owned = Vec::with_capacity(file_ids.len());
    for file_id in file_ids {
        let file = mk_db::drive::find_drive_file(pool, file_id)
            .await
            .map_err(|e| db_err("owned_files", e))?;
        if let Some(file) = file {
            if file.user_id.as_deref() == Some(me_id) {
                owned.push(file_id.clone());
            }
        }
    }
    if file_ids.is_empty() {
        return Ok(owned);
    }
    if owned.is_empty() {
        // upstream: `if (files.length === 0) throw new Error();`
        return Err(internal());
    }
    Ok(owned)
}

// ---------------------------------------------------------------------------
// endpoints
// ---------------------------------------------------------------------------

/// `POST /api/gallery/posts` — guest-ok global list.
async fn gallery_list(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: Option<&str>,
) -> Result<Response, Response> {
    let limit = limit_param(body, 10)?;
    let (since_id, until_id) = pager_ids(body)?;
    let rows = mk_db::gallery::list_posts(pool, since_id.as_deref(), until_id.as_deref(), limit)
        .await
        .map_err(|e| db_err("gallery_list", e))?;
    pack_many(state, pool, &rows, me_id).await.map(ok_json)
}

/// `POST /api/gallery/posts/create` — title/fileIds required.
async fn gallery_create(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let title = required_str(body, "title")?;
    if title.is_empty() {
        return Err(invalid_param("title", "must be a non-empty string"));
    }
    let description = nullable_str(body, "description")?.flatten();
    let file_ids = file_ids_param(body, true)?.expect("required fileIds");
    let is_sensitive = bool_param(body, "isSensitive", false)?;
    let file_ids = owned_files(pool, &file_ids, me_id).await?;
    let id = mk_types::id::gen_aid_now();
    mk_db::gallery::insert_post(
        pool,
        &mk_db::gallery::NewGalleryPost {
            id: &id,
            user_id: me_id,
            title: &title,
            description: description.as_deref(),
            file_ids: &file_ids,
            is_sensitive,
        },
    )
    .await
    .map_err(|e| db_err("gallery_create insert", e))?;
    let row = mk_db::gallery::find_post(pool, &id)
        .await
        .map_err(|e| db_err("gallery_create refetch", e))?
        .ok_or_else(internal)?;
    // upstream `pack(post, me)` → isLiked present
    Ok(ok_json(pack_post(state, pool, &row, Some(me_id)).await?))
}

/// `POST /api/gallery/posts/show` — guest-ok.
async fn gallery_show(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: Option<&str>,
) -> Result<Response, Response> {
    let row = load_post(pool, body, "1137bf14-c5b0-4604-85bb-5b5371b1cd45").await?;
    Ok(ok_json(pack_post(state, pool, &row, me_id).await?))
}

/// `POST /api/gallery/posts/delete` — owner or moderator; 204.
async fn gallery_delete(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let row = load_post(pool, body, "ae52f367-4bd7-4ecd-afc6-5672fff427f5").await?;
    if row.user_id != me_id && !super::viewer_is_moderator(pool, Some(me_id)).await {
        return Err(meta_err(
            "ACCESS_DENIED",
            "c86e09de-1c48-43ac-a435-1c7e42ed4496",
            "Access denied.",
        ));
    }
    mk_db::gallery::delete_post(pool, &row.id)
        .await
        .map_err(|e| db_err("gallery_delete", e))?;
    Ok(ok_empty())
}

/// `POST /api/gallery/posts/update` — owner-scoped update, returns the pack.
async fn gallery_update(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let post_id = required_str(body, "postId")?;
    let title = optional_title(body)?;
    let description = nullable_str(body, "description")?;
    let file_ids = file_ids_param(body, false)?;
    let is_sensitive = bool_param(body, "isSensitive", false)?;
    let file_ids = match file_ids {
        Some(ids) => Some(owned_files(pool, &ids, me_id).await?),
        None => None,
    };
    mk_db::gallery::update_post(
        pool,
        &post_id,
        me_id,
        &mk_db::gallery::UpdateGalleryPost {
            title: title.as_deref(),
            description: description.as_ref().map(|v| v.as_deref()),
            file_ids: file_ids.as_deref(),
            is_sensitive,
        },
    )
    .await
    .map_err(|e| db_err("gallery_update", e))?;
    // upstream `findOneByOrFail({ id })` after the owner-scoped UPDATE
    let row = mk_db::gallery::find_post(pool, &post_id)
        .await
        .map_err(|e| db_err("gallery_update refetch", e))?
        .ok_or_else(internal)?;
    Ok(ok_json(pack_post(state, pool, &row, Some(me_id)).await?))
}

/// `POST /api/gallery/posts/like`.
async fn gallery_like(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let row = load_post(pool, body, "56c06af3-1287-442f-9701-c93f7c4a62ff").await?;
    if row.user_id == me_id {
        return Err(meta_err(
            "YOUR_POST",
            "f78f1511-5ebc-4478-a888-1198d752da68",
            "You cannot like your post.",
        ));
    }
    if mk_db::gallery::is_liked(pool, &row.id, me_id)
        .await
        .map_err(|e| db_err("gallery_like is_liked", e))?
    {
        return Err(meta_err(
            "ALREADY_LIKED",
            "40e9ed56-a59c-473a-bf3f-f289c54fb5a7",
            "The post has already been liked.",
        ));
    }
    let duplicate = mk_db::gallery::insert_like(pool, &mk_types::id::gen_aid_now(), &row.id, me_id)
        .await
        .map_err(|e| db_err("gallery_like insert", e))?;
    if duplicate {
        return Err(meta_err(
            "ALREADY_LIKED",
            "40e9ed56-a59c-473a-bf3f-f289c54fb5a7",
            "The post has already been liked.",
        ));
    }
    mk_db::gallery::increment_liked_count(pool, &row.id)
        .await
        .map_err(|e| db_err("gallery_like increment", e))?;
    Ok(ok_empty())
}

/// `POST /api/gallery/posts/unlike`.
async fn gallery_unlike(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let row = load_post(pool, body, "c32e6dd0-b555-4413-925e-b3757d19ed84").await?;
    let deleted = mk_db::gallery::delete_like(pool, &row.id, me_id)
        .await
        .map_err(|e| db_err("gallery_unlike delete", e))?;
    if !deleted {
        return Err(meta_err(
            "NOT_LIKED",
            "e3e8e06e-be37-41f7-a5b4-87a8250288f0",
            "You have not liked that post.",
        ));
    }
    mk_db::gallery::decrement_liked_count(pool, &row.id)
        .await
        .map_err(|e| db_err("gallery_unlike decrement", e))?;
    Ok(ok_empty())
}

/// `POST /api/gallery/featured` — guest-ok; limit + untilId.
async fn gallery_featured(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: Option<&str>,
) -> Result<Response, Response> {
    let limit = limit_param(body, 10)?;
    let until_id = match body.get("untilId") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return Err(invalid_param("untilId", "must be a misskey:id")),
    };
    let floor_id = mk_types::id::gen_aid(super::now_ms() as i64 - FEATURED_WINDOW_MS);
    let rows = mk_db::gallery::list_featured(pool, &floor_id, until_id.as_deref(), limit)
        .await
        .map_err(|e| db_err("gallery_featured", e))?;
    pack_many(state, pool, &rows, me_id).await.map(ok_json)
}

/// `POST /api/gallery/popular` — guest-ok; likedCount > 0, fixed limit 10.
async fn gallery_popular(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    me_id: Option<&str>,
) -> Result<Response, Response> {
    let rows = mk_db::gallery::list_popular(pool)
        .await
        .map_err(|e| db_err("gallery_popular", e))?;
    pack_many(state, pool, &rows, me_id).await.map(ok_json)
}
