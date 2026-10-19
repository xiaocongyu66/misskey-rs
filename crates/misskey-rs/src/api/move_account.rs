//! arena-move: account migration + the prohibitMoved dispatch gate
//! (upstream endpoints/i/move.ts + AccountMoveService.ts local slice +
//! ApiCallService.ts L421-430 + endpoints/mute/list.ts + endpoints/
//! blocking/list.ts).
//!
//! Slice shape (determined by what test/e2e/move.ts actually asserts):
//! - LOCAL-to-LOCAL moves only. RemoteUserResolve/ApPerson fetch lanes have
//!   no federation path in misskey-rs, so a `host` outside the configured
//!   one fails resolution with NO_SUCH_USER — exactly what the pinned
//!   '@syuilo@example.com' row asserts (400 fcd2eef9).
//! - i/move's updatePerson/resolvePerson refresh of the destination is a
//!   no-op for local users (ApPersonService.updatePerson L497
//!   `isUriLocal` skip + resolvePerson resolving through fetchPerson's
//!   local shortcut), so the resolved row IS the local row.
//! - The SystemAccountService('proxy') exclusion in the follow-copy scan is
//!   a no-op here (no system account rows exist on "user").
//! - antennaService.onMoveAccount (acct-string match inside antenna.users)
//!   has no observable in the pinned suite (alice's antenna has users: [])
//!   and antenna.users matching rides a world without remote accts — left
//!   as the documented divergence; everything the census asserts is covered.
//! - AP deliver legs (renderPerson/Move activity, deliverToFollowers,
//!   relays) likewise have no lane — collisions are irrelevant to the
//!   pinned assertions.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};
use mk_types::api_error::ApiErrorKind;

/// Endpoint names carrying `prohibitMoved: true` in their upstream meta
/// (server/api/endpoints/** — the 60-file set; ApiCallService L421 checks
/// `ep.meta.prohibitMoved` for the credential user and answers 403
/// YOUR_ACCOUNT_MOVED BEFORE paramDef validation or any endpoint body, so a
/// moved account's `{}`-param probes over the 42-name each-matrix + the
/// antennas/update / drive/files/create / i/move specials all land here).
pub(crate) const PROHIBIT_MOVED: &[&str] = &[
    "antennas/create",
    "antennas/remove-note",
    "antennas/update",
    "channels/create",
    "channels/favorite",
    "channels/follow",
    "channels/mute/create",
    "channels/mute/delete",
    "channels/mute/list",
    "channels/unfavorite",
    "channels/unfollow",
    "chat/messages/create-to-room",
    "chat/messages/create-to-user",
    "chat/rooms/create",
    "chat/rooms/invitations/create",
    "clips/add-note",
    "clips/create",
    "clips/favorite",
    "clips/remove-note",
    "clips/unfavorite",
    "clips/update",
    "drive/files/create",
    "drive/files/upload-from-url",
    "flash/create",
    "flash/like",
    "flash/unlike",
    "flash/update",
    "following/create",
    "gallery/posts/create",
    "gallery/posts/like",
    "gallery/posts/unlike",
    "gallery/posts/update",
    "i/claim-achievement",
    "i/import-antennas",
    "i/import-blocking",
    "i/import-following",
    "i/import-muting",
    "i/import-user-lists",
    "i/move",
    "i/pin",
    "mute/create",
    "notes/create",
    "notes/drafts/count",
    "notes/drafts/create",
    "notes/drafts/delete",
    "notes/drafts/list",
    "notes/drafts/update",
    "notes/favorites/create",
    "notes/polls/vote",
    "notes/reactions/create",
    "pages/create",
    "pages/like",
    "pages/unlike",
    "pages/update",
    "renote-mute/create",
    "users/lists/create",
    "users/lists/create-from-public",
    "users/lists/pull",
    "users/lists/push",
    "users/lists/update-membership",
];

pub(crate) fn is_prohibit_moved(name: &str) -> bool {
    PROHIBIT_MOVED.contains(&name)
}

