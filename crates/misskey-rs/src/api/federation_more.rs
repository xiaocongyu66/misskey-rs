//! federation/* domain (upstream `endpoints/federation/*.ts` @ ea2c3aed).
//!
//! Implemented here (all `requireCredential: false`, POST like the other
//! self-contained arena arms):
//! - `federation/instances` — the paged `instance` list with the upstream
//!   host/blocked/notResponding/suspended/silenced/federating/subscribing/
//!   publishing filters + the sort enum (instances.ts).
//! - `federation/show-instance` — `findOneBy({ host })` → `FederationInstance`
//!   or `null` (204 via ApiCallService.send).
//! - `federation/stats` — top follower/following instances + the remote-edge
//!   totals (stats.ts).
//! - `federation/{followers,following}` — host-filtered `following` edges
//!   with the relation-visibility gate, packed `{ populateFollowee: true }`.
//! - `federation/users` — `user."host" = :host` rows packed
//!   `UserDetailedNotMe`.
//!
//! Skipped + documented: `federation/update-remote-user` (upstream
//! `ApPersonService.updatePerson` — an AP fetch lane with no db-only read).
//!
//! The instance pack mirrors `InstanceEntityService.pack`: `isSuspended` is
//! derived from `suspensionState != 'none'` OR the
//! `meta.deliverSuspendedSoftware` software gate; `isBlocked`/`isSilenced`/
//! `isMediaSilenced` are the `UtilityService` host-list checks; and
//! `moderationNote` is only exposed to moderators. Timestamps are epoch-ms
//! in `mk-db` and ride `ms_to_iso` here.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};
use mk_db::federation::{InstanceRow, MetaHostLists};

/// Names routed into this module (single source of truth for the dispatch arm).
pub(crate) const FEDERATION_ENDPOINTS: &[&str] = &[
    "federation/instances",
    "federation/show-instance",
    "federation/stats",
    "federation/followers",
    "federation/following",
    "federation/users",
];

/// The instances.ts sort enum (null/absent → id DESC).
const SORTS: &[&str] = &[
    "+pubSub",
    "-pubSub",
    "+notes",
    "-notes",
    "+users",
    "-users",
    "+following",
    "-following",
    "+followers",
    "-followers",
    "+firstRetrievedAt",
    "-firstRetrievedAt",
    "+latestRequestReceivedAt",
    "-latestRequestReceivedAt",
];

pub(crate) fn handles(name: &str) -> bool {
    FEDERATION_ENDPOINTS.contains(&name)
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_federation", error = %e, "db error: {context}");
    api_err(ApiError::internal_error())
}

// ---------------------------------------------------------------------------
// paramDef helpers
// ---------------------------------------------------------------------------

/// `{ type: 'string', nullable: true }` — absent/null → None.
fn opt_str(body: &Value, key: &str) -> Result<Option<String>, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(super::invalid_param(key, "expected a string or null")),
    }
}

/// `{ type: 'string' }` required.
fn required_str(body: &Value, key: &str) -> Result<String, Response> {
    match body.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        _ => Err(super::invalid_param(key, "expected a string")),
    }
}

/// `{ type: 'boolean', nullable: true }` — absent/null → None.
fn opt_bool(body: &Value, key: &str) -> Result<Option<bool>, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(super::invalid_param(key, "expected a boolean or null")),
    }
}

fn int_param(body: &Value, key: &str, default: i64, min: i64, max: i64) -> Result<i64, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => {
            let Some(n) = v.as_i64() else {
                return Err(super::invalid_param(key, "expected an integer"));
            };
            if n < min || n > max {
                return Err(super::invalid_param(
                    key,
                    &format!("must be >= {min} and <= {max}"),
                ));
            }
            Ok(n)
        }
    }
}

fn id_param(body: &Value, key: &str) -> Result<Option<String>, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if super::is_alnum_id(s) => Ok(Some(s.clone())),
        Some(_) => Err(super::invalid_param(key, "expected an alphanumeric id")),
    }
}

fn date_param(body: &Value, key: &str) -> Result<Option<i64>, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_i64() {
            Some(ms) => Ok(Some(ms)),
            None => Err(super::invalid_param(key, "expected an integer (ms)")),
        },
    }
}

/// `sqlLikeEscape` — backslash-escape LIKE metacharacters.
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\\' || c == '%' || c == '_' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------------------
// InstanceEntityService pack
// ---------------------------------------------------------------------------

/// `UtilityService.isBlockedHost` / `isSilencedHost`: suffix match
/// (`.host`.endsWith(`.entry`)).
fn host_in_suffix_list(list: &[String], host: &str) -> bool {
    let host = host.to_lowercase();
    list.iter()
        .any(|x| format!(".{host}").ends_with(&format!(".{}", x.to_lowercase())))
}

