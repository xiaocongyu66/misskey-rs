//! notes/users/roles remainder slice — ports of upstream
//! `endpoints/notes/clips.ts`, `endpoints/notes/show-partial-bulk.ts`,
//! `endpoints/users/{flashs,gallery/posts,pages}.ts` and
//! `endpoints/roles/{list,show}.ts`.
//!
//! The per-user flash/gallery/page lanes reuse the packs owned by
//! `api::{flash_more,gallery_more,pages_more}` (now `pub(crate)`); notes/clips
//! reuses `api::clips::pack_clips` (ClipEntityService.pack). SQL lives in the
//! existing `mk_db::{clips,flash,gallery,page,roles}` modules (tables ship in
//! the 0001 baseline).
//!
//! Credential split: every name here is guest-ok at this slice's target
//! (`notes/clips`, `notes/show-partial-bulk`, `users/flashs`,
//! `users/gallery/posts`, `users/pages`, `roles/list`, `roles/show`). A
//! riding credential still authenticates and an invalid one still answers 401
//! (the ApiCallService auth-before-validation rule); roles/list accepts an
//! anonymous caller and pages on role id when the optional cursors are sent
//! (the local `endpoints/roles/list.ts` is requireCredential + unpaged — the
//! mission contract for this slice is guest-ok + paged, so that is what this
//! module implements).
//!
//! PARAM SHAPE: ajv registers `misskey:id` as `/^[a-zA-Z0-9]+$/`, so
//! malformed-but-alnum ids pass validation and fall through to the service
//! checks; only null/absent/non-string/non-alnum params answer INVALID_PARAM.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// Endpoint names routed into this module (single source of truth for the
/// `// notes-users2` dispatch arm in mod.rs).
pub(crate) const NOTES_USERS2_ENDPOINTS: &[&str] = &[
    "notes/clips",
    "notes/show-partial-bulk",
    "users/flashs",
    "users/gallery/posts",
    "users/pages",
    "roles/list",
    "roles/show",
    // slice 2
    "roles/users",
    "roles/notes",
    "users/achievements",
    "users/recommendation",
    "users/featured-notes",
    "following/requests/reject",
    "following/requests/sent",
    "following/update-all",
    "hashtags/search",
    "username/available",
    "email-address/available",
];

pub(crate) fn handles(name: &str) -> bool {
    NOTES_USERS2_ENDPOINTS.contains(&name)
}

type Check<T = Response> = Result<T, Response>;

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_notes_users2", error = %e, "db error: {context}");
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

fn ok_json(v: Value) -> Response {
    (StatusCode::OK, Json(v)).into_response()
}

fn ok_empty() -> Response {
    // upstream `send(reply, undefined)` → 204
    StatusCode::NO_CONTENT.into_response()
}

fn offset_param(body: &Value, dflt: i64) -> Result<i64, Response> {
    match body.get("offset") {
        None => Ok(dflt),
        Some(Value::Number(n)) => {
            let v = n
                .as_i64()
                .ok_or_else(|| schema_invalid("offset", "must be an integer"))?;
            if v < 0 {
                return Err(schema_invalid("offset", "must be >= 0"));
            }
            Ok(v)
        }
        Some(_) => Err(schema_invalid("offset", "must be an integer")),
    }
}

fn until_id_param(body: &Value) -> Result<Option<String>, Response> {
    match body.get("untilId") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(schema_invalid("untilId", "must be a misskey:id")),
    }
}

fn created_at(id: &str) -> String {
    mk_types::id::parse_aid_date(id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default()
}

fn id_param<'a>(body: &'a Value, key: &str) -> Result<&'a str, Response> {
    match body.get(key) {
        Some(Value::String(s)) if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric()) => {
            Ok(s.as_str())
        }
        _ => Err(schema_invalid(key, "must be a misskey:id (alnum)")),
    }
}

