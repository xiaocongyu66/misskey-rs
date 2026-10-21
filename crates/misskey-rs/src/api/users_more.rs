//! users/* remainder: `users/relation`, the remaining `users/lists/*`
//! read/write endpoints, and `users/reactions`.
//!
//! Upstream refs (read-only, pinned ea2c3aed):
//! - `endpoints/users/relation.ts` + `core/entities/UserEntityService.ts`
//!   `getRelation/getRelations` (always an ARRAY: the scalar form wraps one
//!   entry; `id` + the eight booleans, no NO_SUCH_USER gate).
//! - `endpoints/users/lists/{show,get-memberships,update,delete,favorite,
//!   unfavorite,pull,create-from-public}.ts` + `UserListService` +
//!   `UserListEntityService` (`pack` / `packMembershipsMany`).
//! - `endpoints/users/reactions.ts` + `NoteReactionEntityService`
//!   `packManyWithNote` (`publicReactions` / remote-user / block gates, then
//!   a visibility-filtered reaction page packed with each note).
//!
//! Auth split mirrors the upstream metas: `relation` and every list write are
//! `requireCredential: true`; `lists/show`, `lists/get-memberships` and
//! `users/reactions` are `requireCredential: false` (a riding credential still
//! authenticates, an invalid one still 401s). All db paths live in
//! `mk_db::lists` / `mk_db::note` (tables ship in the 0001 baseline).

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// The endpoint names routed into this module (single source of truth for the
/// `users-more` dispatch arm in mod.rs).
pub(crate) const USERS_MORE_ENDPOINTS: &[&str] = &[
    "users/relation",
    "users/lists/show",
    "users/lists/get-memberships",
    "users/lists/update",
    "users/lists/delete",
    "users/lists/favorite",
    "users/lists/unfavorite",
    "users/lists/pull",
    "users/lists/create-from-public",
    "users/reactions",
];

pub(crate) fn handles(name: &str) -> bool {
    USERS_MORE_ENDPOINTS.contains(&name)
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_users_more", error = %e, "db error: {context}");
    api_err(ApiError::internal_error())
}

/// ApiError(kind=client) without an httpStatusCode → default 400.
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

fn ok_json(v: Value) -> Response {
    (StatusCode::OK, Json(v)).into_response()
}

/// upstream `send(reply, undefined)` → 204.
fn ok_empty() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

fn opt_id<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(|v| v.as_str())
}

fn required_id<'a>(body: &'a Value, key: &'a str) -> Result<&'a str, Response> {
    opt_id(body, key).ok_or_else(|| invalid_param(key, "required (misskey:id format)"))
}

fn opt_bool(body: &Value, key: &str, dflt: bool) -> Result<bool, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(dflt),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(invalid_param(key, "expected a boolean")),
    }
}

fn opt_i64(body: &Value, key: &str) -> Result<Option<i64>, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_i64()
            .map(Some)
            .ok_or_else(|| invalid_param(key, "expected an integer")),
    }
}

/// paramDef pager: limit 1..100 default `dflt`; id/date bounds with the
/// QueryService.makePaginationQuery branch order (since-only → ASC).
fn pager(body: &Value, dflt: i64) -> Result<(Option<String>, Option<String>), Response> {
    let limit_ok = match body.get("limit") {
        None | Some(Value::Null) => true,
        Some(v) => matches!(v.as_i64(), Some(l) if (1..=100).contains(&l)),
    };
    if !limit_ok {
        return Err(invalid_param(
            "limit",
            "must be an integer between 1 and 100",
        ));
    }
    let since_id = match body.get("sinceId") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.as_str()),
        Some(_) => return Err(invalid_param("sinceId", "expected a string")),
    };
    let until_id = match body.get("untilId") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.as_str()),
        Some(_) => return Err(invalid_param("untilId", "expected a string")),
    };
    let since_date = opt_i64(body, "sinceDate")?;
    let until_date = opt_i64(body, "untilDate")?;
    let (since, until, _desc) =
        super::ff_vis::paginate_bounds(since_id, until_id, since_date, until_date);
    Ok((since, until))
}

