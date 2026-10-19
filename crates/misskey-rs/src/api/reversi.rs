//! arena-reversi: the reversi game domain (upstream
//! server/api/endpoints/reversi/{match,cancel-match,invitations,games,
//! show-game,surrender,verify}.ts + core/ReversiService.ts +
//! ReversiGameEntityService packDetail/packLite). Self-contained module on
//! the two-factor/delete_account-arm pattern: own auth chain, body
//! pre-parsed and token pre-extracted by the reversi dispatch arm in mod.rs.
//! db paths in mk-db/src/reversi.rs (reversi_game + reversi_matching ship in
//! the 0001 baseline — no 0007; baseline census rn39315268103: the lone
//! reversi-game.ts row 404'd POST /api/reversi/match unknown-endpoint).
//!
//! DEVIATIONS (documented): GlobalEventService.publishReversiStream
//! ('invited' on a pending invite / 'matched' on accept) has no port-side
//! consumer — the census never opens a Stream (no WS reversi channel here);
//! the redis game cache folds into the db row (single source of truth); the
//! `reversi:matchAny` waiting room is the in-process MATCH_ANY registry (the
//! upstream redis zset with expire 15 NX); the random-pick of accepted
//! matchAny invitations takes the newest (same invitation set, deterministic
//! port); startGame/putStone/checkTimeout stay unported — the e2e only
//! models invites + game creation (the ready/put legs ride the WS game
//! channel upstream).

use std::sync::Mutex;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

use mk_db::reversi::ReversiGameRow;

/// Endpoint names owned by this module (all POST-only, upstream /api/reversi/*).
pub(super) fn handles(name: &str) -> bool {
    matches!(
        name,
        "reversi/match"
            | "reversi/cancel-match"
            | "reversi/games"
            | "reversi/show-game"
            | "reversi/invitations"
            | "reversi/surrender"
            | "reversi/verify"
    )
}

/// Entry from the reversi dispatch arm in mod.rs.
pub(super) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
) -> Response {
    match name {
        "reversi/match" => match_(state, body, token).await,
        "reversi/cancel-match" => cancel_match(state, body, token).await,
        "reversi/invitations" => invitations(state, token).await,
        "reversi/games" => games(state, body, token).await,
        "reversi/show-game" => show_game(state, body, token.as_deref()).await,
        "reversi/surrender" => surrender(state, body, token).await,
        _ => verify(state, body).await,
    }
}

// ==== error envelopes =======================================================

fn credential_required() -> Response {
    api_err(ApiError::new(
        401,
        mk_types::api_error::ApiErrorKind::Client,
        "CREDENTIAL_REQUIRED",
        "1384574d-a912-4b81-8601-c7b1c4085df1",
        "Credential required.",
    ))
}

fn authentication_failed() -> Response {
    api_err(ApiError::new(
        401,
        mk_types::api_error::ApiErrorKind::Client,
        "AUTHENTICATION_FAILED",
        "b0a7f5f8-dc2f-4171-b91f-de88ad238e14",
        "Authentication failed. Please ensure your token is correct.",
    ))
}

fn account_suspended() -> Response {
    api_err(ApiError::new(
        403,
        mk_types::api_error::ApiErrorKind::Permission,
        "YOUR_ACCOUNT_SUSPENDED",
        "a8c724b3-6e9c-4b46-b1a8-bc3ed6258370",
        "Your account has been suspended.",
    ))
}

fn no_such_user() -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "NO_SUCH_USER",
        "0b4f0559-b484-4e31-9581-3f73cee89b28",
        "No such user.",
    ))
}

fn target_is_yourself() -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "TARGET_IS_YOURSELF",
        "96fd7bd6-d2bc-426c-a865-d055dcd2828e",
        "Target user is yourself.",
    ))
}

/// match/show-game/surrender/verify NO_SUCH_GAME metas — the error ids differ
/// per file. Plain ApiError throws → the 400 client envelope.
fn no_such_game(id: &'static str) -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "NO_SUCH_GAME",
        id,
        "No such game.",
    ))
}

