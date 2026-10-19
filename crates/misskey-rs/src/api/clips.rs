//! Clips endpoints — ports of `endpoints/clips/{create,list,show,update,
//! delete,favorite,unfavorite,my-favorites,notes,add-note,remove-note}.ts`
//! plus `endpoints/users/clips.ts`, funneling through `core/ClipService.ts`
//! (create/update/delete/addNote/removeNote) + `core/entities/
//! ClipEntityService.ts` (pack). Self-contained module on the
//! announcement/channel pattern: the `// clips-agent` marked dispatch arm in
//! mod.rs parses the body, extracts the token and probes a declared
//! credential; this file owns the requireCredential gates and the endpoint
//! bodies. Db paths live in `mk_db::clips` (tables ship in the 0001
//! baseline; no migration).
//!
//! CREDENTIAL SHAPES: everything except clips/show, clips/notes and
//! users/clips is `requireCredential: true` (the kind scopes read:account /
//! write:account / read:clip-favorite / write:clip-favorite collapse into the
//! standard CREDENTIAL_REQUIRED / AUTHENTICATION_FAILED /
//! YOUR_ACCOUNT_SUSPENDED chain via social::require_checked_user — the e2e
//! clients assert only code/message/shape). The guest-ok trio resolves an
//! OPTIONAL me: auth runs before paramDef validation upstream, so `{}` on a
//! write endpoint answers CREDENTIAL_REQUIRED, not INVALID_PARAM.
//!
//! PARAM SHAPE (endpoint-base.ts L20): ajv registers `misskey:id` as plain
//! `/^[a-zA-Z0-9]+$/`, so malformed ids like 'xxxxxx' PASS validation and
//! fall through to the service checks (the e2e legs assert NO_SUCH_* code/id
//! overrides there); only null/absent/non-string/non-alnum params answer
//! INVALID_PARAM with the ajv-validate id 3d81ceae-475f-4600-b2a8-2bc116157532
//! (ApiCallService's 0b5f1631 id belongs to the GET-cast branch, not this
//! path). Status mapping: the meta ApiErrors here carry no httpStatusCode →
//! kind 'client' defaults to 400; void handlers answer 204 NO_CONTENT.
//!
//! PACK (ClipEntityService.pack): { id, createdAt (from the aid),
//! lastClippedAt, userId, user (UserLite), name, description, isPublic,
//! favoritedCount } + `isFavorited` only when me is present +
//! `notesCount` only when me is the clip owner (the e2e asserts the
//! unauthenticated users/clips view must LACK the isFavorited key).

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// The endpoint names routed into this module (single source of truth for the
/// `// clips-agent` dispatch arm in mod.rs).
pub(crate) const CLIP_ENDPOINTS: &[&str] = &[
    "clips/create",
    "clips/list",
    "clips/show",
    "clips/update",
    "clips/delete",
    "clips/favorite",
    "clips/unfavorite",
    "clips/my-favorites",
    "clips/notes",
    "clips/add-note",
    "clips/remove-note",
    // users/clips is endpoints/users/clips.ts (a user's public clips) — not
    // owned by the users/show lane, routed here like upstream's file layout
    "users/clips",
];

pub(crate) fn handles(name: &str) -> bool {
    CLIP_ENDPOINTS.contains(&name)
}

/// Names behind requireCredential (guest-ok: clips/show, clips/notes,
/// users/clips — meta.requireCredential: false).
const REQUIRES_CREDENTIAL: &[&str] = &[
    "clips/create",
    "clips/list",
    "clips/update",
    "clips/delete",
    "clips/favorite",
    "clips/unfavorite",
    "clips/my-favorites",
    "clips/add-note",
    "clips/remove-note",
];

