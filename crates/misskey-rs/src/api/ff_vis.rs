//! ff-visibility arena: the following/followers list endpoints + the AP
//! collections they mirror. Upstream refs (read-only):
//! - `endpoints/users/followers.ts` / `endpoints/users/following.ts` — the
//!   per-visibility gate (`profile.followersVisibility` /
//!   `profile.followingVisibility`): public → anyone; followers → me or a
//!   follower of the user (moderators bypass); private → me only. ApiError
//!   (kind client, no httpStatusCode) surfaces as 400 FORBIDDEN.
//! - `core/entities/FollowingEntityService.pack(packMany)` — the Following
//!   wire shape { id, createdAt, followeeId, followerId, followee?, follower? }.
//! - `server/ActivityPubServerService.ts {followers,following}` — the
//!   unconditional GET `/users/:user/{followers,following}` routes: meta
//!   federation 'none' → 403, private/followers visibility → 403
//!   (+ Cache-Control: public, max-age=30), else an OrderedCollection with
//!   setResponseType's `application/activity+json`.
//!
//! `users/followers`/`users/following` are `requireCredential: false` — the
//! token resolves to an optional me (the users/show convention; the harness
//! client only sends `i` for real users). All fns carry the `ff-arena` wave.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// The endpoint names routed into this module (single source of truth for the
/// `ff-arena` dispatch block in mod.rs).
pub(crate) const FF_ENDPOINTS: &[&str] = &["users/followers", "users/following"];

const NO_SUCH_USER_FOLLOWERS: &str = "27fa5435-88ab-43de-9360-387de88727cd";
const NO_SUCH_USER_FOLLOWING: &str = "63e4aba4-4156-4e53-be25-c9559e42d71b";
const FORBIDDEN_FOLLOWERS: &str = "3c6a84db-d619-26af-ca14-06232a21df8a";
const FORBIDDEN_FOLLOWING: &str = "f6cdb0df-c19f-ec5c-7dbb-0ba84a1f92ba";

/// ApiCallService #sendApiError default for ApiError(kind=client): 400.
fn ff_meta_err(code: &str, id: &str, message: &str) -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        code,
        id,
        message,
    ))
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_ff_vis", error = %e, "db error: {context}");
    api_err(ApiError::internal_error())
}

/// Each endpoint carries its own NO_SUCH_USER / FORBIDDEN ids.
fn endpoint_ids(name: &str) -> (&'static str, &'static str) {
    if name == "users/following" {
        (NO_SUCH_USER_FOLLOWING, FORBIDDEN_FOLLOWING)
    } else {
        (NO_SUCH_USER_FOLLOWERS, FORBIDDEN_FOLLOWERS)
    }
}

/// `QueryService.makePaginationQuery` port: sinceDate/untilDate are converted
/// with `idService.gen(ms)` (the AID carries the creation ms, so an id bound
/// at the boundary ms stands in for the createdAt comparison), and the branch
/// order (sinceId+untilId → DESC range; sinceId → ASC; untilId → DESC; then
/// the date-only branches) mirrors QueryService.ts L54-76. Returns
/// (exclusive since bound, exclusive until bound, order DESC).
fn paginate_bounds(
    since_id: Option<&str>,
    until_id: Option<&str>,
    since_date: Option<i64>,
    until_date: Option<i64>,
) -> (Option<String>, Option<String>, bool) {
    let aid_at = |ms: i64| mk_types::id::gen_aid(ms);
    if since_id.is_some() && until_id.is_some() {
        (since_id.map(String::from), until_id.map(String::from), true)
    } else if let Some(s) = since_id {
        (Some(s.to_string()), None, false)
    } else if let Some(u) = until_id {
        (None, Some(u.to_string()), true)
    } else if let (Some(sd), Some(ud)) = (since_date, until_date) {
        (Some(aid_at(sd)), Some(aid_at(ud)), true)
    } else if let Some(sd) = since_date {
        (Some(aid_at(sd)), None, false)
    } else if let Some(ud) = until_date {
        (None, Some(aid_at(ud)), true)
    } else {
        (None, None, true)
    }
}

