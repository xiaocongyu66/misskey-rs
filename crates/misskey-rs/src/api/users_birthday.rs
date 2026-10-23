//! `users/get-following-users-by-birthday` (upstream
//! `endpoints/users/get-following-users-by-birthday.ts` @ ea2c3aed).
//!
//! Requires a `birthday` param — either a single `{ month, day }` or a
//! `{ begin: {month, day}, end: {month, day} }` range (the caller decides the
//! window; the widget sends the current week, not the current month). The
//! handler walks the caller's `following` edges, inner-joins `user_profile`
//! on the followee and filters on the schema's `get_birthday_date(birthday)`
//! (mmdd smallint, indexed via IDX_USERPROFILE_BIRTHDAY_DATE — both ship in
//! the 0001 baseline; the SQL is inline here so the slice stays within the
//! ≤3-files/commit budget). Ordering is `birthday_date ASC` with plain
//! `offset/limit` pagination (no id cursor — upstream uses
//! `query.offset/limit` too).
//!
//! Divergences from upstream, all documented:
//! - "today" for the next-occurrence year bump is UTC (`now_ms` →
//!   `ms_to_iso`); upstream uses the process-local timezone. Only the output
//!   `birthday` string (and the has-passed-this-year flip) is affected — the
//!   month/day filter itself is timezone-free.
//! - The `user` field packs UserLite without the `me` context
//!   (`user_lite_value_with_roles(..., None)`, same as the other users-*
//!   slices in this workspace); UserLite carries no me-dependent fields.
//! - The JS `Date.setMonth/setFullYear` day-overflow (e.g. Feb 30 → Mar 2,
//!   Feb 29 after a year bump → Mar 1) is emulated explicitly in
//!   `js_set_month_day` so impossible mmdd inputs round-trip like upstream.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// The endpoint names routed into this module (single source of truth for the
/// `users-birthday` dispatch arm in mod.rs).
pub(crate) const USERS_BIRTHDAY_ENDPOINTS: &[&str] = &["users/get-following-users-by-birthday"];

pub(crate) fn handles(name: &str) -> bool {
    USERS_BIRTHDAY_ENDPOINTS.contains(&name)
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_users_birthday", error = %e, "db error: {context}");
    api_err(ApiError::internal_error())
}

fn invalid_param(param: &str, reason: &str) -> Response {
    api_err(ApiError::invalid_param(json!({
        "param": param,
        "reason": reason,
    })))
}

fn ok_json(v: Value) -> Response {
    (StatusCode::OK, Json(v)).into_response()
}

/// One raw row: the followee id + its mmdd birthday number (smallint).
#[derive(Debug, mk_db::sqlx::FromRow)]
struct BirthdayRow {
    user_id: String,
    birthday_date: i16,
}

