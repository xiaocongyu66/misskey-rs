//! game-arena: the in-process reversi + bubble-game plane (upstream
//! server/api/endpoints/reversi/{games,create,join,show,match,cancel}.ts +
//! bubble-game/register.ts).
//!
//! DIVERGENCE (documented, deliberate): upstream keeps reversi games in
//! Redis (ReversiGameService) and bubble-game scores in the
//! `bubble_game_record` table. This port is an in-process equivalent for the
//! 0001 baseline that ships no usable game tables:
//! - every reversi game lives in a process-wide
//!   `OnceLock<Mutex<HashMap<String, Game>>>` (single-instance only, state is
//!   lost on restart; multi-instance deployments would not see each other);
//! - bubble-game scores live in a process-wide `OnceLock<Mutex<Vec<_>>>`
//!   (same single-instance / restart caveat);
//! - the SSE/streaming match channel is NOT wired: these endpoints return the
//!   packed game, they never push a `reversi` stream event.
//!
//! `reversi/match` is dual-shaped: `{ gameId }` is the pairing-info read of
//! this module, while the legacy `{ userId }` / matchAny body delegates to
//! api::reversi (the older upstream endpoint set another lane already serves)
//! so both surfaces keep working. `reversi/games` likewise lists this
//! module's open games.
//!
//! Auth rides the shared `social::require_checked_user` chain
//! (CREDENTIAL_REQUIRED / AUTHENTICATION_FAILED / YOUR_ACCOUNT_SUSPENDED).
//! Error ids mirror the upstream .ts files: NO_SUCH_GAME
//! f13a03db-fae1-46c9-87f3-43c8165419e1 (show-game.ts), INVALID_PARAM
//! 3d81ceae-475f-4600-b2a8-2bc116157532 (endpoint-base.ts), ACCESS_DENIED
//! 6e04164b-a992-4c93-8489-2123069973e1 (surrender.ts).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// Endpoint names owned by this module (all POST-only).
pub(super) fn handles(name: &str) -> bool {
    matches!(
        name,
        "reversi/games"
            | "reversi/create"
            | "reversi/join"
            | "reversi/show"
            | "reversi/match"
            | "reversi/cancel"
            | "bubble-game/register"
            | "bubble-game/ranking"
    )
}

/// Entry from the game-arena dispatch arm in mod.rs.
pub(super) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
) -> Response {
    match name {
        "reversi/games" => games(state, body, token.as_deref()).await,
        "reversi/create" => create(state, body, token.as_deref()).await,
        "reversi/join" => join(state, body, token.as_deref()).await,
        "reversi/show" => show(state, body, token.as_deref()).await,
        "reversi/match" => {
            // Legacy `{ userId }`/matchAny bodies stay on the api::reversi
            // surface; only the `{ gameId }` shape is ours.
            if body.get("gameId").is_some() {
                pairing(state, body, token.as_deref()).await
            } else {
                super::reversi::handle(state, name, body, token).await
            }
        }
        "reversi/cancel" => cancel(state, body, token.as_deref()).await,
        "bubble-game/ranking" => ranking(state, body).await,
        _ => register(state, body, token.as_deref()).await,
    }
}

// ==== error envelopes =======================================================

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn invalid_param(param: &str) -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "INVALID_PARAM",
        "3d81ceae-475f-4600-b2a8-2bc116157532",
        format!("Invalid param: {param}."),
    ))
}

fn no_such_game() -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "NO_SUCH_GAME",
        "f13a03db-fae1-46c9-87f3-43c8165419e1",
        "No such game.",
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

// ==== auth chain ============================================================

async fn require_me(
    state: &AppState,
    token: Option<&str>,
    declared: bool,
) -> Result<String, Response> {
    let Some(pool) = &state.pool else {
        return Err(internal());
    };
    super::social::require_checked_user(pool, token, declared).await
}

/// show/games are `requireCredential: false` upstream: a supplied token is
/// still authenticated, an absent one is allowed through.
async fn optional_me(state: &AppState, token: Option<&str>) -> Result<Option<String>, Response> {
    let Some(token) = token else {
        return Ok(None);
    };
    let Some(pool) = &state.pool else {
        return Err(internal());
    };
    match super::social::require_checked_user(pool, Some(token), true).await {
        Ok(id) => Ok(Some(id)),
        Err(resp) => Err(resp),
    }
}

// ==== in-process game store =================================================

const EIGHTEIGHT_MAP: [&str; 8] = [
    "--------", "--------", "--------", "---wb---", "---bw---", "--------", "--------", "--------",
];