fn already_ended() -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "ALREADY_ENDED",
        "6c2ad4a6-cbf1-4a5b-b187-b772826cfc6d",
        "That game has already ended.",
    ))
}

fn access_denied() -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "ACCESS_DENIED",
        "6e04164b-a992-4c93-8489-2123069973e1",
        "Access denied.",
    ))
}

fn invalid_param() -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "INVALID_PARAM",
        "3d81ceae-475f-4600-b2a8-2bc116157532",
        "Invalid param.",
    ))
}

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

// ==== auth chain ============================================================

/// ApiCallService.requireCredential chain (the two_factor::resolve_me
/// precedent): missing credential → CREDENTIAL_REQUIRED; non-native/
/// unresolvable token → AUTHENTICATION_FAILED; suspended →
/// YOUR_ACCOUNT_SUSPENDED.
async fn resolve_me(
    state: &AppState,
    token: Option<&str>,
) -> Result<(mk_db::sqlx::PgPool, String), Response> {
    let Some(pool) = &state.pool else {
        return Err(internal());
    };
    let Some(token) = token else {
        return Err(credential_required());
    };
    if token.len() != 16 {
        return Err(authentication_failed());
    }
    let me_id = match mk_db::user::find_local_user_id_by_token(pool, token).await {
        Ok(Some(id)) => id,
        Ok(None) => return Err(authentication_failed()),
        Err(e) => {
            tracing::error!(target: "dbg_reversi", error = %e, "find_local_user_id_by_token");
            return Err(internal());
        }
    };
    match mk_db::user::find_user_by_id(pool, &me_id).await {
        Ok(Some(row)) => {
            if row.is_suspended {
                return Err(account_suspended());
            }
        }
        Ok(None) => return Err(authentication_failed()),
        Err(e) => {
            tracing::error!(target: "dbg_reversi", error = %e, "find_user_by_id");
            return Err(internal());
        }
    }
    Ok((pool.clone(), me_id))
}

/// Endpoints with requireCredential: false (games/show-game) or none at all
/// (verify): the token only resolves when PROVIDED (ApiCallService still
/// authenticates a supplied session even on open endpoints).
async fn resolve_me_optional(
    state: &AppState,
    token: Option<&str>,
) -> Result<(mk_db::sqlx::PgPool, Option<String>), Response> {
    let Some(token) = token else {
        let pool = state.pool.clone().ok_or_else(internal)?;
        return Ok((pool, None));
    };
    let (pool, me_id) = resolve_me(state, Some(token)).await?;
    Ok((pool, Some(me_id)))
}

// ==== the matchAny waiting room (reversi:matchAny zset parity) ==============

struct MatchAnyEntry {
    user_id: String,
    no_irregular: bool,
    /// upstream zset score (Date.now()) — expire 15 NX parity
    at_ms: u64,
}

/// The matchAny waiting list with the redis zset's 15s expiry parity; owned
/// by this module (single server process, no db reader).
static MATCH_ANY: Mutex<Vec<MatchAnyEntry>> = Mutex::new(Vec::new());

const MATCH_ANY_TTL_MS: u64 = 15_000;

fn match_any_push(user_id: &str, no_irregular: bool) {
    let now = crate::api::now_ms();
    let mut guard = MATCH_ANY.lock().unwrap();
    guard.retain(|e| now.saturating_sub(e.at_ms) < MATCH_ANY_TTL_MS);
    // zset members unique: my entry refreshes the score
    guard.retain(|e| e.user_id != user_id);
    guard.push(MatchAnyEntry {
        user_id: user_id.to_string(),
        no_irregular,
        at_ms: now,
    });
}

fn match_any_take_for(me_id: &str) -> Option<MatchAnyEntry> {
    let now = crate::api::now_ms();
    let mut guard = MATCH_ANY.lock().unwrap();
    guard.retain(|e| now.saturating_sub(e.at_ms) < MATCH_ANY_TTL_MS);
    // `matchings.filter(id => !id.startsWith(me.id))` — never match myself
    let idx = guard.iter().position(|e| e.user_id != me_id)?;
    let entry = guard.remove(idx);
    // upstream zrem clears BOTH parties' entries + the option flags
    guard.retain(|e| e.user_id != me_id);
    Some(entry)
}

