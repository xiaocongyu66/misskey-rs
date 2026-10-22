//! `meta` server-settings row — the note.ts acceptance subset:
//! `sensitiveWords` / `prohibitedWords` (NoteCreateService force-home +
//! CONTAINS_PROHIBITED_WORDS paths) and the `deeplAuthKey` read the
//! notes/translate UNAVAILABLE gate consumes. Row shape mirrors the freq
//! slice (`meta` keeps a single sentinel row; the e2e test setup writes
//! `meta.id = 'x'`).

use sqlx::PgPool;

/// The word lists + DeepL key read back as (sensitiveWords, prohibitedWords,
/// deeplAuthKey); missing row → empty defaults.
pub async fn note_content_moderation_words(
    pool: &PgPool,
) -> Result<(Vec<String>, Vec<String>, Option<String>), sqlx::Error> {
    sqlx::query_as::<_, (Vec<String>, Vec<String>, Option<String>)>(
        r#"SELECT COALESCE(
               (SELECT "sensitiveWords" FROM "meta" ORDER BY "id" DESC LIMIT 1),
               '{}'::varchar[]),
           COALESCE(
               (SELECT "prohibitedWords" FROM "meta" ORDER BY "id" DESC LIMIT 1),
               '{}'::varchar[]),
           (SELECT "deeplAuthKey" FROM "meta" ORDER BY "id" DESC LIMIT 1)"#,
    )
    .fetch_one(pool)
    .await
}