#[derive(Clone)]
struct Game {
    id: String,
    created_at_ms: u64,
    started_at_ms: Option<u64>,
    ended_at_ms: Option<u64>,
    user1_id: String,
    user2_id: Option<String>,
    is_started: bool,
    is_ended: bool,
    bw: String,
    is_llotheo: bool,
    can_put_everywhere: bool,
    looped_board: bool,
    time_limit_for_each_turn: i64,
    no_irregular_rules: bool,
    logs: Vec<Value>,
}

fn store() -> &'static Mutex<HashMap<String, Game>> {
    static GAMES: OnceLock<Mutex<HashMap<String, Game>>> = OnceLock::new();
    GAMES.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Clone)]
#[allow(dead_code)]
struct ScoreRecord {
    id: String,
    user_id: String,
    score: i64,
    seed: String,
    logs: Value,
    game_mode: Option<String>,
    game_version: Option<i64>,
    at_ms: u64,
}

fn scores() -> &'static Mutex<Vec<ScoreRecord>> {
    static SCORES: OnceLock<Mutex<Vec<ScoreRecord>>> = OnceLock::new();
    SCORES.get_or_init(|| Mutex::new(Vec::new()))
}

/// Bubble-game records are bounded in-process (the oldest entry is dropped
/// once the ring is full).
const MAX_SCORES: usize = 1000;

// ==== packs =================================================================

fn iso_ms(ms: u64) -> String {
    mk_core::services::signup::ms_to_iso(ms as i64)
}

fn opt_iso(ms: Option<u64>) -> Value {
    match ms {
        Some(v) => Value::String(iso_ms(v)),
        None => Value::Null,
    }
}