/// Entry from the `// clips-agent` dispatch arm in mod.rs (body pre-parsed,
/// token + credential-declared pre-extracted).
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
    let result: Check<Response> = async {
        // Auth runs BEFORE paramDef validation upstream (ApiCallService.call
        // authenticates, then endpoint-base validates): `{}` on a write
        // endpoint answers CREDENTIAL_REQUIRED, and the guest-ok trio
        // resolves an optional me (declared-but-invalid → 401, the
        // announcement.rs public-path decision).
        let me_id = if REQUIRES_CREDENTIAL.contains(&name) || token.is_some() || credential_declared
        {
            Some(
                super::social::require_checked_user(pool, token.as_deref(), credential_declared)
                    .await?,
            )
        } else {
            None
        };
        match name {
            "clips/create" => clips_create(state, pool, body, me_id.as_deref().unwrap()).await,
            "clips/list" => clips_list(state, pool, body, me_id.as_deref().unwrap()).await,
            "clips/show" => clips_show(state, pool, body, me_id.as_deref()).await,
            "clips/update" => clips_update(state, pool, body, me_id.as_deref().unwrap()).await,
            "clips/delete" => clips_delete(pool, body, me_id.as_deref().unwrap()).await,
            "clips/favorite" => clips_favorite(pool, body, me_id.as_deref().unwrap()).await,
            "clips/unfavorite" => clips_unfavorite(pool, body, me_id.as_deref().unwrap()).await,
            "clips/my-favorites" => {
                clips_my_favorites(state, pool, me_id.as_deref().unwrap()).await
            }
            "clips/notes" => clips_notes(state, pool, body, me_id.as_deref()).await,
            "clips/add-note" => clips_add_note(pool, body, me_id.as_deref().unwrap()).await,
            "clips/remove-note" => clips_remove_note(pool, body, me_id.as_deref().unwrap()).await,
            "users/clips" => users_clips(state, pool, body, me_id.as_deref()).await,
            _ => Ok(api_err(ApiError::unknown_endpoint())),
        }
    }
    .await;
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

// --- shared shapes ---

type Check<T = Response> = Result<T, Response>;

/// Plain handler Error → ApiCallService.#onExecError → 500 INTERNAL_ERROR.
fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_clips", error = %e, "db error: {context}");
    internal()
}

/// Meta error without httpStatusCode: kind 'client' → default 400.
fn meta_err(code: &str, id: &str, message: &str) -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        code,
        id,
        message,
    ))
}

fn ok_empty() -> Response {
    // upstream `send(reply, undefined)` → 204
    StatusCode::NO_CONTENT.into_response()
}

fn ok_json(v: Value) -> Response {
    (StatusCode::OK, Json(v)).into_response()
}

// --- paramDef validation (ajv flavour) ---

/// endpoint-base.ts L63: the ajv validate() branch of ApiError (the 0b5f1631
/// id belongs to ApiCallService's GET-cast branch, not this path).
fn schema_invalid(param: &str, reason: &str) -> Response {
    api_err(
        ApiError::new(
            400,
            mk_types::api_error::ApiErrorKind::Client,
            "INVALID_PARAM",
            "3d81ceae-475f-4600-b2a8-2bc116157532",
            "Invalid param.",
        )
        .with_info(json!({
            "param": param,
            "reason": reason,
        })),
    )
}

/// `{ type: 'string', format: 'misskey:id' }` — ajv registers the format as
/// `/^[a-zA-Z0-9]+$/` (endpoint-base.ts L20), so 'xxxxxx' is VALID and the
/// service checks raise; null/absent/type-mismatch answers INVALID_PARAM.
fn id_param<'a>(body: &'a Value, key: &str) -> Result<&'a str, Response> {
    match body.get(key) {
        Some(Value::String(s)) if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric()) => {
            Ok(s.as_str())
        }
        _ => Err(schema_invalid(key, "must be a misskey:id (alnum)")),
    }
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

