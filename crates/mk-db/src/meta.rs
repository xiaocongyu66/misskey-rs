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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn words_roundtrip() {
        if std::env::var("DATABASE_URL").is_err() {
            return;
        }
        let pool = crate::test_support::fresh_db("mkdb_meta_words").await;
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