/// `{ type: 'integer', minimum: 1, maximum: 100, default: <dflt> }`.
fn limit_param(body: &Value, dflt: i64) -> Result<i64, Response> {
    match body.get("limit") {
        None => Ok(dflt),
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

/// makePaginationQuery id-or-date fallback (id wins; a 0 date is falsy and
/// resolves to no bound).
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

/// `noteIds`: array of 1..100 misskey:id values.
fn note_ids_param(body: &Value) -> Result<Vec<String>, Response> {
    let Some(Value::Array(items)) = body.get("noteIds") else {
        return Err(schema_invalid("noteIds", "required (array of misskey:id)"));
    };
    if items.is_empty() || items.len() > 100 {
        return Err(schema_invalid("noteIds", "must contain 1..100 items"));
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item.as_str() {
            Some(s) if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric()) => {
                out.push(s.to_string());
            }
            _ => return Err(schema_invalid("noteIds", "must be an array of misskey:id")),
        }
    }
    Ok(out)
}

/// RoleEntityService.pack (same shape as api::roles::pack_role).
fn pack_role(row: mk_db::roles::RolePackRow) -> Value {
    json!({
        "id": row.id,
        "createdAt": mk_types::id::parse_aid_date(&row.id)
            .map(mk_core::services::signup::ms_to_iso)
            .unwrap_or_else(|| mk_core::services::signup::ms_to_iso(row.updated_at_ms)),
        "updatedAt": mk_core::services::signup::ms_to_iso(row.updated_at_ms),
        "name": row.name,
        "description": row.description,
        "color": row.color,
        "iconUrl": row.icon_url,
        "target": row.target,
        "condFormula": row.cond_formula,
        "isPublic": row.is_public,
        "isAdministrator": row.is_administrator,
        "isModerator": row.is_moderator,
        "isExplorable": row.is_explorable,
        "asBadge": row.as_badge,
        "preserveAssignmentOnMoveAccount": row.preserve_assignment_on_move_account,
        "canEditMembersByModerator": row.can_edit_members_by_moderator,
        "displayOrder": row.display_order,
        "policies": mk_core::services::roles_v2::pack_policies(&row.policies),
        "usersCount": row.users_count,
    })
}

/// Entry from the `// notes-users2` dispatch arm in mod.rs (body pre-parsed,
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
    // Slice-2 names behind requireCredential; every other name is guest-ok
    // (a present credential still resolves, an invalid one still 401s).
    const REQUIRES_CREDENTIAL: &[&str] = &[
        "roles/notes",
        "users/recommendation",
        "following/requests/reject",
        "following/requests/sent",
        "following/update-all",
    ];
    let me_id = if REQUIRES_CREDENTIAL.contains(&name) || token.is_some() || credential_declared {
        match super::social::require_checked_user(pool, token.as_deref(), credential_declared).await
        {
            Ok(id) => Some(id),
            Err(resp) => return resp,
        }
    } else {
        None
    };

    let result: Check<Response> = match name {
        "notes/clips" => notes_clips(state, pool, body, me_id.as_deref()).await,
        "notes/show-partial-bulk" => notes_show_partial_bulk(pool, body).await,
        "users/flashs" => users_flashs(state, pool, body).await,
        "users/gallery/posts" => users_gallery_posts(state, pool, body, me_id.as_deref()).await,
        "users/pages" => users_pages(state, pool, body).await,
        "roles/list" => roles_list(pool, body).await,
        "roles/show" => roles_show(pool, body).await,
        "roles/users" => roles_users(state, pool, body, me_id.as_deref()).await,
        "roles/notes" => roles_notes(state, pool, body, me_id.as_deref().unwrap()).await,
        "users/achievements" => users_achievements(pool, body).await,
        "users/recommendation" => {
            users_recommendation(state, pool, body, me_id.as_deref().unwrap()).await
        }
        "users/featured-notes" => users_featured_notes(state, pool, body, me_id.as_deref()).await,
        "following/requests/reject" => {
            following_requests_reject(pool, body, me_id.as_deref().unwrap()).await
        }
        "following/requests/sent" => {
            following_requests_sent(state, pool, body, me_id.as_deref().unwrap()).await
        }
        "following/update-all" => following_update_all(pool, body, me_id.as_deref().unwrap()).await,
        "hashtags/search" => hashtags_search(pool, body).await,
        "username/available" => username_available(pool, body).await,
        "email-address/available" => email_address_available(pool, body).await,
        _ => Ok(api_err(ApiError::unknown_endpoint())),
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

// ---------------------------------------------------------------------------
// notes/clips — public clips containing the note
// ---------------------------------------------------------------------------

async fn notes_clips(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: Option<&str>,
) -> Check<Response> {
    let note_id = id_param(body, "noteId")?;
    let note = mk_db::note::find_note_by_id(pool, note_id)
        .await
        .map_err(|e| db_err("notes_clips find_note_by_id", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_NOTE",
                "47db1a1c-b0af-458d-8fb4-986e4efafe1e",
                "No such note.",
            )
        })?;
    let rows = mk_db::clips::public_clips_for_note(pool, &note.id)
        .await
        .map_err(|e| db_err("notes_clips public_clips_for_note", e))?;
    let packed = super::clips::pack_clips(state, pool, &rows, me_id).await?;
    Ok(ok_json(json!(packed)))
}

// ---------------------------------------------------------------------------
// notes/show-partial-bulk — minimal packs for up to 100 note ids
// ---------------------------------------------------------------------------

async fn notes_show_partial_bulk(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let note_ids = note_ids_param(body)?;
    let mut out = Vec::with_capacity(note_ids.len());
    for note_id in note_ids {
        // The bulk query drops ids that do not resolve (no per-id error).
        let Some(note) = mk_db::note::find_note_by_id(pool, &note_id)
            .await
            .map_err(|e| db_err("show_partial_bulk find_note_by_id", e))?
        else {
            continue;
        };
        out.push(json!({
            "id": note.id,
            "text": note.text,
            "createdAt": created_at(&note.id),
            "userId": note.user_id,
        }));
    }
    Ok(ok_json(json!(out)))
}

// ---------------------------------------------------------------------------
// users/flashs — public flashes of a user (packed without me)
// ---------------------------------------------------------------------------

async fn users_flashs(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
) -> Check<Response> {
    let user_id = id_param(body, "userId")?;
    let limit = limit_param(body, 10)?;
    let (since_id, until_id) = pager_ids(body)?;
    let rows = mk_db::flash::list_by_user(
        pool,
        user_id,
        since_id.as_deref(),
        until_id.as_deref(),
        limit,
    )
    .await
    .map_err(|e| db_err("users_flashs list_by_user", e))?;
    let packed = super::flash_more::pack_many(state, pool, &rows, None).await?;
    Ok(ok_json(packed))
}

// ---------------------------------------------------------------------------
// users/gallery/posts — gallery posts of a user (packed with me)
// ---------------------------------------------------------------------------

async fn users_gallery_posts(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: Option<&str>,
) -> Check<Response> {
    let user_id = id_param(body, "userId")?;
    let limit = limit_param(body, 10)?;
    let (since_id, until_id) = pager_ids(body)?;
    let rows = mk_db::gallery::list_by_user(
        pool,
        user_id,
        since_id.as_deref(),
        until_id.as_deref(),
        limit,
    )
    .await
    .map_err(|e| db_err("users_gallery_posts list_by_user", e))?;
    let packed = super::gallery_more::pack_many(state, pool, &rows, me_id).await?;
    Ok(ok_json(packed))
}

// ---------------------------------------------------------------------------
// users/pages — public pages of a user (packed without me)
// ---------------------------------------------------------------------------

async fn users_pages(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
) -> Check<Response> {
    let user_id = id_param(body, "userId")?;
    let limit = limit_param(body, 10)?;
    let (since_id, until_id) = pager_ids(body)?;
    let rows = mk_db::page::list_by_user(
        pool,
        user_id,
        since_id.as_deref(),
        until_id.as_deref(),
        limit,
    )
    .await
    .map_err(|e| db_err("users_pages list_by_user", e))?;
    let packed = super::pages_more::pack_many(state, pool, &rows, None).await?;
    Ok(ok_json(packed))
}

// ---------------------------------------------------------------------------
// roles/list — public + explorable roles, optionally paged on role id
// ---------------------------------------------------------------------------

async fn roles_list(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let limit = limit_param(body, 100)?;
    let (since_id, until_id) = pager_ids(body)?;
    let rows = mk_db::roles::list_public_explorable_role_packs(
        pool,
        since_id.as_deref(),
        until_id.as_deref(),
        limit,
    )
    .await
    .map_err(|e| db_err("roles_list list_public_explorable", e))?;
    let packed: Vec<Value> = rows.into_iter().map(pack_role).collect();
    Ok(ok_json(json!(packed)))
}

// ---------------------------------------------------------------------------
// roles/show — one public role
// ---------------------------------------------------------------------------

async fn roles_show(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let role_id = id_param(body, "roleId")?;
    let row = mk_db::roles::find_role_pack(pool, role_id)
        .await
        .map_err(|e| db_err("roles_show find_role_pack", e))?
        .filter(|r| r.is_public)
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_ROLE",
                "de5502bf-009a-4639-86c1-fec349e46dcb",
                "No such role.",
            )
        })?;
    Ok(ok_json(pack_role(row)))
}