fn pager_limit(body: &Value, dflt: i64) -> i64 {
    match body.get("limit") {
        None | Some(Value::Null) => dflt,
        Some(v) => v.as_i64().unwrap_or(dflt).clamp(1, 100),
    }
}

/// Optional-credential resolution (requireCredential: false endpoints): no
/// token → guest; a present-but-unresolvable native token → 401.
async fn optional_me(
    pool: &mk_db::sqlx::PgPool,
    token: Option<&str>,
) -> Result<Option<String>, Response> {
    match token {
        None => Ok(None),
        Some(t) => {
            if t.len() != 16 {
                return Err(super::authentication_failed());
            }
            match super::resolve_me(pool, Some(t)).await {
                Some(id) => Ok(Some(id)),
                None => Err(super::authentication_failed()),
            }
        }
    }
}

fn user_lite(
    state: &AppState,
    row: &mk_db::user::LocalUserRow,
) -> Value {
    let ctx = mk_core::services::user_pack::InstanceCtx {
        url: &state.config.url,
        host: &state.config.host(),
    };
    mk_core::services::user_pack::user_lite_value_with_roles(row, ctx, None)
}

fn created_at(id: &str) -> String {
    mk_types::id::parse_aid_date(id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default()
}

/// `UserListEntityService.pack` — { id, createdAt, name, userIds, isPublic }.
async fn pack_list(
    pool: &mk_db::sqlx::PgPool,
    row: &mk_db::lists::UserListRow,
) -> Result<Value, Response> {
    let user_ids = mk_db::lists::list_member_ids(pool, &row.id)
        .await
        .map_err(|e| db_err("pack_list list_member_ids", e))?;
    Ok(json!({
        "id": row.id,
        "createdAt": created_at(&row.id),
        "name": row.name,
        "userIds": user_ids,
        "isPublic": row.is_public,
    }))
}

fn no_such_list(id: &'static str) -> Response {
    meta_err("NO_SUCH_LIST", id, "No such list.")
}

/// users/lists/show + get-memberships list scope:
/// `!forPublic && me !== null` → owner row, otherwise the public row.
async fn resolve_list(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me: Option<&str>,
    no_such_id: &'static str,
) -> Result<(mk_db::lists::UserListRow, bool), Response> {
    let list_id = required_id(body, "listId")?;
    let for_public = opt_bool(body, "forPublic", false)?;
    let row = if !for_public && me.is_some() {
        mk_db::lists::find_list(pool, list_id, me.unwrap())
            .await
            .map_err(|e| db_err("resolve_list find_list", e))?
    } else {
        mk_db::lists::find_public_list(pool, list_id)
            .await
            .map_err(|e| db_err("resolve_list find_public_list", e))?
    };
    match row {
        Some(row) => Ok((row, for_public)),
        None => Err(no_such_list(no_such_id)),
    }
}

/// Entry from the `users-more` dispatch arm in mod.rs (body pre-parsed,
/// token + credential-declared pre-extracted).
pub(crate) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
    credential_declared: bool,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    // requireCredential endpoints (all but show/get-memberships/reactions).
    let me_id = if matches!(
        name,
        "users/relation"
            | "users/lists/update"
            | "users/lists/delete"
            | "users/lists/favorite"
            | "users/lists/unfavorite"
            | "users/lists/pull"
            | "users/lists/create-from-public"
    ) {
        match super::social::require_checked_user(pool, token.as_deref(), credential_declared)
            .await
        {
            Ok(id) => Some(id),
            Err(resp) => return resp,
        }
    } else {
        None
    };

    let result: Result<Response, Response> = match name {
        "users/relation" => users_relation(pool, body, me_id.as_deref().unwrap_or_default()).await,
        "users/lists/show" => lists_show(state, pool, body, token.as_deref()).await,
        "users/lists/get-memberships" => {
            lists_get_memberships(state, pool, body, token.as_deref()).await
        }
        "users/lists/update" => lists_update(pool, body, me_id.as_deref().unwrap_or_default()).await,
        "users/lists/delete" => lists_delete(pool, body, me_id.as_deref().unwrap_or_default()).await,
        "users/lists/favorite" => {
            lists_favorite(pool, body, me_id.as_deref().unwrap_or_default()).await
        }
        "users/lists/unfavorite" => {
            lists_unfavorite(pool, body, me_id.as_deref().unwrap_or_default()).await
        }
        "users/lists/pull" => lists_pull(pool, body, me_id.as_deref().unwrap_or_default()).await,
        "users/lists/create-from-public" => {
            lists_create_from_public(pool, body, me_id.as_deref().unwrap_or_default()).await
        }
        "users/reactions" => users_reactions(state, pool, body, token.as_deref()).await,
        _ => Ok(api_err(ApiError::unknown_endpoint())),
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

// ---------------------------------------------------------------------------
// users/relation
// ---------------------------------------------------------------------------

async fn users_relation(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let targets: Vec<String> = match body.get("userId") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(s) => out.push(s.to_string()),
                    None => {
                        return Err(invalid_param(
                            "userId",
                            "array items must be misskey:id strings",
                        ))
                    }
                }
            }
            out
        }
        _ => {
            return Err(invalid_param(
                "userId",
                "required (string or array of misskey:id)",
            ))
        }
    };

    // UserEntityService.getRelations returns a Map keyed by target id — the
    // endpoint spreads `.values()`, so duplicate targets collapse.
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for target in targets {
        if !seen.insert(target.clone()) {
            continue;
        }
        let rel = mk_db::relation::get_relation(pool, me_id, &target)
            .await
            .map_err(|e| db_err("users_relation get_relation", e))?;
        out.push(json!({
            "id": target,
            "isFollowing": rel.is_following,
            "isFollowed": rel.is_followed,
            "hasPendingFollowRequestFromYou": rel.has_pending_follow_request_from_you,
            "hasPendingFollowRequestToYou": rel.has_pending_follow_request_to_you,
            "isBlocking": rel.is_blocking,
            "isBlocked": rel.is_blocked,
            "isMuted": rel.is_muted,
            "isRenoteMuted": rel.is_renote_muted,
        }));
    }
    Ok(ok_json(json!(out)))
}