/// Entry from the `users-birthday` dispatch arm in mod.rs (body pre-parsed,
/// token + credential-declared pre-extracted).
pub(crate) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
    credential_declared: bool,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    debug_assert_eq!(name, "users/get-following-users-by-birthday");

    // meta.requireCredential: true (kind read:account).
    let me_id = match super::social::require_checked_user(
        pool,
        token.as_deref(),
        credential_declared,
    )
    .await
    {
        Ok(id) => id,
        Err(resp) => return resp,
    };

    match get_following_users_by_birthday(state, pool, body, &me_id).await {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

/// paramDef month/day ints: month 1..=12, day 1..=31 (no per-month day check
/// upstream — impossible dates are handled by the year-bump overflow).
struct MmDay {
    month: i64,
    day: i64,
}

fn parse_month_day(v: &Value) -> Option<MmDay> {
    let month = v.get("month")?.as_i64()?;
    let day = v.get("day")?.as_i64()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(MmDay { month, day })
}

async fn get_following_users_by_birthday(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    // limit: 1..=100 default 10; offset: plain integer default 0 (negative
    // offset reaches Postgres and 500s exactly like upstream's TypeORM).
    let limit = match body.get("limit") {
        None | Some(Value::Null) => 10,
        Some(v) => {
            let Some(l) = v.as_i64() else {
                return Err(invalid_param("limit", "expected an integer"));
            };
            if !(1..=100).contains(&l) {
                return Err(invalid_param(
                    "limit",
                    "must be an integer between 1 and 100",
                ));
            }
            l
        }
    };
    let offset = match body.get("offset") {
        None | Some(Value::Null) => 0,
        Some(v) => match v.as_i64() {
            Some(o) => o,
            None => return Err(invalid_param("offset", "expected an integer")),
        },
    };

    // required: ['birthday'] — a missing param is INVALID_PARAM.
    let Some(birthday) = body.get("birthday") else {
        return Err(invalid_param("birthday", "required"));
    };
    let Some(obj) = birthday.as_object() else {
        return Err(invalid_param("birthday", "must be an object"));
    };
    // paramDef oneOf: exactly one of the {month, day} / {begin, end} branches
    // may validate (both key-pairs present, or neither → INVALID_PARAM).
    let single_keys = obj.contains_key("month") && obj.contains_key("day");
    let range_keys = obj.contains_key("begin") && obj.contains_key("end");
    if single_keys == range_keys {
        return Err(invalid_param(
            "birthday",
            "must be {month, day} or {begin, end}",
        ));
    }
    let (where_sql, begin, end) = if single_keys {
        let Some(md) = parse_month_day(birthday) else {
            return Err(invalid_param(
                "birthday",
                "month must be 1..=12 and day 1..=31",
            ));
        };
        // なぜか get_birthday_date() = :birthday だとインデックスが効かないので、
        // BETWEEN で対応 (upstream keeps the BETWEEN form).
        let mmdd = md.month * 100 + md.day;
        (
            r#"get_birthday_date("user_profile"."birthday") BETWEEN $2 AND $2"#,
            mmdd,
            mmdd,
        )
    } else {
        let Some(md) = obj.get("begin").and_then(parse_month_day) else {
            return Err(invalid_param(
                "birthday.begin",
                "must be a {month, day} object",
            ));
        };
        let Some(md_end) = obj.get("end").and_then(parse_month_day) else {
            return Err(invalid_param(
                "birthday.end",
                "must be a {month, day} object",
            ));
        };
        let begin = md.month * 100 + md.day;
        let end = md_end.month * 100 + md_end.day;
        if begin <= end {
            (
                r#"get_birthday_date("user_profile"."birthday") BETWEEN $2 AND $3"#,
                begin,
                end,
            )
        } else {
            // 12/31 → 1/1 wrap-around gets the upstream OR split.
            (
                r#"(get_birthday_date("user_profile"."birthday") BETWEEN $2 AND 1231 OR get_birthday_date("user_profile"."birthday") BETWEEN 101 AND $3)"#,
                begin,
                end,
            )
        }
    };

    // Upstream query: followings of me, inner-join followee profile, mmdd
    // filter, ordered by the mmdd number, offset/limit page.
    let sql = format!(
        r#"
        SELECT "following"."followeeId" AS "user_id", get_birthday_date("user_profile"."birthday") AS "birthday_date"
        FROM "following"
        INNER JOIN "user_profile" ON "user_profile"."userId" = "following"."followeeId"
        WHERE "following"."followerId" = $1
          AND {where_sql}
        ORDER BY "birthday_date" ASC
        OFFSET $4
        LIMIT $5
        "#
    );
    let rows = mk_db::sqlx::query_as::<_, BirthdayRow>(&sql)
        .bind(me_id)
        .bind(begin)
        .bind(end)
        .bind(offset)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(|e| db_err("get_following_users_by_birthday", e))?;

    let today = today_ymd();
    let ctx = mk_core::services::user_pack::InstanceCtx {
        url: &state.config.url,
        host: &state.config.host(),
    };
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        // me-view pack: a missing user row can't happen through the inner
        // join, but the upstream map/filter collapses such items anyway.
        let Ok(Some(user_row)) = mk_db::user::find_user_by_id(pool, &row.user_id).await else {
            continue;
        };
        let user = mk_core::services::user_pack::user_lite_value_with_roles(&user_row, ctx, None);
        out.push(json!({
            "id": row.user_id,
            "birthday": next_occurrence(i64::from(row.birthday_date), today),
            "user": user,
        }));
    }
    Ok(ok_json(json!(out)))
}

// ---------------------------------------------------------------------------
// next-occurrence date math (JS Date semantics)
// ---------------------------------------------------------------------------

/// (year, month 1-12, day) of "now" — UTC (documented divergence: upstream
/// uses the process-local timezone).
fn today_ymd() -> (i64, u32, u32) {
    let iso = mk_core::services::signup::ms_to_iso(super::now_ms() as i64);
    let date = &iso[..10]; // "YYYY-MM-DD"
    let mut it = date.split('-');
    let y = it.next().and_then(|s| s.parse().ok()).unwrap_or(1970);
    let m = it.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    let d = it.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    (y, m, d)
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// JS `Date.setMonth(month0, day)` day-overflow: day-of-month spills into the
/// following months (Feb 30 → Mar 1/2 depending on leap-ness).
fn js_set_month_day(y: i64, month0: u32, mut day: u32) -> (i64, u32, u32) {
    let total = y * 12 + i64::from(month0);
    let mut y = total.div_euclid(12);
    let mut m0 = total.rem_euclid(12) as u32;
    loop {
        let dim = days_in_month(y, m0 + 1);
        if day <= dim {
            break;
        }
        day -= dim;
        m0 += 1;
        if m0 == 12 {
            m0 = 0;
            y += 1;
        }
    }
    (y, m0 + 1, day)
}

/// upstream output mapping: the mmdd number becomes the next occurrence of
/// that month/day (this year, or next year once it has already passed);
/// `setFullYear(+1)` keeps the month/day with the same overflow rule.
fn next_occurrence(mmdd: i64, today: (i64, u32, u32)) -> String {
    let m = (mmdd / 100).max(1).min(12) as u32;
    let d = (mmdd % 100).max(1) as u32;
    let (mut y, mut m, mut d) = js_set_month_day(today.0, m - 1, d);
    if (y, m, d) < today {
        (y, m, d) = js_set_month_day(y + 1, m - 1, d);
    }
    format!("{y:04}-{m:02}-{d:02}")
}
