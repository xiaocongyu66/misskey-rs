//! Note charts (upstream `core/chart/charts/notes.ts` + `core.ts`).
//!
//! The `__chart__notes` (hour span) and `__chart_day__notes` (day span)
//! tables ship in the 0001 baseline with the upstream column set; `date` is
//! the span-normalized epoch second under a UNIQUE btree, which makes the
//! commit an UPSERT with `+diff` arithmetic and the read a range scan.
//!
//! Commit mirrors `NoteChart.update(note, isAdditional)`: the local/remote
//! prefix rides `note.userHost`, `total` moves ±1, `inc`/`dec` count the
//! direction once, and the four diffs (normal/renote/reply/withFile) each
//! move ±1 when their predicate holds.
//!
//! Read mirrors `getChartRaw`: rows `date BETWEEN gt AND lt` newest-first,
//! zero-filled per span slot; slots older than every row extrapolate the
//! most recent known values (upstream's "fetch the latest log" step), and
//! the flat `local.total` keys nest into `{local: {total: [...]}, ...}` the
//! way `nestedProperty.set` does for the endpoint response.

use sqlx::{Column, PgPool, Row, TypeInfo};

/// Epoch second normalized to the span bucket (`Chart.dateToTimestamp`).
pub fn bucket(span: &str, at_ms: i64) -> i64 {
    let secs = at_ms.div_euclid(1000);
    match span {
        "hour" => secs.div_euclid(3600) * 3600,
        _ => secs.div_euclid(86_400) * 86_400,
    }
}

/// `NoteChart.update` — one UPSERT per note create/delete.
///
/// `is_additional` is `true` on create and `false` on delete.
#[allow(clippy::too_many_arguments)]
pub async fn update_note(
    pool: &PgPool,
    span: &str,
    date: i64,
    user_host: Option<&str>,
    reply_id: Option<&str>,
    renote_id: Option<&str>,
    has_files: bool,
    is_additional: bool,
) -> Result<(), sqlx::Error> {
    let table = match span {
        "hour" => "__chart__notes",
        _ => "__chart_day__notes",
    };
    let prefix = if user_host.is_none() {
        "local"
    } else {
        "remote"
    };
    let (total, dir) = if is_additional { (1, 1) } else { (-1, -1) };
    let d_normal = if reply_id.is_none() && renote_id.is_none() {
        dir
    } else {
        0
    };
    let d_renote = if renote_id.is_some() { dir } else { 0 };
    let d_reply = if reply_id.is_some() { dir } else { 0 };
    let d_file = if has_files { dir } else { 0 };
    let sql = format!(
        r#"INSERT INTO "{table}" ("date",
               "___{prefix}_total", "___{prefix}_inc", "___{prefix}_dec",
               "___{prefix}_diffs_normal", "___{prefix}_diffs_renote",
               "___{prefix}_diffs_reply", "___{prefix}_diffs_withFile")
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
           ON CONFLICT ("date") DO UPDATE SET
               "___{prefix}_total" = "{table}"."___{prefix}_total" + EXCLUDED."___{prefix}_total",
               "___{prefix}_inc" = "{table}"."___{prefix}_inc" + EXCLUDED."___{prefix}_inc",
               "___{prefix}_dec" = "{table}"."___{prefix}_dec" + EXCLUDED."___{prefix}_dec",
               "___{prefix}_diffs_normal" = "{table}"."___{prefix}_diffs_normal" + EXCLUDED."___{prefix}_diffs_normal",
               "___{prefix}_diffs_renote" = "{table}"."___{prefix}_diffs_renote" + EXCLUDED."___{prefix}_diffs_renote",
               "___{prefix}_diffs_reply" = "{table}"."___{prefix}_diffs_reply" + EXCLUDED."___{prefix}_diffs_reply",
               "___{prefix}_diffs_withFile" = "{table}"."___{prefix}_diffs_withFile" + EXCLUDED."___{prefix}_diffs_withFile""#
    );
    sqlx::query(&sql)
        .bind(date as i32)
        .bind(total)
        .bind(if is_additional { 1 } else { 0 })
        .bind(if is_additional { 0 } else { 1 })
        .bind(d_normal)
        .bind(d_renote)
        .bind(d_reply)
        .bind(d_file)
        .execute(pool)
        .await?;
    Ok(())
}