// ---------------------------------------------------------------------------
// users/lists/show
// ---------------------------------------------------------------------------

async fn lists_show(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    token: Option<&str>,
) -> Result<Response, Response> {
    let me = optional_me(pool, token).await?;
    let (row, for_public) = resolve_list(
        pool,
        body,
        me.as_deref(),
        "7bc05c21-1d7a-41ae-88f1-66820f4dc686",
    )
    .await?;
    let mut packed = pack_list(pool, &row).await?;
    if for_public && row.is_public {
        let liked_count = mk_db::lists::count_favorites(pool, &row.id)
            .await
            .map_err(|e| db_err("lists_show count_favorites", e))?;
        let is_liked = match me.as_deref() {
            Some(m) => mk_db::lists::favorite_exists(pool, m, &row.id)
                .await
                .map_err(|e| db_err("lists_show favorite_exists", e))?,
            None => false,
        };
        if let Some(obj) = packed.as_object_mut() {
            obj.insert("likedCount".into(), json!(liked_count));
            obj.insert("isLiked".into(), json!(is_liked));
        }
    }
    let _ = state;
    Ok(ok_json(packed))
}

// ---------------------------------------------------------------------------
// users/lists/get-memberships
// ---------------------------------------------------------------------------

async fn lists_get_memberships(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    token: Option<&str>,
) -> Result<Response, Response> {
    let me = optional_me(pool, token).await?;
    let (row, _for_public) = resolve_list(
        pool,
        body,
        me.as_deref(),
        "7bc05c21-1d7a-41ae-88f1-66820f4dc686",
    )
    .await?;
    let (since_id, until_id) = pager(body, 30)?;
    let limit = pager_limit(body, 30);
    let memberships = mk_db::lists::list_memberships(
        pool,
        &row.id,
        since_id.as_deref(),
        until_id.as_deref(),
        limit,
    )
    .await
    .map_err(|e| db_err("lists_get_memberships list_memberships", e))?;

    let mut out = Vec::with_capacity(memberships.len());
    for m in memberships {
        // upstream innerJoinAndSelect('membership.user') — a dangling row is
        // not returned.
        let user = match mk_db::user::find_user_by_id(pool, &m.user_id).await {
            Ok(Some(u)) => u,
            Ok(None) => continue,
            Err(e) => return Err(db_err("lists_get_memberships find_user", e)),
        };
        out.push(json!({
            "id": m.id,
            "createdAt": created_at(&m.id),
            "userId": m.user_id,
            "user": user_lite(state, &user),
            "withReplies": m.with_replies,
        }));
    }
    Ok(ok_json(json!(out)))
}