/// Names this module owns end to end. `i/move` rides its own arm (the
/// prohibitMoved gate handles the moved caller); mute/list + blocking/list
/// are the two list endpoints the pinned suite reads after the move.
pub(crate) fn handles(name: &str) -> bool {
    matches!(name, "i/move" | "mute/list" | "blocking/list")
}

/// `hadRequest`-agnostic list param line shared by mute/list + blocking/list
/// (QueryService.makePaginationQuery: sinceId/untilId/sinceDate/untilDate +
/// limit 1..=100 default 30 — the schema default fills before the handler).
struct ListParams {
    limit: u32,
    since_id: Option<String>,
    until_id: Option<String>,
    since_date: Option<i64>,
    until_date: Option<i64>,
}

fn list_params(body: &Value) -> Result<ListParams, Response> {
    let limit = match body.get("limit") {
        None | Some(Value::Null) => 30,
        Some(Value::Number(n)) => {
            let v = n.as_i64().unwrap_or(-1);
            if !(1..=100).contains(&v) {
                return Err(schema_invalid_param(
                    "limit",
                    "must be integer between 1 and 100",
                ));
            }
            v as u32
        }
        Some(_) => return Err(schema_invalid_param("limit", "must be integer")),
    };
    let id = |k: &str| -> Result<Option<String>, Response> {
        match body.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(schema_invalid_param(k, "must be id")),
        }
    };
    let date = |k: &str| -> Result<Option<i64>, Response> {
        match body.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(n)) => Ok(n.as_i64()),
            Some(_) => Err(schema_invalid_param(k, "must be integer")),
        }
    };
    Ok(ListParams {
        limit,
        since_id: id("sinceId")?,
        until_id: id("untilId")?,
        since_date: date("sinceDate")?,
        until_date: date("untilDate")?,
    })
}

/// endpoint-base.ts L55-67: paramDef (schema) validation rejects with
/// INVALID_PARAM **3d81ceae** — a DIFFERENT id from the ApiCallService
/// string-cast INVALID_PARAM (0b5f1631) that ApiError::invalid_param pins.
pub(crate) fn schema_invalid_param(param: &str, reason: &str) -> Response {
    api_err(
        ApiError::new(
            400,
            ApiErrorKind::Client,
            "INVALID_PARAM",
            "3d81ceae-475f-4600-b2a8-2bc116157532",
            "Invalid param.",
        )
        .with_info(json!({ "param": param, "reason": reason })),
    )
}

/// ApiCallService L421-430 — `kind: 'permission'`, httpStatusCode 403.
pub(crate) fn moved_forbidden() -> Response {
    api_err(ApiError::new(
        403,
        ApiErrorKind::Permission,
        "YOUR_ACCOUNT_MOVED",
        "56f20ec9-fd06-4fa5-841b-edd6d7d4fa31",
        "You have moved your account.",
    ))
}

/// The central prohibitMoved barrier: resolve the credential; when its user
/// carries `movedToUri`, answer the 403 before any endpoint body or paramDef
/// validation can run. An absent/unresolvable token falls through — the
/// downstream arm produces the upstream-ordered 401 (CREDENTIAL_REQUIRED /
/// AUTHENTICATION_FAILED) exactly like ApiCallService's own auth stage.
pub(crate) async fn moved_gate(
    pool: &mk_db::sqlx::PgPool,
    token: Option<&str>,
) -> Result<(), Response> {
    let Some(me_id) = super::resolve_me(pool, token).await else {
        return Ok(());
    };
    if matches!(
        mk_db::move_account::get_moved_to_uri(pool, &me_id).await,
        Ok(Some(_))
    ) {
        return Err(moved_forbidden());
    }
    Ok(())
}

pub(crate) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    match name {
        "i/move" => move_route(state, pool, body, token).await,
        "mute/list" => mute_list(state, pool, body, token).await,
        "blocking/list" => blocking_list(state, pool, body, token).await,
        _ => api_err(ApiError::unknown_endpoint()),
    }
}