fn match_any_cancel(me_id: &str) {
    let mut guard = MATCH_ANY.lock().unwrap();
    guard.retain(|e| e.user_id != me_id);
}

// ==== ReversiGameEntityService packs ========================================

fn iso_ms_opt(ms: Option<i64>) -> Value {
    match ms {
        Some(v) => Value::String(mk_core::services::signup::ms_to_iso(v)),
        None => Value::Null,
    }
}

/// UserEntityService.pack(user) — the user1/user2 seats (UserLite; pack runs
/// without a `me` hint and without detail).
async fn pack_user_lite(state: &AppState, pool: &mk_db::sqlx::PgPool, user_id: &str) -> Value {
    let row = match mk_db::user::find_user_by_id(pool, user_id).await {
        Ok(Some(r)) => r,
        _ => return json!({ "id": user_id }),
    };
    let profile = match mk_db::user::find_profile(pool, user_id).await {
        Ok(p) => p,
        Err(_) => return json!({ "id": user_id }),
    };
    let ctx = mk_core::services::user_pack::InstanceCtx {
        url: &state.config.url,
        host: &state.config.host(),
    };
    mk_core::services::user_pack::user_lite_value(&row, &profile, ctx)
}

/// `packDetail` — the game + UserLite seats + logs/map/form fields.
async fn pack_game_detail(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    game: &ReversiGameRow,
) -> Value {
    let user1 = pack_user_lite(state, pool, &game.user1_id).await;
    let user2 = pack_user_lite(state, pool, &game.user2_id).await;
    let winner = winner_value(game, &user1, &user2);
    json!({
        "id": game.id.clone(),
        "createdAt": mk_core::services::signup::ms_to_iso(
            mk_types::id::parse_aid_date(&game.id).unwrap_or(0)),
        "startedAt": iso_ms_opt(game.started_ms),
        "endedAt": iso_ms_opt(game.ended_ms),
        "isStarted": game.is_started,
        "isEnded": game.is_ended,
        "form1": game.form1.clone(),
        "form2": game.form2.clone(),
        "user1Ready": game.user1_ready,
        "user2Ready": game.user2_ready,
        "user1Id": game.user1_id.clone(),
        "user2Id": game.user2_id.clone(),
        "user1": user1,
        "user2": user2,
        "winnerId": game.winner_id.clone(),
        "winner": winner,
        "surrenderedUserId": game.surrendered_user_id.clone(),
        "timeoutUserId": game.timeout_user_id.clone(),
        "black": game.black,
        // assertBw guard: anything off ['random','1','2'] → 'random'
        "bw": bw_value(&game.bw),
        "isLlotheo": game.is_llotheo,
        "canPutEverywhere": game.can_put_everywhere,
        "loopedBoard": game.looped_board,
        "timeLimitForEachTurn": game.time_limit_for_each_turn,
        "noIrregularRules": game.no_irregular_rules,
        "logs": game.logs.clone(),
        "map": game.map.clone(),
    })
}

/// `packLite` — the list variant: no form1/form2/logs/map.
async fn pack_game_lite(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    game: &ReversiGameRow,
) -> Value {
    let user1 = pack_user_lite(state, pool, &game.user1_id).await;
    let user2 = pack_user_lite(state, pool, &game.user2_id).await;
    let winner = winner_value(game, &user1, &user2);
    json!({
        "id": game.id.clone(),
        "createdAt": mk_core::services::signup::ms_to_iso(
            mk_types::id::parse_aid_date(&game.id).unwrap_or(0)),
        "startedAt": iso_ms_opt(game.started_ms),
        "endedAt": iso_ms_opt(game.ended_ms),
        "isStarted": game.is_started,
        "isEnded": game.is_ended,
        "user1Id": game.user1_id.clone(),
        "user2Id": game.user2_id.clone(),
        "user1": user1,
        "user2": user2,
        "winnerId": game.winner_id.clone(),
        "winner": winner,
        "surrenderedUserId": game.surrendered_user_id.clone(),
        "timeoutUserId": game.timeout_user_id.clone(),
        "black": game.black,
        "bw": bw_value(&game.bw),
        "isLlotheo": game.is_llotheo,
        "canPutEverywhere": game.can_put_everywhere,
        "loopedBoard": game.looped_board,
        "timeLimitForEachTurn": game.time_limit_for_each_turn,
        "noIrregularRules": game.no_irregular_rules,
    })
}

