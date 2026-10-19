//! arena-synalio: system-webhook + abuse-report db paths.
//! Upstream `SystemWebhookRepository` (`system_webhook` table ships in the
//! 0001 baseline with the `"on"` varchar[] event list), the
//! `abuse_report_notification_recipient` plane and `AbuseUserReportRepository`
//! (`abuse_user_report`). All three tables are 0001-baseline — no new
//! migration (the exports.py mk_queue_jobs / reversi ensure_matching_table
//! precedent: the drop-schema census shim only re-ensures what the pinned
//! entity set already carries; these tables ARE pinned, so nothing to shim).

use sqlx::PgPool;

/// `system_webhook` row (`MiSystemWebhook`); `updated_at_ms` is derived —
/// upstream packs `updatedAt` from the column, the synalio entity pack reads
/// it off the row (LatestSentAt/LatestStatus stay null until a delivery).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SystemWebhookRow {
    pub id: String,
    #[sqlx(rename = "isActive")]
    pub is_active: bool,
    pub name: String,
    #[sqlx(rename = "on")]
    pub on: Vec<String>,
    pub url: String,
    pub secret: String,
    #[sqlx(rename = "latestStatus")]
    pub latest_status: Option<i32>,
    #[sqlx(rename = "updated_at_ms")]
    pub updated_at_ms: i64,
}

/// `abuse_user_report` row (`MiAbuseUserReport`) — the payload fields the
/// AbuseReportPayload / AbuseUserReportEntityService.pack projection needs.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AbuseReportRow {
    pub id: String,
    #[sqlx(rename = "targetUserId")]
    pub target_user_id: String,
    #[sqlx(rename = "reporterId")]
    pub reporter_id: String,
    #[sqlx(rename = "assigneeId")]
    pub assignee_id: Option<String>,
    pub resolved: bool,
    pub comment: String,
    #[sqlx(rename = "targetUserHost")]
    pub target_user_host: Option<String>,
    #[sqlx(rename = "reporterHost")]
    pub reporter_host: Option<String>,
    pub forwarded: bool,
    #[sqlx(rename = "moderationNote")]
    pub moderation_note: String,
}

/// `abuse_report_notification_recipient` row (`MiAbuseReportNotificationRecipient`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RecipientRow {
    pub id: String,
    #[sqlx(rename = "isActive")]
    pub is_active: bool,
    pub name: String,
    pub method: String,
    #[sqlx(rename = "userId")]
    pub user_id: Option<String>,
    #[sqlx(rename = "systemWebhookId")]
    pub system_webhook_id: Option<String>,
    #[sqlx(rename = "updated_at_ms")]
    pub updated_at_ms: i64,
}

pub async fn insert_system_webhook(
    pool: &PgPool,
    id: &str,
    name: &str,
    url: &str,
    secret: &str,
    on: &[String],
    is_active: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO "system_webhook" ("id", "isActive", "updatedAt", "latestSentAt", "latestStatus", "name", "on", "url", "secret")
        VALUES ($1, $2, now(), NULL, NULL, $3, $4, $5, $6)
        "#,
    )
    .bind(id)
    .bind(is_active)
    .bind(name)
    .bind(on.to_vec())
    .bind(url)
    .bind(secret)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn find_system_webhook(
    pool: &PgPool,
    id: &str,
) -> Result<Option<SystemWebhookRow>, sqlx::Error> {
    Ok(sqlx::query_as::<_, SystemWebhookRow>(
        r#"
        SELECT "id", "isActive", "name", "on", "url", "secret", "latestStatus",
               (EXTRACT(EPOCH FROM "updatedAt") * 1000)::bigint AS "updated_at_ms"
        FROM "system_webhook" WHERE "id" = $1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?)
}

pub async fn list_system_webhooks(pool: &PgPool) -> Result<Vec<SystemWebhookRow>, sqlx::Error> {
    Ok(sqlx::query_as::<_, SystemWebhookRow>(
        r#"
        SELECT "id", "isActive", "name", "on", "url", "secret", "latestStatus",
               (EXTRACT(EPOCH FROM "updatedAt") * 1000)::bigint AS "updated_at_ms"
        FROM "system_webhook" ORDER BY "id" DESC
        "#,
    )
    .fetch_all(pool)
    .await?)
}

/// The enqueue-leg hook query (SystemWebhookService.fetchActiveSystemWebhooks —
/// active hooks only; the `on.includes(type)` filter runs at the enqueue site's
/// per-hook gate): ONLY active hooks reach the deliver fan-out, so an inactive
/// `system_webhook` row can never produce a userCreated POST. Pulled out as a
/// pub const so the synalio-final test tail pins the inactive-no-post gate
/// without a live DB.
pub const ACTIVE_SYSTEM_WEBHOOKS_SQL: &str = r#"
    SELECT "id", "isActive", "name", "on", "url", "secret", "latestStatus",
           (EXTRACT(EPOCH FROM "updatedAt") * 1000)::bigint AS "updated_at_ms"
    FROM "system_webhook" WHERE "isActive" = true ORDER BY "id" DESC
"#;

pub async fn list_active_system_webhooks(
    pool: &PgPool,
) -> Result<Vec<SystemWebhookRow>, sqlx::Error> {
    Ok(
        sqlx::query_as::<_, SystemWebhookRow>(ACTIVE_SYSTEM_WEBHOOKS_SQL)
            .fetch_all(pool)
            .await?,
    )
}

pub async fn delete_system_webhook(pool: &PgPool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "system_webhook" WHERE "id" = $1"#)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn update_system_webhook(
    pool: &PgPool,
    id: &str,
    name: &str,
    url: &str,
    secret: &str,
    on: &[String],
    is_active: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE "system_webhook"
        SET "name" = $2, "url" = $3, "secret" = $4, "on" = $5, "isActive" = $6, "updatedAt" = now()
        WHERE "id" = $1
        "#,
    )
    .bind(id)
    .bind(name)
    .bind(url)
    .bind(secret)
    .bind(on.to_vec())
    .bind(is_active)
    .execute(pool)
    .await?;
    Ok(())
}