// ---------------------------------------------------------------------------
// mute/list + blocking/list (endpoints/mute/list.ts + blocking/list.ts) —
// the two post-move read lanes the pinned suite drives (`{}` params over the
// rows copied by the move)
// ---------------------------------------------------------------------------

/// MutingEntityService.pack / BlockingEntityService.pack: { id, createdAt
/// (from the aid), expiresAt (muting only), <partner>Id, <partner>: packed
/// UserDetailedNotMe }.
async fn pack_list_row(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    row: (String, String, Option<i64>),
    expires_key: Option<&str>,
    partner_key: &str,
) -> Result<Value, Response> {
    let (id, partner_id, expires_ms) = row;
    let partner = mk_db::user::find_user_by_id(pool, &partner_id)
        .await
        .map_err(|e| db_err("pack_list_row find_user", e))?;
    let mut created_ms: Option<i64> = None;
    if let Some(d) = mk_types::id::parse_aid_date(&id) {
        created_ms = Some(d);
    }
    let created_iso = created_ms
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default();
    let expires_at = match (expires_key, expires_ms) {
        (Some(_), Some(ms)) => json!(mk_core::services::signup::ms_to_iso(ms)),
        (Some(_), None) => json!(null),
        _ => Value::Null, // blocking pack carries no expiresAt at all
    };
    let mut out = json!({
        "id": id,
        "createdAt": created_iso,
    });
    if let Some(obj) = out.as_object_mut() {
        obj.insert(partner_key.to_string(), json!(partner_id));
        if expires_key.is_some() {
            obj.insert("expiresAt".to_string(), expires_at);
        }
        match partner {
            Some(p) => {
                let profile = mk_db::user::find_profile(pool, &p.id)
                    .await
                    .map_err(|e| db_err("pack_list_row find_profile", e))?;
                let ctx = mk_core::services::user_pack::InstanceCtx {
                    url: &state.config.url,
                    host: &state.config.host(),
                };
                let packed =
                    mk_core::services::user_pack::pack_inline(&p, &profile, false, None, ctx);
                obj.insert(partner_json_key(partner_key), packed);
            }
            None => {
                obj.insert(partner_json_key(partner_key), Value::Null);
            }
        }
    }
    Ok(out)
}

fn partner_json_key(partner_id_key: &str) -> String {
    // "muteeId"/"blockeeId" → "mutee"/"blockee"
    partner_id_key.trim_end_matches("Id").to_string()
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_move", error = %e, "db error: {context}");
    api_err(ApiError::internal_error())
}

async fn partner_list(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    token: Option<String>,
    table: &str,
    owner_col: &str,
    partner_col: &str,
    with_expiry: bool,
) -> Response {
    // requireCredential chain (social::require_checked_user parity: 401s then
    // the suspended 403)
    let me_id = match super::social::require_checked_user(pool, token.as_deref(), true).await {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let p = match list_params(body) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    // the unbacked createdAt date arm (see mk-db list_by_owner note)
    if p.since_date.is_some() || p.until_date.is_some() {
        return api_err(ApiError::internal_error());
    }
    let rows = match mk_db::move_account::list_by_owner(
        pool,
        table,
        owner_col,
        partner_col,
        &me_id,
        with_expiry,
        p.since_id.as_deref(),
        p.until_id.as_deref(),
        p.since_date,
        p.until_date,
        p.limit,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return db_err("partner_list", e),
    };
    let expires_key = if with_expiry { Some("expiresAt") } else { None };
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        match pack_list_row(state, pool, row, expires_key, partner_col).await {
            Ok(v) => out.push(v),
            Err(resp) => return resp,
        }
    }
    (StatusCode::OK, Json(Value::Array(out))).into_response()
}

async fn mute_list(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    token: Option<String>,
) -> Response {
    partner_list(
        state, pool, body, token, "muting", "muterId", "muteeId", true,
    )
    .await
}