fn bw_value(bw: &str) -> String {
    if matches!(bw, "random" | "1" | "2") {
        bw.to_string()
    } else {
        "random".to_string()
    }
}

/// `winner: game.winnerId ? [user1, user2].find(u => u.id === winnerId)! : null`
/// (packDetail L61 — the find is bang'd; a dangling winnerId degrades to null
/// rather than panic in the port).
fn winner_value(game: &ReversiGameRow, user1: &Value, user2: &Value) -> Value {
    match &game.winner_id {
        Some(w) if &game.user1_id == w => user1.clone(),
        Some(w) if &game.user2_id == w => user2.clone(),
        _ => Value::Null,
    }
}

/// ReversiService.matched — the game row the accept path inserts (default
/// eighteight map, bw 'random', logs []). `state` rides the signature for
/// stream-event parity (publishReversiStream 'matched' — unported, see the
/// module header).
async fn matched(
    _state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    parent_id: &str,
    child_id: &str,
    no_irregular_rules: bool,
) -> Result<ReversiGameRow, Response> {
    let id = mk_types::id::gen_aid_now();
    mk_db::reversi::insert_game(
        pool,
        &id,
        parent_id,
        child_id,
        no_irregular_rules,
        mk_db::reversi::EIGHTEIGHT_MAP,
    )
    .await
    .map_err(|e| {
        tracing::error!(target: "dbg_reversi", error = %e, "insert_game");
        internal()
    })?;
    let game = mk_db::reversi::find_game(pool, &id)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_reversi", error = %e, "find_game");
            internal()
        })?
        .ok_or_else(internal)?;
    // publishReversiStream(parentId, 'matched', packDetail) — no WS lane in
    // this census (see module header)
    Ok(game)
}

/// The shared pack-and-send tail for the match accepts.
async fn send_game_detail(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    game: &ReversiGameRow,
) -> Response {
    let packed = pack_game_detail(state, pool, game).await;
    (StatusCode::OK, axum::Json(packed)).into_response()
}

// ==== endpoints =============================================================

/// `reversi/match` — match.ts: userId given → matchSpecificUser (invite or
/// cross-accept), null/absent → matchAnyUser (waiting-room rendezvous). A
/// null result (pending invitation) is a 204 with no body (the e2e asserts
/// status 204 + body null via the content-type probe).
async fn match_(state: &AppState, body: &Value, token: Option<String>) -> Response {
    let (pool, me_id) = match resolve_me(state, token.as_deref()).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let multiple = body
        .get("multiple")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let no_irregular = body
        .get("noIrregularRules")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let user_id = body.get("userId").and_then(Value::as_str);
    if let Some(uid) = user_id {
        if !crate::api::is_alnum_id(uid) {
            // paramDef format misskey:id — a malformed id can never resolve;
            // mirror the INVALID_PARAM 400 the endpoint base throws
            return invalid_param();
        }
        if uid == me_id {
            return target_is_yourself();
        }
        // GetterService.getUser — not found rides noSuchUser (match.ts maps
        // the 15348ddd getter error onto it); the target's suspension is NOT
        // checked upstream
        match mk_db::user::find_user_by_id(&pool, uid).await {
            Ok(Some(_)) => {}
            Ok(None) => return no_such_user(),
            Err(e) => {
                tracing::error!(target: "dbg_reversi", error = %e, "match target lookup");
                return internal();
            }
        }
        return match_specific(state, &pool, &me_id, uid, multiple).await;
    }
    match_any(state, &pool, &me_id, no_irregular, multiple).await
}

