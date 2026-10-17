//! Relation queries: the 10 protobuf... the 10 boolean-ish fields of
//! `UserEntityService.getRelation(me, target)` (upstream Promise.all of
//! one findOneBy + seven exists).

use sqlx::PgPool;

pub struct UserRelation {
    pub is_following: bool,
    pub is_followed: bool,
    pub has_pending_follow_request_from_you: bool,
    pub has_pending_follow_request_to_you: bool,
    pub is_blocking: bool,
    pub is_blocked: bool,
    pub is_muted: bool,
    pub is_renote_muted: bool,
    /// `"following"."notify"` when a follow row exists (pack overlays
    /// `?? 'none'` — the raw value may be NULL).
    pub notify: Option<String>,
    /// `"following"."withReplies"` when a follow row exists.
    pub with_replies: Option<bool>,
}

pub async fn get_relation(
    pool: &PgPool,
    me: &str,
    target: &str,
) -> Result<UserRelation, sqlx::Error> {
    let is_following_row: Option<(Option<String>, Option<bool>)> =
        sqlx::query_as(r#"SELECT "notify", "withReplies" FROM "following" WHERE "followerId" = $1 AND "followeeId" = $2"#)
            .bind(me)
            .bind(target)
            .fetch_optional(pool)
            .await?;

    async fn exists(
        pool: &PgPool,
        table: &str,
        a_col: &str,
        a: &str,
        b_col: &str,
        b: &str,
    ) -> Result<bool, sqlx::Error> {
        // table/column names come from call sites below (static), so the
        // format! interpolation is constant — no injection surface.
        let n: i64 = sqlx::query_scalar(&format!(
            r#"SELECT count(*) FROM "{}" WHERE "{}" = $1 AND "{}" = $2"#,
            table, a_col, b_col
        ))
        .bind(a)
        .bind(b)
        .fetch_one(pool)
        .await?;
        Ok(n > 0)
    }

    let rel = UserRelation {
        is_following: is_following_row.is_some(),
        is_followed: exists(pool, "following", "followerId", target, "followeeId", me).await?,
        has_pending_follow_request_from_you: exists(
            pool,
            "follow_request",
            "followerId",
            me,
            "followeeId",
            target,
        )
        .await?,
        has_pending_follow_request_to_you: exists(
            pool,
            "follow_request",
            "followerId",
            target,
            "followeeId",
            me,
        )
        .await?,
        is_blocking: exists(pool, "blocking", "blockerId", me, "blockeeId", target).await?,
        is_blocked: exists(pool, "blocking", "blockerId", target, "blockeeId", me).await?,
        is_muted: exists(pool, "muting", "muterId", me, "muteeId", target).await?,
        is_renote_muted: exists(pool, "renote_muting", "muterId", me, "muteeId", target).await?,
        notify: is_following_row.as_ref().and_then(|r| r.0.clone()),
        with_replies: is_following_row.as_ref().and_then(|r| r.1),
    };
    Ok(rel)
}