/// makePaginationQuery id-or-date fallback (QueryService via the lists.rs
/// precedent): the id form wins; a date falls to `idService.gen(date)` — a 0
/// date is falsy upstream and resolves to no bound.
fn pager_ids(body: &Value) -> Result<(Option<String>, Option<String>), Response> {
    let id_or_date = |key: &str, date_key: &str| -> Result<Option<String>, Response> {
        match body.get(key) {
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(schema_invalid(key, "must be a misskey:id (alnum)")),
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

/// `{ type: 'string', minLength: 1, maxLength: 100 }` — required (create) or
/// optional (update, None = keep). Returns Ok(None) on absence when optional.
fn name_param(body: &Value, required: bool) -> Result<Option<String>, Response> {
    match body.get("name") {
        Some(Value::String(s)) if !s.is_empty() && s.chars().count() <= 100 => Ok(Some(s.clone())),
        Some(Value::String(_)) => Err(schema_invalid("name", "must be 1..100 characters")),
        Some(_) => Err(schema_invalid("name", "must be a string")),
        None if required => Err(schema_invalid("name", "must be 1..100 characters")),
        None => Ok(None),
    }
}

/// `{ type: 'boolean', [default: false] }` — None on absence; null rejects
/// (neither schema is nullable).
fn is_public_param(body: &Value, dflt: Option<bool>) -> Result<Option<bool>, Response> {
    match body.get("isPublic") {
        None => Ok(dflt),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(schema_invalid("isPublic", "must be a boolean")),
    }
}

/// `{ type: 'string', nullable: true, maxLength: 2048 }` — tri-state:
/// Ok(None) = absent (keep on update; create normalizes to NULL),
/// Ok(Some(None)) = present null or '' → NULL column (upstream
/// `ps.description || null`), Ok(Some(Some(s))) = value.
fn description_param(body: &Value) -> Result<Option<Option<String>>, Response> {
    match body.get("description") {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(s)) if s.chars().count() <= 2048 => Ok(Some(if s.is_empty() {
            None // `ps.description || null` — the empty string normalizes
        } else {
            Some(s.clone())
        })),
        Some(_) => Err(schema_invalid(
            "description",
            "must be a string or null (<= 2048 characters)",
        )),
    }
}

// ---------------------------------------------------------------------------
// ClipEntityService.pack
// ---------------------------------------------------------------------------

async fn pack_clip(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    row: &mk_db::clips::ClipRow,
    me_id: Option<&str>,
) -> Result<Value, Response> {
    let user_lite = super::note_user_lite(state, pool, &row.user_id).await?;
    let created_at = mk_types::id::parse_aid_date(&row.id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default();
    let favorited_count = mk_db::clips::favorite_count(pool, &row.id)
        .await
        .map_err(|e| db_err("pack_clip favorite_count", e))?;
    let mut v = json!({
        "id": row.id,
        "createdAt": created_at,
        "lastClippedAt": row
            .last_clipped_at_ms
            .map(mk_core::services::signup::ms_to_iso),
        "userId": row.user_id,
        "user": user_lite,
        "name": row.name,
        "description": row.description,
        "isPublic": row.is_public,
        "favoritedCount": favorited_count,
    });
    let obj = v.as_object_mut().expect("pack_clip object");
    // isFavorited: exists({ clipId, userId: meId }) — present only with a me
    if let Some(me) = me_id {
        let favorited = mk_db::clips::is_favorited(pool, &row.id, me)
            .await
            .map_err(|e| db_err("pack_clip is_favorited", e))?;
        obj.insert("isFavorited".to_string(), Value::Bool(favorited));
        // notesCount: countBy({ clipId }) — owner-gated (meId === clip.userId)
        if me == row.user_id {
            let notes_count = mk_db::clips::count_clip_notes(pool, &row.id)
                .await
                .map_err(|e| db_err("pack_clip count_clip_notes", e))?;
            obj.insert("notesCount".to_string(), json!(notes_count));
        }
    }
    Ok(v)
}

async fn pack_clips(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    rows: &[mk_db::clips::ClipRow],
    me_id: Option<&str>,
) -> Result<Vec<Value>, Response> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(pack_clip(state, pool, row, me_id).await?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// clips/create — create.ts + ClipService.create
// ---------------------------------------------------------------------------

async fn clips_create(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let name = name_param(body, true)?.expect("required name validated");
    let is_public = is_public_param(body, Some(false))?.unwrap_or(false);
    // create folds the tri-state to the column value (absent/null/'' → NULL)
    let description: Option<String> = description_param(body)?.flatten();
    // RoleService.getUserPolicies(me).clipLimit — DEFAULT_POLICIES value (10);
    // no per-user role overrides ride in this slice (lists.rs precedent).
    let count = mk_db::clips::count_clips_owned(pool, me_id)
        .await
        .map_err(|e| db_err("clips_create count", e))?;
    if count >= 10 {
        return Err(meta_err(
            "TOO_MANY_CLIPS",
            "920f7c2d-6208-4b76-8082-e632020f5883",
            "You cannot create clip any more.",
        ));
    }
    let id = mk_types::id::gen_aid_now();
    mk_db::clips::insert_clip(pool, &id, me_id, &name, is_public, description.as_deref())
        .await
        .map_err(|e| db_err("clips_create insert", e))?;
    let row = mk_db::clips::find_clip(pool, &id)
        .await
        .map_err(|e| db_err("clips_create refetch", e))?
        .ok_or_else(internal)?;
    Ok(ok_json(pack_clip(state, pool, &row, Some(me_id)).await?))
}

// ---------------------------------------------------------------------------
// clips/list — list.ts (requireCredential: always the OWNER's clips)
// ---------------------------------------------------------------------------

async fn clips_list(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let limit = limit_param(body)?;
    let (since_id, until_id) = pager_ids(body)?;
    let rows = mk_db::clips::list_clips(
        pool,
        me_id,
        since_id.as_deref(),
        until_id.as_deref(),
        limit,
        false,
    )
    .await
    .map_err(|e| db_err("clips_list list_clips", e))?;
    let packed = pack_clips(state, pool, &rows, Some(me_id)).await?;
    Ok(ok_json(json!(packed)))
}

// ---------------------------------------------------------------------------
// clips/show — show.ts (guest-ok; private clips answer NO_SUCH_CLIP to
// non-owners / guests)
// ---------------------------------------------------------------------------

/// `!clip.isPublic && (me == null || clip.userId !== me.id)` — shared by
/// show (c3c5fe33) and notes (1d7645e6) with their own error ids.
fn private_clip_gate(
    row: &mk_db::clips::ClipRow,
    me_id: Option<&str>,
    id: &str,
) -> Option<Response> {
    if !row.is_public {
        let owner_viewed = me_id.is_some_and(|me| me == row.user_id);
        if !owner_viewed {
            return Some(meta_err("NO_SUCH_CLIP", id, "No such clip."));
        }
    }
    None
}

async fn clips_show(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: Option<&str>,
) -> Result<Response, Response> {
    let clip_id = id_param(body, "clipId")?;
    let row = mk_db::clips::find_clip(pool, clip_id)
        .await
        .map_err(|e| db_err("clips_show find_clip", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_CLIP",
                "c3c5fe33-d62c-44d2-9ea5-d997703f5c20",
                "No such clip.",
            )
        })?;
    if let Some(resp) = private_clip_gate(&row, me_id, "c3c5fe33-d62c-44d2-9ea5-d997703f5c20") {
        return Err(resp);
    }
    Ok(ok_json(pack_clip(state, pool, &row, me_id).await?))
}

// ---------------------------------------------------------------------------
// clips/update — update.ts + ClipService.update (owner-scoped; undefined
// params are TypeORM no-ops; `description || null` in the endpoint)
// ---------------------------------------------------------------------------

async fn clips_update(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let clip_id = id_param(body, "clipId")?;
    let name = name_param(body, false)?;
    let is_public = is_public_param(body, None)?;
    let description = description_param(body)?;
    let delta = mk_db::clips::ClipUpdate {
        name,
        is_public,
        description,
    };
    let row = mk_db::clips::find_clip_owned(pool, clip_id, me_id)
        .await
        .map_err(|e| db_err("clips_update find_clip_owned", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_CLIP",
                "b4d92d70-b216-46fa-9a3f-a8c811699257",
                "No such clip.",
            )
        })?;
    mk_db::clips::update_clip(pool, &row.id, &delta)
        .await
        .map_err(|e| db_err("clips_update update_clip", e))?;
    // `pack(ps.clipId, me)` — refetch by id (same row the owner verified)
    let fresh = mk_db::clips::find_clip(pool, clip_id)
        .await
        .map_err(|e| db_err("clips_update refetch", e))?
        .ok_or_else(internal)?;
    Ok(ok_json(pack_clip(state, pool, &fresh, Some(me_id)).await?))
}

// ---------------------------------------------------------------------------
// clips/delete — delete.ts + ClipService.delete (owner-scoped, 204)
// ---------------------------------------------------------------------------

async fn clips_delete(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let clip_id = id_param(body, "clipId")?;
    let row = mk_db::clips::find_clip_owned(pool, clip_id, me_id)
        .await
        .map_err(|e| db_err("clips_delete find_clip_owned", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_CLIP",
                "70ca08ba-6865-4630-b6fb-8494759aa754",
                "No such clip.",
            )
        })?;
    mk_db::clips::delete_clip(pool, &row.id)
        .await
        .map_err(|e| db_err("clips_delete delete_clip", e))?;
    Ok(ok_empty())
}

// ---------------------------------------------------------------------------
// clips/favorite — favorite.ts (any clip visible to me; unique per
// (clipId, userId) → ALREADY_FAVORITED)
// ---------------------------------------------------------------------------

async fn clips_favorite(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let clip_id = id_param(body, "clipId")?;
    let row = mk_db::clips::find_clip(pool, clip_id)
        .await
        .map_err(|e| db_err("clips_favorite find_clip", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_CLIP",
                "4c2aaeae-80d8-4250-9606-26cb1fdb77a5",
                "No such clip.",
            )
        })?;
    // `clip.userId !== me.id && !clip.isPublic` → NO_SUCH_CLIP (a private
    // clip cannot be favored by a non-owner; a public one can)
    if !row.is_public && row.user_id != me_id {
        return Err(meta_err(
            "NO_SUCH_CLIP",
            "4c2aaeae-80d8-4250-9606-26cb1fdb77a5",
            "No such clip.",
        ));
    }
    if mk_db::clips::find_favorite_id(pool, &row.id, me_id)
        .await
        .map_err(|e| db_err("clips_favorite find_favorite_id", e))?
        .is_some()
    {
        return Err(meta_err(
            "ALREADY_FAVORITED",
            "92658936-c625-4273-8326-2d790129256e",
            "The clip has already been favorited.",
        ));
    }
    mk_db::clips::insert_favorite(pool, &mk_types::id::gen_aid_now(), &row.id, me_id)
        .await
        .map_err(|e| db_err("clips_favorite insert", e))?;
    Ok(ok_empty())
}

