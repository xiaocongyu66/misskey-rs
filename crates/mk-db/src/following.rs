//! `following/*` remainder DB paths (the `following/create|delete` +
//! `requests/accept|cancel` writes live in `social.rs`).
//!
//! - `following/invalidate` reuses `social::delete_following` +
//!   `social::bump_follow_counts` (UserFollowingService.invalidate →
//!   unfollow), so this module only owns the read side;
//! - `following/requests/list` reads the pending `follow_request` rows of the
//!   authenticated followee (endpoints/following/requests/list.ts).

use sqlx::PgPool;

/// One pending follow request: the row id (cursor) + the requester id the
/// endpoint packs. Upstream `followRequestsRepository.find({ where: {
/// followeeId }, relations: ['follower'], order: { id: 'DESC' }, cursor })`.
#[derive(Debug, sqlx::FromRow)]
pub struct FollowRequestRow {
    pub id: String,
    #[sqlx(rename = "followerId")]
    pub follower_id: String,
}

/// Pending requests addressed to `followee_id`, id DESC, with the TypeORM
/// `cursor: { id }, skip: 1` bound the endpoint passes (`id < cursor` in the
/// DESC walk; `None` → unbounded). `limit` is already paramDef-validated
/// (1..=100).
pub async fn list_follow_requests(
    pool: &PgPool,
    followee_id: &str,
    cursor_id: Option<&str>,
    limit: u32,
) -> Result<Vec<FollowRequestRow>, sqlx::Error> {
    sqlx::query_as::<_, FollowRequestRow>(
        r#"
        SELECT "id", "followerId" FROM "follow_request"
        WHERE "followeeId" = $1
          AND ($2::varchar IS NULL OR "id" < $2)
        ORDER BY "id" DESC
        LIMIT $3
        "#,
    )
    .bind(followee_id)
    .bind(cursor_id)
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await
}

/// `endpoints/following/requests/sent.ts` — the caller's outgoing pending
/// requests, id DESC with the makePaginationQuery id bounds.
#[derive(Debug, sqlx::FromRow)]
pub struct SentFollowRequestRow {
    pub id: String,
    #[sqlx(rename = "followerId")]
    pub follower_id: String,
    #[sqlx(rename = "followeeId")]
    pub followee_id: String,
}

pub async fn list_sent_follow_requests(
    pool: &PgPool,
    me_id: &str,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<SentFollowRequestRow>, sqlx::Error> {
    sqlx::query_as::<_, SentFollowRequestRow>(
        r#"
        SELECT "id", "followerId", "followeeId" FROM "follow_request"
        WHERE "followerId" = $1
          AND ($2::varchar IS NULL OR "id" > $2)
          AND ($3::varchar IS NULL OR "id" < $3)
        ORDER BY "id" DESC
        LIMIT $4
        "#,
    )
    .bind(me_id)
    .bind(since_id)
    .bind(until_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// `UserFollowingService.rejectFollowRequest` delete half (the endpoint's
/// caller applies the counter decrement when a row was removed).
pub async fn reject_follow_request(
    pool: &PgPool,
    follower_id: &str,
    followee_id: &str,
) -> Result<bool, sqlx::Error> {
    let n = sqlx::query(
        r#"DELETE FROM "follow_request" WHERE "followerId" = $1 AND "followeeId" = $2"#,
    )
    .bind(follower_id)
    .bind(followee_id)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(n > 0)
}

/// `endpoints/following/update-all.ts` — update every row the caller owns;
/// `None` leaves the column untouched (TypeORM `undefined`).
pub async fn update_all_followings(
    pool: &PgPool,
    me_id: &str,
    notify: Option<Option<&str>>,
    with_replies: Option<bool>,
) -> Result<(), sqlx::Error> {
    if let Some(n) = notify {
        sqlx::query(r#"UPDATE "following" SET "notify" = $2 WHERE "followerId" = $1"#)
            .bind(me_id)
            .bind(n)
            .execute(pool)
            .await?;
    }
    if let Some(w) = with_replies {
        sqlx::query(r#"UPDATE "following" SET "withReplies" = $2 WHERE "followerId" = $1"#)
            .bind(me_id)
            .bind(w)
            .execute(pool)
            .await?;
    }
    Ok(())
}
