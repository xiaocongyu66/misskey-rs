//! Hashtag domain db paths (upstream `endpoints/hashtags/*.ts` +
//! `core/entities/HashtagEntityService.ts`, `core/HashtagService.ts`,
//! `core/FeaturedService.ts`).
//!
//! `hashtag` ships in the 0001 baseline with the full mentioned/attached
//! counter set (`HashtagEntityService.pack` reads seven of them; the
//! `*UserIds` arrays are not exposed). `__chart__hashtag` is the hourly
//! chart table: `date` is the epoch-second hour bucket (`charts::bucket`),
//! `group` is the normalized tag, `___local_users`/`___remote_users` are the
//! per-bucket unique-user counters (the `unique_temp___*` arrays are the
//! chart engine's scratch state and are not read here).
//!
//! `hashtags/trend` upstream ranks from Redis (`FeaturedService.
//! getHashtagsRanking`) and renders `HashtagService.getCharts` (20 ten-minute
//! HyperLogLog windows). This port has no Redis chart plane, so it reads the
//! `__chart__hashtag` hourly rows of the last 20 hours instead, zero-filled
//! oldest→newest; `usersCount` is the window maximum (upstream's
//! `Math.max(...charts[tag])`). When the chart has no rows at all it falls
//! back to the `hashtag` table ordered by `mentionedUsersCount` (the count
//! column upstream's ranking is derived from) and returns `chart: []`, since
//! there is no time series to render.
//!
//! `hashtags/users` is not wired: upstream selects `user.tags`, but the
//! `UserDetailed` relationship pack it returns is outside this slice.

use crate::sqlx::PgPool;

/// The seven `HashtagEntityService.pack` counters.
const COLS: &str = r#""name",
"mentionedUsersCount" AS mentioned_users_count,
"mentionedLocalUsersCount" AS mentioned_local_users_count,
"mentionedRemoteUsersCount" AS mentioned_remote_users_count,
"attachedUsersCount" AS attached_users_count,
"attachedLocalUsersCount" AS attached_local_users_count,
"attachedRemoteUsersCount" AS attached_remote_users_count"#;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct HashtagRow {
    pub name: String,
    pub mentioned_users_count: i32,
    pub mentioned_local_users_count: i32,
    pub mentioned_remote_users_count: i32,
    pub attached_users_count: i32,
    pub attached_local_users_count: i32,
    pub attached_remote_users_count: i32,
}