// ---------------------------------------------------------------------------
// clips/unfavorite — unfavorite.ts (row-missing → NOT_FAVORITED; unlike
// favorite.ts there is NO visibility gate on the clip lookup)
// ---------------------------------------------------------------------------

async fn clips_unfavorite(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let clip_id = id_param(body, "clipId")?;
    let row = mk_db::clips::find_clip(pool, clip_id)
        .await
        .map_err(|e| db_err("clips_unfavorite find_clip", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_CLIP",
                "2603966e-b865-426c-94a7-af4a01241dc1",
                "No such clip.",
            )
        })?;
    let Some(favorite_id) = mk_db::clips::find_favorite_id(pool, &row.id, me_id)
        .await
        .map_err(|e| db_err("clips_unfavorite find_favorite_id", e))?
    else {
        return Err(meta_err(
            "NOT_FAVORITED",
            "90c3a9e8-b321-4dae-bf57-2bf79bbcc187",
            "You have not favorited the clip.",
        ));
    };
    mk_db::clips::delete_favorite(pool, &favorite_id)
        .await
        .map_err(|e| db_err("clips_unfavorite delete", e))?;
    Ok(ok_empty())
}

// ---------------------------------------------------------------------------
// clips/my-favorites — my-favorites.ts (no pagination; all my favorites)
// ---------------------------------------------------------------------------