// ---------------------------------------------------------------------------
// users/lists/update
// ---------------------------------------------------------------------------

async fn lists_update(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let list_id = required_id(body, "listId")?;
    let name = match body.get("name") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.is_empty() && s.chars().count() <= 100 => Some(s.as_str()),
        Some(Value::String(_)) => {
            return Err(invalid_param("name", "must be 1..100 characters"));
        }
        Some(_) => return Err(invalid_param("name", "expected a string")),
    };
    let is_public = match body.get("isPublic") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => return Err(invalid_param("isPublic", "expected a boolean")),
    };
    mk_db::lists::find_list(pool, list_id, me_id)
        .await
        .map_err(|e| db_err("lists_update find_list", e))?
        .ok_or_else(|| no_such_list("796666fe-3dff-4d39-becb-8a5932c1d5b7"))?;
    mk_db::lists::update_list_fields(pool, list_id, name, is_public)
        .await
        .map_err(|e| db_err("lists_update update_list_fields", e))?;
    let row = mk_db::lists::find_list(pool, list_id, me_id)
        .await
        .map_err(|e| db_err("lists_update refetch", e))?
        .ok_or_else(|| api_err(ApiError::internal_error()))?;
    Ok(ok_json(pack_list(pool, &row).await?))
}

// ---------------------------------------------------------------------------
// users/lists/delete
// ---------------------------------------------------------------------------

async fn lists_delete(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let list_id = required_id(body, "listId")?;
    mk_db::lists::find_list(pool, list_id, me_id)
        .await
        .map_err(|e| db_err("lists_delete find_list", e))?
        .ok_or_else(|| no_such_list("78436795-db79-42f5-b1e2-55ea2cf19166"))?;
    mk_db::lists::delete_list(pool, list_id)
        .await
        .map_err(|e| db_err("lists_delete delete_list", e))?;
    Ok(ok_empty())
}

// ---------------------------------------------------------------------------
// users/lists/favorite + unfavorite
// ---------------------------------------------------------------------------

async fn lists_favorite(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let list_id = required_id(body, "listId")?;
    mk_db::lists::find_public_list(pool, list_id)
        .await
        .map_err(|e| db_err("lists_favorite find_public_list", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_USER_LIST",
                "7dbaf3cf-7b42-4b8f-b431-b3919e580dbe",
                "No such user list.",
            )
        })?;
    if mk_db::lists::favorite_exists(pool, me_id, list_id)
        .await
        .map_err(|e| db_err("lists_favorite favorite_exists", e))?
    {
        return Err(meta_err(
            "ALREADY_FAVORITED",
            "6425bba0-985b-461e-af1b-518070e72081",
            "The list has already been favorited.",
        ));
    }
    mk_db::lists::insert_favorite(pool, &mk_types::id::gen_aid_now(), me_id, list_id)
        .await
        .map_err(|e| db_err("lists_favorite insert_favorite", e))?;
    Ok(ok_empty())
}

async fn lists_unfavorite(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let list_id = required_id(body, "listId")?;
    mk_db::lists::find_public_list(pool, list_id)
        .await
        .map_err(|e| db_err("lists_unfavorite find_public_list", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_USER_LIST",
                "baedb33e-76b8-4b0c-86a8-9375c0a7b94b",
                "No such user list.",
            )
        })?;
    let favorite_id = mk_db::lists::find_favorite_id(pool, me_id, list_id)
        .await
        .map_err(|e| db_err("lists_unfavorite find_favorite_id", e))?
        .ok_or_else(|| {
            // upstream code is (verbatim) ALREADY_FAVORITED here.
            meta_err(
                "ALREADY_FAVORITED",
                "835c4b27-463d-4cfa-969b-a9058678d465",
                "You have not favorited the list.",
            )
        })?;
    mk_db::lists::delete_favorite(pool, &favorite_id)
        .await
        .map_err(|e| db_err("lists_unfavorite delete_favorite", e))?;
    Ok(ok_empty())
}