/// `hashtagsRepository.findOneBy({ name: normalizeForSearch(tag) })`.
pub async fn find_by_name(pool: &PgPool, name: &str) -> Result<Option<HashtagRow>, sqlx::Error> {
    let sql = format!(r#"SELECT {COLS} FROM "hashtag" WHERE "name" = $1"#);
    sqlx::query_as::<_, HashtagRow>(&sql)
        .bind(name)
        .fetch_optional(pool)
        .await
}

/// `hashtags/list` query builder: the optional attached-only predicates plus
/// one of the twelve whitelisted `sort` orders.
pub async fn list(
    pool: &PgPool,
    limit: i64,
    attached_to_user_only: bool,
    attached_to_local_user_only: bool,
    attached_to_remote_user_only: bool,
    sort: &str,
) -> Result<Vec<HashtagRow>, sqlx::Error> {
    let (column, direction) = sort_column(sort);
    let mut sql = format!(r#"SELECT {COLS} FROM "hashtag" WHERE TRUE"#);
    if attached_to_user_only {
        sql.push_str(r#" AND "attachedUsersCount" != 0"#);
    }
    if attached_to_local_user_only {
        sql.push_str(r#" AND "attachedLocalUsersCount" != 0"#);
    }
    if attached_to_remote_user_only {
        sql.push_str(r#" AND "attachedRemoteUsersCount" != 0"#);
    }
    sql.push_str(&format!(r#" ORDER BY {column} {direction}"#));
    sql.push_str(" LIMIT $1");
    sqlx::query_as::<_, HashtagRow>(&sql)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// `hashtags/list` sort enum → (column, direction). Unknown values fall back
/// to `+mentionedUsers` (the api layer rejects them first).
fn sort_column(sort: &str) -> (&'static str, &'static str) {
    match sort {
        "+mentionedUsers" => (r#""mentionedUsersCount""#, "DESC"),
        "-mentionedUsers" => (r#""mentionedUsersCount""#, "ASC"),
        "+mentionedLocalUsers" => (r#""mentionedLocalUsersCount""#, "DESC"),
        "-mentionedLocalUsers" => (r#""mentionedLocalUsersCount""#, "ASC"),
        "+mentionedRemoteUsers" => (r#""mentionedRemoteUsersCount""#, "DESC"),
        "-mentionedRemoteUsers" => (r#""mentionedRemoteUsersCount""#, "ASC"),
        "+attachedUsers" => (r#""attachedUsersCount""#, "DESC"),
        "-attachedUsers" => (r#""attachedUsersCount""#, "ASC"),
        "+attachedLocalUsers" => (r#""attachedLocalUsersCount""#, "DESC"),
        "-attachedLocalUsers" => (r#""attachedLocalUsersCount""#, "ASC"),
        "+attachedRemoteUsers" => (r#""attachedRemoteUsersCount""#, "DESC"),
        "-attachedRemoteUsers" => (r#""attachedRemoteUsersCount""#, "ASC"),
        _ => (r#""mentionedUsersCount""#, "DESC"),
    }
}

/// One `hashtags/trend` element (`{ tag, chart, usersCount }`).
#[derive(Debug, Clone)]
pub struct TrendEntry {
    pub tag: String,
    pub chart: Vec<i64>,
    pub users_count: i64,
}

/// Number of hourly buckets rendered in `chart` (mirrors upstream's 20
/// ten-minute windows).
const TREND_SLOTS: i64 = 20;

/// `hashtags/trend` over the `__chart__hashtag` hourly table. See the module
/// docs for the chart→`hashtag` fallback.
pub async fn trend(pool: &PgPool, now_ms: i64) -> Result<Vec<TrendEntry>, sqlx::Error> {
    let hour = crate::charts::bucket("hour", now_ms);
    let from = hour - (TREND_SLOTS - 1) * 3600;

    let rows: Vec<(String, i32, i32)> = sqlx::query_as(
        r#"SELECT "group", "date", ("___local_users" + "___remote_users")
           FROM "__chart__hashtag"
           WHERE "date" >= $1 AND "date" <= $2
           ORDER BY "date" ASC"#,
    )
    .bind(from as i32)
    .bind(hour as i32)
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        // Chart empty: rank the `hashtag` table's mentioned-user counter.
        let fallback: Vec<(String, i32)> = sqlx::query_as(
            r#"SELECT "name", "mentionedUsersCount"
               FROM "hashtag"
               ORDER BY "mentionedUsersCount" DESC, "name" ASC
               LIMIT 10"#,
        )
        .fetch_all(pool)
        .await?;
        return Ok(fallback
            .into_iter()
            .map(|(tag, users)| TrendEntry {
                tag,
                chart: Vec::new(),
                users_count: users as i64,
            })
            .collect());
    }

    let mut per_tag: std::collections::BTreeMap<String, std::collections::BTreeMap<i32, i64>> =
        std::collections::BTreeMap::new();
    for (tag, date, users) in rows {
        per_tag.entry(tag).or_default().insert(date, users as i64);
    }

    let mut entries: Vec<TrendEntry> = per_tag
        .into_iter()
        .map(|(tag, buckets)| {
            let mut chart = Vec::with_capacity(TREND_SLOTS as usize);
            let mut max = 0;
            for slot in 0..TREND_SLOTS {
                let date = (from + slot * 3600) as i32;
                let users = *buckets.get(&date).unwrap_or(&0);
                max = max.max(users);
                chart.push(users);
            }
            TrendEntry {
                tag,
                chart,
                users_count: max,
            }
        })
        .collect();

    entries.sort_by(|a, b| {
        b.users_count
            .cmp(&a.users_count)
            .then_with(|| a.tag.cmp(&b.tag))
    });
    entries.truncate(10);
    Ok(entries)
}

/// `endpoints/hashtags/search.ts` — `name LIKE '<lowercased query>%'`
/// ordered by mentionedLocalUsersCount DESC, returning the names only.
pub async fn search(
    pool: &PgPool,
    query_lower: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<String>, sqlx::Error> {
    let escaped = query_lower
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let pattern = format!("{escaped}%");
    sqlx::query_scalar::<_, String>(
        r#"SELECT "name" FROM "hashtag"
           WHERE "name" LIKE $1 ESCAPE '\'
           ORDER BY "mentionedLocalUsersCount" DESC, "name" ASC
           LIMIT $2 OFFSET $3"#,
    )
    .bind(pattern)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}