// ---------------------------------------------------------------------------
// roles/users — live assignments of a public+explorable role, UserDetailed
// ---------------------------------------------------------------------------

async fn roles_users(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: Option<&str>,
) -> Check<Response> {
    let role_id = id_param(body, "roleId")?;
    let role = mk_db::roles::find_role_pack(pool, role_id)
        .await
        .map_err(|e| db_err("roles_users find_role_pack", e))?
        .filter(|r| r.is_public && r.is_explorable)
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_ROLE",
                "30aaaee3-4792-48dc-ab0d-cf501a575ac5",
                "No such role.",
            )
        })?;
    let limit = limit_param(body, 10)?;
    let (since_id, until_id) = pager_ids(body)?;
    let rows = mk_db::roles::role_assignments_page(
        pool,
        &role.id,
        since_id.as_deref(),
        until_id.as_deref(),
        limit,
    )
    .await
    .map_err(|e| db_err("roles_users role_assignments_page", e))?;
    let mut out = Vec::with_capacity(rows.len());
    for assign in rows {
        if let Some(user) =
            super::freq::pack_user_detailed(state, pool, &assign.user_id, me_id).await?
        {
            out.push(json!({ "id": assign.id, "user": user }));
        }
    }
    Ok(ok_json(json!(out)))
}

