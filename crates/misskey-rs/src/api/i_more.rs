//! The `i/*` remainder — slice 1: `i/favorites`, `i/gallery/likes`,
//! `i/pages/likes` (upstream `endpoints/i/{favorites,gallery/likes,pages/
//! likes}.ts`).
//!
//! All three are `requireCredential: true` paged reads over a like/favorite
//! table owned by the caller:
//! - `i/favorites`     → `note_favorite` (id/userId/noteId; the 0001 baseline
//!   has no createdAt column — upstream derives it from the aid id, same as
//!   every other table here);
//! - `i/gallery/likes` → `gallery_like` joined to `gallery_post`;
//! - `i/pages/likes`   → `page_like` joined to `page`.
//!
//! ParamDef mirrors upstream: `limit` (integer 1..=100, default 10) +
//! `sinceId`/`untilId` (misskey:id cursors, exclusive). Rows order by id DESC
//! (`id < untilId` / `id > sinceId`), the same makePaginationQuery shape the
//! other paged reads use.
//!
//! RESPONSE SHAPE: an array of entity-shaped objects. Minimal documented
//! fields: favorites `{ id, createdAt, noteId }`, gallery likes `{ id,
//! createdAt, post: { id, title, description, userId, fileIds,
//! isSensitive, likedCount, tags } }`, page likes `{ id, createdAt, page:
//! { id, title, name, userId, visibility, likedCount } }`. Upstream packs the
//! full Note/GalleryPost/Page entities; the nested here is the identity
//! subset (no user pack / files expansion) — documented divergence, the same
//! minimal-pack convention used by the other slice modules.
//!
//! Auth runs before paramDef validation (ApiCallService authenticates first):
//! `{}` answers CREDENTIAL_REQUIRED, a malformed credential
//! AUTHENTICATION_FAILED, a suspended user YOUR_ACCOUNT_SUSPENDED.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// The endpoint names routed into this module (single source of truth for the
/// `// i-more` dispatch arm in mod.rs).
pub(crate) const ENDPOINTS: &[&str] = &["i/favorites", "i/gallery/likes", "i/pages/likes"];

pub(crate) fn handles(name: &str) -> bool {
    ENDPOINTS.contains(&name)
}

/// Entry from the `// i-more` dispatch arm in mod.rs (body pre-parsed, token
/// pre-extracted).
pub(crate) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
) -> Response {
    let Some(pool) = &state.pool else {
        return internal();
    };
    let result: Check<Response> = async {
        // requireCredential (upstream meta on all three names).
        let me_id =
            super::social::require_checked_user(pool, token.as_deref(), false).await?;
        let (limit, since_id, until_id) = paging(body)?;
        match name {
            "i/favorites" => {
                favorites(pool, &me_id, limit, since_id.as_deref(), until_id.as_deref()).await
            }
            "i/gallery/likes" => {
                gallery_likes(pool, &me_id, limit, since_id.as_deref(), until_id.as_deref())
                    .await
            }
            "i/pages/likes" => {
                pages_likes(pool, &me_id, limit, since_id.as_deref(), until_id.as_deref()).await
            }
            _ => Ok(api_err(ApiError::unknown_endpoint())),
        }
    }
    .await;
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
    tracing::error!(target: "dbg_i_more", error = %e, "db error: {context}");
    internal()
}

fn ok_json(v: Value) -> Response {
    (StatusCode::OK, Json(v)).into_response()
}

/// endpoint-base.ts L63 ajv-validate branch (`INVALID_PARAM`).
fn schema_invalid(param: &str, reason: &str) -> Response {
    api_err(
        ApiError::new(
            400,
            mk_types::api_error::ApiErrorKind::Client,
            "INVALID_PARAM",
            "3d81ceae-475f-4600-b2a8-2bc116157532",
            "Invalid param.",
        )
        .with_info(json!({ "param": param, "reason": reason })),
    )
}

/// `{ type: 'integer', minimum: 1, maximum: 100, default: 10 }`.
fn limit_param(body: &Value) -> Result<i64, Response> {
    match body.get("limit") {
        None => Ok(10),
        Some(Value::Number(n)) => {
            let v = n
                .as_i64()
                .ok_or_else(|| schema_invalid("limit", "must be an integer"))?;
            if !(1..=100).contains(&v) {
                return Err(schema_invalid("limit", "must be >= 1 and <= 100"));
            }
            Ok(v)
        }
        Some(_) => Err(schema_invalid("limit", "must be an integer")),
    }
}

/// `{ type: 'string', format: 'misskey:id' }` cursor (null/absent = unset).
fn opt_id_param(body: &Value, key: &str) -> Result<Option<String>, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s))
            if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            Ok(Some(s.clone()))
        }
        Some(Value::String(_)) => Err(schema_invalid(key, "must be a misskey:id (alnum)")),
        Some(_) => Err(schema_invalid(key, "must be a string")),
    }
}