/// matchSpecificUser (ReversiService.ts L94-142): recent-unstarted probe →
/// accept a pending invite from the target → else register my invite (204).
async fn match_specific(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    me_id: &str,
    target_id: &str,
    multiple: bool,
) -> Response {
    // drop-schema shim: setup.e2e.ts's initTestDb drops the whole public
    // schema AFTER the server migrations (TypeORM dropSchema: !justBorrow) —
    // reversi_matching has no pinned entity, so the invite store must
    // re-ensure on first use (mk_queue_jobs/mk_export_blobs precedent).
    if let Err(e) = mk_db::reversi::ensure_matching_table(pool).await {
        tracing::error!(target: "dbg_reversi", error = %e, "ensure_matching_table");
        return internal();
    }
    if !multiple {
        // `id > gen(Date.now() - 3min)` recent unstarted game between the two
        let restart_min = mk_types::id::gen_aid(
            crate::api::now_ms() as i64 - mk_db::reversi::MATCH_RESTART_WINDOW_MS,
        );
        match mk_db::reversi::find_recent_unstarted_between(pool, me_id, target_id, &restart_min)
            .await
        {
            Ok(Some(game)) => return send_game_detail(state, pool, &game).await,
            Ok(None) => {}
            Err(e) => {
                tracing::error!(target: "dbg_reversi", error = %e, "find_recent_unstarted_between");
                return internal();
            }
        }
    }

    let invite_min =
        mk_types::id::gen_aid(crate::api::now_ms() as i64 - mk_db::reversi::INVITATION_TIMEOUT_MS);
    let invitations = match mk_db::reversi::recent_inviters(pool, me_id, &invite_min).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(target: "dbg_reversi", error = %e, "recent_inviters");
            return internal();
        }
    };
    if invitations.iter().any(|i| i == target_id) {
        // the accept path: matched(parentId=target [the invitor],
        // childId=me [the acceptor]) — user1 = invitor, user2 = acceptor
        if let Err(e) = mk_db::reversi::remove_invite(pool, me_id, target_id).await {
            tracing::error!(target: "dbg_reversi", error = %e, "remove_invite");
            return internal();
        }
        let game = match matched(state, pool, target_id, me_id, false).await {
            Ok(g) => g,
            Err(resp) => return resp,
        };
        return send_game_detail(state, pool, &game).await;
    }

    // zadd(reversi:matchSpecific:<target>, now, me) + the 'invited' stream
    // event (unported, no WS lane in this census)
    let id = mk_types::id::gen_aid_now();
    if let Err(e) = mk_db::reversi::upsert_invite(pool, &id, me_id, target_id).await {
        tracing::error!(target: "dbg_reversi", error = %e, "upsert_invite");
        return internal();
    }
    StatusCode::NO_CONTENT.into_response()
}

/// matchAnyUser (ReversiService.ts L145-213): recent-unstarted probe → any
/// pending invite (random pick upstream; newest here) → waiting-room
/// rendezvous → enqueue myself (204).
async fn match_any(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    me_id: &str,
    no_irregular: bool,
    multiple: bool,
) -> Response {
    if let Err(e) = mk_db::reversi::ensure_matching_table(pool).await {
        tracing::error!(target: "dbg_reversi", error = %e, "ensure_matching_table");
        return internal();
    }
    if !multiple {
        let restart_min = mk_types::id::gen_aid(
            crate::api::now_ms() as i64 - mk_db::reversi::MATCH_RESTART_WINDOW_MS,
        );
        match mk_db::reversi::find_recent_unstarted_of(pool, me_id, &restart_min).await {
            Ok(Some(game)) => return send_game_detail(state, pool, &game).await,
            Ok(None) => {}
            Err(e) => {
                tracing::error!(target: "dbg_reversi", error = %e, "find_recent_unstarted_of");
                return internal();
            }
        }
    }

    let invite_min =
        mk_types::id::gen_aid(crate::api::now_ms() as i64 - mk_db::reversi::INVITATION_TIMEOUT_MS);
    let invitations = match mk_db::reversi::recent_inviters(pool, me_id, &invite_min).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(target: "dbg_reversi", error = %e, "recent_inviters");
            return internal();
        }
    };
    if let Some(invitor) = invitations.last() {
        if let Err(e) = mk_db::reversi::remove_invite(pool, me_id, invitor).await {
            tracing::error!(target: "dbg_reversi", error = %e, "remove_invite");
            return internal();
        }
        let game = match matched(state, pool, invitor, me_id, false).await {
            Ok(g) => g,
            Err(resp) => return resp,
        };
        return send_game_detail(state, pool, &game).await;
    }

    match match_any_take_for(me_id) {
        Some(entry) => {
            let game = match matched(state, pool, &entry.user_id, me_id, entry.no_irregular).await {
                Ok(g) => g,
                Err(resp) => return resp,
            };
            send_game_detail(state, pool, &game).await
        }
        None => {
            match_any_push(me_id, no_irregular);
            StatusCode::NO_CONTENT.into_response()
        }
    }
}