/// One packed edge user the way `userEntityService.pack(id, me, {
/// schema: 'UserDetailedNotMe' })` lands in the Following items: the same
/// roles/memo pipeline users/show runs (badge/public roles + relation block).
async fn pack_edge_user(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    target: mk_db::user::LocalUserRow,
    me_id: Option<&str>,
) -> Result<Value, Response> {
    let profile = mk_db::user::find_profile(pool, &target.id)
        .await
        .map_err(|e| db_err("pack_edge_user find_profile", e))?;
    let is_me = me_id == Some(target.id.as_str());
    let relation = super::build_relation(pool, me_id, &target.id, is_me)
        .await
        .map_err(|r| *r)?;
    let assigned = mk_db::role::user_assigned_roles(pool, &target.id)
        .await
        .map_err(|e| db_err("pack_edge_user user_assigned_roles", e))?;
    let viewer_mod = super::viewer_is_moderator(pool, me_id).await;
    let role_pack = mk_core::services::user_pack::RolePack {
        assigned: &assigned,
        target_is_root: target.username_lower == "root",
        viewer_is_moderator: viewer_mod,
    };
    let ctx = mk_core::services::user_pack::InstanceCtx {
        url: &state.config.url,
        host: &state.config.host(),
    };
    let mut packed = mk_core::services::user_pack::pack_inline_with_roles(
        &target,
        &profile,
        is_me,
        relation.as_ref(),
        ctx,
        false, // list items keep the users/show secrets-off default
        Some(&role_pack),
    );
    super::set_viewers_memo(pool, me_id, &target.id, &mut packed).await;
    Ok(packed)
}

/// `FollowingEntityService.pack` for one edge (populateFollowee/populateFollower
/// both on — the list endpoints always join+pack the counterpart user).
async fn pack_edge(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    edge: mk_db::ff_visibility::FollowingEdge,
    me_id: Option<&str>,
) -> Result<Value, Response> {
    let mut item = json!({
        "id": edge.id,
        "createdAt": mk_core::services::signup::ms_to_iso(
            mk_types::id::parse_aid_date(&edge.id).unwrap_or_default(),
        ),
        "followeeId": edge.followee_id,
        "followerId": edge.follower_id,
    });
    let follower = mk_db::user::find_user_by_id(pool, &edge.follower_id)
        .await
        .map_err(|e| db_err("pack_edge find_user_by_id[follower]", e))?;
    if let Some(row) = follower {
        item["follower"] = pack_edge_user(state, pool, row, me_id).await?;
    }
    let followee = mk_db::user::find_user_by_id(pool, &edge.followee_id)
        .await
        .map_err(|e| db_err("pack_edge find_user_by_id[followee]", e))?;
    if let Some(row) = followee {
        item["followee"] = pack_edge_user(state, pool, row, me_id).await?;
    }
    Ok(item)
}

/// List-param line shared by both endpoints (paramDef allOf: anyOf({userId},
/// {username, host}) + since/until id/date + limit 1..100 default 10).
struct ListParams {
    user_id: Option<String>,
    username: Option<String>,
    host: Option<String>,
    since_id: Option<String>,
    until_id: Option<String>,
    since_date: Option<i64>,
    until_date: Option<i64>,
    limit: u32,
}

fn parse_list_params(body: &Value) -> Result<ListParams, Response> {
    let user_id = match body.get("userId") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if super::is_alnum_id(s) => Some(s.clone()),
        Some(_) => {
            return Err(super::invalid_param(
                "userId",
                "expected an alphanumeric id",
            ));
        }
    };
    let str_param = |key: &str| -> Result<Option<String>, Response> {
        match body.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(super::invalid_param(key, "expected a string")),
        }
    };
    let username = str_param("username")?;
    let host = str_param("host")?;
    let id_param = |key: &str| -> Result<Option<String>, Response> {
        match body.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if super::is_alnum_id(s) => Ok(Some(s.clone())),
            Some(_) => Err(super::invalid_param(key, "expected an alphanumeric id")),
        }
    };
    let since_id = id_param("sinceId")?;
    let until_id = id_param("untilId")?;
    let date_param = |key: &str, v: Option<&Value>| -> Result<Option<i64>, Response> {
        match v {
            None | Some(Value::Null) => Ok(None),
            Some(v) => match v.as_i64() {
                Some(ms) => Ok(Some(ms)),
                None => Err(super::invalid_param(key, "expected an integer (ms)")),
            },
        }
    };
    let since_date = date_param("sinceDate", body.get("sinceDate"))?;
    let until_date = date_param("untilDate", body.get("untilDate"))?;
    let limit = match body.get("limit") {
        None | Some(Value::Null) => 10,
        Some(v) => {
            let Some(n) = v.as_u64() else {
                return Err(super::invalid_param("limit", "expected an integer"));
            };
            if !(1..=100).contains(&n) {
                return Err(super::invalid_param("limit", "must be >= 1 and <= 100"));
            }
            n as u32
        }
    };
    if user_id.is_none() && (username.is_none() || host.is_none()) {
        // neither the userId shape nor the username+host shape is complete
        return Err(super::invalid_param(
            "userId",
            "either userId or username+host is required",
        ));
    }
    Ok(ListParams {
        user_id,
        username,
        host,
        since_id,
        until_id,
        since_date,
        until_date,
        limit,
    })
}