/// UserEntityService.pack(user) — UserLite seats.
async fn pack_user(state: &AppState, pool: &mk_db::sqlx::PgPool, user_id: &str) -> Value {
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

/// ReversiGameEntityService.packDetail/packLite (detail=false drops logs/map).
async fn pack_game(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    game: &Game,
    detail: bool,
) -> Value {
    let user1 = pack_user(state, pool, &game.user1_id).await;
    let user2 = match &game.user2_id {
        Some(id) => pack_user(state, pool, id).await,
        None => Value::Null,
    };
    let mut out = json!({
        "id": game.id,
        "createdAt": iso_ms(game.created_at_ms),
        "startedAt": opt_iso(game.started_at_ms),
        "endedAt": opt_iso(game.ended_at_ms),
        "isStarted": game.is_started,
        "isEnded": game.is_ended,
        "user1Id": game.user1_id,
        "user2Id": game.user2_id,
        "user1": user1,
        "user2": user2,
        "winnerId": Value::Null,
        "winner": Value::Null,
        "surrenderedUserId": Value::Null,
        "timeoutUserId": Value::Null,
        "black": Value::Null,
        "bw": game.bw,
        "isLlotheo": game.is_llotheo,
        "canPutEverywhere": game.can_put_everywhere,
        "loopedBoard": game.looped_board,
        "timeLimitForEachTurn": game.time_limit_for_each_turn,
        "noIrregularRules": game.no_irregular_rules,
    });
    if detail {
        let obj = out.as_object_mut().expect("object");
        obj.insert("form1".into(), Value::Null);
        obj.insert("form2".into(), Value::Null);
        obj.insert("user1Ready".into(), json!(game.is_started));
        obj.insert("user2Ready".into(), json!(game.is_started));
        obj.insert("logs".into(), Value::Array(game.logs.clone()));
        obj.insert(
            "map".into(),
            Value::Array(
                EIGHTEIGHT_MAP
                    .iter()
                    .map(|row| Value::String((*row).to_string()))
                    .collect(),
            ),
        );
    }
    out
}

/// Pull the upstream create-settings (bw/isLlotheo/canPutEverywhere/
/// loopedBoard/timeLimitForEachTurn/noIrregularRules) from either the nested
/// `settings` object or the flat body.
fn create_settings(body: &Value) -> GameSettings {
    let src = body
        .get("settings")
        .filter(|v| v.is_object())
        .unwrap_or(body);
    let bool_of = |key: &str| src.get(key).and_then(Value::as_bool).unwrap_or(false);
    let bw = match src.get("bw").and_then(Value::as_str) {
        Some(v) if matches!(v, "random" | "1" | "2") => v.to_string(),
        _ => "random".to_string(),
    };
    GameSettings {
        bw,
        is_llotheo: bool_of("isLlotheo"),
        can_put_everywhere: bool_of("canPutEverywhere"),
        looped_board: bool_of("loopedBoard"),
        time_limit_for_each_turn: src
            .get("timeLimitForEachTurn")
            .and_then(Value::as_i64)
            .unwrap_or(90),
        no_irregular_rules: bool_of("noIrregularRules"),
    }
}

struct GameSettings {
    bw: String,
    is_llotheo: bool,
    can_put_everywhere: bool,
    looped_board: bool,
    time_limit_for_each_turn: i64,
    no_irregular_rules: bool,
}

// ==== endpoints =============================================================

/// `reversi/games` — list the open (not yet joined, not ended) games.
/// `my: true` narrows to games involving the caller.
async fn games(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let me = match optional_me(state, token).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let Some(pool) = &state.pool else {
        return internal();
    };
    let limit = body
        .get("limit")
        .and_then(Value::as_i64)
        .map(|v| v.clamp(1, 100) as usize)
        .unwrap_or(10);
    let my = body.get("my").and_then(Value::as_bool).unwrap_or(false) && me.is_some();
    let mut rows: Vec<Game> = {
        let guard = store().lock().unwrap();
        guard
            .values()
            .filter(|g| !g.is_ended && g.user2_id.is_none())
            .filter(|g| {
                !my || {
                    let me = me.as_deref().unwrap_or_default();
                    g.user1_id == me || g.user2_id.as_deref() == Some(me)
                }
            })
            .cloned()
            .collect()
    };
    // newest first, matching the pagination default
    rows.sort_by(|a, b| b.created_at_ms.cmp(&a.created_at_ms));
    rows.truncate(limit);
    let mut out = Vec::with_capacity(rows.len());
    for game in &rows {
        out.push(pack_game(state, pool, game, false).await);
    }
    (StatusCode::OK, axum::Json(Value::Array(out))).into_response()
}

/// `reversi/create` — { settings? } → a new open game owned by the caller.
async fn create(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let me = match require_me(state, token, body.get("i").is_some()).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let Some(pool) = &state.pool else {
        return internal();
    };
    let settings = create_settings(body);
    let now = crate::api::now_ms();
    let game = Game {
        id: mk_types::id::gen_aid_now(),
        created_at_ms: now,
        started_at_ms: None,
        ended_at_ms: None,
        user1_id: me,
        user2_id: None,
        is_started: false,
        is_ended: false,
        bw: settings.bw,
        is_llotheo: settings.is_llotheo,
        can_put_everywhere: settings.can_put_everywhere,
        looped_board: settings.looped_board,
        time_limit_for_each_turn: settings.time_limit_for_each_turn,
        no_irregular_rules: settings.no_irregular_rules,
        logs: Vec::new(),
    };
    let packed = pack_game(state, pool, &game, true).await;
    store().lock().unwrap().insert(game.id.clone(), game);
    (StatusCode::OK, axum::Json(packed)).into_response()
}

/// `reversi/join` — { gameId } → seat the caller as user2 and start the game.
async fn join(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let me = match require_me(state, token, body.get("i").is_some()).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let Some(pool) = &state.pool else {
        return internal();
    };
    let Some(game_id) = body.get("gameId").and_then(Value::as_str) else {
        return invalid_param("gameId");
    };
    let updated = {
        let mut guard = store().lock().unwrap();
        let Some(game) = guard.get_mut(game_id) else {
            return no_such_game();
        };
        if game.is_ended {
            return no_such_game();
        }
        if let Some(seat) = game.user2_id.clone() {
            if seat != me {
                return access_denied();
            }
        } else {
            if game.user1_id == me {
                return access_denied();
            }
            game.user2_id = Some(me);
            game.is_started = true;
            game.started_at_ms = Some(crate::api::now_ms());
        }
        game.clone()
    };
    let packed = pack_game(state, pool, &updated, true).await;
    (StatusCode::OK, axum::Json(packed)).into_response()
}

/// `reversi/show` — { gameId } → packDetail (NO_SUCH_GAME on the miss).
async fn show(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    if let Err(resp) = optional_me(state, token).await {
        return resp;
    }
    let Some(pool) = &state.pool else {
        return internal();
    };
    let Some(game_id) = body.get("gameId").and_then(Value::as_str) else {
        return invalid_param("gameId");
    };
    let game = match store().lock().unwrap().get(game_id) {
        Some(g) => g.clone(),
        None => return no_such_game(),
    };
    let packed = pack_game(state, pool, &game, true).await;
    (StatusCode::OK, axum::Json(packed)).into_response()
}

/// `reversi/match` — { gameId } → the pairing info of the two seats.
async fn pairing(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let me = match require_me(state, token, body.get("i").is_some()).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let Some(pool) = &state.pool else {
        return internal();
    };
    let Some(game_id) = body.get("gameId").and_then(Value::as_str) else {
        return invalid_param("gameId");
    };
    let game = match store().lock().unwrap().get(game_id) {
        Some(g) => g.clone(),
        None => return no_such_game(),
    };
    let _ = me;
    let packed = pack_game(state, pool, &game, true).await;
    let body = json!({
        "gameId": game.id,
        "user1Id": game.user1_id,
        "user2Id": game.user2_id,
        "isStarted": game.is_started,
        "isEnded": game.is_ended,
        "game": packed,
    });
    (StatusCode::OK, axum::Json(body)).into_response()
}

/// `reversi/cancel` — { gameId? } → drop the game (or every game of mine).
/// Idempotent: an unknown id is still a 204 (upstream cancel-match never
/// errors on an absent entry).
async fn cancel(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let me = match require_me(state, token, body.get("i").is_some()).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let mut guard = store().lock().unwrap();
    if let Some(game_id) = body.get("gameId").and_then(Value::as_str) {
        if let Some(game) = guard.get(game_id) {
            if game.user1_id != me && game.user2_id.as_deref() != Some(me.as_str()) {
                return access_denied();
            }
            guard.remove(game_id);
        }
    } else {
        guard.retain(|_, g| g.user1_id != me && g.user2_id.as_deref() != Some(me.as_str()));
    }
    drop(guard);
    StatusCode::NO_CONTENT.into_response()
}

/// `bubble-game/register` — { score, seed, logs, ... } → record the score.
/// The replay validation upstream runs is not ported (see module header);
/// the simple score record is stored and `{ score }` is returned.
async fn register(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let me = match require_me(state, token, body.get("i").is_some()).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let Some(score) = body.get("score").and_then(Value::as_i64) else {
        return invalid_param("score");
    };
    if score < 0 {
        return invalid_param("score");
    }
    let Some(seed) = body.get("seed").and_then(Value::as_str) else {
        return invalid_param("seed");
    };
    // paramDef bounds (minLength 1 / maxLength 1024); the replay/seed-window
    // validation upstream runs is deliberately not ported (module header).
    if seed.is_empty() || seed.len() > 1024 {
        return invalid_param("seed");
    }
    let logs = body.get("logs").cloned().unwrap_or_else(|| json!([]));
    let at_ms = crate::api::now_ms();
    let record = ScoreRecord {
        id: mk_types::id::gen_aid(at_ms as i64),
        user_id: me,
        score,
        seed: seed.to_string(),
        logs,
        game_mode: body
            .get("gameMode")
            .and_then(Value::as_str)
            .map(str::to_string),
        game_version: body.get("gameVersion").and_then(Value::as_i64),
        at_ms,
    };
    {
        let mut guard = scores().lock().unwrap();
        guard.push(record);
        if guard.len() > MAX_SCORES {
            let drop_n = guard.len() - MAX_SCORES;
            let _ = guard.drain(0..drop_n);
        }
    }
    (StatusCode::OK, axum::Json(json!({ "score": score }))).into_response()
}

/// `bubble-game/ranking` — top 10 scores in the process-wide ring, newest
/// 7 days only (upstream `seededAt > now - 7d`, `gameMode` filter, score
/// DESC). DIVERGENCE: upstream reads the `bubble_game_record` table and
/// packs `UserLite`; this port ranks the in-process `scores()` ring (see
/// module header) and packs users through the same UserLite helper.
async fn ranking(state: &AppState, body: &Value) -> Response {
    let Some(pool) = &state.pool else {
        return internal();
    };
    let game_mode = body.get("gameMode").and_then(Value::as_str);
    let now = crate::api::now_ms();
    let cutoff = now.saturating_sub(7 * 24 * 60 * 60 * 1000);

    let mut rows: Vec<ScoreRecord> = {
        let guard = scores().lock().unwrap();
        guard
            .iter()
            .filter(|r| r.at_ms >= cutoff)
            .filter(|r| game_mode.map_or(true, |m| r.game_mode.as_deref() == Some(m)))
            .cloned()
            .collect()
    };
    rows.sort_by(|a, b| b.score.cmp(&a.score).then(a.at_ms.cmp(&b.at_ms)));
    rows.truncate(10);

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(json!({
            "id": r.id,
            "score": r.score,
            "user": pack_user(state, pool, &r.user_id).await,
        }));
    }
    (StatusCode::OK, axum::Json(json!(out))).into_response()
}