/// admin/update-meta patch for the word lists; `None` keys are untouched.
pub async fn update_meta_word_lists(
    pool: &PgPool,
    sensitive_words: Option<&[String]>,
    prohibited_words: Option<&[String]>,
) -> Result<(), sqlx::Error> {
    if sensitive_words.is_none() && prohibited_words.is_none() {
        return Ok(());
    }
    let updated = sqlx::query(
        r#"
        UPDATE "meta" SET
            "sensitiveWords" = COALESCE($1, "sensitiveWords"),
            "prohibitedWords" = COALESCE($2, "prohibitedWords")
        "#,
    )
    .bind(sensitive_words.map(|v| v.to_vec()))
    .bind(prohibited_words.map(|v| v.to_vec()))
    .execute(pool)
    .await?
    .rows_affected();
    if updated == 0 {
        // Sentinel row seeded on first write (id 'x' per the baseline shape).
        sqlx::query(
            r#"
            INSERT INTO "meta" ("id", "sensitiveWords", "prohibitedWords")
            VALUES ('x',
                    COALESCE($1, '{}'::varchar[]),
                    COALESCE($2, '{}'::varchar[]))
            ON CONFLICT ("id") DO UPDATE SET
                "sensitiveWords" = COALESCE($1, "meta"."sensitiveWords"),
                "prohibitedWords" = COALESCE($2, "meta"."prohibitedWords")
            "#,
        )
        .bind(sensitive_words.map(|v| v.to_vec()))
        .bind(prohibited_words.map(|v| v.to_vec()))
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// meta."enableFanoutTimeline" as the pushToTl gate reads it (ftt2 lane;
/// timelines.ts L104 flips it per describe.each arm via admin/update-meta):
/// absent row → schema/upstream default TRUE.
pub async fn enable_fanout_timeline(pool: &PgPool) -> Result<bool, sqlx::Error> {
    let v: Option<bool> = sqlx::query_scalar(
        r#"SELECT "enableFanoutTimeline" FROM "meta" ORDER BY "id" DESC LIMIT 1"#,
    )
    .fetch_optional(pool)
    .await?
    .flatten();
    Ok(v.unwrap_or(true))
}

/// admin/update-meta persist for the fanout flag (MetaService.update partial
/// patch: the sentinel row is updated in place, or seeded on first write —
/// same shape as update_meta_word_lists).
pub async fn update_meta_enable_fanout_timeline(
    pool: &PgPool,
    enable: bool,
) -> Result<(), sqlx::Error> {
    let updated = sqlx::query(
        r#"
        UPDATE "meta" SET "enableFanoutTimeline" = $1
        WHERE "id" = (SELECT "id" FROM "meta" ORDER BY "id" DESC LIMIT 1)
        "#,
    )
    .bind(enable)
    .execute(pool)
    .await?
    .rows_affected();
    if updated == 0 {
        sqlx::query(
            r#"
            INSERT INTO "meta" ("id", "enableFanoutTimeline") VALUES ('x', $1)
            ON CONFLICT ("id") DO UPDATE SET "enableFanoutTimeline" = $1
            "#,
        )
        .bind(enable)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// NodeinfoServerService.nodeinfo2 meta slice — the columns the nodeinfo
/// document mirrors (well-known arena; MetaService.fetch(true) semantics:
/// the sentinel row is ensured on read, absent columns fall back to the
/// 0001-baseline/TypeORM defaults). (meta."federation" read/update ride the
/// ff-visibility lane: mk-db/src/ff_visibility.rs::meta_federation /
/// update_meta_federation.)
#[derive(Debug, sqlx::FromRow)]
pub struct MetaNodeinfoRow {
    pub name: Option<String>,
    pub description: Option<String>,
    #[sqlx(rename = "maintainerName")]
    pub maintainer_name: Option<String>,
    #[sqlx(rename = "maintainerEmail")]
    pub maintainer_email: Option<String>,
    #[sqlx(rename = "disableRegistration")]
    pub disable_registration: bool,
    pub langs: Vec<String>,
    #[sqlx(rename = "termsOfServiceUrl")]
    pub terms_of_service_url: Option<String>,
    #[sqlx(rename = "privacyPolicyUrl")]
    pub privacy_policy_url: Option<String>,
    #[sqlx(rename = "inquiryUrl")]
    pub inquiry_url: Option<String>,
    #[sqlx(rename = "impressumUrl")]
    pub impressum_url: Option<String>,
    #[sqlx(rename = "repositoryUrl")]
    pub repository_url: String,
    #[sqlx(rename = "feedbackUrl")]
    pub feedback_url: String,
    #[sqlx(rename = "emailRequiredForSignup")]
    pub email_required_for_signup: bool,
    #[sqlx(rename = "enableHcaptcha")]
    pub enable_hcaptcha: bool,
    #[sqlx(rename = "enableRecaptcha")]
    pub enable_recaptcha: bool,
    #[sqlx(rename = "enableMcaptcha")]
    pub enable_mcaptcha: bool,
    #[sqlx(rename = "enableTurnstile")]
    pub enable_turnstile: bool,
    #[sqlx(rename = "enableEmail")]
    pub enable_email: bool,
    #[sqlx(rename = "enableServiceWorker")]
    pub enable_service_worker: bool,
    #[sqlx(rename = "themeColor")]
    pub theme_color: Option<String>,
    pub policies: serde_json::Value,
}

/// MetaService.fetch(true) mirror: ensures the sentinel row ('x') then
/// returns the latest row (fetch with allowCache is called through this
/// path by nodeinfo2 and the /.well-known/* federation gates).
pub async fn fetch_nodeinfo_meta(pool: &PgPool) -> Result<MetaNodeinfoRow, sqlx::Error> {
    sqlx::query(r#"INSERT INTO "meta" ("id") VALUES ('x') ON CONFLICT ("id") DO NOTHING"#)
        .execute(pool)
        .await?;
    sqlx::query_as::<_, MetaNodeinfoRow>(
        r#"SELECT "name", "description", "maintainerName", "maintainerEmail",
                  "disableRegistration", "langs", "termsOfServiceUrl", "privacyPolicyUrl",
                  "inquiryUrl", "impressumUrl", "repositoryUrl", "feedbackUrl",
                  "emailRequiredForSignup", "enableHcaptcha", "enableRecaptcha",
                  "enableMcaptcha", "enableTurnstile", "enableEmail", "enableServiceWorker",
                  "themeColor", "policies"
           FROM "meta" ORDER BY "id" DESC LIMIT 1"#,
    )
    .fetch_one(pool)
    .await
}

/// `endpoints/retention.ts`: the newest 30 `retention_aggregation` rows
/// (`order: { id: 'DESC' }, take: 30`). `createdAt` bridges to epoch-ms like
/// the rest of mk-db (the workspace sqlx has no chrono decoder); `data` is
/// the raw jsonb counter map. The api pack renders `usersCount` as `users`
/// and `createdAt` through `ms_to_iso`.
#[derive(Debug, sqlx::FromRow)]
pub struct RetentionAggregationRow {
    #[sqlx(rename = "createdAtMs")]
    pub created_at_ms: i64,
    #[sqlx(rename = "usersCount")]
    pub users_count: i32,
    pub data: serde_json::Value,
}

pub async fn retention_aggregations(
    pool: &PgPool,
) -> Result<Vec<RetentionAggregationRow>, sqlx::Error> {
    sqlx::query_as::<_, RetentionAggregationRow>(
        r#"SELECT (EXTRACT(EPOCH FROM "createdAt") * 1000)::bigint AS "createdAtMs",
                  "usersCount", "data"
           FROM "retention_aggregation"
           ORDER BY "id" DESC
           LIMIT 30"#,
    )
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn words_roundtrip() {
        if std::env::var("DATABASE_URL").is_err() {
            return;
        }
        // fresh_db only provisions the database; the meta table rides the
        // explicit migrations (the api-side fresh_pool contract).
        let pool = crate::test_support::fresh_db("mkdb_meta_words").await;
        crate::run_migrations(&pool).await.unwrap();
        let (s, p, d) = note_content_moderation_words(&pool).await.unwrap();
        assert!(s.is_empty() && p.is_empty() && d.is_none());

        update_meta_word_lists(&pool, Some(&["test".to_string()]), Some(&["ngo".into()]))
            .await
            .unwrap();
        let (s, p, d) = note_content_moderation_words(&pool).await.unwrap();
        assert_eq!(s, vec!["test".to_string()]);
        assert_eq!(p, vec!["ngo".to_string()]);
        assert!(d.is_none());
    }
}