/// The upstream user lookup: `{ id: userId }` or
/// `{ usernameLower, host: toPunyNullable(host) ?? IsNull() }` (local hosts
/// only — a non-empty host resolves no local row).
async fn find_target_user(
    pool: &mk_db::sqlx::PgPool,
    params: &ListParams,
) -> Result<Option<mk_db::user::LocalUserRow>, Response> {
    if let Some(id) = &params.user_id {
        return mk_db::user::find_user_by_id(pool, id)
            .await
            .map_err(|e| db_err("find_target_user find_user_by_id", e));
    }
    if params.host.as_deref().is_some_and(|h| !h.is_empty()) {
        return Ok(None); // remote host: no local row (M2 local-only flow)
    }
    let Some(username) = &params.username else {
        return Ok(None);
    };
    mk_db::user::find_local_user_by_username(pool, &username.to_lowercase())
        .await
        .map_err(|e| db_err("find_target_user find_local_user_by_username", e))
}

/// The per-visibility gate both endpoints run ahead of the list query
/// (users/followers.ts L115-135 / users/following.ts L123-143 — the
/// visibility column and the FORBIDDEN id differ per endpoint; moderators
/// bypass everything non-public via roleService.isModerator(me)).
async fn ff_gate(
    pool: &mk_db::sqlx::PgPool,
    visibility: &str,
    target_user_id: &str,
    me_id: Option<&str>,
    forbidden_id: &str,
) -> Result<(), Response> {
    if visibility == "public" || super::viewer_is_moderator(pool, me_id).await {
        return Ok(());
    }
    if visibility == "private" {
        if me_id.is_none() || me_id != Some(target_user_id) {
            return Err(ff_meta_err("FORBIDDEN", forbidden_id, "Forbidden."));
        }
        return Ok(());
    }
    // 'followers': me, or a follower of the user
    match me_id {
        None => Err(ff_meta_err("FORBIDDEN", forbidden_id, "Forbidden.")),
        Some(me) if me == target_user_id => Ok(()),
        Some(me) => {
            let is_following = mk_db::social::following_exists(pool, me, target_user_id)
                .await
                .map_err(|e| db_err("ff_gate following_exists", e))?;
            if !is_following {
                return Err(ff_meta_err("FORBIDDEN", forbidden_id, "Forbidden."));
            }
            Ok(())
        }
    }
}