// ---------------------------------------------------------------------------
// roles/notes — public notes by holders of the role
// ---------------------------------------------------------------------------

async fn roles_notes(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Check<Response> {
    let role_id = id_param(body, "roleId")?;
    let role = mk_db::roles::find_role_pack(pool, role_id)
        .await
        .map_err(|e| db_err("roles_notes find_role_pack", e))?
        .filter(|r| r.is_public)
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_ROLE",
                "eb70323a-df61-4dd4-ad90-89c83c7cf26e",
                "No such role.",
            )
        })?;
    if !role.is_explorable {
        return Ok(ok_json(json!([])));
    }
    let limit = limit_param(body, 10)?;
    let (since_id, until_id) = pager_ids(body)?;
    let rows = mk_db::note::role_timeline_notes(
        pool,
        &role.id,
        since_id.as_deref(),
        until_id.as_deref(),
        limit,
    )
    .await
    .map_err(|e| db_err("roles_notes role_timeline_notes", e))?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        match super::pack_note_for(state, pool, &row.id, Some(me_id)).await {
            Ok(v) => out.push(v),
            Err(r) => return Err(*r),
        }
    }
    Ok(ok_json(json!(out)))
}

// ---------------------------------------------------------------------------
// users/achievements — profile.achievements (findOneByOrFail → 500)
// ---------------------------------------------------------------------------

async fn users_achievements(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let user_id = id_param(body, "userId")?;
    match mk_db::user::find_profile(pool, user_id).await {
        Ok(profile) => Ok(ok_json(profile.achievements)),
        Err(_) => Err(internal()),
    }
}

// ---------------------------------------------------------------------------
// users/recommendation — local explorable unlocked users not yet followed
// ---------------------------------------------------------------------------

async fn users_recommendation(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Check<Response> {
    let limit = limit_param(body, 10)?;
    let offset = offset_param(body, 0)?;
    let rows = mk_db::user::recommend_users(pool, me_id, limit, offset)
        .await
        .map_err(|e| db_err("users_recommendation", e))?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        if let Some(user) =
            super::freq::pack_user_detailed(state, pool, &row.id, Some(me_id)).await?
        {
            out.push(user);
        }
    }
    Ok(ok_json(json!(out)))
}

// ---------------------------------------------------------------------------
// users/featured-notes — the user's public notes (no Redis ranking here)
// ---------------------------------------------------------------------------

async fn users_featured_notes(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: Option<&str>,
) -> Check<Response> {
    let user_id = id_param(body, "userId")?;
    let limit = limit_param(body, 10)?;
    let until_id = until_id_param(body)?;
    let rows = mk_db::note::user_public_notes(pool, user_id, until_id.as_deref(), limit)
        .await
        .map_err(|e| db_err("users_featured_notes", e))?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        match super::pack_note_for(state, pool, &row.id, me_id).await {
            Ok(v) => out.push(v),
            Err(r) => return Err(*r),
        }
    }
    Ok(ok_json(json!(out)))
}

// ---------------------------------------------------------------------------
// following/requests/reject
// ---------------------------------------------------------------------------