// ---------------------------------------------------------------------------
// users/lists/pull
// ---------------------------------------------------------------------------

async fn lists_pull(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let list_id = required_id(body, "listId")?;
    let user_id = required_id(body, "userId")?;
    mk_db::lists::find_list(pool, list_id, me_id)
        .await
        .map_err(|e| db_err("lists_pull find_list", e))?
        .ok_or_else(|| no_such_list("7f44670e-ab16-43b8-b4c1-ccd2ee89cc02"))?;
    mk_db::user::find_user_by_id(pool, user_id)
        .await
        .map_err(|e| db_err("lists_pull find_user", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_USER",
                "588e7f72-c744-4a61-b180-d354e912bda2",
                "No such user.",
            )
        })?;
    mk_db::lists::delete_membership(pool, list_id, user_id)
        .await
        .map_err(|e| db_err("lists_pull delete_membership", e))?;
    Ok(ok_empty())
}

// ---------------------------------------------------------------------------
// users/lists/create-from-public
// ---------------------------------------------------------------------------

async fn lists_create_from_public(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let name = match body.get("name") {
        Some(Value::String(s)) if !s.is_empty() && s.chars().count() <= 100 => s.as_str(),
        Some(Value::String(_)) => {
            return Err(invalid_param("name", "must be 1..100 characters"));
        }
        _ => return Err(invalid_param("name", "required (1..100 characters)")),
    };
    let list_id = required_id(body, "listId")?;
    mk_db::lists::find_public_list(pool, list_id)
        .await
        .map_err(|e| db_err("create_from_public find_public_list", e))?
        .ok_or_else(|| no_such_list("9292f798-6175-4f7d-93f4-b6742279667d"))?;

    // getUserPolicies(me).userListLimit — DEFAULT_POLICIES value.
    let current_count = mk_db::lists::count_lists_owned(pool, me_id)
        .await
        .map_err(|e| db_err("create_from_public count_lists", e))?;
    if current_count >= 10 {
        return Err(meta_err(
            "TOO_MANY_USERLISTS",
            "e9c105b2-c595-47de-97fb-7f7c2c33e92f",
            "You cannot create user list any more.",
        ));
    }

    let new_id = mk_types::id::gen_aid_now();
    mk_db::lists::insert_list(pool, &new_id, me_id, name)
        .await
        .map_err(|e| db_err("create_from_public insert_list", e))?;

    let members = mk_db::lists::list_member_ids(pool, list_id)
        .await
        .map_err(|e| db_err("create_from_public list_member_ids", e))?;
    for user_id in members {
        // getterService.getUser → NO_SUCH_USER remap
        if mk_db::user::find_user_by_id(pool, &user_id)
            .await
            .map_err(|e| db_err("create_from_public find_user", e))?
            .is_none()
        {
            return Err(meta_err(
                "NO_SUCH_USER",
                "13c457db-a8cb-4d88-b70a-211ceeeabb5f",
                "No such user.",
            ));
        }
        if user_id != me_id
            && mk_db::social::blocking_exists(pool, &user_id, me_id)
                .await
                .map_err(|e| db_err("create_from_public blocking_exists", e))?
        {
            return Err(meta_err(
                "YOU_HAVE_BEEN_BLOCKED",
                "a2497f2a-2389-439c-8626-5298540530f4",
                "You cannot push this user because you have been blocked by this user.",
            ));
        }
        if mk_db::lists::membership_exists(pool, &new_id, &user_id)
            .await
            .map_err(|e| db_err("create_from_public membership_exists", e))?
        {
            return Err(meta_err(
                "ALREADY_ADDED",
                "c3ad6fdb-692b-47ee-a455-7bd12c7af615",
                "That user has already been added to that list.",
            ));
        }
        let count = mk_db::lists::count_memberships(pool, &new_id)
            .await
            .map_err(|e| db_err("create_from_public count_memberships", e))?;
        if count >= 50 {
            return Err(meta_err(
                "TOO_MANY_USERS",
                "1845ea77-38d1-426e-8e4e-8b83b24f5bd7",
                "You can not push users any more.",
            ));
        }
        mk_db::lists::insert_membership(
            pool,
            &mk_types::id::gen_aid_now(),
            &user_id,
            &new_id,
            me_id,
            false,
        )
        .await
        .map_err(|e| db_err("create_from_public insert_membership", e))?;
    }

    let row = mk_db::lists::find_list(pool, &new_id, me_id)
        .await
        .map_err(|e| db_err("create_from_public refetch", e))?
        .ok_or_else(|| api_err(ApiError::internal_error()))?;
    Ok(ok_json(pack_list(pool, &row).await?))
}