fn paging(body: &Value) -> Result<(i64, Option<String>, Option<String>), Response> {
    Ok((
        limit_param(body)?,
        opt_id_param(body, "sinceId")?,
        opt_id_param(body, "untilId")?,
    ))
}

/// `new Date(id timestamp).toISOString()` — null when the id is not an aid.
fn created_at(id: &str) -> Value {
    match mk_types::id::parse_aid_date(id) {
        Some(ms) => json!(mk_core::services::signup::ms_to_iso(ms)),
        None => Value::Null,
    }
}

async fn favorites(
    pool: &mk_db::sqlx::PgPool,
    me_id: &str,
    limit: i64,
    since_id: Option<&str>,
    until_id: Option<&str>,
) -> Check<Response> {
    let rows = mk_db::sqlx::query(
        r#"SELECT "id", "noteId"
           FROM "note_favorite"
           WHERE "userId" = $1
             AND ($2::text IS NULL OR "id" < $2)
             AND ($3::text IS NULL OR "id" > $3)
           ORDER BY "id" DESC
           LIMIT $4"#,
    )
    .bind(me_id)
    .bind(until_id)
    .bind(since_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| db_err("i/favorites", e))?;

    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            use mk_db::sqlx::Row;
            let id: String = r.get("id");
            let note_id: String = r.get("noteId");
            json!({
                "id": id,
                "createdAt": created_at(&id),
                "noteId": note_id,
            })
        })
        .collect();
    Ok(ok_json(Value::Array(out)))
}

async fn gallery_likes(
    pool: &mk_db::sqlx::PgPool,
    me_id: &str,
    limit: i64,
    since_id: Option<&str>,
    until_id: Option<&str>,
) -> Check<Response> {
    let rows = mk_db::sqlx::query(
        r#"SELECT gl."id", gp."id" AS "postId", gp."title", gp."description",
                  gp."userId", gp."fileIds", gp."isSensitive", gp."likedCount",
                  gp."tags"
           FROM "gallery_like" gl
           INNER JOIN "gallery_post" gp ON gp."id" = gl."postId"
           WHERE gl."userId" = $1
             AND ($2::text IS NULL OR gl."id" < $2)
             AND ($3::text IS NULL OR gl."id" > $3)
           ORDER BY gl."id" DESC
           LIMIT $4"#,
    )
    .bind(me_id)
    .bind(until_id)
    .bind(since_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| db_err("i/gallery/likes", e))?;

    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            use mk_db::sqlx::Row;
            let id: String = r.get("id");
            let post_id: String = r.get("postId");
            let file_ids: Vec<String> = r.get("fileIds");
            let tags: Vec<String> = r.get("tags");
            json!({
                "id": id,
                "createdAt": created_at(&id),
                "post": {
                    "id": post_id,
                    "title": r.get::<String, _>("title"),
                    "description": r.get::<Option<String>, _>("description"),
                    "userId": r.get::<String, _>("userId"),
                    "fileIds": file_ids,
                    "isSensitive": r.get::<bool, _>("isSensitive"),
                    "likedCount": r.get::<i32, _>("likedCount"),
                    "tags": tags,
                },
            })
        })
        .collect();
    Ok(ok_json(Value::Array(out)))
}

async fn pages_likes(
    pool: &mk_db::sqlx::PgPool,
    me_id: &str,
    limit: i64,
    since_id: Option<&str>,
    until_id: Option<&str>,
) -> Check<Response> {
    let rows = mk_db::sqlx::query(
        r#"SELECT pl."id", p."id" AS "pageId", p."title", p."name", p."userId",
                  (p."visibility"::text) AS "visibility", p."likedCount"
           FROM "page_like" pl
           INNER JOIN "page" p ON p."id" = pl."pageId"
           WHERE pl."userId" = $1
             AND ($2::text IS NULL OR pl."id" < $2)
             AND ($3::text IS NULL OR pl."id" > $3)
           ORDER BY pl."id" DESC
           LIMIT $4"#,
    )
    .bind(me_id)
    .bind(until_id)
    .bind(since_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| db_err("i/pages/likes", e))?;

    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            use mk_db::sqlx::Row;
            let id: String = r.get("id");
            let page_id: String = r.get("pageId");
            json!({
                "id": id,
                "createdAt": created_at(&id),
                "page": {
                    "id": page_id,
                    "title": r.get::<String, _>("title"),
                    "name": r.get::<String, _>("name"),
                    "userId": r.get::<String, _>("userId"),
                    "visibility": r.get::<String, _>("visibility"),
                    "likedCount": r.get::<i32, _>("likedCount"),
                },
            })
        })
        .collect();
    Ok(ok_json(Value::Array(out)))
}
