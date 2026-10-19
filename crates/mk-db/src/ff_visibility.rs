//! ff-visibility arena: the following/followers list domain shared by the
//! `users/followers` + `users/following` endpoints and the AP
//! `/users/:id/{followers,following}` collections (upstream
//! `endpoints/users/{followers,following}.ts` and
//! `server/ActivityPubServerService.ts {followers,following}`).
//!
//! - meta."federation" today stays 'none' on the default meta row; the
//!   ff-visibility.ts beforeAll flips it to 'all' via admin/update-meta and
//!   the AP collections gate on it (`if (this.meta.federation === 'none')` 403).
//! - the `following` table has no createdAt column (the AID id encodes the
//!   creation ms; upstream derives createdAt from the id too), so pagination
//!   runs purely on id bounds — the endpoint layer converts since/untilDate
//!   to id boundaries exactly like `QueryService.makePaginationQuery` does
//!   via `idService.gen(date)`.

use sqlx::PgPool;

/// meta."federation" of the newest meta row ("id" DESC — the sentinel row);
/// missing row → schema default 'none' (upstream MetaService.fetch upserts
/// with TypeORM column defaults before any cache exists).
pub async fn meta_federation(pool: &PgPool) -> Result<String, sqlx::Error> {
    let v: Option<String> =
        sqlx::query_scalar(r#"SELECT "federation" FROM "meta" ORDER BY "id" DESC LIMIT 1"#)
            .fetch_optional(pool)
            .await?
            .flatten();
    Ok(v.unwrap_or_else(|| "none".to_string()))
}

/// admin/update-meta persist for meta."federation" (MetaService.update partial
/// patch: the sentinel row is updated in place, or seeded on first write —
/// same shape as update_meta_word_lists).
pub async fn update_meta_federation(pool: &PgPool, value: &str) -> Result<(), sqlx::Error> {
    let updated = sqlx::query(
        r#"
        UPDATE "meta" SET "federation" = $1
        WHERE "id" = (SELECT "id" FROM "meta" ORDER BY "id" DESC LIMIT 1)
        "#,
    )
    .bind(value)
    .execute(pool)
    .await?
    .rows_affected();
    if updated == 0 {
        sqlx::query(
            r#"
            INSERT INTO "meta" ("id", "federation") VALUES ('x', $1)
            ON CONFLICT ("id") DO UPDATE SET "federation" = $1
            "#,
        )
        .bind(value)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// One `following` row (the endpoint layer packs the edge together with the
/// followee/follower users it joins upstream).
#[derive(Debug, sqlx::FromRow)]
pub struct FollowingEdge {
    pub id: String,
    #[sqlx(rename = "followerId")]
    pub follower_id: String,
    #[sqlx(rename = "followeeId")]
    pub followee_id: String,
}

/// `EdgeSide::Followers` = edges INTO the user (followeeId = user; upstream
/// `users/followers`), `EdgeSide::Following` = edges OUT of the user
/// (followerId = user; upstream `users/following`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeSide {
    Followers,
    Following,
}

/// makePaginationQuery over the following table (all bounds already id-shaped;
/// the endpoint layer gen()-converts date params): from_id is exclusive
/// `>` (since), until_id exclusive `<` (until), desc picks the ORDER BY.
pub async fn list_edges(
    pool: &PgPool,
    side: EdgeSide,
    user_id: &str,
    from_id: Option<&str>,
    until_id: Option<&str>,
    desc: bool,
    limit: u32,
) -> Result<Vec<FollowingEdge>, sqlx::Error> {
    // users/followers joins the follower, users/following the followee —
    // the WHERE column flips with the side (followers → followeeId = user).
    let where_col = match side {
        EdgeSide::Followers => "followeeId",
        EdgeSide::Following => "followerId",
    };
    let order = if desc { "DESC" } else { "ASC" };
    sqlx::query_as::<_, FollowingEdge>(&format!(
        r#"
        SELECT "id", "followerId", "followeeId" FROM "following"
        WHERE "{where_col}" = $1
          AND ($2::varchar IS NULL OR "id" > $2)
          AND ($3::varchar IS NULL OR "id" < $3)
        ORDER BY "id" {order}
        LIMIT $4
        "#
    ))
    .bind(user_id)
    .bind(from_id)
    .bind(until_id)
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn edge(pool: &PgPool, follower: &str, followee: &str, id: &str) {
        sqlx::query(
            r#"
            INSERT INTO "following"
              ("id", "followeeId", "followerId", "followerHost", "followerInbox",
               "followerSharedInbox", "followeeHost", "followeeInbox",
               "followeeSharedInbox", "notify", "withReplies", "isFollowerHibernated")
            VALUES ($1, $2, $3, NULL, NULL, NULL, NULL, NULL, NULL, 'none', FALSE, FALSE)
            "#,
        )
        .bind(id)
        .bind(followee)
        .bind(follower)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn ff_meta_federation_roundtrip() {
        if std::env::var("DATABASE_URL").is_err() {
            return;
        }
        let pool = crate::test_support::fresh_db("mkdb_ff_meta").await;
        crate::run_migrations(&pool).await.unwrap();
        // missing row → 'none' (the schema default)
        assert_eq!(meta_federation(&pool).await.unwrap(), "none");
        update_meta_federation(&pool, "all").await.unwrap();
        assert_eq!(meta_federation(&pool).await.unwrap(), "all");
    }

    #[tokio::test]
    async fn ff_list_edges_side_and_bounds() {
        if std::env::var("DATABASE_URL").is_err() {
            return;
        }
        let pool = crate::test_support::fresh_db("mkdb_ff_edges").await;
        crate::run_migrations(&pool).await.unwrap();
        let a = "aaaa".to_owned();
        let b = "bbbb".to_owned();
        edge(&pool, &b, &a, "0000000001").await; // bob follows alice
        edge(&pool, &a, &b, "0000000002").await; // alice follows bob

        let followers = list_edges(&pool, EdgeSide::Followers, &a, None, None, true, 10)
            .await
            .unwrap();
        assert_eq!(followers.len(), 1);
        assert_eq!(followers[0].follower_id, b);
        assert_eq!(followers[0].followee_id, a);

        let following_desc = list_edges(&pool, EdgeSide::Following, &a, None, None, true, 10)
            .await
            .unwrap();
        assert_eq!(following_desc.len(), 1);
        assert_eq!(following_desc[0].followee_id, b);

        let until_bound = list_edges(
            &pool,
            EdgeSide::Following,
            &a,
            None,
            Some("0000000002"),
            true,
            10,
        )
        .await
        .unwrap();
        assert!(until_bound.is_empty(), "exclusive until drops the row");

        let since_asc = list_edges(
            &pool,
            EdgeSide::Following,
            &a,
            Some("0000000001"),
            None,
            false,
            10,
        )
        .await
        .unwrap();
        assert_eq!(since_asc.len(), 1);
        assert_eq!(since_asc[0].id, "0000000002");

        let _ = pool;
    }
}