/// `POST /api/users/followers` + `/api/users/following` (upstream
/// endpoints/users/{followers,following}.ts): user lookup → gate → the
/// paginated edge list packed as Following items.
pub(crate) async fn route(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<&str>,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let (no_such_id, forbidden_id) = endpoint_ids(name);
    let me_id = super::resolve_me(pool, token).await;

    let params = match parse_list_params(body) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let user = match find_target_user(pool, &params).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let Some(user) = user else {
        return ff_meta_err("NO_SUCH_USER", no_such_id, "No such user.");
    };

    let profile = match mk_db::user::find_profile(pool, &user.id).await {
        Ok(p) => p,
        Err(e) => return db_err("ff_route find_profile", e),
    };
    let visibility = if name == "users/following" {
        &profile.following_visibility
    } else {
        &profile.followers_visibility
    };
    if let Err(resp) = ff_gate(pool, visibility, &user.id, me_id.as_deref(), forbidden_id).await {
        return resp;
    }

    let side = if name == "users/following" {
        mk_db::ff_visibility::EdgeSide::Following
    } else {
        mk_db::ff_visibility::EdgeSide::Followers
    };
    let (from_id, until_id, desc) = paginate_bounds(
        params.since_id.as_deref(),
        params.until_id.as_deref(),
        params.since_date,
        params.until_date,
    );
    let edges = match mk_db::ff_visibility::list_edges(
        pool,
        side,
        &user.id,
        from_id.as_deref(),
        until_id.as_deref(),
        desc,
        params.limit,
    )
    .await
    {
        Ok(e) => e,
        Err(e) => return db_err("ff_route list_edges", e),
    };

    let mut out = Vec::with_capacity(edges.len());
    for edge in edges {
        match pack_edge(state, pool, edge, me_id.as_deref()).await {
            Ok(item) => out.push(item),
            Err(resp) => return resp,
        }
    }
    (
        StatusCode::OK,
        Json(serde_json::to_value(out).unwrap_or(json!([]))),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// AP collections: GET /users/:user/{following,followers}
// (ActivityPubServerService.ts followers/following; activity+json responses)
// ---------------------------------------------------------------------------

const AP_CONTEXT: [&str; 2] = [
    "https://www.w3.org/ns/activitystreams",
    "https://w3id.org/security/v1",
];

const ACTIVITY_JSON_CT: &str = "application/activity+json; charset=utf-8";

fn with_cache_control(mut resp: Response, value: &str) -> Response {
    if let Ok(v) = axum::http::header::HeaderValue::from_str(value) {
        resp.headers_mut()
            .insert(axum::http::header::CACHE_CONTROL, v);
    }
    resp
}

/// AP reply with setResponseType's activity+json content type (the
/// json_utf8_charset middleware only rewrites exact `application/json`, so a
/// manual header survives it).
fn ap_json_response(status: StatusCode, body: Value, cache: Option<&str>) -> Response {
    let mut resp = (status, Json(body)).into_response();
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::header::HeaderValue::from_static(ACTIVITY_JSON_CT),
    );
    match cache {
        Some(c) => with_cache_control(resp, c),
        None => resp,
    }
}

/// The shared AP collection handler (federation gate → user lookup →
/// ff-visibility gate → OrderedCollection index render).
async fn ap_collection(
    state: AppState,
    user_id: &str,
    side: mk_db::ff_visibility::EdgeSide,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };

    // both handlers open with `if (this.meta.federation === 'none') → 403`
    match mk_db::ff_visibility::meta_federation(pool).await {
        Ok(f) if f == "none" => return StatusCode::FORBIDDEN.into_response(),
        Ok(_) => {}
        Err(e) => return db_err("ap_collection meta_federation", e),
    }

    // findOneBy({ id, host: IsNull() }) — the user table only holds local rows
    let user = match mk_db::user::find_user_by_id(pool, user_id).await {
        Ok(u) => u,
        Err(e) => return db_err("ap_collection find_user_by_id", e),
    };
    let Some(user) = user else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let profile = match mk_db::user::find_profile(pool, &user.id).await {
        Ok(p) => p,
        Err(e) => return db_err("ap_collection find_profile", e),
    };
    // ff-visibility: anything but public is 403 for AP consumers (the private
    // and followers branches share the 403 + Cache-Control shape — upstream
    // followers.ts L225-234 / following.ts L322-330)
    let visibility = match side {
        mk_db::ff_visibility::EdgeSide::Followers => &profile.followers_visibility,
        mk_db::ff_visibility::EdgeSide::Following => &profile.following_visibility,
    };
    if visibility != "public" {
        return with_cache_control(StatusCode::FORBIDDEN.into_response(), "public, max-age=30");
    }

    let base = state.config.url.trim_end_matches('/');
    let part_of = match side {
        mk_db::ff_visibility::EdgeSide::Followers => {
            format!("{base}/users/{user_id}/followers")
        }
        mk_db::ff_visibility::EdgeSide::Following => {
            format!("{base}/users/{user_id}/following")
        }
    };
    let total = match side {
        mk_db::ff_visibility::EdgeSide::Followers => user.followers_count,
        mk_db::ff_visibility::EdgeSide::Following => user.following_count,
    };
    let rendered = json!({
        "@context": AP_CONTEXT,
        "id": part_of,
        "type": "OrderedCollection",
        "totalItems": total,
        "first": format!("{part_of}?page=true"),
    });
    // index page: renderOrderedCollection + Cache-Control public, max-age=180
    ap_json_response(StatusCode::OK, rendered, Some("public, max-age=180"))
}

pub(crate) async fn ap_followers(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
) -> Response {
    ap_collection(state, &user_id, mk_db::ff_visibility::EdgeSide::Followers).await
}

pub(crate) async fn ap_following(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
) -> Response {
    ap_collection(state, &user_id, mk_db::ff_visibility::EdgeSide::Following).await
}

// ---------------------------------------------------------------------------
// admin/update-meta federation side-patch
// ---------------------------------------------------------------------------

/// admin/update-meta carrying `federation` — persist it before the note/freq
/// lane arms apply their keys (MetaService.update partial patch). The admin
/// gate runs here so a non-admin federation patch never lands; Ok(()) means
/// "persisted — fall through so the other lanes' keys keep landing" and Err
/// is the final response.
pub(crate) async fn persist_federation(
    state: &AppState,
    body: &Value,
    token: Option<&str>,
) -> Result<(), Response> {
    let Some(pool) = &state.pool else {
        return Err(api_err(ApiError::internal_error()));
    };
    // requireAdmin meta (upstream admin/update-meta.ts requireCredential +
    // requireAdmin) — one gate source of truth with the admin/* family.
    super::admin::gate(state, token, super::admin::Gate::Admin).await?;

    let Some(value) = body
        .get("federation")
        .and_then(|v| v.as_str())
        .filter(|v| matches!(*v, "all" | "specified" | "none"))
    else {
        return Err(super::invalid_param(
            "federation",
            "must be one of all/specified/none",
        ));
    };
    mk_db::ff_visibility::update_meta_federation(pool, value)
        .await
        .map_err(|e| db_err("persist_federation update_meta_federation", e))?;
    Ok(())
}