/// `reversi/cancel-match` — matchSpecificUserCancel (my invite TO the target
/// dies) or matchAnyUserCancel (leave the waiting room). Always 204.
async fn cancel_match(state: &AppState, body: &Value, token: Option<String>) -> Response {
    let (pool, me_id) = match resolve_me(state, token.as_deref()).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if let Err(e) = mk_db::reversi::ensure_matching_table(&pool).await {
        tracing::error!(target: "dbg_reversi", error = %e, "ensure_matching_table");
        return internal();
    }
    if let Some(uid) = body.get("userId").and_then(Value::as_str) {
        if let Err(e) = mk_db::reversi::cancel_invite_to(&pool, uid, &me_id).await {
            tracing::error!(target: "dbg_reversi", error = %e, "cancel_invite_to");
            return internal();
        }
        return StatusCode::NO_CONTENT.into_response();
    }
    match_any_cancel(&me_id);
    StatusCode::NO_CONTENT.into_response()
}

/// `reversi/invitations` — the pending invitor ids packed as UserLite[]
/// (packMany over the zrange members).
async fn invitations(state: &AppState, token: Option<String>) -> Response {
    let (pool, me_id) = match resolve_me(state, token.as_deref()).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if let Err(e) = mk_db::reversi::ensure_matching_table(&pool).await {
        tracing::error!(target: "dbg_reversi", error = %e, "ensure_matching_table");
        return internal();
    }
    let invite_min =
        mk_types::id::gen_aid(crate::api::now_ms() as i64 - mk_db::reversi::INVITATION_TIMEOUT_MS);
    let ids = match mk_db::reversi::recent_inviters(&pool, &me_id, &invite_min).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(target: "dbg_reversi", error = %e, "recent_inviters");
            return internal();
        }
    };
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.push(pack_user_lite(state, &pool, &id).await);
    }
    (StatusCode::OK, axum::Json(Value::Array(out))).into_response()
}

/// `reversi/games` — games.ts: makePaginationQuery + the my/isStarted filter,
/// packs packLiteMany.
async fn games(state: &AppState, body: &Value, token: Option<String>) -> Response {
    let (pool, me) = match resolve_me_optional(state, token.as_deref()).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    // paramDef limit 1..=100 default 10
    let limit = body
        .get("limit")
        .and_then(Value::as_i64)
        .map(|v| v.clamp(1, 100) as i32)
        .unwrap_or(10);
    let my = body.get("my").and_then(Value::as_bool).unwrap_or(false);
    let str_param = |key: &str| -> Option<String> {
        body.get(key)
            .and_then(Value::as_str)
            .filter(|s| crate::api::is_alnum_id(s))
            .map(|s| s.to_string())
    };
    let since_id = str_param("sinceId").or_else(|| {
        body.get("sinceDate")
            .and_then(Value::as_i64)
            .map(mk_types::id::gen_aid)
    });
    let until_id = str_param("untilId").or_else(|| {
        body.get("untilDate")
            .and_then(Value::as_i64)
            .map(mk_types::id::gen_aid)
    });
    // upstream `if (ps.my && me)` — an unauthenticated `my` degrades to the
    // isStarted = TRUE listing
    let my_only = my && me.is_some();
    let rows = match mk_db::reversi::list_games(
        &pool,
        limit,
        since_id.as_deref(),
        until_id.as_deref(),
        my_only,
        me.as_deref(),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(target: "dbg_reversi", error = %e, "list_games");
            return internal();
        }
    };
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(pack_game_lite(state, &pool, row).await);
    }
    (StatusCode::OK, axum::Json(Value::Array(out))).into_response()
}

