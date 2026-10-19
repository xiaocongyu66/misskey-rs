//! Antennas domain (upstream `endpoints/antennas/*` + the note fanout):
//! the `antenna` CRUD plane from the 0001 baseline plus the `antenna_note`
//! projection table the 0004 migration restores (upstream streams the same
//! feed through FanoutTimelineService `antennaTimeline:{id}` redis lists —
//! the SQL projection keeps the read path filter/paginate deterministically).
//!
//! Fanout mirrors core/AntennaService.ts `checkHitAntenna` arm for arm, in
//! order: channel sensitivity → bots → localOnly → replies → specified/
//! followers visibility → src (home/all/list/users/users_blacklist) →
//! keywords (AND inside a group, OR across groups, `text + '\n' + cw`
//! material, caseSensitive) → excludeKeywords (any group hit drops) →
//! withFile. Rows are inserted ONLY at note-creation time (under
//! `isActive = TRUE`, like upstream `getAntennas()`) — antenna lists.ts
//! `を取得できること。` posts a keyword note BEFORE creating the antenna and
//! expects it absent, so the match is never recomputed at read time.
//!
//! Read path (antenna_notes_page) mirrors notes/antennas.ts: owner-scoped
//! feed over the projection join, visibility
//! (QueryService.generateVisibilityQuery) + base filtering
//! (generateBaseNoteFilteringQuery — the mk-db lists.rs BASE_ME fusion) + the
//! muted-channel planes, EXCLUSIVE aid bounds, sinceId-only → oldest-first
//! ASC, otherwise newest-first DESC. `excludeNotesInSensitiveChannel` applies
//! at FANOUT only (notes.ts pins it: "センシティブ除外の設定はこのエンド
//! ポイントでは無視する").

use sqlx::PgPool;

use crate::lists::{BASE_ME, VIS_ME};

/// One antenna row. All antennas/* endpoints scope by owner, so `find_antenna`
/// fuses the `findOneBy({ id, userId })` shape. `src` is the
/// `antenna_src_enum` cast to text; keywords/excludeKeywords ride as raw
/// jsonb text for the api pack to re-serialize untouched.
#[derive(Debug, sqlx::FromRow)]
pub struct AntennaRow {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub src: String,
    pub user_list_id: Option<String>,
    pub keywords: String,
    pub exclude_keywords: String,
    pub users: Vec<String>,
    pub case_sensitive: bool,
    pub local_only: bool,
    pub exclude_bots: bool,
    pub with_replies: bool,
    pub with_file: bool,
    pub exclude_notes_in_sensitive_channel: bool,
    pub is_active: bool,
}

const ANTENNA_COLS: &str = r#""id", "userId" AS "user_id", "name", src::text AS "src",
           "userListId" AS "user_list_id", keywords::text AS "keywords",
           "excludeKeywords"::text AS "exclude_keywords", "users",
           "caseSensitive" AS "case_sensitive", "localOnly" AS "local_only",
           "excludeBots" AS "exclude_bots", "withReplies" AS "with_replies",
           "withFile" AS "with_file",
           "excludeNotesInSensitiveChannel" AS "exclude_notes_in_sensitive_channel",
           "isActive" AS "is_active""#;