/// One span bucket's counters (0 where the row is absent).
#[derive(Debug, Default, Clone)]
pub struct NoteCounts {
    pub total: i32,
    pub inc: i32,
    pub dec: i32,
    pub diffs_normal: i32,
    pub diffs_renote: i32,
    pub diffs_reply: i32,
    pub diffs_with_file: i32,
}

async fn bucket_counts(
    pool: &PgPool,
    span: &str,
    date: i64,
) -> Result<[NoteCounts; 2], sqlx::Error> {
    let table = match span {
        "hour" => "__chart__notes",
        _ => "__chart_day__notes",
    };
    let sql = format!(
        r#"SELECT "___local_total", "___local_inc", "___local_dec",
                  "___local_diffs_normal", "___local_diffs_renote",
                  "___local_diffs_reply", "___local_diffs_withFile",
                  "___remote_total", "___remote_inc", "___remote_dec",
                  "___remote_diffs_normal", "___remote_diffs_renote",
                  "___remote_diffs_reply", "___remote_diffs_withFile"
           FROM "{table}" WHERE "date" = $1"#
    );
    let row = sqlx::query_as::<
        _,
        (
            i32,
            i32,
            i32,
            i32,
            i32,
            i32,
            i32,
            i32,
            i32,
            i32,
            i32,
            i32,
            i32,
            i32,
        ),
    >(&sql)
    .bind(date as i32)
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        None => [NoteCounts::default(), NoteCounts::default()],
        Some((lt, li, ld, ln, lr, lp, lw, rt, ri, rd, rn, rr, rp, rw)) => {
            let local = NoteCounts {
                total: lt,
                inc: li,
                dec: ld,
                diffs_normal: ln,
                diffs_renote: lr,
                diffs_reply: lp,
                diffs_with_file: lw,
            };
            let remote = NoteCounts {
                total: rt,
                inc: ri,
                dec: rd,
                diffs_normal: rn,
                diffs_renote: rr,
                diffs_reply: rp,
                diffs_with_file: rw,
            };
            [local, remote]
        }
    })
}

/// `getChart` for the notes chart — `limit` slots ending at `until_ms`
/// (now when null), oldest→newest, zero-filled, with the most recent known
/// values extrapolated over leading empty slots (upstream's latest-log step).
pub async fn notes_chart(
    pool: &PgPool,
    span: &str,
    limit: usize,
    until_ms: Option<i64>,
) -> Result<serde_json::Value, sqlx::Error> {
    let step: i64 = match span {
        "hour" => 3600,
        _ => 86_400,
    };
    let end = bucket(
        span,
        until_ms.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0)
        }),
    );
    let mut slots: Vec<(i64, [NoteCounts; 2])> = Vec::with_capacity(limit);
    for k in (0..limit as i64).rev() {
        let date = end - k * step;
        slots.push((date, bucket_counts(pool, span, date).await?));
    }
    // latest-known extrapolation over leading zero rows
    let known = slots
        .iter()
        .rposition(|(_, c)| c[0].total != 0 || c[1].total != 0);
    if let Some(last_known) = known {
        let base = slots[slots.len() - 1 - last_known].1.clone();
        let cut = slots.len() - 1 - last_known;
        for (_, counts) in slots.iter_mut().take(cut) {
            *counts = base.clone();
        }
    }
    // build the nested response — every flat key becomes its own array
    let mut local = serde_json::Map::new();
    let mut remote = serde_json::Map::new();
    for (idx, key) in ["total", "inc", "dec"].iter().enumerate() {
        let get = |c: &NoteCounts| match idx {
            0 => c.total,
            1 => c.inc,
            _ => c.dec,
        };
        local.insert(
            (*key).to_string(),
            serde_json::json!(slots.iter().map(|(_, c)| get(&c[0])).collect::<Vec<_>>()),
        );
        remote.insert(
            (*key).to_string(),
            serde_json::json!(slots.iter().map(|(_, c)| get(&c[1])).collect::<Vec<_>>()),
        );
    }
    let mut diffs_l = serde_json::Map::new();
    let mut diffs_r = serde_json::Map::new();
    for (idx, key) in ["normal", "renote", "reply", "withFile"].iter().enumerate() {
        let get = |c: &NoteCounts| match idx {
            0 => c.diffs_normal,
            1 => c.diffs_renote,
            2 => c.diffs_reply,
            _ => c.diffs_with_file,
        };
        diffs_l.insert(
            (*key).to_string(),
            serde_json::json!(slots.iter().map(|(_, c)| get(&c[0])).collect::<Vec<_>>()),
        );
        diffs_r.insert(
            (*key).to_string(),
            serde_json::json!(slots.iter().map(|(_, c)| get(&c[1])).collect::<Vec<_>>()),
        );
    }
    local.insert("diffs".into(), serde_json::Value::Object(diffs_l));
    remote.insert("diffs".into(), serde_json::Value::Object(diffs_r));
    Ok(serde_json::json!({ "local": local, "remote": remote }))
}

