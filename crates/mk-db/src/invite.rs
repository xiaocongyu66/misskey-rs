//! invite: db paths behind upstream `endpoints/invite/*.ts` +
//! `endpoints/admin/invite/*.ts` (RegistrationTicketsRepository).
//!
//! The `registration_ticket` table ships in the 0001 baseline (code has a
//! unique index, createdById/usedById are indexed), so no migration is
//! needed. Timestamps follow the workspace convention: writes use
//! `TIMESTAMP 'epoch' + $n::bigint * interval '1 millisecond'`; reads map
//! timestamptz back to epoch ms with
//! `(EXTRACT(EPOCH FROM col) * 1000)::bigint`.

use crate::sqlx::PgPool;

/// `registration_ticket` columns in InviteCodeEntityService.pack order.
const COLS: &str = r#""id", "code",
(EXTRACT(EPOCH FROM "expiresAt") * 1000)::bigint AS "expires_at_ms",
(EXTRACT(EPOCH FROM "usedAt") * 1000)::bigint AS "used_at_ms",
"pendingUserId" AS "pending_user_id",
"createdById" AS "created_by_id",
"usedById" AS "used_by_id""#;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TicketRow {
    pub id: String,
    pub code: String,
    pub expires_at_ms: Option<i64>,
    pub used_at_ms: Option<i64>,
    pub pending_user_id: Option<String>,
    pub created_by_id: Option<String>,
    pub used_by_id: Option<String>,
}

/// Insert one ticket. `expires_at_ms == None` writes NULL.
pub async fn insert(
    pool: &PgPool,
    id: &str,
    code: &str,
    expires_at_ms: Option<i64>,
    created_by_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO "registration_ticket" ("id", "code", "expiresAt", "createdById")
           VALUES ($1, $2, TIMESTAMP 'epoch' + $3::bigint * interval '1 millisecond', $4)"#,
    )
    .bind(id)
    .bind(code)
    .bind(expires_at_ms)
    .bind(created_by_id)
    .execute(pool)
    .await
    .map(|_| ())
}

/// invite/list: the caller's tickets, newest id first, with the
/// makePaginationQuery since/until bounds (`id > since`, `id < until`).
pub async fn list_by_creator(
    pool: &PgPool,
    creator_id: &str,
    limit: i64,
    since_id: Option<&str>,
    until_id: Option<&str>,
) -> Result<Vec<TicketRow>, sqlx::Error> {
    sqlx::query_as::<_, TicketRow>(&format!(
        r#"SELECT {COLS} FROM "registration_ticket"
           WHERE "createdById" = $1
             AND ($2::text IS NULL OR "id" > $2)
             AND ($3::text IS NULL OR "id" < $3)
           ORDER BY "id" DESC
           LIMIT $4"#
    ))
    .bind(creator_id)
    .bind(since_id)
    .bind(until_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// invite/create + invite/limit window count: tickets the caller created with
/// `id > since_aid` (upstream `countBy({ id: MoreThan(idService.gen(t)) })`).
pub async fn count_created_since(
    pool: &PgPool,
    creator_id: &str,
    since_aid: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        r#"SELECT count(*) FROM "registration_ticket"
           WHERE "createdById" = $1 AND "id" > $2"#,
    )
    .bind(creator_id)
    .bind(since_aid)
    .fetch_one(pool)
    .await
}

/// admin/invite/list: all tickets with the upstream `type` filter and `sort`
/// order (`+createdAt` is id DESC upstream, `-createdAt` id ASC).
pub async fn list_admin(
    pool: &PgPool,
    limit: i64,
    offset: i64,
    ty: &str,
    sort: Option<&str>,
) -> Result<Vec<TicketRow>, sqlx::Error> {
    let filter = match ty {
        "unused" => r#" AND "usedById" IS NULL"#,
        "used" => r#" AND "usedById" IS NOT NULL"#,
        "expired" => r#" AND "expiresAt" < now()"#,
        _ => "",
    };
    let order = match sort {
        Some("+createdAt") => r#""id" DESC"#,
        Some("-createdAt") => r#""id" ASC"#,
        Some("+usedAt") => r#""usedAt" DESC NULLS LAST"#,
        Some("-usedAt") => r#""usedAt" ASC NULLS FIRST"#,
        _ => r#""id" DESC"#,
    };
    sqlx::query_as::<_, TicketRow>(&format!(
        r#"SELECT {COLS} FROM "registration_ticket"
           WHERE TRUE{filter}
           ORDER BY {order}
           LIMIT $1 OFFSET $2"#
    ))
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// Parse an ISO-8601 `expiresAt` string through Postgres and return epoch ms
/// (admin/invite/create). An invalid date is a sqlx error; the caller maps it
/// to the upstream INVALID_DATE_TIME meta error.
pub async fn parse_timestamptz_ms(pool: &PgPool, s: &str) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar(r#"SELECT (EXTRACT(EPOCH FROM ($1::text)::timestamptz) * 1000)::bigint"#)
        .bind(s)
        .fetch_one(pool)
        .await
}

/// `invite/delete`: `registrationTicketsRepository.findOneBy({ id })`.
pub async fn find_by_id(pool: &PgPool, id: &str) -> Result<Option<TicketRow>, sqlx::Error> {
    sqlx::query_as::<_, TicketRow>(&format!(
        r#"SELECT {COLS} FROM "registration_ticket" WHERE "id" = $1"#
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// `invite/delete`: `registrationTicketsRepository.delete(ticket.id)`.
pub async fn delete_by_id(pool: &PgPool, id: &str) -> Result<u64, sqlx::Error> {
    let res = sqlx::query(r#"DELETE FROM "registration_ticket" WHERE "id" = $1"#)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}
