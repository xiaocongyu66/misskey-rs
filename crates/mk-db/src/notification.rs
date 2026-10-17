//! Notification SQL (M2 truthful slice: mention / reply / renote / reaction /
//! follow). See `endpoints/i/notifications.ts` for endpoint semantics and
//! `core/NotificationService.ts` for create/read semantics (upstream moved the
//! store to Redis; the local schema keeps the pre-cleanup table — see
//! migrations/0002_notifications.sql).

use sqlx::PgPool;

pub struct NewNotification<'a> {
    pub id: &'a str,
    /// `notification_type_enum` value, e.g. "mention" / "reaction" / "follow".
    pub r#type: &'a str,
    /// Recipient (`notifieeId`).
    pub user_id: &'a str,
    /// Sender; `None` for notifier-less types (upstream `notifierId` nullable).
    pub notifier_id: Option<&'a str>,
    /// Note-bound types (mention/reply/renote/reaction) carry the source note.
    pub note_id: Option<&'a str>,
    /// `reaction` type only.
    pub reaction: Option<&'a str>,
}

/// Upstream `NotificationService.createNotification`-ish insert. Follower
/// self-notification suppression (= notifiee == notifier) lives at callers
/// upstream; insert_notification stays dumb.
pub async fn insert_notification(
    pool: &PgPool,
    n: &NewNotification<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO "notification"
            ("id", "createdAt", "notifieeId", "notifierId", "type", "isRead", "noteId", "reaction")
        VALUES ($1, now(), $2, $3, $4::notification_type_enum, FALSE, $5, $6)
        "#,
    )
    .bind(n.id)
    .bind(n.user_id)
    .bind(n.notifier_id)
    .bind(n.r#type)
    .bind(n.note_id)
    .bind(n.reaction)
    .execute(pool)
    .await?;
    Ok(())
}

/// Columns carried by the M2 notification pack (createdAt is packed from the
/// aid timestamp at the pack layer, like notes).
pub const NOTIFICATION_COLS: &str = r#""id", ("type"::text) AS "type", ("isRead") AS "is_read", "noteId" AS "note_id", "notifierId" AS "notifier_id", "reaction""#;

#[derive(Debug, sqlx::FromRow)]
pub struct NotificationRow {
    pub id: String,
    pub r#type: String,
    pub is_read: bool,
    pub note_id: Option<String>,
    pub notifier_id: Option<String>,
    pub reaction: Option<String>,
}

/// `NotificationService.getNotifications` / `QueryService.makePaginationQuery`
/// pagination: `sinceId`-only → oldest-first (id ASC); otherwise newest-first
/// (id DESC). Both bounds are EXCLUSIVE (`(untilTime`, `(sinceTime` in
/// upstream; `misc/id.ts` aid ids compare monotonically as strings).
/// `unreadOnly` filters `isRead`; `include_types` (non-empty) filters
/// `"type" = ANY(...)`. upstream `following` filter not ported — see TODO in
/// the handler.
pub async fn list_notifications(
    pool: &PgPool,
    me_id: &str,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
    unread_only: bool,
    include_types: &[String],
) -> Result<Vec<NotificationRow>, sqlx::Error> {
    let ascending = since_id.is_some() && until_id.is_none();
    let order = if ascending { "ASC" } else { "DESC" };
    let sql = format!(
        r#"
        SELECT {NOTIFICATION_COLS} FROM "notification"
        WHERE "notifieeId" = $1
          AND ($2 = FALSE OR "isRead" = FALSE)
          AND ($3::text IS NULL OR "id" > $3)
          AND ($4::text IS NULL OR "id" < $4)
          AND ($5::text[] IS NULL OR ("type"::text) = ANY($5))
        ORDER BY "id" {order}
        LIMIT $6
        "#
    );
    let types: Option<Vec<String>> = if include_types.is_empty() {
        None
    } else {
        Some(include_types.to_vec())
    };
    Ok(sqlx::query_as::<_, NotificationRow>(&sql)
        .bind(me_id)
        .bind(unread_only)
        .bind(since_id)
        .bind(until_id)
        .bind(types)
        .bind(limit)
        .fetch_all(pool)
        .await?)
}

/// `NotificationService.readAllNotification` (DB-era equivalent): marks every
/// unread notification of the user read.
pub async fn mark_as_read_all(pool: &PgPool, user_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"UPDATE "notification" SET "isRead" = TRUE WHERE "notifieeId" = $1 AND "isRead" = FALSE"#,
    )
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// `i` endpoint's `hasUnreadNotification` read (user pack wiring is co-owned —
/// see coordinator notes).
pub async fn has_unread(pool: &PgPool, user_id: &str) -> Result<bool, sqlx::Error> {
    let v: Option<i32> = sqlx::query_scalar(
        r#"SELECT 1 FROM "notification" WHERE "notifieeId" = $1 AND "isRead" = FALSE LIMIT 1"#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(v.is_some())
}