/// `UtilityService.isMediaSilencedHost`: exact match.
fn host_in_exact_list(list: &[String], host: &str) -> bool {
    let host = host.to_lowercase();
    list.iter().any(|x| host == x.to_lowercase())
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().trim_start_matches('v');
    let core = core.split(|c| c == '-' || c == '+').next()?;
    let mut it = core.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next().unwrap_or("0").parse().ok()?;
    let patch = it.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

fn cmp_version(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    Some(parse_version(a)?.cmp(&parse_version(b)?))
}

/// Minimal `semver.satisfies(version, range, { includePrerelease: true })`
/// subset: `*`, exact, whitespace-joined comparators (`>=`, `<=`, `>`, `<`,
/// `=`), `||` alternatives, and `^`/`~` approximated as `>=`. The meta
/// `deliverSuspendedSoftware` list is empty by default; no endpoint in this
/// slice writes it.
fn version_satisfies(version: &str, range: &str) -> bool {
    let range = range.trim();
    if range.is_empty() || range == "*" {
        return true;
    }
    range.split("||").any(|alt| {
        alt.split_whitespace().all(|token| {
            let (op, rest) = if let Some(r) = token.strip_prefix(">=") {
                (">=", r)
            } else if let Some(r) = token.strip_prefix("<=") {
                ("<=", r)
            } else if let Some(r) = token.strip_prefix('>') {
                (">", r)
            } else if let Some(r) = token.strip_prefix('<') {
                ("<", r)
            } else if let Some(r) = token.strip_prefix('=') {
                ("=", r)
            } else if let Some(r) = token.strip_prefix('^') {
                ("^", r)
            } else if let Some(r) = token.strip_prefix('~') {
                ("~", r)
            } else {
                ("=", token)
            };
            let Some(ord) = cmp_version(version, rest) else {
                return false;
            };
            use std::cmp::Ordering::{Equal, Greater, Less};
            match op {
                ">=" => matches!(ord, Greater | Equal),
                "<=" => matches!(ord, Less | Equal),
                ">" => ord == Greater,
                "<" => ord == Less,
                "=" => ord == Equal,
                // caret/tilde approximated as the lower bound.
                "^" | "~" => matches!(ord, Greater | Equal),
                _ => false,
            }
        })
    })
}

/// `UtilityService.isDeliverSuspendedSoftware`.
fn software_suspended(meta: &MetaHostLists, row: &InstanceRow) -> bool {
    let Some(name) = row.software_name.as_deref() else {
        return false;
    };
    let Some(entries) = meta.deliver_suspended_software.as_array() else {
        return false;
    };
    entries.iter().any(|e| {
        if e.get("software").and_then(|v| v.as_str()) != Some(name) {
            return false;
        }
        let Some(range) = e.get("versionRange").and_then(|v| v.as_str()) else {
            return false;
        };
        match row.software_version.as_deref() {
            None => range.trim() == "*",
            Some(v) => version_satisfies(v, range),
        }
    })
}

/// `InstanceEntityService.pack(instance, me)`.
fn pack_instance(row: &InstanceRow, meta: &MetaHostLists, moderator: bool) -> Value {
    let is_software_suspended = software_suspended(meta, row);
    let is_suspended = row.suspension_state != "none" || is_software_suspended;
    let suspension_state = if row.suspension_state == "none" && is_software_suspended {
        "softwareSuspended"
    } else {
        row.suspension_state.as_str()
    };
    json!({
        "id": row.id,
        "firstRetrievedAt": mk_core::services::signup::ms_to_iso(row.first_retrieved_at_ms),
        "host": row.host,
        "usersCount": row.users_count,
        "notesCount": row.notes_count,
        "followingCount": row.following_count,
        "followersCount": row.followers_count,
        "isNotResponding": row.is_not_responding,
        "isSuspended": is_suspended,
        "suspensionState": suspension_state,
        "isBlocked": host_in_suffix_list(&meta.blocked_hosts, &row.host),
        "softwareName": row.software_name,
        "softwareVersion": row.software_version,
        "openRegistrations": row.open_registrations,
        "name": row.name,
        "description": row.description,
        "maintainerName": row.maintainer_name,
        "maintainerEmail": row.maintainer_email,
        "isSilenced": host_in_suffix_list(&meta.silenced_hosts, &row.host),
        "isMediaSilenced": host_in_exact_list(&meta.media_silenced_hosts, &row.host),
        "iconUrl": row.icon_url,
        "faviconUrl": row.favicon_url,
        "themeColor": row.theme_color,
        "infoUpdatedAt": row.info_updated_at_ms.map(mk_core::services::signup::ms_to_iso),
        "latestRequestReceivedAt": row
            .latest_request_received_at_ms
            .map(mk_core::services::signup::ms_to_iso),
        "moderationNote": if moderator { json!(row.moderation_note) } else { Value::Null },
    })
}

// ---------------------------------------------------------------------------
// endpoints
// ---------------------------------------------------------------------------

pub(crate) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = super::resolve_me(pool, token.as_deref()).await;
    let meta = match mk_db::federation::meta_host_lists(pool).await {
        Ok(m) => m,
        Err(e) => return db_err("meta_host_lists", e),
    };
    match name {
        "federation/instances" => instances(pool, body, me_id.as_deref(), &meta).await,
        "federation/show-instance" => show_instance(pool, body, me_id.as_deref(), &meta).await,
        "federation/stats" => stats(pool, body, me_id.as_deref(), &meta).await,
        "federation/followers" => edges(state, pool, true, body, me_id.as_deref()).await,
        "federation/following" => edges(state, pool, false, body, me_id.as_deref()).await,
        "federation/users" => users(state, pool, body, me_id.as_deref()).await,
        _ => api_err(ApiError::unknown_endpoint()),
    }
}

