//! reversi: the match-invitation + game plane (upstream core/ReversiService.ts
//! + server/api/endpoints/reversi/{match,cancel-match,invitations,games,
//! show-game,surrender,verify}.ts).
//!
//! Tables ship in the 0001 baseline — no migration here:
//! - `reversi_game` IS the pinned MiReversiGame entity table (all 23 columns
//!   ReversiService reads, incl. `noIrregularRules` + `timeLimitForEachTurn`
//!   column defaults 'false'/'90').
//! - `reversi_matching` exists in the pinned schema but has NO pinned entity —
//!   upstream moved match invitations to the redis zset
//!   `reversi:matchSpecific:<userId>` (member = the invitor, score = ms). The
//!   port models that zset ON reversi_matching with its legacy shape untouched:
//!   "parentId" = the invitor (the zset member), "childId" = the invited user
//!   (the zset-key owner); zadd member-refresh semantics mirror as
//!   delete-then-insert per (child, parent) pair.
//! - the INVITATION_TIMEOUT_MS window (zrange BYSCORE Date.now() - 20s) and
//!   the 3-min "already matched" probe both ride the aid id's time prefix
//!   instead of zset scores / idService gens — the caller passes pre-computed
//!   gen_aid(now - window) floor ids.
//! The `reversi:matchAny` waiting room (zset, expire 15 NX) stays in-process
//! in api/reversi.rs (crate::api::reversi::MATCH_ANY): a single server process
//! owns all match state and no db reader exists for it.

use sqlx::PgPool;

/// Upstream INVITATION_TIMEOUT_MS (ReversiService.ts L28) — redis zrange
/// BYSCORE cutoff; the caller turns it into a gen_aid floor id.
pub const INVITATION_TIMEOUT_MS: i64 = 1000 * 20;

/// The "already matched recently?" window (ReversiService.ts L100/L147:
/// `id > gen(Date.now() - 3min)` on unstarted games) — also a caller-side
/// min_id computation.
pub const MATCH_RESTART_WINDOW_MS: i64 = 1000 * 60 * 3;

/// ReversiService.matched default map (Reversi.maps.eighteight.data, pinned
/// misskey-reversi/src/maps.ts L75-87).
pub const EIGHTEIGHT_MAP: &[&str] = &[
    "--------", "--------", "--------", "---wb---", "---bw---", "--------", "--------", "--------",
];

// ==== drop-schema shim ======================================================
// initTestDb(dropSchema: true) lanes re-sync from the PINNED entities and
// reversi_matching has none — rebuild it on first use like mk_queue_jobs /
// mk_export_blobs do (0001 stays the canonical replay for fresh databases).

pub async fn ensure_matching_table(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS public.reversi_matching (
            id character varying(32) NOT NULL,
            "parentId" character varying(32) NOT NULL,
            "childId" character varying(32) NOT NULL,
            CONSTRAINT "PK_880bd0afbab232f21c8b9d146cf" PRIMARY KEY ("id")
        )"#,
    )
    .execute(pool)
    .await
    .map(|_| ())
}

// ==== reversi_matching (the matchSpecific zset) =============================