async fn clips_my_favorites(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    me_id: &str,
) -> Result<Response, Response> {
    let rows = mk_db::clips::list_favorited_clips(pool, me_id)
        .await
        .map_err(|e| db_err("clips_my_favorites list", e))?;
    let packed = pack_clips(state, pool, &rows, Some(me_id)).await?;
    Ok(ok_json(json!(packed)))
}

// ---------------------------------------------------------------------------
// clips/notes — notes.ts (guest-ok; private clips gate on me; the note pack
// runs the shared min-pipeline like the other mk-db note lists)
// ---------------------------------------------------------------------------

async fn clips_notes(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: Option<&str>,
) -> Result<Response, Response> {
    let clip_id = id_param(body, "clipId")?;
    let limit = limit_param(body)?;
    let (since_id, until_id) = pager_ids(body)?;
    let row = mk_db::clips::find_clip(pool, clip_id)
        .await
        .map_err(|e| db_err("clips_notes find_clip", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_CLIP",
                "1d7645e6-2b6d-4635-b0fe-fe22b0e72e00",
                "No such clip.",
            )
        })?;
    if let Some(resp) = private_clip_gate(&row, me_id, "1d7645e6-2b6d-4635-b0fe-fe22b0e72e00") {
        return Err(resp);
    }
    let rows = match me_id {
        Some(me) => {
            let followees = mk_db::social::followee_ids(pool, me)
                .await
                .map_err(|e| db_err("clips_notes followee_ids", e))?;
            mk_db::clips::clip_notes_for_me(
                pool,
                &row.id,
                me,
                &followees,
                since_id.as_deref(),
                until_id.as_deref(),
                limit,
            )
            .await
            .map_err(|e| db_err("clips_notes_for_me", e))?
        }
        None => mk_db::clips::clip_notes_for_guest(
            pool,
            &row.id,
            since_id.as_deref(),
            until_id.as_deref(),
            limit,
        )
        .await
        .map_err(|e| db_err("clips_notes_for_guest", e))?,
    };

    // Shared pack pipeline (notes/show / lists.rs pack_min shape)
    let mut out = Vec::with_capacity(rows.len());
    for note in &rows {
        let user_lite = super::note_user_lite(state, pool, &note.user_id).await?;
        let created_at = mk_types::id::parse_aid_date(&note.id)
            .map(mk_core::services::signup::ms_to_iso)
            .unwrap_or_default();
        out.push(mk_core::packers::note::note_pack_min(
            note,
            user_lite,
            created_at,
            &note.mentions,
            &note.tags,
            json!({}),
        ));
    }
    Ok(ok_json(json!(out)))
}

