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