async fn instances(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: Option<&str>,
    meta: &MetaHostLists,
) -> Response {
    let host = match opt_str(body, "host") {
        Ok(v) => v,
        Err(r) => return r,
    };
    let blocked = match opt_bool(body, "blocked") {
        Ok(v) => v,
        Err(r) => return r,
    };
    let not_responding = match opt_bool(body, "notResponding") {
        Ok(v) => v,
        Err(r) => return r,
    };
    let suspended = match opt_bool(body, "suspended") {
        Ok(v) => v,
        Err(r) => return r,
    };
    let silenced = match opt_bool(body, "silenced") {
        Ok(v) => v,
        Err(r) => return r,
    };
    let federating = match opt_bool(body, "federating") {
        Ok(v) => v,
        Err(r) => return r,
    };
    let subscribing = match opt_bool(body, "subscribing") {
        Ok(v) => v,
        Err(r) => return r,
    };
    let publishing = match opt_bool(body, "publishing") {
        Ok(v) => v,
        Err(r) => return r,
    };
    let sort = match opt_str(body, "sort") {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Some(s) = &sort {
        if !SORTS.contains(&s.as_str()) {
            return super::invalid_param("sort", "invalid sort value");
        }
    }
    let limit = match int_param(body, "limit", 30, 1, 100) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let offset = match int_param(body, "offset", 0, 0, i64::MAX) {
        Ok(v) => v,
        Err(r) => return r,
    };

    // instances.ts: `ps.host` truthy → LIKE %lowercased-escaped% (empty string
    // is falsy → no filter).
    let host_like = host
        .filter(|h| !h.is_empty())
        .map(|h| format!("%{}%", escape_like(&h.to_lowercase())));
    let q = mk_db::federation::InstanceListQuery {
        host_like: host_like.as_deref(),
        blocked,
        blocked_hosts: &meta.blocked_hosts,
        not_responding,
        suspended,
        silenced,
        silenced_hosts: &meta.silenced_hosts,
        federating,
        subscribing,
        publishing,
        sort: sort.as_deref(),
        limit,
        offset,
    };
    let rows = match mk_db::federation::list_instances(pool, &q).await {
        Ok(r) => r,
        Err(e) => return db_err("list_instances", e),
    };
    let moderator = super::viewer_is_moderator(pool, me_id).await;
    let out: Vec<Value> = rows
        .iter()
        .map(|r| pack_instance(r, meta, moderator))
        .collect();
    (StatusCode::OK, Json(json!(out))).into_response()
}

async fn show_instance(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: Option<&str>,
    meta: &MetaHostLists,
) -> Response {
    let host = match required_str(body, "host") {
        Ok(h) => h.to_lowercase(),
        Err(r) => return r,
    };
    let row = match mk_db::federation::find_instance_by_host(pool, &host).await {
        Ok(r) => r,
        Err(e) => return db_err("find_instance_by_host", e),
    };
    let Some(row) = row else {
        // ApiCallService.send: `null` → 204.
        return StatusCode::NO_CONTENT.into_response();
    };
    let moderator = super::viewer_is_moderator(pool, me_id).await;
    (StatusCode::OK, Json(pack_instance(&row, meta, moderator))).into_response()
}

async fn stats(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: Option<&str>,
    meta: &MetaHostLists,
) -> Response {
    let limit = match int_param(body, "limit", 10, 1, 100) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let st = match mk_db::federation::stats(pool, limit).await {
        Ok(s) => s,
        Err(e) => return db_err("federation_stats", e),
    };
    let moderator = super::viewer_is_moderator(pool, me_id).await;
    let got_sub: i64 = st
        .top_sub
        .iter()
        .map(|r| i64::from(r.followers_count))
        .sum();
    let got_pub: i64 = st
        .top_pub
        .iter()
        .map(|r| i64::from(r.following_count))
        .sum();
    let pack = |rows: &[InstanceRow]| -> Vec<Value> {
        rows.iter()
            .map(|r| pack_instance(r, meta, moderator))
            .collect()
    };
    let result = json!({
        "topSubInstances": pack(&st.top_sub),
        "otherFollowersCount": (st.all_sub_count - got_sub).max(0),
        "topPubInstances": pack(&st.top_pub),
        "otherFollowingCount": (st.all_pub_count - got_pub).max(0),
    });
    (StatusCode::OK, Json(result)).into_response()
}

/// `federation/{followers,following}` — `followers=true` is the followers
/// endpoint (owner = followee, `followeeHost = host`).
async fn edges(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    followers: bool,
    body: &Value,
    me_id: Option<&str>,
) -> Response {
    let host = match required_str(body, "host") {
        Ok(h) => h,
        Err(r) => return r,
    };
    let (since_id, until_id, since_date, until_date) = match (
        id_param(body, "sinceId"),
        id_param(body, "untilId"),
        date_param(body, "sinceDate"),
        date_param(body, "untilDate"),
    ) {
        (Ok(a), Ok(b), Ok(c), Ok(d)) => (a, b, c, d),
        (Err(r), _, _, _) | (_, Err(r), _, _) | (_, _, Err(r), _) | (_, _, _, Err(r)) => {
            return r;
        }
    };
    let limit = match int_param(body, "limit", 10, 1, 100) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let (from_id, until_id, desc) = super::ff_vis::paginate_bounds(
        since_id.as_deref(),
        until_id.as_deref(),
        since_date,
        until_date,
    );
    let moderator = super::viewer_is_moderator(pool, me_id).await;
    let rows = match mk_db::federation::list_edges_by_host(
        pool,
        followers,
        &host,
        from_id.as_deref(),
        until_id.as_deref(),
        desc,
        limit as u32,
        me_id,
        moderator,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return db_err("list_edges_by_host", e),
    };
    let mut out = Vec::with_capacity(rows.len());
    for edge in rows {
        match pack_following(state, pool, edge, me_id).await {
            Ok(v) => out.push(v),
            Err(r) => return r,
        }
    }
    (StatusCode::OK, Json(json!(out))).into_response()
}

/// `FollowingEntityService.pack(..., { populateFollowee: true })`.
async fn pack_following(
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
    let followee = mk_db::user::find_user_by_id(pool, &edge.followee_id)
        .await
        .map_err(|e| db_err("pack_following find_user_by_id", e))?;
    if let Some(row) = followee {
        item["followee"] = super::ff_vis::pack_edge_user(state, pool, row, me_id).await?;
    }
    Ok(item)
}

/// `federation/users` — host rows packed `UserDetailedNotMe`.
async fn users(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: Option<&str>,
) -> Response {
    let host = match required_str(body, "host") {
        Ok(h) => h,
        Err(r) => return r,
    };
    let (since_id, until_id, since_date, until_date) = match (
        id_param(body, "sinceId"),
        id_param(body, "untilId"),
        date_param(body, "sinceDate"),
        date_param(body, "untilDate"),
    ) {
        (Ok(a), Ok(b), Ok(c), Ok(d)) => (a, b, c, d),
        (Err(r), _, _, _) | (_, Err(r), _, _) | (_, _, Err(r), _) | (_, _, _, Err(r)) => {
            return r;
        }
    };
    let limit = match int_param(body, "limit", 10, 1, 100) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let (from_id, until_id, desc) = super::ff_vis::paginate_bounds(
        since_id.as_deref(),
        until_id.as_deref(),
        since_date,
        until_date,
    );
    let ids = match mk_db::federation::list_user_ids_by_host(
        pool,
        &host,
        from_id.as_deref(),
        until_id.as_deref(),
        desc,
        limit as u32,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return db_err("list_user_ids_by_host", e),
    };
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let row = match mk_db::user::find_user_by_id(pool, &id).await {
            Ok(r) => r,
            Err(e) => return db_err("federation_users find_user_by_id", e),
        };
        let Some(row) = row else { continue };
        match super::ff_vis::pack_edge_user(state, pool, row, me_id).await {
            Ok(v) => out.push(v),
            Err(r) => return r,
        }
    }
    (StatusCode::OK, Json(json!(out))).into_response()
}