async fn blocking_list(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    token: Option<String>,
) -> Response {
    partner_list(
        state,
        pool,
        body,
        token,
        "blocking",
        "blockerId",
        "blockeeId",
        false,
    )
    .await
}

// ---------------------------------------------------------------------------
// i/move (endpoints/i/move.ts) — the local-to-local migration endpoint
// ---------------------------------------------------------------------------

/// meta errors (endpoints/i/move.ts errors table) — pub(crate) so the
/// arena-move tail in tests.rs can pin the ids.
pub(crate) const NO_SUCH_USER_ID: &str = "fcd2eef9-a9b2-4c4f-8624-038099e90aa5";
pub(crate) const DESTINATION_ACCOUNT_FORBIDS_ID: &str = "b5c90186-4ab0-49c8-9bba-a1f766282ba4";
pub(crate) const NOT_ROOT_FORBIDDEN_ID: &str = "4362e8dc-731f-4ad8-a694-be2a88922a24";
pub(crate) const ALREADY_MOVED_ID: &str = "b234a14e-9ebe-4581-8000-074b3c215962";

fn no_such_user() -> Response {
    api_err(ApiError::new(
        400,
        ApiErrorKind::Client,
        "NO_SUCH_USER",
        NO_SUCH_USER_ID,
        "No such user.",
    ))
}

fn destination_account_forbids() -> Response {
    api_err(ApiError::new(
        400,
        ApiErrorKind::Client,
        "DESTINATION_ACCOUNT_FORBIDS",
        DESTINATION_ACCOUNT_FORBIDS_ID,
        "Destination account doesn't have proper 'Known As' alias, or has already moved.",
    ))
}

fn not_root_forbidden() -> Response {
    api_err(ApiError::new(
        400,
        ApiErrorKind::Client,
        "NOT_ROOT_FORBIDDEN",
        NOT_ROOT_FORBIDDEN_ID,
        "The root can't migrate.",
    ))
}

fn already_moved() -> Response {
    api_err(ApiError::new(
        400,
        ApiErrorKind::Client,
        "ALREADY_MOVED",
        ALREADY_MOVED_ID,
        "Account was already moved to another account.",
    ))
}

/// RemoteUserResolveService.resolveUser for the local slice: `host` absent or
/// equal to the configured host → the local row; anything else has no
/// federation resolve lane here → unresolvable (NO_SUCH_USER at the caller).
pub(crate) async fn resolve_acct_user(
    pool: &mk_db::sqlx::PgPool,
    username: &str,
    host: Option<&str>,
    config_host: &str,
) -> Result<Option<mk_db::user::LocalUserRow>, mk_db::sqlx::Error> {
    let local = match host {
        None => true,
        Some(h) => same_host(h, config_host),
    };
    if !local || username.is_empty() {
        return Ok(None);
    }
    mk_db::user::find_local_user_by_username(pool, &username.to_lowercase()).await
}

/// census rn39319126124: `Config.host()` carries the port (`127.0.0.1:3000`
/// — the URI host-token split used by the identicon/badge planes) while a
/// parsed acct NEVER carries one (`@bob@127.0.0.1` → host `127.0.0.1` like
/// `new URL(config.url).hostname`). RemoteUserResolveService.compare-config
/// parity therefore normalizes BOTH sides: strip the port, lowercase (the
/// JS side punycodes/lowercases hosts).
pub(crate) fn same_host(a: &str, b: &str) -> bool {
    let bare = |h: &str| -> String {
        h.split_once(':')
            .map(|(host, _)| host)
            .unwrap_or(h)
            .to_lowercase()
    };
    bare(a) == bare(b)
}

