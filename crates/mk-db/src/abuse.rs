//! admin abuse-report family db paths (upstream
//! `AbuseUserReportsRepository` / `AbuseReportNotificationRecipientRepository`
//! through the admin endpoints). Both tables ship in the 0001 baseline — no
//! migration. The synalio webhook plane keeps its minimal queries in
//! `webhook.rs`; this module carries the admin-list filters, the resolve
//! `resolvedAs` write, forward and the recipient CRUD the admin endpoints
//! drive.

use sqlx::{PgPool, Postgres, QueryBuilder};

use crate::webhook::RecipientRow;

/// `abuse_user_report` row for the admin pack (`MiAbuseUserReport`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AdminReportRow {
    pub id: String,
    #[sqlx(rename = "targetUserId")]
    pub target_user_id: String,
    #[sqlx(rename = "targetUserHost")]
    pub target_user_host: Option<String>,
    #[sqlx(rename = "reporterId")]
    pub reporter_id: String,
    #[sqlx(rename = "reporterHost")]
    pub reporter_host: Option<String>,
    #[sqlx(rename = "assigneeId")]
    pub assignee_id: Option<String>,
    pub resolved: bool,
    pub forwarded: bool,
    pub comment: String,
    #[sqlx(rename = "moderationNote")]
    pub moderation_note: String,
    #[sqlx(rename = "resolvedAs")]
    pub resolved_as: Option<String>,
}

const REPORT_COLS: &str = r#""id", "targetUserId", "targetUserHost", "reporterId",
    "reporterHost", "assigneeId", "resolved", "forwarded", "comment",
    "moderationNote", "resolvedAs""#;

/// `QueryService.makePaginationQuery` + the admin/abuse-user-reports state /
/// origin filters. sinceId switches the order to ASC (the upstream helper's
/// `sinceId != null` branch); everything else stays newest-first.
pub async fn list_reports(
    pool: &PgPool,
    limit: i64,
    since_id: Option<&str>,
    until_id: Option<&str>,
    state: Option<&str>,
    reporter_origin: &str,
    target_user_origin: &str,
) -> Result<Vec<AdminReportRow>, sqlx::Error> {
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(format!(
        "SELECT {REPORT_COLS} FROM \"abuse_user_report\" WHERE TRUE"
    ));
    if let Some(since_id) = since_id {
        qb.push(" AND \"id\" > ").push_bind(since_id);
    }
    if let Some(until_id) = until_id {
        qb.push(" AND \"id\" < ").push_bind(until_id);
    }
    match state {
        Some("resolved") => {
            qb.push(" AND \"resolved\" = TRUE");
        }
        Some("unresolved") => {
            qb.push(" AND \"resolved\" = FALSE");
        }
        _ => {}
    }
    match reporter_origin {
        "local" => {
            qb.push(" AND \"reporterHost\" IS NULL");
        }
        "remote" => {
            qb.push(" AND \"reporterHost\" IS NOT NULL");
        }
        _ => {}
    }
    match target_user_origin {
        "local" => {
            qb.push(" AND \"targetUserHost\" IS NULL");
        }
        "remote" => {
            qb.push(" AND \"targetUserHost\" IS NOT NULL");
        }
        _ => {}
    }
    qb.push(" ORDER BY \"id\" ");
    qb.push(if since_id.is_some() { "ASC" } else { "DESC" });
    qb.push(" LIMIT ").push_bind(limit);
    qb.build_query_as::<AdminReportRow>().fetch_all(pool).await
}

pub async fn find_report(pool: &PgPool, id: &str) -> Result<Option<AdminReportRow>, sqlx::Error> {
    sqlx::query_as::<_, AdminReportRow>(&format!(
        r#"SELECT {REPORT_COLS} FROM "abuse_user_report" WHERE "id" = $1"#
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// `AbuseReportService.resolve` write leg: resolved + assignee + resolvedAs.
pub async fn resolve_report(
    pool: &PgPool,
    id: &str,
    assignee_id: &str,
    resolved_as: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let res = sqlx::query(
        r#"
        UPDATE "abuse_user_report"
        SET "resolved" = true, "assigneeId" = $2, "resolvedAs" = $3
        WHERE "id" = $1
        "#,
    )
    .bind(id)
    .bind(assignee_id)
    .bind(resolved_as)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() > 0)
}

/// `AbuseReportService.forward` write leg (the AP Flag delivery is out of
/// scope; the forwarded flag is what the admin list reads).
pub async fn forward_report(pool: &PgPool, id: &str) -> Result<bool, sqlx::Error> {
    let res = sqlx::query(
        r#"
        UPDATE "abuse_user_report" SET "forwarded" = true WHERE "id" = $1
        "#,
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() > 0)
}

// ---------------------------------------------------------------------------
// abuse_report_notification_recipient
// ---------------------------------------------------------------------------

const RECIPIENT_COLS: &str = r#""id", "isActive", "name", "method", "userId",
    "systemWebhookId", (EXTRACT(EPOCH FROM "updatedAt") * 1000)::bigint AS "updated_at_ms""#;

/// `AbuseReportNotificationService.fetchRecipients` (ids / method filter).
/// The method filter carries upstream's correlation: email recipients need a
/// userId, webhook recipients have none.
pub async fn list_recipients(
    pool: &PgPool,
    methods: &[String],
) -> Result<Vec<RecipientRow>, sqlx::Error> {
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(format!(
        "SELECT {RECIPIENT_COLS} FROM \"abuse_report_notification_recipient\" WHERE TRUE"
    ));
    let email = methods.iter().any(|m| m == "email");
    let webhook = methods.iter().any(|m| m == "webhook");
    if email || webhook {
        qb.push(" AND (");
        let mut first = true;
        if email {
            qb.push("(\"method\" = 'email' AND \"userId\" IS NOT NULL)");
            first = false;
        }
        if webhook {
            if !first {
                qb.push(" OR ");
            }
            qb.push("(\"method\" = 'webhook' AND \"userId\" IS NULL)");
        }
        qb.push(")");
    }
    qb.push(" ORDER BY \"id\" ASC");
    qb.build_query_as::<RecipientRow>().fetch_all(pool).await
}

pub async fn find_recipient(pool: &PgPool, id: &str) -> Result<Option<RecipientRow>, sqlx::Error> {
    sqlx::query_as::<_, RecipientRow>(&format!(
        r#"SELECT {RECIPIENT_COLS} FROM "abuse_report_notification_recipient" WHERE "id" = $1"#
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub async fn update_recipient(
    pool: &PgPool,
    id: &str,
    is_active: bool,
    name: &str,
    method: &str,
    user_id: Option<&str>,
    system_webhook_id: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let res = sqlx::query(
        r#"
        UPDATE "abuse_report_notification_recipient"
        SET "isActive" = $2, "updatedAt" = now(), "name" = $3, "method" = $4,
            "userId" = $5, "systemWebhookId" = $6
        WHERE "id" = $1
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
    Ok(res.rows_affected() > 0)
}

pub async fn delete_recipient(pool: &PgPool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "abuse_report_notification_recipient" WHERE "id" = $1"#)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
