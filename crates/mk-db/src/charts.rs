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

use sqlx::PgPool;

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