/// `AntennasRepository.findOneBy({ id, userId })` — every antennas/* endpoint
/// scopes the antenna to its OWNER (another user's antennaId resolves
/// NO_SUCH_ANTENNA, never 403).
pub async fn find_antenna(
    pool: &PgPool,
    antenna_id: &str,
    owner_id: &str,
) -> Result<Option<AntennaRow>, sqlx::Error> {
    let sql = format!(r#"SELECT {ANTENNA_COLS} FROM "antenna" WHERE "id" = $1 AND "userId" = $2"#);
    sqlx::query_as::<_, AntennaRow>(&sql)
        .bind(antenna_id)
        .bind(owner_id)
        .fetch_optional(pool)
        .await
}

/// Refetch by id alone (update.ts returns `pack(antenna.id)` which re-reads
/// the UPDATED row through findOneByOrFail).
pub async fn find_antenna_by_id(
    pool: &PgPool,
    antenna_id: &str,
) -> Result<Option<AntennaRow>, sqlx::Error> {
    let sql = format!(r#"SELECT {ANTENNA_COLS} FROM "antenna" WHERE "id" = $1"#);
    sqlx::query_as::<_, AntennaRow>(&sql)
        .bind(antenna_id)
        .fetch_optional(pool)
        .await
}

/// `antennasRepository.countBy({ userId: me.id })` — the
/// `roleService.getUserPolicies(me.id).antennaLimit` gate input.
pub async fn count_antennas_owned(pool: &PgPool, owner_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(r#"SELECT count(*) FROM "antenna" WHERE "userId" = $1"#)
        .bind(owner_id)
        .fetch_one(pool)
        .await
}

/// `antennasRepository.insertOne(...)` — create.ts sets `lastUsedAt: now`;
/// `isActive` stays the column default (true).
#[allow(clippy::too_many_arguments)]
pub async fn insert_antenna(
    pool: &PgPool,
    id: &str,
    owner_id: &str,
    name: &str,
    src: &str,
    user_list_id: Option<&str>,
    keywords: &str,
    exclude_keywords: &str,
    users: &[String],
    case_sensitive: bool,
    local_only: bool,
    exclude_bots: bool,
    with_replies: bool,
    with_file: bool,
    exclude_notes_in_sensitive_channel: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO "antenna"
            ("id", "userId", "lastUsedAt", "name", src, "userListId",
             keywords, "excludeKeywords", "users",
             "caseSensitive", "localOnly", "excludeBots",
             "withReplies", "withFile", "excludeNotesInSensitiveChannel")
        VALUES ($1, $2, now(), $3, $4::antenna_src_enum, $5,
             $6::jsonb, $7::jsonb, $8,
             $9, $10, $11, $12, $13, $14)"#,
    )
    .bind(id)
    .bind(owner_id)
    .bind(name)
    .bind(src)
    .bind(user_list_id)
    .bind(keywords)
    .bind(exclude_keywords)
    .bind(users)
    .bind(case_sensitive)
    .bind(local_only)
    .bind(exclude_bots)
    .bind(with_replies)
    .bind(with_file)
    .bind(exclude_notes_in_sensitive_channel)
    .execute(pool)
    .await?;
    Ok(())
}

/// update.ts patch shape: every field optional,
/// `userListId` is a double option (absent = don't touch; `Some(None)` = an
/// explicit null param), `isActive: true` + `lastUsedAt: new Date()` write
/// unconditionally. Rendered as one single-purpose UPDATE per present arm
/// (the endpoint never requires cross-field atomicity; upstream TypeORM
/// update() writes one row in one statement per ps field set).
#[derive(Debug, Default)]
pub struct AntennaPatch {
    pub owner_id: String,
    pub antenna_id: String,
    pub name: Option<String>,
    pub src: Option<String>,
    pub user_list_id: Option<Option<String>>,
    pub keywords: Option<String>,
    pub exclude_keywords: Option<String>,
    pub users: Option<Vec<String>>,
    pub case_sensitive: Option<bool>,
    pub local_only: Option<bool>,
    pub exclude_bots: Option<bool>,
    pub with_replies: Option<bool>,
    pub with_file: Option<bool>,
    pub exclude_notes_in_sensitive_channel: Option<bool>,
}

pub async fn apply_patch(pool: &PgPool, p: &AntennaPatch) -> Result<(), sqlx::Error> {
    const SCOPE: &str = r#" WHERE "id" = $1 AND "userId" = $2"#;
    // update.ts L116-118: the activation + touch ride every update.
    sqlx::query(&format!(
        r#"UPDATE "antenna" SET "isActive" = TRUE, "lastUsedAt" = now(){SCOPE}"#
    ))
    .bind(&p.antenna_id)
    .bind(&p.owner_id)
    .execute(pool)
    .await?;
    if let Some(name) = &p.name {
        sqlx::query(&format!(r#"UPDATE "antenna" SET "name" = $3{SCOPE}"#))
            .bind(&p.antenna_id)
            .bind(&p.owner_id)
            .bind(name)
            .execute(pool)
            .await?;
    }
    if let Some(src) = &p.src {
        sqlx::query(&format!(
            r#"UPDATE "antenna" SET src = $3::antenna_src_enum{SCOPE}"#
        ))
        .bind(&p.antenna_id)
        .bind(&p.owner_id)
        .bind(src)
        .execute(pool)
        .await?;
    }
    match &p.user_list_id {
        // `ps.userListId !== undefined ? userList ? userList.id : null : ...`
        Some(Some(list_id)) => {
            sqlx::query(&format!(r#"UPDATE "antenna" SET "userListId" = $3{SCOPE}"#))
                .bind(&p.antenna_id)
                .bind(&p.owner_id)
                .bind(list_id)
                .execute(pool)
                .await?;
        }
        Some(None) => {
            sqlx::query(&format!(
                r#"UPDATE "antenna" SET "userListId" = NULL{SCOPE}"#
            ))
            .bind(&p.antenna_id)
            .bind(&p.owner_id)
            .execute(pool)
            .await?;
        }
        None => {}
    }
    if let Some(keywords) = &p.keywords {
        sqlx::query(&format!(
            r#"UPDATE "antenna" SET keywords = $3::jsonb{SCOPE}"#
        ))
        .bind(&p.antenna_id)
        .bind(&p.owner_id)
        .bind(keywords)
        .execute(pool)
        .await?;
    }
    if let Some(exclude_keywords) = &p.exclude_keywords {
        sqlx::query(&format!(
            r#"UPDATE "antenna" SET "excludeKeywords" = $3::jsonb{SCOPE}"#
        ))
        .bind(&p.antenna_id)
        .bind(&p.owner_id)
        .bind(exclude_keywords)
        .execute(pool)
        .await?;
    }
    if let Some(users) = &p.users {
        sqlx::query(&format!(r#"UPDATE "antenna" SET "users" = $3{SCOPE}"#))
            .bind(&p.antenna_id)
            .bind(&p.owner_id)
            .bind(users)
            .execute(pool)
            .await?;
    }
    if let Some(v) = p.case_sensitive {
        sqlx::query(&format!(
            r#"UPDATE "antenna" SET "caseSensitive" = $3{SCOPE}"#
        ))
        .bind(&p.antenna_id)
        .bind(&p.owner_id)
        .bind(v)
        .execute(pool)
        .await?;
    }
    if let Some(v) = p.local_only {
        sqlx::query(&format!(r#"UPDATE "antenna" SET "localOnly" = $3{SCOPE}"#))
            .bind(&p.antenna_id)
            .bind(&p.owner_id)
            .bind(v)
            .execute(pool)
            .await?;
    }
    if let Some(v) = p.exclude_bots {
        sqlx::query(&format!(
            r#"UPDATE "antenna" SET "excludeBots" = $3{SCOPE}"#
        ))
        .bind(&p.antenna_id)
        .bind(&p.owner_id)
        .bind(v)
        .execute(pool)
        .await?;
    }
    if let Some(v) = p.with_replies {
        sqlx::query(&format!(
            r#"UPDATE "antenna" SET "withReplies" = $3{SCOPE}"#
        ))
        .bind(&p.antenna_id)
        .bind(&p.owner_id)
        .bind(v)
        .execute(pool)
        .await?;
    }
    if let Some(v) = p.with_file {
        sqlx::query(&format!(r#"UPDATE "antenna" SET "withFile" = $3{SCOPE}"#))
            .bind(&p.antenna_id)
            .bind(&p.owner_id)
            .bind(v)
            .execute(pool)
            .await?;
    }
    if let Some(v) = p.exclude_notes_in_sensitive_channel {
        sqlx::query(&format!(
            r#"UPDATE "antenna" SET "excludeNotesInSensitiveChannel" = $3{SCOPE}"#
        ))
        .bind(&p.antenna_id)
        .bind(&p.owner_id)
        .bind(v)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// `antennasRepository.delete(antenna.id)` — the projection rows follow the
/// antenna FK ON DELETE CASCADE (upstream purges the redis list instead).
pub async fn delete_antenna(pool: &PgPool, antenna_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "antenna" WHERE "id" = $1"#)
        .bind(antenna_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// `antennasRepository.findBy({ userId: me.id })` — antennas/list (no order
/// is asserted upstream; stable by id).
pub async fn list_antennas(pool: &PgPool, owner_id: &str) -> Result<Vec<AntennaRow>, sqlx::Error> {
    let sql = format!(r#"SELECT {ANTENNA_COLS} FROM "antenna" WHERE "userId" = $1 ORDER BY "id""#);
    sqlx::query_as::<_, AntennaRow>(&sql)
        .bind(owner_id)
        .fetch_all(pool)
        .await
}

/// notes.ts re-activation write (`antenna.isActive = true; lastUsedAt = now`;
/// the needPublishEvent leg is a streaming concern and stays out).
pub async fn mark_used(pool: &PgPool, antenna_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "antenna" SET "isActive" = TRUE, "lastUsedAt" = now() WHERE "id" = $1"#)
        .bind(antenna_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// `antennas/remove-note` (remove-note.ts → fanout `LREM`): drops one note id
/// from one feed. No existence check on either leg — removing an unknown
/// noteId succeeds (`から存在しないノートを削除しても成功すること。`).
pub async fn remove_note_from_antenna(
    pool: &PgPool,
    antenna_id: &str,
    note_id: &str,
) -> Result<(), sqlx::Error> {
    match remove_note_once(pool, antenna_id, note_id).await {
        Err(e) if crate::notification::is_undefined_table(&e) => {
            ensure_antenna_note_table(pool).await?;
            remove_note_once(pool, antenna_id, note_id).await
        }
        r => r,
    }
}

async fn remove_note_once(
    pool: &PgPool,
    antenna_id: &str,
    note_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "antenna_note" WHERE "antennaId" = $1 AND "noteId" = $2"#)
        .bind(antenna_id)
        .bind(note_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Idempotent replay of migrations/0004_antennas.sql (ntf-mute idiom): the
/// harness's per-suite initTestDb dropSchema+synchronize rebuilds the schema
/// from upstream ENTITIES only — the pinned upstream has no AntennaNote
/// entity (redis fanout), so the projection table vanishes under a live
/// server between suite files (SQLSTATE 42P01). Index/constraint re-creates
/// swallow duplicates; PK/FK lack IF NOT EXISTS, so they ride DO blocks.
pub async fn ensure_antenna_note_table(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS public.antenna_note (
            id character varying(32) NOT NULL,
            "noteId" character varying(32) NOT NULL,
            "antennaId" character varying(32) NOT NULL,
            read boolean DEFAULT false NOT NULL
        )"#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"
        DO $do$ BEGIN
            ALTER TABLE ONLY public.antenna_note
                ADD CONSTRAINT "PK_antenna_note" PRIMARY KEY (id);
        EXCEPTION WHEN duplicate_object THEN NULL; END $do$;"#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"CREATE INDEX IF NOT EXISTS "IDX_antenna_note_antennaId"
               ON public.antenna_note USING btree ("antennaId")"#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"CREATE UNIQUE INDEX IF NOT EXISTS "IDX_antenna_note_pair"
               ON public.antenna_note USING btree ("antennaId", "noteId")"#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"
        DO $do$ BEGIN
            ALTER TABLE ONLY public.antenna_note
                ADD CONSTRAINT "FK_antenna_note_note"
                FOREIGN KEY ("noteId") REFERENCES public."note"("id") ON DELETE CASCADE;
        EXCEPTION WHEN duplicate_object THEN NULL; END $do$;"#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"
        DO $do$ BEGIN
            ALTER TABLE ONLY public.antenna_note
                ADD CONSTRAINT "FK_antenna_note_antenna"
                FOREIGN KEY ("antennaId") REFERENCES public."antenna"("id") ON DELETE CASCADE;
        EXCEPTION WHEN duplicate_object THEN NULL; END $do$;"#,
    )
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Fanout — AntennaService.addNoteToAntennas
// ---------------------------------------------------------------------------

/// The note + noteUser input (`{ ...note, channel: data.channel ?? null }`),
/// author join folded in (checkHitAntenna needs username/host/isBot of the
/// AUTHOR, not the antenna owner).
#[derive(Debug, sqlx::FromRow)]
struct FanoutNoteRow {
    id: String,
    reply_id: Option<String>,
    text: Option<String>,
    cw: Option<String>,
    user_id: String,
    visibility: String,
    visible_user_ids: Vec<String>,
    file_ids: Vec<String>,
    channel_id: Option<String>,
    username_lower: String,
    user_host: Option<String>,
    is_bot: bool,
}

/// Acct normalization below `Acct.parse` + `getFullApAccount`
/// (UtilityService.ts L28: `${username}@${host}` with LOCAL users riding the
/// config host — both sides pass through the same string, so the empty host
/// component slot stands in for the config host and local-local matching
/// cancels out; a remote author only ever matches a `@user@remote` param of
/// the same host. Lowercasing both sides mirrors the endpoint's
/// `toLowerCase()` compare).
fn acct_str(username: &str, host: Option<&str>) -> String {
    format!(
        "{}@{}",
        username.to_lowercase(),
        host.unwrap_or_default().to_lowercase()
    )
}

/// `keywords.some(and => and.every(keyword => _text.includes(keyword)))` with
/// the caseSensitive arm. The upstream cleanup runs first (`map(xs =>
/// xs.filter(x => x !== '')).filter(xs => xs.length > 0)`), so whether a
/// cleaned matrix has any group decides the check at all; an empty cleaned
/// matrix means "no gate" and returns false for "not matched".
fn keyword_groups_match(groups: &[Vec<String>], text: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        groups
            .iter()
            .any(|g| g.iter().all(|k| text.contains(k.as_str())))
    } else {
        let t = text.to_lowercase();
        groups
            .iter()
            .any(|g| g.iter().all(|k| t.contains(&k.to_lowercase())))
    }
}

/// jsonb `string[][]` decode dropping the '' members and the emptied groups
/// (upstream `keywords.map(xs => xs.filter(x => x !== '')).filter(xs =>
/// xs.length > 0)`) — the caller skips the keyword gate when the result is
/// empty. Unparseable jsonb (impossible through the param-linted writes)
/// yields no gate.
fn cleaned_groups(raw: &str) -> Vec<Vec<String>> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for group in arr {
        let Some(inner) = group.as_array() else {
            continue;
        };
        let g: Vec<String> = inner
            .iter()
            .filter_map(|k| k.as_str())
            .filter(|k| !k.is_empty())
            .map(str::to_string)
            .collect();
        if !g.is_empty() {
            out.push(g);
        }
    }
    out
}

/// `getAntennas()` — `findBy({ isActive: true })` for the note-creation fanout.
async fn list_active_for_fanout(pool: &PgPool) -> Result<Vec<AntennaRow>, sqlx::Error> {
    let sql =
        format!(r#"SELECT {ANTENNA_COLS} FROM "antenna" WHERE "isActive" = TRUE ORDER BY "id""#);
    sqlx::query_as::<_, AntennaRow>(&sql).fetch_all(pool).await
}

/// checkHitAntenna (full ladder, upstream order preserved).
async fn check_hit(
    pool: &PgPool,
    a: &AntennaRow,
    note: &FanoutNoteRow,
    channel_is_sensitive: bool,
) -> Result<bool, sqlx::Error> {
    // 1. excludeNotesInSensitiveChannel && note.channel?.isSensitive
    if a.exclude_notes_in_sensitive_channel && channel_is_sensitive {
        return Ok(false);
    }
    // 2. excludeBots && noteUser.isBot
    if a.exclude_bots && note.is_bot {
        return Ok(false);
    }
    // 3. localOnly && noteUser.host != null
    if a.local_only && note.user_host.is_some() {
        return Ok(false);
    }
    // 4. !withReplies && note.replyId != null
    if !a.with_replies && note.reply_id.is_some() {
        return Ok(false);
    }
    // 5. visibility === 'specified': visible to the antenna owner (or authored
    //    by them) only — the DB row stores '{}' for non-specified notes.
    if note.visibility == "specified" && note.user_id != a.user_id {
        if !note.visible_user_ids.iter().any(|v| v == &a.user_id) {
            return Ok(false);
        }
    }
    // 6. visibility === 'followers': the antenna owner follows the author (or
    //    owns the note).
    if note.visibility == "followers"
        && note.user_id != a.user_id
        && !following_exists(pool, &a.user_id, &note.user_id).await?
    {
        return Ok(false);
    }
    // 7. src arms (home is an upstream TODO → behaves as all).
    match a.src.as_str() {
        "list" => {
            let Some(list_id) = &a.user_list_id else {
                return Ok(false);
            };
            if !crate::lists::membership_exists(pool, list_id, &note.user_id).await? {
                return Ok(false);
            }
        }
        "users" => {
            let author = acct_str(&note.username_lower, note.user_host.as_deref());
            let params: Vec<String> = a
                .users
                .iter()
                .map(|u| {
                    let (name, host) = parse_acct(u);
                    acct_str(&name, host.as_deref())
                })
                .collect();
            if !params.iter().any(|p| p == &author) {
                return Ok(false);
            }
        }
        "users_blacklist" => {
            let author = acct_str(&note.username_lower, note.user_host.as_deref());
            let params: Vec<String> = a
                .users
                .iter()
                .map(|u| {
                    let (name, host) = parse_acct(u);
                    acct_str(&name, host.as_deref())
                })
                .collect();
            if params.iter().any(|p| p == &author) {
                return Ok(false);
            }
        }
        // "home" | "all" pass (everything else cannot exist past paramDef).
        _ => {}
    }
    // 8/9. keyword planes over `text + '\n' + cw`.
    let keywords = cleaned_groups(&a.keywords);
    let exclude_keywords = cleaned_groups(&a.exclude_keywords);
    if !keywords.is_empty() || !exclude_keywords.is_empty() {
        if note.text.is_none() && note.cw.is_none() {
            return Ok(false);
        }
        let material = format!(
            "{}\n{}",
            note.text.as_deref().unwrap_or(""),
            note.cw.as_deref().unwrap_or("")
        );
        if !keywords.is_empty() && !keyword_groups_match(&keywords, &material, a.case_sensitive) {
            return Ok(false);
        }
        if !exclude_keywords.is_empty()
            && keyword_groups_match(&exclude_keywords, &material, a.case_sensitive)
        {
            return Ok(false);
        }
    }
    // 10. withFile
    if a.with_file && note.file_ids.is_empty() {
        return Ok(false);
    }
    Ok(true)
}

/// `Acct.parse(acct)`: a leading '@' is optional, the host is the SEGMENT
/// after the first '@' (empty host → None → local).
fn parse_acct(acct: &str) -> (String, Option<String>) {
    let s = acct.strip_prefix('@').unwrap_or(acct);
    let mut parts = s.splitn(2, '@');
    let name = parts.next().unwrap_or_default().to_string();
    let host = parts.next().filter(|h| !h.is_empty()).map(str::to_string);
    (name, host)
}

/// `cacheService.userFollowingsCache.fetch(antenna.userId)` membership probe:
/// the antenna OWNER follows the note AUTHOR (Object.hasOwn over the owner's
/// followings map keyed by followeeId).
async fn following_exists(
    pool: &PgPool,
    follower_id: &str,
    followee_id: &str,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query_scalar::<_, i64>(
        r#"SELECT count(*) FROM "following" WHERE "followerId" = $1 AND "followeeId" = $2"#,
    )
    .bind(follower_id)
    .bind(followee_id)
    .fetch_one(pool)
    .await?
        > 0)
}

/// addNoteToAntennas for a just-created note. The projection row PK is minted
/// server-side (any unique 32-char string; nothing downstream orders by it —
/// the feed sorts on the note id). `ON CONFLICT ("antennaId", "noteId") DO
/// NOTHING` keeps the write idempotent under handler retries. Best-effort by
/// contract — the caller logs failures and never fails the note creation.
/// (ntf-mute idiom: the harness's per-suite `initTestDb` dropSchema+
/// synchronize wipes every table without an upstream ENTITY — `antenna_note`
/// has none in the pinned upstream (redis fanout) — so every touch replays
/// the idempotent 0004 DDL once on SQLSTATE 42P01 and retries.)
pub async fn fanout_created_note(pool: &PgPool, note_id: &str) -> Result<(), sqlx::Error> {
    let Some(note) = sqlx::query_as::<_, FanoutNoteRow>(
        r#"SELECT n."id", n."replyId" AS "reply_id", n."text", n."cw",
                  n."userId" AS "user_id", n."visibility"::text AS "visibility",
                  n."visibleUserIds" AS "visible_user_ids",
                  n."fileIds" AS "file_ids", n."channelId" AS "channel_id",
                  u."usernameLower" AS "username_lower", u."host" AS "user_host",
                  u."isBot" AS "is_bot"
           FROM "note" n JOIN "user" u ON u."id" = n."userId"
           WHERE n."id" = $1"#,
    )
    .bind(note_id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(());
    };

    // `note.channel` join — checkHitAntenna reads only channel?.isSensitive.
    let channel_is_sensitive = match &note.channel_id {
        Some(cid) => sqlx::query_scalar::<_, bool>(
            r#"SELECT COALESCE("isSensitive", false) FROM "channel" WHERE "id" = $1"#,
        )
        .bind(cid)
        .fetch_one(pool)
        .await
        .unwrap_or(false),
        None => false,
    };

    let antennas = list_active_for_fanout(pool).await?;
    for a in &antennas {
        if !check_hit(pool, a, &note, channel_is_sensitive).await? {
            continue;
        }
        match insert_projection_row(pool, &note.id, &a.id).await {
            Err(e) if crate::notification::is_undefined_table(&e) => {
                ensure_antenna_note_table(pool).await?;
                insert_projection_row(pool, &note.id, &a.id).await?;
            }
            r => r?,
        }
    }
    Ok(())
}

async fn insert_projection_row(
    pool: &PgPool,
    note_id: &str,
    antenna_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO "antenna_note" ("id", "noteId", "antennaId")
        VALUES (md5(random()::text || clock_timestamp()::text), $1, $2)
        ON CONFLICT ("antennaId", "noteId") DO NOTHING"#,
    )
    .bind(note_id)
    .bind(antenna_id)
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// antennas/notes — notes.ts get-from-feed with the shared mk-db filter set
// ---------------------------------------------------------------------------

/// channelMuting read-side plane (notes.ts L127-139): the note's own channel
/// drops out when muted; a renote of a muted-channel note drops out too
/// (lists.rs RENOTE_CHANNEL_MUTE shape, $2 = me).
const CHANNEL_MUTE_PLANE: &str = r#" AND (n."channelId" IS NULL OR n."channelId" NOT IN (
              SELECT "channelId" FROM "channel_muting" WHERE "userId" = $2
                AND ("expiresAt" IS NULL OR "expiresAt" > now())))
        AND (n."renoteId" IS NULL OR NOT EXISTS (
              SELECT 1 FROM "note" rc WHERE rc."id" = n."renoteId"
                AND rc."channelId" IS NOT NULL
                AND rc."channelId" IN (
                    SELECT "channelId" FROM "channel_muting" WHERE "userId" = $2
                      AND ("expiresAt" IS NULL OR "expiresAt" > now()))))"#;

/// The feed page: projection join + visibility + base filtering + muted
/// channels. Bind map: $1 antennaId, $2 me, $3 [me], $4 my followees,
/// $5 untilId, $6 sinceId; limit + direction format-inlined. sinceId-only is
/// the only oldest-first ASC shape (`notes.sort` in antennas.ts notes).
pub async fn antenna_notes_page(
    pool: &PgPool,
    antenna_id: &str,
    me_id: &str,
    followee_ids: &[String],
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<String>, sqlx::Error> {
    let dir = if since_id.is_some() && until_id.is_none() {
        "ASC"
    } else {
        "DESC"
    };

    let mut inner = String::from(
        r#"SELECT n."id" FROM "note" n
           INNER JOIN "user" u ON u."id" = n."userId"
           INNER JOIN "antenna_note" an
                ON an."noteId" = n."id" AND an."antennaId" = $1
           WHERE TRUE"#,
    );
    inner.push_str(VIS_ME);
    inner.push_str(BASE_ME);
    inner.push_str(CHANNEL_MUTE_PLANE);
    inner.push_str(&format!(
        r#" AND ($5::varchar IS NULL OR n."id" < $5::varchar)
            AND ($6::varchar IS NULL OR n."id" > $6::varchar)
            ORDER BY n."id" {dir} LIMIT {limit}"#
    ));

    let me_list = vec![me_id.to_string()];
    let sql = inner
        .replace("MUTE_SEL", crate::lists::MUTE_SEL)
        .replace("BLOCK_SEL", crate::lists::BLOCK_SEL);
    match antenna_notes_page_once(
        pool,
        &sql,
        antenna_id,
        me_id,
        &me_list,
        followee_ids,
        until_id,
        since_id,
    )
    .await
    {
        Err(e) if crate::notification::is_undefined_table(&e) => {
            ensure_antenna_note_table(pool).await?;
            antenna_notes_page_once(
                pool,
                &sql,
                antenna_id,
                me_id,
                &me_list,
                followee_ids,
                until_id,
                since_id,
            )
            .await
        }
        r => r,
    }
}

#[allow(clippy::too_many_arguments)]
async fn antenna_notes_page_once(
    pool: &PgPool,
    sql: &str,
    antenna_id: &str,
    me_id: &str,
    me_list: &[String],
    followee_ids: &[String],
    until_id: Option<&str>,
    since_id: Option<&str>,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(sql)
        .bind(antenna_id)
        .bind(me_id)
        .bind(me_list)
        .bind(followee_ids)
        .bind(until_id)
        .bind(since_id)
        .fetch_all(pool)
        .await
}

/// The fixed bind layout must stay hole-free: the mute plane binds only the
/// $2 view (lists.rs fragment-test convention).
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mute_plane_reference_only_fixed_binds() {
        let mut max = 0;
        let mut rest = CHANNEL_MUTE_PLANE;
        while let Some(i) = rest.find('$') {
            let digit = rest[i + 1..].chars().next().unwrap();
            assert!(digit.is_ascii_digit(), "malformed bind in {rest}");
            max = max.max(digit as u8 - b'0');
            rest = &rest[i + 1..];
        }
        assert!(max <= 2, "bind ${max} out of range");
    }

    #[test]
    fn acct_parse_matches_endpoint_semantics() {
        // Acct.parse: leading '@' optional, '@'-split host.
        assert_eq!(parse_acct("bob"), ("bob".to_string(), None));
        assert_eq!(parse_acct("@bob"), ("bob".to_string(), None));
        assert_eq!(
            parse_acct("@bob@remote.example"),
            ("bob".to_string(), Some("remote.example".to_string()))
        );
        assert_eq!(parse_acct("bob@"), ("bob".to_string(), None));
        // getFullApAccount + toLowerCase on both sides: local users share the
        // empty host slot, remote hosts compare verbatim.
        assert_eq!(acct_str("BOB", None), "bob@");
        assert_eq!(
            acct_str("bob", Some("Remote.Example")),
            "bob@remote.example"
        );
    }

    #[test]
    fn cleaned_groups_drop_blanks_and_empty_groups() {
        // `[[]]` cleans to an empty matrix → the keyword gate is skipped.
        assert_eq!(cleaned_groups(r#"[[""],[""]]"#), Vec::<Vec<String>>::new());
        assert_eq!(
            cleaned_groups(r#"[["A",""],["B"]]"#),
            vec![vec!["A".to_string()], vec!["B".to_string()]]
        );
    }

    #[test]
    fn keyword_matching_and_or_case() {
        // キーワード3つ(AND): every keyword inside one group
        let and3 = vec![vec!["A".to_string(), "B".to_string(), "C".to_string()]];
        assert!(keyword_groups_match(&and3, "test C B A A B C", false));
        assert!(!keyword_groups_match(&and3, "test B C", false));
        // キーワード3つ(OR): any group
        let or3 = vec![
            vec!["A".to_string()],
            vec!["B".to_string()],
            vec!["C".to_string()],
        ];
        assert!(keyword_groups_match(&or3, "test C", false));
        assert!(!keyword_groups_match(&or3, "test", false));
        // caseSensitive arm
        let kw = vec![vec!["KEYWORD".to_string()]];
        assert!(keyword_groups_match(&kw, "keyword", false));
        assert!(!keyword_groups_match(&kw, "keyword", true));
        assert!(keyword_groups_match(&kw, "KEYWORD", true));
    }
}