/// `abuse_report_notification_recipient` insert (createRecipient).
pub async fn insert_recipient(
    pool: &PgPool,
    id: &str,
    name: &str,
    method: &str,
    is_active: bool,
    user_id: Option<&str>,
    system_webhook_id: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO "abuse_report_notification_recipient" ("id", "isActive", "updatedAt", "name", "method", "userId", "systemWebhookId")
        VALUES ($1, $2, now(), $3, $4, $5, $6)
        "#,
    )
    .bind(id)
    .bind(is_active)
    .bind(name)
    .bind(method)
    .bind(user_id)
    .bind(system_webhook_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// AbuseReportNotificationService.notifySystemWebhook: the recipient loop
/// excludes the webhooks an INACTIVE webhook-method recipient references
/// (fetchWebhookRecipients is NOT isActive-filtered; the inactive slice
/// becomes the `withoutWebhookIds` excludes). A recipient is not required
/// for delivery — active hooks with the event in `"on"` fire regardless.
pub async fn inactive_recipient_hook_ids(pool: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    Ok(sqlx::query_scalar(
        r#"
        SELECT DISTINCT "systemWebhookId" FROM "abuse_report_notification_recipient"
        WHERE method = 'webhook' AND "isActive" = false AND "systemWebhookId" IS NOT NULL
        "#,
    )
    .fetch_all(pool)
    .await?)
}

pub async fn insert_abuse_report(
    pool: &PgPool,
    id: &str,
    target_user_id: &str,
    reporter_id: &str,
    comment: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO "abuse_user_report" ("id", "targetUserId", "targetUserHost", "reporterId", "reporterHost", "assigneeId", "resolved", "comment", "forwarded", "moderationNote")
        VALUES ($1, $2, (SELECT "user"."host" FROM "user" WHERE "user"."id" = $2), $3, NULL, NULL, false, $4, false, '')
        "#,
    )
    .bind(id)
    .bind(target_user_id)
    .bind(reporter_id)
    .bind(comment)
    .execute(pool)
    .await?;
    Ok(())
}

/// admin/abuse-user-reports — upstream orders the admin list newest-first
/// (find, order id DESC + limit) and the synalio timeouts read `[0]` after a
/// fresh report, so DESC is load-bearing.
pub async fn list_abuse_reports(
    pool: &PgPool,
    limit: i32,
) -> Result<Vec<AbuseReportRow>, sqlx::Error> {
    Ok(sqlx::query_as::<_, AbuseReportRow>(
        r#"
        SELECT "id", "targetUserId", "reporterId", "assigneeId", "resolved", "comment",
               "targetUserHost", "reporterHost", "forwarded", "moderationNote"
        FROM "abuse_user_report" ORDER BY "id" DESC LIMIT $1
        "#,
    )
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

pub async fn find_abuse_report(
    pool: &PgPool,
    id: &str,
) -> Result<Option<AbuseReportRow>, sqlx::Error> {
    Ok(sqlx::query_as::<_, AbuseReportRow>(
        r#"
        SELECT "id", "targetUserId", "reporterId", "assigneeId", "resolved", "comment",
               "targetUserHost", "reporterHost", "forwarded", "moderationNote"
        FROM "abuse_user_report" WHERE "id" = $1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?)
}

/// admin/resolve-abuse-user-report → AbuseReportService.resolve: stamps the
/// assignee + resolved flag (resolvedAs stays null; the endpoint passes no
/// resolve type).
pub async fn resolve_abuse_report(
    pool: &PgPool,
    id: &str,
    assignee_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE "abuse_user_report"
        SET "resolved" = true, "assigneeId" = $2
        WHERE "id" = $1
        "#,
    )
    .bind(id)
    .bind(assignee_id)
    .execute(pool)
    .await?;
    Ok(())
}