// ---------------------------------------------------------------------------
// clips/add-note — add-note.ts + ClipService.addNote (owner-scoped; count cap
// BEFORE insert; insert errors classify to ALREADY_CLIPPED / NO_SUCH_NOTE)
// ---------------------------------------------------------------------------

async fn clips_add_note(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let clip_id = id_param(body, "clipId")?;
    let note_id = id_param(body, "noteId")?;
    let row = mk_db::clips::find_clip_owned(pool, clip_id, me_id)
        .await
        .map_err(|e| db_err("clips_add_note find_clip_owned", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_CLIP",
                "d6e76cc0-a1b5-4c7c-a287-73fa9c716dcf",
                "No such clip.",
            )
        })?;
    // getUserPolicies(me).noteEachClipsLimit — DEFAULT_POLICIES value (200)
    let count = mk_db::clips::count_clip_notes(pool, &row.id)
        .await
        .map_err(|e| db_err("clips_add_note count", e))?;
    if count >= 200 {
        return Err(meta_err(
            "TOO_MANY_CLIP_NOTES",
            "f0dba960-ff73-4615-8df4-d6ac5d9dc118",
            "You cannot add notes to the clip any more.",
        ));
    }
    match mk_db::clips::insert_clip_note(pool, &mk_types::id::gen_aid_now(), &row.id, note_id).await
    {
        Ok(()) => {}
        Err(mk_db::clips::InsertClipNoteError::Duplicate) => {
            return Err(meta_err(
                "ALREADY_CLIPPED",
                "734806c4-542c-463a-9311-15c512803965",
                "The note has already been clipped.",
            ));
        }
        Err(mk_db::clips::InsertClipNoteError::NoSuchNote) => {
            return Err(meta_err(
                "NO_SUCH_NOTE",
                "fc8c0b49-c7a3-4664-a0a6-b418d386bb8b",
                "No such note.",
            ));
        }
        Err(mk_db::clips::InsertClipNoteError::Db(e)) => {
            return Err(db_err("clips_add_note insert", e));
        }
    }
    // Fire-and-forget upstream (unawaited promises): lastClippedAt bump +
    // the notes/show clippedCount counter
    mk_db::clips::set_last_clipped_at_now(pool, &row.id)
        .await
        .map_err(|e| db_err("clips_add_note last_clipped_at", e))?;
    mk_db::clips::bump_note_clipped_count(pool, note_id, 1)
        .await
        .map_err(|e| db_err("clips_add_note increment", e))?;
    Ok(ok_empty())
}