// ==== users chart (site-wide; upstream `core/chart/charts/users.ts`) ====

/// `UsersChart.update(group, isAdditional)` — the site-wide users-count
/// chart (`__chart__users` hour / `__chart_day__users` day, 0001:861+).
/// `group` is the local/remote prefix; is_additional=true is a new user
/// (total+1, inc+1), false is a departure (total-1, dec+1).
pub async fn update_users(
    pool: &PgPool,
    span: &str,
    date: i64,
    group: &str,
    is_additional: bool,
) -> Result<(), sqlx::Error> {
    let table = match span {
        "hour" => "__chart__users",
        _ => "__chart_day__users",
    };
    let sql = format!(
        r#"INSERT INTO "{table}" ("date",
               "___{group}_total", "___{group}_inc", "___{group}_dec")
           VALUES ($1, $2, $3, $4)
           ON CONFLICT ("date") DO UPDATE SET
               "___{group}_total" = "{table}"."___{group}_total" + EXCLUDED."___{group}_total",
               "___{group}_inc" = "{table}"."___{group}_inc" + EXCLUDED."___{group}_inc",
               "___{group}_dec" = "{table}"."___{group}_dec" + EXCLUDED."___{group}_dec""#
    );
    sqlx::query(&sql)
        .bind(date as i32)
        .bind(if is_additional { 1 } else { -1 })
        .bind(if is_additional { 1 } else { 0 })
        .bind(if is_additional { 0 } else { 1 })
        .execute(pool)
        .await?;
    Ok(())
}

/// One span bucket's user counters for both groups (0 where absent).
#[derive(Debug, Default, Clone)]
struct UserCounts {
    total: i32,
    // 0001:865-869 — inc/dec are smallint on the users chart (total integer)
    inc: i16,
    dec: i16,
}

async fn users_counts(
    pool: &PgPool,
    span: &str,
    date: i64,
) -> Result<[UserCounts; 2], sqlx::Error> {
    let table = match span {
        "hour" => "__chart__users",
        _ => "__chart_day__users",
    };
    let sql = format!(
        r#"SELECT "___local_total", "___local_inc", "___local_dec",
                  "___remote_total", "___remote_inc", "___remote_dec"
           FROM "{table}" WHERE "date" = $1"#
    );
    let row = sqlx::query_as::<_, (i32, i16, i16, i32, i16, i16)>(&sql)
        .bind(date as i32)
        .fetch_optional(pool)
        .await?;
    Ok(match row {
        None => [UserCounts::default(), UserCounts::default()],
        Some((lt, li, ld, rt, ri, rd)) => [
            UserCounts {
                total: lt,
                inc: li,
                dec: ld,
            },
            UserCounts {
                total: rt,
                inc: ri,
                dec: rd,
            },
        ],
    })
}