// ---------------------------------------------------------------------------
// users/reactions
// ---------------------------------------------------------------------------

/// `ReactionService.convertLegacyReaction` — the legacy name table only; any
/// other value passes through unchanged (no normalize fallback).
fn convert_legacy_reaction(reaction: &str) -> String {
    match reaction {
        "like" => "👍",
        "love" => "\u{2764}",
        "laugh" => "😆",
        "hmm" => "🤔",
        "surprise" => "😮",
        "congrats" => "🎉",
        "angry" => "💢",
        "confused" => "😥",
        "rip" => "😇",
        "pudding" => "🍮",
        "star" => "⭐",
        other => other,
    }
    .to_string()
}

async fn users_reactions(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    token: Option<&str>,
) -> Result<Response, Response> {
    let me = optional_me(pool, token).await?;
    let user_id = required_id(body, "userId")?;
    let (since_id, until_id) = pager(body, 10)?;
    let limit = pager_limit(body, 10);

    // cacheService.findUserById uses findOneByOrFail → an unknown id is an
    // uncaught 500 upstream.
    let target = mk_db::user::find_user_by_id(pool, user_id)
        .await
        .map_err(|e| db_err("users_reactions find_user", e))?
        .ok_or_else(|| api_err(ApiError::internal_error()))?;

    let moderator = super::viewer_is_moderator(pool, me.as_deref()).await;
    if !moderator {
        let host = mk_db::note::user_host_of(pool, user_id)
            .await
            .map_err(|e| db_err("users_reactions user_host_of", e))?;
        if host.is_some() {
            return Err(meta_err(
                "IS_REMOTE_USER",
                "6b95fa98-8cf9-2350-e284-f0ffdb54a805",
                "Currently unavailable to display reactions of remote users. See https://github.com/misskey-dev/misskey/issues/12964",
            ));
        }
        let profile = mk_db::user::find_profile(pool, user_id)
            .await
            .map_err(|e| db_err("users_reactions find_profile", e))?;
        if me.as_deref() != Some(user_id) && !profile.public_reactions {
            return Err(meta_err(
                "REACTIONS_NOT_PUBLIC",
                "673a7dd2-6924-1093-e0c0-e68456ceae5c",
                "Reactions of the user is not public.",
            ));
        }
        // early return when the requesting user blocks me
        if let Some(me_id) = me.as_deref() {
            if mk_db::social::blocking_exists(pool, user_id, me_id)
                .await
                .map_err(|e| db_err("users_reactions blocking_exists", e))?
            {
                return Ok(ok_json(json!([])));
            }
        }
    }

    let followees = match me.as_deref() {
        Some(me_id) => mk_db::social::followee_ids(pool, me_id)
            .await
            .map_err(|e| db_err("users_reactions followee_ids", e))?,
        None => Vec::new(),
    };
    let rows = mk_db::note::user_reaction_rows(
        pool,
        user_id,
        me.as_deref(),
        &followees,
        since_id.as_deref(),
        until_id.as_deref(),
        limit,
    )
    .await
    .map_err(|e| db_err("users_reactions user_reaction_rows", e))?;

    let reacting_user = user_lite(state, &target);
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let note = match super::pack_note_for(state, pool, &row.note_id, me.as_deref()).await {
            Ok(n) => n,
            Err(r) => return Err(*r),
        };
        out.push(json!({
            "id": row.id,
            "createdAt": created_at(&row.id),
            "user": reacting_user,
            "type": convert_legacy_reaction(&row.reaction),
            "note": note,
        }));
    }
    Ok(ok_json(json!(out)))
}
