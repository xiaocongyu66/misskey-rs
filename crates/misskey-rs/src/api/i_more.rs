//! The `i/*` remainder — slice 1: `i/favorites`, `i/gallery/likes`,
//! `i/pages/likes` (upstream `endpoints/i/{favorites,gallery/likes,pages/
//! likes}.ts`), plus slice 2: `i/flashs`, `i/flash/likes` (upstream
//! `endpoints/i/{flashs,flash/likes}.ts`) and `i/authorized-apps`
//! (`endpoints/i/authorized-apps.ts`). `i/page-likes` (upstream
//! `endpoints/i/page-likes.ts` — the actual upstream name) rides the same
//! `page_like` table with the full PageLikeEntityService.pack shape.
//!
//! All are `requireCredential: true` reads over rows owned by the caller:
//! - `i/favorites`       → `note_favorite` (id/userId/noteId; the 0001 baseline
//!   has no createdAt column — upstream derives it from the aid id, same as
//!   every other table here);
//! - `i/gallery/likes`   → `gallery_like` joined to `gallery_post`;
//! - `i/pages/likes`     → `page_like` joined to `page`;
//! - `i/flashs`          → `flash` rows where userId = caller;
//! - `i/flash/likes`     → `flash_like` joined to `flash`;
//! - `i/authorized-apps` → `app` rows where userId = caller.
//!
//! `i/pinned-users` is deliberately not implemented: upstream reads the
//! caller's `user.pinnedUsers` column, which the 0001 baseline does not
//! carry (the only `pinnedUsers` array in this schema is the instance-wide
//! `meta.pinnedUsers`, owned by the freq/admin lane). Per the slice brief the
//! `i/authorized-apps` fallback is ported instead.
//!
//! The paged endpoints mirror the upstream paramDef: `limit` (integer
//! 1..=100, default 10) + `sinceId`/`untilId` (misskey:id cursors, exclusive).
//! Rows order by id DESC (`id < untilId` / `id > sinceId`), the same
//! makePaginationQuery shape the other paged reads use. `i/authorized-apps`
//! takes no parameters upstream and returns the caller's apps id DESC.
//!
//! RESPONSE SHAPE: an array of entity-shaped objects. Minimal documented
//! fields: favorites `{ id, createdAt, noteId }`, gallery likes `{ id,
//! createdAt, post: { id, title, description, userId, fileIds,
//! isSensitive, likedCount, tags } }`, page likes `{ id, createdAt, page:
//! { id, title, name, userId, visibility, likedCount } }`, flashs `{ id,
//! createdAt, title, summary, script, visibility, userId, likedCount }`,
//! flash likes `{ id, createdAt, flash: <same Flash shape> }`, authorized
//! apps `{ id, name, callbackUrl, permission, secret }` (secret included
//! because the caller owns the app). Upstream packs the full
//! Note/GalleryPost/Page/Flash entities; the nested here is the identity
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
pub(crate) const ENDPOINTS: &[&str] = &[
    "i/favorites",
    "i/gallery/likes",
    "i/pages/likes",
    "i/page-likes",
    "i/flashs",
    "i/flash/likes",
    "i/authorized-apps",
];

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
        // requireCredential (upstream meta on every name routed here).
        let me_id = super::social::require_checked_user(pool, token.as_deref(), false).await?;
        // `i/authorized-apps` has an empty upstream paramDef (no paging).
        if name == "i/authorized-apps" {
            return authorized_apps(pool, &me_id).await;
        }
        let (limit, since_id, until_id) = paging(body)?;
        match name {
            "i/favorites" => {
                favorites(
                    pool,
                    &me_id,
                    limit,
                    since_id.as_deref(),
                    until_id.as_deref(),
                )
                .await
            }
            "i/gallery/likes" => {
                gallery_likes(
                    pool,
                    &me_id,
                    limit,
                    since_id.as_deref(),
                    until_id.as_deref(),
                )
                .await
            }
            "i/pages/likes" => {
                pages_likes(
                    pool,
                    &me_id,
                    limit,
                    since_id.as_deref(),
                    until_id.as_deref(),
                )
                .await
            }
            // the upstream name (endpoints/i/page-likes.ts) — same table as
            // the pre-existing `i/pages/likes` slice above, but the
            // PageLikeEntityService.pack shape: `{ id, page: <full Page
            // pack> }` with no createdAt on the like itself.
            "i/page-likes" => {
                page_likes(
                    state,
                    pool,
                    &me_id,
                    limit,
                    since_id.as_deref(),
                    until_id.as_deref(),
                )
                .await
            }
            "i/flashs" => {
                flashs(
                    pool,
                    &me_id,
                    limit,
                    since_id.as_deref(),
                    until_id.as_deref(),
                )
                .await
            }
            "i/flash/likes" => {
                flash_likes(
                    pool,
                    &me_id,
                    limit,
                    since_id.as_deref(),
                    until_id.as_deref(),
                )
                .await
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
        Some(Value::String(s)) if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric()) => {
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

/// `i/page-likes` (endpoints/i/page-likes.ts): `page_like` rows of the caller
/// (makePaginationQuery, id DESC), each packed as
/// `{ id, page: PageEntityService.pack(page, me) }` via
/// `pages_more::pack_page` (full pack incl. user/eyeCatchingImage + isLiked).
/// A like whose page row vanished answers INTERNAL_ERROR like upstream's
/// `findOneByOrFail` inside the entity pack (unreachable under the FK).
async fn page_likes(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    me_id: &str,
    limit: i64,
    since_id: Option<&str>,
    until_id: Option<&str>,
) -> Check<Response> {
    let rows = mk_db::sqlx::query(
        r#"SELECT "id", "pageId"
           FROM "page_like"
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
    .map_err(|e| db_err("i/page-likes", e))?;

    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        use mk_db::sqlx::Row;
        let id: String = r.get("id");
        let page_id: String = r.get("pageId");
        let page = match mk_db::page::find_page(pool, &page_id).await {
            Ok(Some(row)) => row,
            Ok(None) => return Err(internal()),
            Err(e) => return Err(db_err("i/page-likes find_page", e)),
        };
        let packed = super::pages_more::pack_page(state, pool, &page, Some(me_id)).await?;
        out.push(json!({ "id": id, "page": packed }));
    }
    Ok(ok_json(Value::Array(out)))
}

/// FlashEntityService.pack identity subset (see the module docs). The id is
/// passed in so the joined `i/flash/likes` query can reuse this with its
/// aliased `flashId`.
fn flash_json(r: &mk_db::sqlx::postgres::PgRow, id: String) -> Value {
    use mk_db::sqlx::Row;
    json!({
        "id": id,
        "createdAt": created_at(&id),
        "title": r.get::<String, _>("title"),
        "summary": r.get::<String, _>("summary"),
        "script": r.get::<String, _>("script"),
        "visibility": r.get::<String, _>("visibility"),
        "userId": r.get::<String, _>("userId"),
        "likedCount": r.get::<i32, _>("likedCount"),
    })
}

/// `endpoints/i/flashs.ts` — Flashs.find({ userId: me.id }), id DESC.
async fn flashs(
    pool: &mk_db::sqlx::PgPool,
    me_id: &str,
    limit: i64,
    since_id: Option<&str>,
    until_id: Option<&str>,
) -> Check<Response> {
    let rows = mk_db::sqlx::query(
        r#"SELECT "id", "title", "summary", "script", "visibility", "userId",
                  "likedCount"
           FROM "flash"
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
    .map_err(|e| db_err("i/flashs", e))?;

    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            use mk_db::sqlx::Row;
            let id: String = r.get("id");
            flash_json(r, id)
        })
        .collect();
    Ok(ok_json(Value::Array(out)))
}

/// `endpoints/i/flash/likes.ts` — FlashLikes.find({ userId: me.id }) joined
/// to the liked flash, id DESC.
async fn flash_likes(
    pool: &mk_db::sqlx::PgPool,
    me_id: &str,
    limit: i64,
    since_id: Option<&str>,
    until_id: Option<&str>,
) -> Check<Response> {
    let rows = mk_db::sqlx::query(
        r#"SELECT fl."id", f."id" AS "flashId", f."title", f."summary",
                  f."script", f."visibility", f."userId", f."likedCount"
           FROM "flash_like" fl
           INNER JOIN "flash" f ON f."id" = fl."flashId"
           WHERE fl."userId" = $1
             AND ($2::text IS NULL OR fl."id" < $2)
             AND ($3::text IS NULL OR fl."id" > $3)
           ORDER BY fl."id" DESC
           LIMIT $4"#,
    )
    .bind(me_id)
    .bind(until_id)
    .bind(since_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| db_err("i/flash/likes", e))?;

    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            use mk_db::sqlx::Row;
            let id: String = r.get("id");
            let flash_id: String = r.get("flashId");
            json!({
                "id": id,
                "createdAt": created_at(&id),
                "flash": flash_json(r, flash_id),
            })
        })
        .collect();
    Ok(ok_json(Value::Array(out)))
}

/// `endpoints/i/authorized-apps.ts` — Apps.find({ userId: me.id }) id DESC,
/// AppEntityService.pack (secret is included because the caller is the
/// owner).
async fn authorized_apps(pool: &mk_db::sqlx::PgPool, me_id: &str) -> Check<Response> {
    let rows = mk_db::sqlx::query(
        r#"SELECT "id", "name", "callbackUrl", "permission", "secret"
           FROM "app"
           WHERE "userId" = $1
           ORDER BY "id" DESC"#,
    )
    .bind(me_id)
    .fetch_all(pool)
    .await
    .map_err(|e| db_err("i/authorized-apps", e))?;

    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            use mk_db::sqlx::Row;
            json!({
                "id": r.get::<String, _>("id"),
                "name": r.get::<String, _>("name"),
                "callbackUrl": r.get::<Option<String>, _>("callbackUrl"),
                "permission": r.get::<Vec<String>, _>("permission"),
                "secret": r.get::<String, _>("secret"),
            })
        })
        .collect();
    Ok(ok_json(Value::Array(out)))
}