/// `getChart` for the users chart — same slot walk/extrapolation as
/// [`notes_chart`], three flat arrays per group, no diffs.
pub async fn users_chart(
    pool: &PgPool,
    span: &str,
    limit: usize,
    until_ms: Option<i64>,
) -> Result<serde_json::Value, sqlx::Error> {
    let step: i64 = match span {
        "hour" => 3600,
        _ => 86_400,
    };
    let end = bucket(
        span,
        until_ms.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0)
        }),
    );
    let mut slots: Vec<(i64, [UserCounts; 2])> = Vec::with_capacity(limit);
    for k in (0..limit as i64).rev() {
        let date = end - k * step;
        slots.push((date, users_counts(pool, span, date).await?));
    }
    let known = slots
        .iter()
        .rposition(|(_, c)| c[0].total != 0 || c[1].total != 0);
    if let Some(last_known) = known {
        let base = slots[slots.len() - 1 - last_known].1.clone();
        let cut = slots.len() - 1 - last_known;
        for (_, counts) in slots.iter_mut().take(cut) {
            *counts = base.clone();
        }
    }
    let mut local = serde_json::Map::new();
    let mut remote = serde_json::Map::new();
    for (idx, key) in ["total", "inc", "dec"].iter().enumerate() {
        let get = |c: &UserCounts| match idx {
            0 => c.total,
            1 => c.inc as i32,
            _ => c.dec as i32,
        };
        local.insert(
            (*key).to_string(),
            serde_json::json!(slots.iter().map(|(_, c)| get(&c[0])).collect::<Vec<_>>()),
        );
        remote.insert(
            (*key).to_string(),
            serde_json::json!(slots.iter().map(|(_, c)| get(&c[1])).collect::<Vec<_>>()),
        );
    }
    Ok(serde_json::json!({ "local": local, "remote": remote }))
}

// ==== federation chart (site-wide; upstream `core/chart/charts/federation.ts`) ====

/// The federation chart's flat counter columns — all smallint on the 0001
/// baseline (363+), no local/remote prefix (the site-wide totals).
const FEDERATION_KEYS: [&str; 8] = [
    "deliveredInstances",
    "inboxInstances",
    "stalled",
    "sub",
    "pub",
    "pubsub",
    "subActive",
    "pubActive",
];