/// `zadd(reversi:matchSpecific:<child>, now, parent)` — zset members are
/// unique, so a re-invite refreshes the score (delete-then-insert on the
/// (child, parent) pair).
pub async fn upsert_invite(
    pool: &PgPool,
    id: &str,
    parent_id: &str,
    child_id: &str,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query(r#"DELETE FROM "reversi_matching" WHERE "childId" = $1 AND "parentId" = $2"#)
        .bind(child_id)
        .bind(parent_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        r#"INSERT INTO "reversi_matching" ("id", "parentId", "childId") VALUES ($1, $2, $3)"#,
    )
    .bind(id)
    .bind(parent_id)
    .bind(child_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

/// `zrange(reversi:matchSpecific:<me>, now - timeout, +inf, BYSCORE)` — the
/// userIds that invited me inside the window, newest first (upstream picks
/// `Math.floor(Math.random() * n)`; newest is in the same invitation set).
pub async fn recent_inviters(
    pool: &PgPool,
    child_id: &str,
    min_id: &str,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        r#"SELECT "parentId" FROM "reversi_matching"
           WHERE "childId" = $1 AND "id" > $2 ORDER BY "id" DESC"#,
    )
    .bind(child_id)
    .bind(min_id)
    .fetch_all(pool)
    .await
}

/// `zrem(reversi:matchSpecific:<me>, invitor)` — the accept path removes the
/// accepted invitation.
pub async fn remove_invite(
    pool: &PgPool,
    child_id: &str,
    parent_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "reversi_matching" WHERE "childId" = $1 AND "parentId" = $2"#)
        .bind(child_id)
        .bind(parent_id)
        .execute(pool)
        .await
        .map(|_| ())
}

/// `matchSpecificUserCancel` — zrem(reversi:matchSpecific:<target>, me): only
/// MY invitation TO that one user dies.
pub async fn cancel_invite_to(
    pool: &PgPool,
    target_id: &str,
    invitor_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "reversi_matching" WHERE "childId" = $1 AND "parentId" = $2"#)
        .bind(target_id)
        .bind(invitor_id)
        .execute(pool)
        .await
        .map(|_| ())
}

// ==== reversi_game ==========================================================

/// The full MiReversiGame column set with the camelCase → snake_case aliases
/// (find_drive_file FILE_COLS convention). timestamptz columns ride the
/// epoch-ms bigint extraction the announcements lane standardized. `map` is
/// `varchar(64)[]` in the pinned schema — read through `::text[]` so the
/// Vec<String> decode rides the plain text-array codec.
pub const GAME_COLS: &str = r#""id",
    ((EXTRACT(EPOCH FROM "startedAt")) * 1000)::bigint AS "started_ms",
    "user1Id" AS "user1_id", "user2Id" AS "user2_id",
    "user1Ready" AS "user1_ready", "user2Ready" AS "user2_ready",
    black, "isStarted" AS "is_started", "isEnded" AS "is_ended",
    "winnerId" AS "winner_id", "surrenderedUserId" AS "surrendered_user_id",
    logs, ("map"::text[]) AS "map", bw,
    "isLlotheo" AS "is_llotheo", "canPutEverywhere" AS "can_put_everywhere",
    "loopedBoard" AS "looped_board", form1, form2, crc32,
    "timeoutUserId" AS "timeout_user_id",
    ((EXTRACT(EPOCH FROM "endedAt")) * 1000)::bigint AS "ended_ms",
    "timeLimitForEachTurn" AS "time_limit_for_each_turn",
    "noIrregularRules" AS "no_irregular_rules""#;

/// The reversi_game row (ReversiGamesRepository.findOne — the user1/user2
/// relations are fetched + packed by the api layer).
#[derive(Debug, sqlx::FromRow)]
pub struct ReversiGameRow {
    pub id: String,
    pub started_ms: Option<i64>,
    pub user1_id: String,
    pub user2_id: String,
    pub user1_ready: bool,
    pub user2_ready: bool,
    pub black: Option<i32>,
    pub is_started: bool,
    pub is_ended: bool,
    pub winner_id: Option<String>,
    pub surrendered_user_id: Option<String>,
    pub logs: serde_json::Value,
    pub map: Vec<String>,
    pub bw: String,
    pub is_llotheo: bool,
    pub can_put_everywhere: bool,
    pub looped_board: bool,
    pub form1: Option<serde_json::Value>,
    pub form2: Option<serde_json::Value>,
    pub crc32: Option<String>,
    pub timeout_user_id: Option<String>,
    pub ended_ms: Option<i64>,
    pub time_limit_for_each_turn: i16,
    pub no_irregular_rules: bool,
}

/// `reversiGamesRepository.findOne({ where: { id }, relations user1/user2 })`
/// — ReversiService.get, id-only branch (the redis game-cache layer is not
/// modeled; the db row is the same source of truth between lanes).
pub async fn find_game(pool: &PgPool, id: &str) -> Result<Option<ReversiGameRow>, sqlx::Error> {
    let sql = format!(
        r#"SELECT {} FROM "reversi_game" WHERE "id" = $1"#,
        GAME_COLS
    );
    sqlx::query_as::<_, ReversiGameRow>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// matchSpecificUser/matchAnyUser "already matched within 3 min" probe —
/// `id > gen(now - 3min) AND isStarted = false` between the two users,
/// newest first (order id DESC + take 1, upstream games[0]).
pub async fn find_recent_unstarted_between(
    pool: &PgPool,
    a: &str,
    b: &str,
    min_id: &str,
) -> Result<Option<ReversiGameRow>, sqlx::Error> {
    let sql = format!(
        r#"SELECT {} FROM "reversi_game"
           WHERE ("user1Id" = $1 AND "user2Id" = $2 OR "user1Id" = $2 AND "user2Id" = $1)
             AND "isStarted" = FALSE AND "id" > $3
           ORDER BY "id" DESC LIMIT 1"#,
        GAME_COLS
    );
    sqlx::query_as::<_, ReversiGameRow>(&sql)
        .bind(a)
        .bind(b)
        .bind(min_id)
        .fetch_optional(pool)
        .await
}

/// Same probe scoped to me on either seat (matchAnyUser L148-155).
pub async fn find_recent_unstarted_of(
    pool: &PgPool,
    me: &str,
    min_id: &str,
) -> Result<Option<ReversiGameRow>, sqlx::Error> {
    let sql = format!(
        r#"SELECT {} FROM "reversi_game"
           WHERE ("user1Id" = $1 OR "user2Id" = $1)
             AND "isStarted" = FALSE AND "id" > $2
           ORDER BY "id" DESC LIMIT 1"#,
        GAME_COLS
    );
    sqlx::query_as::<_, ReversiGameRow>(&sql)
        .bind(me)
        .bind(min_id)
        .fetch_optional(pool)
        .await
}

/// `reversiGamesRepository.insertOne(...)` in matched() — the eighteight map,
/// bw 'random', ready/started/ended all false, logs [] and the rule flags at
/// their column defaults (isLlotheo/canPutEverywhere/loopedBoard false,
/// timeLimitForEachTurn 90) because the pinned insert names only these
/// columns.
pub async fn insert_game(
    pool: &PgPool,
    id: &str,
    user1_id: &str,
    user2_id: &str,
    no_irregular_rules: bool,
    map: &[&str],
) -> Result<(), sqlx::Error> {
    let map: Vec<String> = map.iter().map(|s| s.to_string()).collect();
    sqlx::query(
        r#"INSERT INTO "reversi_game"
           ("id", "user1Id", "user2Id", "user1Ready", "user2Ready",
            "isStarted", "isEnded", logs, "map", "bw", "isLlotheo",
            "canPutEverywhere", "loopedBoard", "noIrregularRules")
           VALUES ($1, $2, $3, FALSE, FALSE, FALSE, FALSE, '[]'::jsonb,
                   $4::varchar(64)[], 'random', FALSE, FALSE, FALSE, $5)"#,
    )
    .bind(id)
    .bind(user1_id)
    .bind(user2_id)
    .bind(&map)
    .bind(no_irregular_rules)
    .execute(pool)
    .await
    .map(|_| ())
}

/// `surrender()` → `endGame` (the createQueryBuilder().update().set()
/// .returning('*') the service rides, rebuilt as UPDATE): isEnded + endedAt +
/// winnerId + the reason-attributed loser column (surrender vs timeout), NULL
/// for the other reason. The getBakeProps no-op fields (user readiness, rules)
/// are unchanged by construction.
pub async fn end_game(
    pool: &PgPool,
    game_id: &str,
    ended_ms: i64,
    winner_id: Option<&str>,
    surrendered_user_id: Option<&str>,
    timeout_user_id: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"UPDATE "reversi_game" SET
             "isEnded" = TRUE,
             "endedAt" = to_timestamp($2 / 1000.0),
             "winnerId" = $3,
             "surrenderedUserId" = $4,
             "timeoutUserId" = $5
           WHERE "id" = $1"#,
    )
    .bind(game_id)
    .bind(ended_ms)
    .bind(winner_id)
    .bind(surrendered_user_id)
    .bind(timeout_user_id)
    .execute(pool)
    .await
    .map(|_| ())
}

/// games.ts makePaginationQuery + the my/isStarted filter:
/// - my && me → mine on either seat;
/// - otherwise isStarted = TRUE (invisible-unstarted parity);
/// - sinceId only → id > since ASC; untilId only → id < until DESC; both →
///   id > since < until DESC; sinceDate/untilDate ride the pre-computed
///   gen(date) floor ids (caller side, like every window id here);
/// - limit = the take() bound.
pub async fn list_games(
    pool: &PgPool,
    limit: i32,
    since_id: Option<&str>,
    until_id: Option<&str>,
    my_only: bool,
    me_id: Option<&str>,
) -> Result<Vec<ReversiGameRow>, sqlx::Error> {
    let mut sql = String::from("SELECT ");
    sql.push_str(GAME_COLS);
    sql.push_str(r#" FROM "reversi_game" WHERE 1 = 1"#);
    let mut binds: Vec<String> = Vec::new();
    if let Some(since) = since_id {
        // placeholders are 1-based in bind order: the next slot is len+1
        let ph = binds.len() + 1;
        sql.push_str(&format!(r#" AND "id" > ${ph} COLLATE "C""#));
        binds.push(since.to_string());
    }
    if let Some(until) = until_id {
        let ph = binds.len() + 1;
        sql.push_str(&format!(r#" AND "id" < ${ph} COLLATE "C""#));
        binds.push(until.to_string());
    }
    if let Some(me) = me_id.filter(|_| my_only) {
        let ph = binds.len() + 1;
        sql.push_str(&format!(r#" AND ("user1Id" = ${ph} OR "user2Id" = ${ph})"#));
        binds.push(me.to_string());
    } else {
        sql.push_str(r#" AND "isStarted" = TRUE"#);
    }
    let dir = if since_id.is_some() && until_id.is_none() {
        "ASC"
    } else {
        "DESC"
    };
    sql.push_str(r#" ORDER BY "id" COLLATE "C" "#);
    sql.push_str(dir);
    sql.push_str(&format!(" LIMIT {limit}"));

    let mut q = sqlx::query_as::<_, ReversiGameRow>(&sql);
    for b in &binds {
        q = q.bind(b);
    }
    q.fetch_all(pool).await
}