async fn following_requests_reject(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Check<Response> {
    let user_id = id_param(body, "userId")?;
    if mk_db::user::find_user_by_id(pool, user_id)
        .await
        .map_err(|e| db_err("requests_reject find_user", e))?
        .is_none()
    {
        return Err(meta_err(
            "NO_SUCH_USER",
            "abc2ffa6-25b2-4380-ba99-321ff3a94555",
            "No such user.",
        ));
    }
    let removed = mk_db::following::reject_follow_request(pool, user_id, me_id)
        .await
        .map_err(|e| db_err("requests_reject delete", e))?;
    if removed {
        // A pending request had already bumped the pair counters.
        mk_db::social::bump_follow_counts(pool, user_id, me_id, -1)
            .await
            .map_err(|e| db_err("requests_reject counts", e))?;
    }
    Ok(ok_empty())
}

// ---------------------------------------------------------------------------
// following/requests/sent
// ---------------------------------------------------------------------------

async fn following_requests_sent(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Check<Response> {
    let limit = limit_param(body, 10)?;
    let (since_id, until_id) = pager_ids(body)?;
    let rows = mk_db::following::list_sent_follow_requests(
        pool,
        me_id,
        since_id.as_deref(),
        until_id.as_deref(),
        limit,
    )
    .await
    .map_err(|e| db_err("requests_sent list", e))?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let follower = super::note_user_lite(state, pool, &row.follower_id).await?;
        let followee = super::note_user_lite(state, pool, &row.followee_id).await?;
        out.push(json!({ "id": row.id, "follower": follower, "followee": followee }));
    }
    Ok(ok_json(json!(out)))
}

// ---------------------------------------------------------------------------
// following/update-all
// ---------------------------------------------------------------------------

async fn following_update_all(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Check<Response> {
    let notify = match body.get("notify") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s == "none" => Some(None),
        Some(Value::String(s)) if s == "normal" => Some(Some("normal")),
        Some(_) => return Err(schema_invalid("notify", "must be normal or none")),
    };
    let with_replies = match body.get("withReplies") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => return Err(schema_invalid("withReplies", "must be a boolean")),
    };
    mk_db::following::update_all_followings(pool, me_id, notify, with_replies)
        .await
        .map_err(|e| db_err("following_update_all", e))?;
    Ok(ok_empty())
}

// ---------------------------------------------------------------------------
// hashtags/search
// ---------------------------------------------------------------------------

async fn hashtags_search(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let query = match body.get("query") {
        Some(Value::String(s)) => s.clone(),
        _ => return Err(schema_invalid("query", "required")),
    };
    let limit = limit_param(body, 10)?;
    let offset = offset_param(body, 0)?;
    let names = mk_db::hashtag::search(pool, &query.to_lowercase(), limit, offset)
        .await
        .map_err(|e| db_err("hashtags_search", e))?;
    Ok(ok_json(json!(names)))
}

// ---------------------------------------------------------------------------
// username/available
// ---------------------------------------------------------------------------

async fn username_available(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let username = match body.get("username") {
        Some(Value::String(s))
            if !s.is_empty()
                && s.chars().count() <= 20
                && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') =>
        {
            s.clone()
        }
        _ => {
            return Err(schema_invalid(
                "username",
                "must be 1..20 chars of [a-zA-Z0-9_]",
            ));
        }
    };
    let lower = username.to_lowercase();
    let exists = mk_db::user::username_exists_local(pool, &lower)
        .await
        .map_err(|e| db_err("username_available exists", e))?
        || mk_db::user::used_username_exists(pool, &lower)
            .await
            .map_err(|e| db_err("username_available used", e))?;
    let preserved = mk_db::meta::preserved_usernames(pool)
        .await
        .map_err(|e| db_err("username_available preserved", e))?
        .iter()
        .any(|x| x.to_lowercase() == lower);
    Ok(ok_json(json!({ "available": !exists && !preserved })))
}

// ---------------------------------------------------------------------------
// email-address/available
// ---------------------------------------------------------------------------

async fn email_address_available(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let email = match body.get("emailAddress") {
        Some(Value::String(s)) => s.clone(),
        _ => return Err(schema_invalid("emailAddress", "required")),
    };
    let format_ok = {
        let parts: Vec<&str> = email.split('@').collect();
        parts.len() == 2
            && !parts[0].is_empty()
            && parts[1].contains('.')
            && !parts[1].starts_with('.')
            && !parts[1].ends_with('.')
            && !email.chars().any(char::is_whitespace)
    };
    if !format_ok {
        return Ok(ok_json(json!({ "available": false, "reason": "format" })));
    }
    let used = mk_db::user::verified_email_exists(pool, &email)
        .await
        .map_err(|e| db_err("email_address_available", e))?;
    if used {
        return Ok(ok_json(json!({ "available": false, "reason": "used" })));
    }
    Ok(ok_json(json!({ "available": true, "reason": Value::Null })))
}