// ---------------------------------------------------------------------------
// clips/remove-note — remove-note.ts + ClipService.removeNote (owner-scoped;
// affected === 0 → NO_SUCH_NOTE; clippedCount decremented only on a hit)
// ---------------------------------------------------------------------------

async fn clips_remove_note(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let clip_id = id_param(body, "clipId")?;
    let note_id = id_param(body, "noteId")?;
    let row = mk_db::clips::find_clip_owned(pool, clip_id, me_id)
        .await
        .map_err(|e| db_err("clips_remove_note find_clip_owned", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_CLIP",
                "b80525c6-97f7-49d7-a42d-ebccd49cfd52",
                "No such clip.",
            )
        })?;
    let deleted = mk_db::clips::delete_clip_note(pool, &row.id, note_id)
        .await
        .map_err(|e| db_err("clips_remove_note delete", e))?;
    if !deleted {
        return Err(meta_err(
            "NO_SUCH_NOTE",
            "aff017de-190e-434b-893e-33a9ff5049d8",
            "No such note.",
        ));
    }
    mk_db::clips::bump_note_clipped_count(pool, note_id, -1)
        .await
        .map_err(|e| db_err("clips_remove_note decrement", e))?;
    Ok(ok_empty())
}

// ---------------------------------------------------------------------------
// users/clips — users/clips.ts (public clips of a user; userId required,
// unknown user → empty array like an empty query result)
// ---------------------------------------------------------------------------

async fn users_clips(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: Option<&str>,
) -> Result<Response, Response> {
    let user_id = id_param(body, "userId")?;
    let limit = limit_param(body)?;
    let (since_id, until_id) = pager_ids(body)?;
    let rows = mk_db::clips::list_clips(
        pool,
        user_id,
        since_id.as_deref(),
        until_id.as_deref(),
        limit,
        true,
    )
    .await
    .map_err(|e| db_err("users_clips list_clips", e))?;
    let packed = pack_clips(state, pool, &rows, me_id).await?;
    Ok(ok_json(json!(packed)))
}