/// `reversi/show-game` — show-game.ts: get → packDetail (NO_SUCH_GAME
/// f13a03db on the miss).
async fn show_game(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let (pool, _) = match resolve_me_optional(state, token).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let Some(game_id) = body
        .get("gameId")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
    else {
        return invalid_param();
    };
    let row = match mk_db::reversi::find_game(&pool, &game_id).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(target: "dbg_reversi", error = %e, "find_game");
            return internal();
        }
    };
    let Some(row) = row else {
        return no_such_game("f13a03db-fae1-46c9-87f3-43c8165419e1");
    };
    let packed = pack_game_detail(state, &pool, &row).await;
    (StatusCode::OK, axum::Json(packed)).into_response()
}

/// `reversi/surrender` — surrender.ts endpoint gates (NO_SUCH_GAME ace0b11f /
/// ALREADY_ENDED 6c2ad4a6 / ACCESS_DENIED 6e04164b) then
/// ReversiService.surrender → endGame(winner = the OTHER user,
/// surrenderedUserId = me). 204.
async fn surrender(state: &AppState, body: &Value, token: Option<String>) -> Response {
    let (pool, me_id) = match resolve_me(state, token.as_deref()).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let Some(game_id) = body
        .get("gameId")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
    else {
        return invalid_param();
    };
    let row = match mk_db::reversi::find_game(&pool, &game_id).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(target: "dbg_reversi", error = %e, "find_game");
            return internal();
        }
    };
    let Some(game) = row else {
        return no_such_game("ace0b11f-e0a6-4076-a30d-e8284c81b2df");
    };
    if game.is_ended {
        return already_ended();
    }
    if game.user1_id != me_id && game.user2_id != me_id {
        return access_denied();
    }
    let winner_id = if game.user1_id == me_id {
        game.user2_id.clone()
    } else {
        game.user1_id.clone()
    };
    if let Err(e) = mk_db::reversi::end_game(
        &pool,
        &game.id,
        crate::api::now_ms() as i64,
        Some(&winner_id),
        Some(&me_id),
        None,
    )
    .await
    {
        tracing::error!(target: "dbg_reversi", error = %e, "end_game");
        return internal();
    }
    // publishReversiGameStream 'ended' — no WS lane in this census
    StatusCode::NO_CONTENT.into_response()
}

/// `reversi/verify` — verify.ts: NO requireCredential; checkCrc answers
/// {desynced: true, game} on a crc mismatch (a never-started game has crc32
/// null — always a mismatch) and {desynced: false} otherwise.
async fn verify(state: &AppState, body: &Value) -> Response {
    let pool = match state.pool.clone() {
        Some(p) => p,
        None => return internal(),
    };
    let Some(game_id) = body
        .get("gameId")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
    else {
        return invalid_param();
    };
    let Some(crc32) = body
        .get("crc32")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
    else {
        return invalid_param();
    };
    let row = match mk_db::reversi::find_game(&pool, &game_id).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(target: "dbg_reversi", error = %e, "find_game");
            return internal();
        }
    };
    let Some(game) = row else {
        return no_such_game("8fb05624-b525-43dd-90f7-511852bdfeee");
    };
    let synced = game.crc32.as_deref() == Some(crc32.as_str());
    if synced {
        (StatusCode::OK, axum::Json(json!({ "desynced": false }))).into_response()
    } else {
        let packed = pack_game_detail(state, &pool, &game).await;
        (
            StatusCode::OK,
            axum::Json(json!({ "desynced": true, "game": packed })),
        )
            .into_response()
    }
}