async fn move_route(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    token: Option<String>,
) -> Response {
    // requireCredential (secure: true endpoint): native 16-char tokens only.
    let Some(tok) = token.as_deref().filter(|t| t.len() == 16) else {
        return super::authentication_failed();
    };
    let me_id = match mk_db::user::find_local_user_id_by_token(pool, tok).await {
        Ok(v) => v,
        Err(e) => return db_err("move_route find_token", e),
    };
    let Some(me_id) = me_id else {
        return super::authentication_failed();
    };
    let me = match mk_db::user::find_user_by_id(pool, &me_id).await {
        Ok(r) => r,
        Err(e) => return db_err("move_route find_me", e),
    };
    let Some(me) = me else {
        return super::authentication_failed();
    };
    if me.is_suspended {
        return api_err(ApiError::new(
            403,
            ApiErrorKind::Permission,
            "YOUR_ACCOUNT_SUSPENDED",
            "a8c724b3-6e9c-4b46-b1a8-bc3ed6258370",
            "Your account has been suspended.",
        ));
    }

    // L98: `if (!ps.moveToAccount) throw noSuchUser` (missing or empty).
    let move_to_account = body.get("moveToAccount").and_then(|v| v.as_str());
    let move_to_account = match move_to_account {
        Some(s) if !s.is_empty() => s,
        _ => return no_such_user(),
    };
    // L100 root check (serverSettings.rootUserId === me.id).
    match mk_db::admin::root_user_id(pool).await {
        Ok(Some(root)) if root == me.id => return not_root_forbidden(),
        Ok(_) => {}
        Err(e) => return db_err("move_route root_user_id", e),
    }
    // L102 already-moved self gate.
    if me.moved_to_uri.is_some() {
        return already_moved();
    }

    // L105-110: Acct.parse + resolveUser (updatePerson/resolvePerson of the
    // local URI is a no-op for local destinations — module header).
    let Some((username, host)) = mk_core::services::move_account::parse_acct(move_to_account)
    else {
        return no_such_user();
    };
    let config_host = state.config.host();
    let dst = match resolve_acct_user(pool, &username, host.as_deref(), &config_host).await {
        Ok(u) => u,
        Err(e) => return db_err("move_route resolve_acct", e),
    };
    let Some(dst) = dst else {
        return no_such_user();
    };

    // L120-132: the destination must point BACK at us (substring
    // `knownAs.includes(fromUrl)` over its alsoKnownAs) and must not have
    // moved itself.
    let from_url = mk_core::services::move_account::local_user_uri(&state.config.url, &me.id);
    let dst_aliases =
        mk_core::services::move_account::unmarshal_aliases(dst.also_known_as.as_deref());
    let allowed =
        mk_core::services::move_account::aliases_admit_local_user(&dst_aliases, &from_url);
    if !allowed || dst.moved_to_uri.is_some() {
        return destination_account_forbids();
    }

    // ---- AccountMoveService.moveFromLocal ----
    let dst_uri = mk_core::services::move_account::local_user_uri(&state.config.url, &dst.id);
    let mut new_aliases =
        mk_core::services::move_account::unmarshal_aliases(me.also_known_as.as_deref());
    if !new_aliases.iter().any(|u| u == &dst_uri) {
        new_aliases.push(dst_uri.clone());
    }
    let new_alias_csv = mk_core::services::move_account::marshal_aliases(&new_aliases);
    if let Err(e) =
        mk_db::move_account::set_move_out(pool, &me.id, &dst_uri, Some(&new_alias_csv)).await
    {
        return db_err("move_route set_move_out", e);
    }

    // L105-112: delayed unfollow of the OLD account's own followings (10s in
    // NODE_ENV=test, else 24h) — queueService.createDelayedUnfollowJob.
    let followees = match mk_db::move_account::own_followings(pool, &me.id).await {
        Ok(v) => v,
        Err(e) => return db_err("move_route own_followings", e),
    };
    let queue = mk_queue::QueueHandle::new(pool.clone());
    let delay_secs: u64 = if state.config.test { 10 } else { 24 * 60 * 60 };
    for followee in &followees {
        let payload = json!({ "from": me.id, "to": followee });
        if let Err(e) = queue
            .enqueue_delayed(
                "relationship:unfollow",
                &payload,
                std::time::Duration::from_secs(delay_secs),
            )
            .await
        {
            return db_err("move_route enqueue_delayed_unfollow", e);
        }
    }

    // ---- postMoveProcess ----
    // copyBlocking (blockers of src re-block dst; skip existing dst blockers)
    let src_blockers = match mk_db::move_account::blockers_of(pool, &me.id).await {
        Ok(v) => v,
        Err(e) => return db_err("move_route blockers_of", e),
    };
    let dst_blockers = match mk_db::move_account::blockers_of(pool, &dst.id).await {
        Ok(v) => v,
        Err(e) => return db_err("move_route blockers_of[dst]", e),
    };
    for blocker in &src_blockers {
        if dst_blockers.contains(blocker) {
            continue;
        }
        let payload = json!({ "from": blocker, "to": dst.id });
        if let Err(e) = queue.enqueue("relationship:block", &payload).await {
            return db_err("move_route enqueue_block", e);
        }
    }

    // copyMutings (direct insert — no queue leg upstream; skip muters who
    // already mute dst indefinitely)
    let old_mutings = match mk_db::move_account::mutings_of(pool, &me.id).await {
        Ok(v) => v,
        Err(e) => return db_err("move_route mutings_of", e),
    };
    let dst_muters = match mk_db::move_account::indefinite_muters_of(pool, &dst.id).await {
        Ok(v) => v,
        Err(e) => return db_err("move_route indefinite_muters_of", e),
    };
    for (muter, expires_ms) in &old_mutings {
        if dst_muters.contains(muter) {
            continue;
        }
        if let Err(e) = mk_db::social::insert_muting(
            pool,
            &mk_types::id::gen_aid_now(),
            muter,
            &dst.id,
            *expires_ms,
        )
        .await
        {
            return db_err("move_route insert_muting[copy]", e);
        }
    }

    // copyRoles (preserveAssignmentOnMoveAccount only; AlreadyAssigned → skip)
    let roles = match mk_db::move_account::portable_role_assignments(pool, &me.id).await {
        Ok(v) => v,
        Err(e) => return db_err("move_route portable_role_assignments", e),
    };
    for (role_id, expires_ms) in &roles {
        match mk_db::move_account::role_assignment_exists(pool, role_id, &dst.id).await {
            Ok(true) => continue,
            Ok(false) => {}
            Err(e) => return db_err("move_route role_assignment_exists", e),
        }
        if let Err(e) = mk_db::move_account::insert_role_assignment(
            pool,
            &mk_types::id::gen_aid_now(),
            role_id,
            &dst.id,
            *expires_ms,
        )
        .await
        {
            return db_err("move_route insert_role_assignment", e);
        }
    }

    // updateLists (lists containing src gain dst; skip lists already holding
    // dst; withReplies NOT carried — column default false, upstream insert
    // payload omits it)
    let memberships = match mk_db::move_account::list_memberships_of(pool, &me.id).await {
        Ok(v) => v,
        Err(e) => return db_err("move_route list_memberships_of", e),
    };
    let dst_list_ids = match mk_db::move_account::list_ids_containing(pool, &dst.id).await {
        Ok(v) => v,
        Err(e) => return db_err("move_route list_ids_containing", e),
    };
    for (list_id, list_owner) in &memberships {
        if dst_list_ids.contains(list_id) {
            continue;
        }
        if let Err(e) = mk_db::lists::insert_membership(
            pool,
            &mk_types::id::gen_aid_now(),
            &dst.id,
            list_id,
            list_owner,
            false,
        )
        .await
        {
            return db_err("move_route insert_membership[copy]", e);
        }
    }

    // the follow-copy queue (local followers of src → follow dst)
    let followers = match mk_db::move_account::local_followers_of(pool, &me.id).await {
        Ok(v) => v,
        Err(e) => return db_err("move_route local_followers_of", e),
    };
    for follower in &followers {
        let payload = json!({ "from": follower, "to": dst.id, "withReplies": false });
        if let Err(e) = queue.enqueue("relationship:follow", &payload).await {
            return db_err("move_route enqueue_follow", e);
        }
    }

    // adjustFollowingCounts (old account counters zeroed; local followers'
    // followingCount -1; old followees' followersCount -1)
    if let Err(e) = mk_db::move_account::zero_move_counts(pool, &me.id).await {
        return db_err("move_route zero_move_counts", e);
    }
    let old_followees = followees.clone();
    if let Err(e) = mk_db::move_account::decrement_following_counts(pool, &followers).await {
        return db_err("move_route decrement_following_counts", e);
    }
    if let Err(e) = mk_db::move_account::decrement_followers_counts(pool, &old_followees).await {
        return db_err("move_route decrement_followers_counts", e);
    }

    // moveFromLocal returns the packed MeDetailed(includeSecrets) iObj.
    let row = match mk_db::user::find_user_by_id(pool, &me.id).await {
        Ok(r) => r,
        Err(e) => return db_err("move_route find_me[fresh]", e),
    };
    let Some(row) = row else {
        return api_err(ApiError::internal_error());
    };
    let profile = match mk_db::user::find_profile(pool, &row.id).await {
        Ok(p) => p,
        Err(e) => return db_err("move_route find_profile[fresh]", e),
    };
    let ctx = mk_core::services::user_pack::InstanceCtx {
        url: &state.config.url,
        host: &state.config.host(),
    };
    (
        StatusCode::OK,
        Json({
            let mut v = mk_core::services::user_pack::pack_inline(&row, &profile, true, None, ctx);
            super::apply_me_unread(pool, &mut v).await;
            super::apply_me_role_compat(&mut v);
            v
        }),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// the relationship-queue worker (RelationshipProcessorService follow/block
// arms + the delayed unfollow)
// ---------------------------------------------------------------------------

/// Boot wiring (main.rs): the in-process worker for the relationship kinds.
/// follow → mk_db follow_leg (moved-aware count increments), block → the
/// full UserBlockingService.block parity in mk_db::social::block, unfollow →
/// the moved-aware edge delete.
pub fn spawn_move_worker(pool: mk_db::sqlx::PgPool) {
    mk_queue::spawn_worker(
        pool.clone(),
        vec![
            "relationship:follow".to_string(),
            "relationship:block".to_string(),
            "relationship:unfollow".to_string(),
        ],
        std::sync::Arc::new(move |_queue, job| {
            let pool = pool.clone();
            Box::pin(async move {
                let from = job
                    .payload
                    .get("from")
                    .and_then(Value::as_str)
                    .map(|s| s.to_string());
                let to = job
                    .payload
                    .get("to")
                    .and_then(Value::as_str)
                    .map(|s| s.to_string());
                let (Some(from), Some(to)) = (from, to) else {
                    return Err(format!("relationship job {}: malformed payload", job.id));
                };
                match job.kind.as_str() {
                    "relationship:follow" => {
                        let with_replies = job
                            .payload
                            .get("withReplies")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        mk_db::move_account::follow_leg(
                            &pool,
                            &mk_types::id::gen_aid_now(),
                            &from,
                            &to,
                            with_replies,
                        )
                        .await
                        .map_err(|e| format!("follow leg: {e}"))
                    }
                    "relationship:block" => {
                        mk_db::social::block(&pool, &mk_types::id::gen_aid_now(), &from, &to)
                            .await
                            .map_err(|e| format!("block leg: {e}"))
                    }
                    "relationship:unfollow" => mk_db::move_account::unfollow_leg(&pool, &from, &to)
                        .await
                        .map_err(|e| format!("unfollow leg: {e}")),
                    other => Err(format!("unknown relationship kind: {other}")),
                }
            })
        }),
    );
}