/// `FederationChart.update` — one multi-column UPSERT per event batch.
/// `fields` carries only the counters this event moves (absent = +0).
pub async fn update_federation(
    pool: &PgPool,
    span: &str,
    date: i64,
    fields: &[(&str, i16)],
) -> Result<(), sqlx::Error> {
    let table = match span {
        "hour" => "__chart__federation",
        _ => "__chart_day__federation",
    };
    // every shipped column, the event's delta or 0 — keeps the UPSERT shape
    // uniform (placeholder order matches the bind order by construction)
    let mut cols = String::new();
    let mut sets = String::new();
    let mut vals: Vec<i16> = Vec::with_capacity(FEDERATION_KEYS.len() + 1);
    vals.push(0); // placeholder for the date bind, never read directly
    for (n, key) in FEDERATION_KEYS.iter().enumerate() {
        if n > 0 {
            cols.push_str(", ");
            sets.push_str(", ");
        }
        cols.push_str(&format!(r#""___{key}""#));
        let delta = fields
            .iter()
            .find(|(k, _)| *k == *key)
            .map(|(_, v)| *v)
            .unwrap_or(0);
        vals.push(delta);
        sets.push_str(&format!(
            r#""___{key}" = "{table}"."___{key}" + EXCLUDED."___{key}""#
        ));
    }
    let placeholders: Vec<String> = (1..=FEDERATION_KEYS.len() + 1)
        .map(|i| format!("${i}"))
        .collect();
    let sql = format!(
        r#"INSERT INTO "{table}" ("date", {cols})
           VALUES ({})
           ON CONFLICT ("date") DO UPDATE SET {sets}"#,
        placeholders.join(", ")
    );
    let mut q = sqlx::query(&sql).bind(date as i32);
    for v in &vals[1..] {
        q = q.bind(v);
    }
    q.execute(pool).await?;
    Ok(())
}

/// `getChart` for the federation chart — the endpoint returns the log keys
/// as one flat object of arrays (no local/remote nesting).
pub async fn federation_chart(
    pool: &PgPool,
    span: &str,
    limit: usize,
    until_ms: Option<i64>,
) -> Result<serde_json::Value, sqlx::Error> {
    let step: i64 = match span {
        "hour" => 3600,
        _ => 86_400,
    };
    let end = bucket(
        span,
        until_ms.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0)
        }),
    );
    let select_cols: Vec<String> = FEDERATION_KEYS
        .iter()
        .map(|k| format!(r#""___{k}""#))
        .collect();
    let table = match span {
        "hour" => "__chart__federation",
        _ => "__chart_day__federation",
    };
    let sql = format!(
        r#"SELECT {} FROM "{table}" WHERE "date" = $1"#,
        select_cols.join(", ")
    );
    let mut slots: Vec<(i16, i16, i16, i16, i16, i16, i16, i16)> = Vec::with_capacity(limit);
    for k in (0..limit as i64).rev() {
        let date = end - k * step;
        let row = sqlx::query_as::<_, (i16, i16, i16, i16, i16, i16, i16, i16)>(&sql)
            .bind(date as i32)
            .fetch_optional(pool)
            .await?;
        slots.push(row.unwrap_or((0, 0, 0, 0, 0, 0, 0, 0)));
    }
    // latest-known extrapolation over leading all-zero rows (getChartRaw)
    let known = slots.iter().rposition(|c| {
        [c.0, c.1, c.2, c.3, c.4, c.5, c.6, c.7]
            .iter()
            .any(|v| *v != 0)
    });
    if let Some(last_known) = known {
        let base = slots[slots.len() - 1 - last_known];
        let cut = slots.len() - 1 - last_known;
        for c in slots.iter_mut().take(cut) {
            *c = base;
        }
    }
    let mut out = serde_json::Map::new();
    for (idx, key) in FEDERATION_KEYS.iter().enumerate() {
        let get = |c: &(i16, i16, i16, i16, i16, i16, i16, i16)| match idx {
            0 => c.0,
            1 => c.1,
            2 => c.2,
            3 => c.3,
            4 => c.4,
            5 => c.5,
            6 => c.6,
            _ => c.7,
        };
        out.insert(
            (*key).to_string(),
            serde_json::json!(slots.iter().map(|c| get(c)).collect::<Vec<_>>()),
        );
    }
    Ok(serde_json::Value::Object(out))
}

// ==== shared grouped-chart plumbing ====
//
// Every chart table in the 0001 baseline stores one row per (date[, group])
// with flat `___`-prefixed counter columns. The read walks the same slot
// window as [`notes_chart`], zero-fills, extrapolates the latest known row
// over leading empty slots, and nests each column by splitting its name on
// `_` (`___local_incCount` → local.incCount, `___diffs_withFile` →
// diffs.withFile). `unique_temp___*` columns are the unique-count scratch
// arrays and are never part of the response.

/// Chart span step in seconds.
fn span_step(span: &str) -> i64 {
    match span {
        "hour" => 3600,
        _ => 86_400,
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// One row's non-unique `___` counters as (column base, value) pairs.
/// Decodes INT2 (smallint) as i16 — the baseline mixes INT2/INT4 columns.
async fn flat_counts(
    pool: &PgPool,
    table: &str,
    date: i64,
    group: Option<&str>,
) -> Result<Vec<(String, i64)>, sqlx::Error> {
    let sql = match group {
        Some(_) => format!(r#"SELECT * FROM "{table}" WHERE "date" = $1 AND "group" = $2"#),
        None => format!(r#"SELECT * FROM "{table}" WHERE "date" = $1"#),
    };
    let mut q = sqlx::query(&sql).bind(date as i32);
    if let Some(group) = group {
        q = q.bind(group);
    }
    let Some(row) = q.fetch_optional(pool).await? else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (idx, col) in row.columns().iter().enumerate() {
        // id/date/group and unique_temp___* all fail the `___` prefix strip.
        let Some(key) = col.name().strip_prefix("___") else {
            continue;
        };
        let value = match col.type_info().name() {
            "INT2" => row.try_get::<i16, _>(idx)? as i64,
            "INT8" => row.try_get::<i64, _>(idx)?,
            _ => row.try_get::<i32, _>(idx)? as i64,
        };
        out.push((key.to_string(), value));
    }
    Ok(out)
}

/// Insert a leaf value at its nested path (`["local", "incCount"]`).
fn insert_nested(
    map: &mut serde_json::Map<String, serde_json::Value>,
    path: &[&str],
    value: serde_json::Value,
) {
    if path.len() == 1 {
        map.insert(path[0].to_string(), value);
        return;
    }
    let entry = map
        .entry(path[0].to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if let serde_json::Value::Object(child) = entry {
        insert_nested(child, &path[1..], value);
    }
}

/// Transpose per-slot counters into the endpoint's nested arrays. Every key
/// gets an array of exactly `slots.len()` entries (missing → 0).
fn chart_object(keys: &[&str], slots: &[Vec<(String, i64)>]) -> serde_json::Value {
    let mut root = serde_json::Map::new();
    for key in keys {
        let arr: Vec<i64> = slots
            .iter()
            .map(|slot| {
                slot.iter()
                    .find(|(k, _)| k.as_str() == *key)
                    .map(|(_, v)| *v)
                    .unwrap_or(0)
            })
            .collect();
        let path: Vec<&str> = key.split('_').collect();
        insert_nested(&mut root, &path, serde_json::json!(arr));
    }
    serde_json::Value::Object(root)
}

/// Latest-known extrapolation over leading empty slots (upstream getChart's
/// "fetch the latest log" step), matching [`users_chart`].
fn extrapolate(slots: &mut [Vec<(String, i64)>]) {
    let known = slots
        .iter()
        .rposition(|slot| slot.iter().any(|(_, v)| *v != 0));
    if let Some(last_known) = known {
        let base = slots[slots.len() - 1 - last_known].clone();
        let cut = slots.len() - 1 - last_known;
        for slot in slots.iter_mut().take(cut) {
            *slot = base.clone();
        }
    }
}

/// `getChart` over one grouped table: `limit` slots ending at `until_ms`
/// (now when null), oldest→newest, zero-filled + extrapolated, nested.
async fn grouped_chart(
    pool: &PgPool,
    table: &str,
    keys: &[&str],
    span: &str,
    limit: usize,
    until_ms: Option<i64>,
    group: Option<&str>,
) -> Result<serde_json::Value, sqlx::Error> {
    let step = span_step(span);
    let end = bucket(span, until_ms.unwrap_or_else(now_ms));
    let mut slots: Vec<Vec<(String, i64)>> = Vec::with_capacity(limit);
    for k in (0..limit as i64).rev() {
        let date = end - k * step;
        slots.push(flat_counts(pool, table, date, group).await?);
    }
    extrapolate(&mut slots);
    Ok(chart_object(keys, &slots))
}

// ==== per-user charts (upstream `core/chart/charts/per-user-*.ts`) ====

/// `__chart[_day]__per_user_{kind}` (0001:570+). `kind` is one of notes,
/// drive, following, pv, reaction; every table keys rows by (date, "group")
/// where "group" carries the userId.
fn per_user_table(kind: &str, span: &str) -> String {
    let prefix = if span == "hour" {
        "__chart"
    } else {
        "__chart_day"
    };
    format!("{prefix}__per_user_{kind}")
}

/// The non-unique counter columns of each per-user table (0001:570-754).
fn per_user_keys(kind: &str) -> &'static [&'static str] {
    match kind {
        "notes" => &[
            "total",
            "inc",
            "dec",
            "diffs_normal",
            "diffs_reply",
            "diffs_renote",
            "diffs_withFile",
        ],
        "drive" => &[
            "totalCount",
            "totalSize",
            "incCount",
            "incSize",
            "decCount",
            "decSize",
        ],
        "following" => &[
            "local_followings_total",
            "local_followings_inc",
            "local_followings_dec",
            "local_followers_total",
            "local_followers_inc",
            "local_followers_dec",
            "remote_followings_total",
            "remote_followings_inc",
            "remote_followings_dec",
            "remote_followers_total",
            "remote_followers_inc",
            "remote_followers_dec",
        ],
        "pv" => &["upv_user", "pv_user", "upv_visitor", "pv_visitor"],
        "reaction" => &["local_count", "remote_count"],
        _ => &[],
    }
}

/// Per-user chart commit — one UPSERT per (date, userId) adding `deltas`.
/// `deltas` carries only the counters this event moves.
pub async fn update_per_user(
    pool: &PgPool,
    kind: &str,
    span: &str,
    date: i64,
    user_id: &str,
    deltas: &[(&str, i32)],
) -> Result<(), sqlx::Error> {
    if deltas.is_empty() {
        return Ok(());
    }
    let table = per_user_table(kind, span);
    let mut cols = String::new();
    let mut placeholders = String::new();
    let mut sets = String::new();
    for (n, (col, _)) in deltas.iter().enumerate() {
        // cols and placeholders are comma lists after "group" / $2; only the
        // SET fragments follow the bare "SET " keyword (no leading comma)
        cols.push_str(&format!(", \"___{col}\""));
        placeholders.push_str(&format!(", ${}", n + 3));
        let sep = if n == 0 { "" } else { ", " };
        sets.push_str(&format!(
            "{sep}\"___{col}\" = \"{table}\".\"___{col}\" + EXCLUDED.\"___{col}\""
        ));
    }
    let sql = format!(
        "INSERT INTO \"{table}\" (\"date\", \"group\"{cols}) VALUES ($1, $2{placeholders}) \
         ON CONFLICT (\"date\", \"group\") DO UPDATE SET {sets}"
    );
    let mut q = sqlx::query(&sql).bind(date as i32).bind(user_id);
    for (_, delta) in deltas {
        q = q.bind(*delta);
    }
    q.execute(pool).await?;
    Ok(())
}

/// `getChart` for a per-user chart — the `group` row for `user_id`, same
/// slot walk/extrapolation as [`users_chart`].
pub async fn per_user_chart(
    pool: &PgPool,
    kind: &str,
    span: &str,
    limit: usize,
    until_ms: Option<i64>,
    user_id: &str,
) -> Result<serde_json::Value, sqlx::Error> {
    let table = per_user_table(kind, span);
    grouped_chart(
        pool,
        &table,
        per_user_keys(kind),
        span,
        limit,
        until_ms,
        Some(user_id),
    )
    .await
}

// ==== remaining site-wide charts (upstream `core/chart/charts/*.ts`) ====

/// `__chart[_day]__{kind}` for the ungrouped site-wide tables.
fn site_table(kind: &str, span: &str) -> String {
    let prefix = if span == "hour" {
        "__chart"
    } else {
        "__chart_day"
    };
    format!("{prefix}__{kind}")
}

/// The non-unique counter columns of each site-wide table (0001:242-522).
fn site_keys(kind: &str) -> &'static [&'static str] {
    match kind {
        "active_users" => &[
            "registeredWithinWeek",
            "registeredWithinMonth",
            "registeredWithinYear",
            "registeredOutsideWeek",
            "registeredOutsideMonth",
            "registeredOutsideYear",
            "readWrite",
            "read",
            "write",
        ],
        "ap_request" => &["deliverFailed", "deliverSucceeded", "inboxReceived"],
        "drive" => &[
            "local_incCount",
            "local_incSize",
            "local_decCount",
            "local_decSize",
            "remote_incCount",
            "remote_incSize",
            "remote_decCount",
            "remote_decSize",
        ],
        "hashtag" => &["local_users", "remote_users"],
        "instance" => &[
            "requests_failed",
            "requests_succeeded",
            "requests_received",
            "notes_total",
            "notes_inc",
            "notes_dec",
            "notes_diffs_normal",
            "notes_diffs_reply",
            "notes_diffs_renote",
            "users_total",
            "users_inc",
            "users_dec",
            "following_total",
            "following_inc",
            "following_dec",
            "followers_total",
            "followers_inc",
            "followers_dec",
            "drive_totalFiles",
            "drive_incFiles",
            "drive_incUsage",
            "drive_decFiles",
            "drive_decUsage",
            "notes_diffs_withFile",
        ],
        "network" => &[
            "incomingRequests",
            "outgoingRequests",
            "totalTime",
            "incomingBytes",
            "outgoingBytes",
        ],
        _ => &[],
    }
}

/// `charts/active-users` — flat `registeredWithin*`/`readWrite`/`read`/`write`.
pub async fn active_users_chart(
    pool: &PgPool,
    span: &str,
    limit: usize,
    until_ms: Option<i64>,
) -> Result<serde_json::Value, sqlx::Error> {
    grouped_chart(
        pool,
        &site_table("active_users", span),
        site_keys("active_users"),
        span,
        limit,
        until_ms,
        None,
    )
    .await
}

/// `charts/ap-request` — flat deliver/inbox counters.
pub async fn ap_request_chart(
    pool: &PgPool,
    span: &str,
    limit: usize,
    until_ms: Option<i64>,
) -> Result<serde_json::Value, sqlx::Error> {
    grouped_chart(
        pool,
        &site_table("ap_request", span),
        site_keys("ap_request"),
        span,
        limit,
        until_ms,
        None,
    )
    .await
}

/// `charts/drive` — `local`/`remote` inc/dec count+size.
pub async fn drive_chart(
    pool: &PgPool,
    span: &str,
    limit: usize,
    until_ms: Option<i64>,
) -> Result<serde_json::Value, sqlx::Error> {
    grouped_chart(
        pool,
        &site_table("drive", span),
        site_keys("drive"),
        span,
        limit,
        until_ms,
        None,
    )
    .await
}

/// `charts/hashtag` — `local`/`remote` user counts for one tag ("group").
pub async fn hashtag_chart(
    pool: &PgPool,
    span: &str,
    limit: usize,
    until_ms: Option<i64>,
    tag: &str,
) -> Result<serde_json::Value, sqlx::Error> {
    grouped_chart(
        pool,
        &site_table("hashtag", span),
        site_keys("hashtag"),
        span,
        limit,
        until_ms,
        Some(tag),
    )
    .await
}

/// `charts/instance` — the per-host request/note/user/follow/drive counters
/// for one host ("group"). Requests are fed by the AP transport upstream;
/// this lane only ships the read side.
pub async fn instance_chart(
    pool: &PgPool,
    span: &str,
    limit: usize,
    until_ms: Option<i64>,
    host: &str,
) -> Result<serde_json::Value, sqlx::Error> {
    grouped_chart(
        pool,
        &site_table("instance", span),
        site_keys("instance"),
        span,
        limit,
        until_ms,
        Some(host),
    )
    .await
}

/// `charts/network` — flat request/byte/time counters.
pub async fn network_chart(
    pool: &PgPool,
    span: &str,
    limit: usize,
    until_ms: Option<i64>,
) -> Result<serde_json::Value, sqlx::Error> {
    grouped_chart(
        pool,
        &site_table("network", span),
        site_keys("network"),
        span,
        limit,
        until_ms,
        None,
    )
    .await
}
