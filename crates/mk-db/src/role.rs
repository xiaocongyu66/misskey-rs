//! Role assignment fetch for the policy engine: `role_assignment` joined
//! with `role` per user. Mirrors `RoleService.getUserAssigns/getUserRoles`
//! (upstream `src/core/RoleService.ts`):
//! - assignments whose `expiresAt` is set and already passed are excluded
//!   (upstream: `assigns.filter(a => a.expiresAt == null || a.expiresAt.getTime() > now())`);
//! - rows are ordered `"displayOrder" DESC`, the order every upstream
//!   consumer re-sorts by (badgeRoles / public roles lists).
//!
//! MVP scope note: conditional roles (`role.target = 'conditional'`) are
//! granted by `condFormula` evaluation, not by assignment rows — they are
//! out of scope here; only manual assignments are fetched.

use sqlx::PgPool;

/// One assigned role for a user: `role` columns reached through a
/// non-expired `role_assignment` row. `policies` is the raw role `policies`
/// jsonb: `{ [policyName]: { priority, useDefault, value } }`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AssignedRole {
    pub id: String,
    pub name: String,
    #[sqlx(rename = "isPublic")]
    pub is_public: bool,
    #[sqlx(rename = "isModerator")]
    pub is_moderator: bool,
    #[sqlx(rename = "isAdministrator")]
    pub is_administrator: bool,
    #[sqlx(rename = "asBadge")]
    pub as_badge: bool,
    pub color: Option<String>,
    #[sqlx(rename = "iconUrl")]
    pub icon_url: Option<String>,
    pub description: String,
    #[sqlx(rename = "displayOrder")]
    pub display_order: i64,
    pub policies: serde_json::Value,
}

/// Non-expired roles assigned to `user_id`, `"displayOrder" DESC`.
/// (`mk-core/src/services/role.rs` holds the policy-merge engine.)
pub async fn user_assigned_roles(
    pool: &PgPool,
    user_id: &str,
) -> Result<Vec<AssignedRole>, sqlx::Error> {
    sqlx::query_as::<_, AssignedRole>(
        r#"
        SELECT r."id" AS "id",
               r."name" AS "name",
               r."isPublic" AS "isPublic",
               r."isModerator" AS "isModerator",
               r."isAdministrator" AS "isAdministrator",
               r."asBadge" AS "asBadge",
               r."color" AS "color",
               r."iconUrl" AS "iconUrl",
               r."description" AS "description",
               r."displayOrder"::bigint AS "displayOrder",
               r."policies" AS "policies"
        FROM "role_assignment" a
        JOIN "role" r ON r."id" = a."roleId"
        WHERE a."userId" = $1
          AND (a."expiresAt" IS NULL OR a."expiresAt" > now())
        ORDER BY r."displayOrder" DESC
        "#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_migrations;
    use crate::test_support::fresh_db;

    async fn setup(db_name: &str) -> PgPool {
        let pool = fresh_db(db_name).await;
        run_migrations(&pool).await.expect("migrations");
        pool
    }

    async fn insert_user(pool: &PgPool, id: &str) {
        sqlx::query(
            r#"INSERT INTO "user" ("id", "username", "usernameLower") VALUES ($1, $2, $2)"#,
        )
        .bind(id)
        .bind(format!("u{id}"))
        .execute(pool)
        .await
        .expect("insert user");
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_role(
        pool: &PgPool,
        id: &str,
        display_order: i32,
        is_public: bool,
        is_moderator: bool,
        is_administrator: bool,
        as_badge: bool,
        policies: &str,
    ) {
        sqlx::query(
            r#"
            INSERT INTO "role" ("id", "updatedAt", "name", "description", "lastUsedAt",
                                "isPublic", "isModerator", "isAdministrator", "asBadge",
                                "displayOrder", "policies")
            VALUES ($1, now(), $1, $1, now(), $2, $3, $4, $5, $6, $7::jsonb)
            "#,
        )
        .bind(id)
        .bind(is_public)
        .bind(is_moderator)
        .bind(is_administrator)
        .bind(as_badge)
        .bind(display_order)
        .bind(policies)
        .execute(pool)
        .await
        .expect("insert role");
    }

    async fn assign(pool: &PgPool, user_id: &str, role_id: &str, expires_at: Option<&str>) {
        sqlx::query(
            r#"
            INSERT INTO "role_assignment" ("id", "userId", "roleId", "expiresAt")
            VALUES ($1, $2, $3, $4::timestamptz)
            "#,
        )
        .bind(format!("a{}{}", user_id, role_id))
        .bind(user_id)
        .bind(role_id)
        .bind(expires_at)
        .execute(pool)
        .await
        .expect("insert role_assignment");
    }

    #[tokio::test]
    async fn no_assignments_returns_empty() {
        if std::env::var("DATABASE_URL").is_err() {
            return;
        }
        let pool = setup("mkdb_role_empty").await;
        insert_user(&pool, "u1").await;
        let rows = user_assigned_roles(&pool, "u1").await.expect("fetch");
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn expired_excluded_and_ordered_display_order_desc() {
        if std::env::var("DATABASE_URL").is_err() {
            return;
        }
        let pool = setup("mkdb_role_order").await;
        insert_user(&pool, "u1").await;
        insert_user(&pool, "u2").await;
        insert_role(&pool, "rLow", 1, true, false, false, false, "{}").await;
        insert_role(&pool, "rHigh", 5, true, true, false, true, "{}").await;
        insert_role(&pool, "rExpired", 9, false, false, false, false, "{}").await;

        assign(&pool, "u1", "rLow", None).await; // never expires
        assign(&pool, "u1", "rHigh", Some("2099-01-01T00:00:00Z")).await; // future → kept
        assign(&pool, "u1", "rExpired", Some("2000-01-01T00:00:00Z")).await; // past → filtered
        assign(&pool, "u2", "rHigh", None).await; // only u2

        let rows = user_assigned_roles(&pool, "u1").await.expect("fetch");
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["rHigh", "rLow"]);
        let high = &rows[0];
        assert!(high.as_badge && high.is_public && high.is_moderator);
        assert!(!high.is_administrator);
        assert_eq!(high.display_order, 5);
        assert_eq!(rows[1].display_order, 1);
        assert_eq!(rows[1].policies, serde_json::json!({}));

        let rows2 = user_assigned_roles(&pool, "u2").await.expect("fetch u2");
        let ids2: Vec<&str> = rows2.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids2, vec!["rHigh"]);
    }

    #[tokio::test]
    async fn policies_jsonb_carried_through() {
        if std::env::var("DATABASE_URL").is_err() {
            return;
        }
        let pool = setup("mkdb_role_policies").await;
        insert_user(&pool, "u1").await;
        let policies = r#"{"pinLimit":{"priority":0,"useDefault":false,"value":9}}"#;
        insert_role(&pool, "r1", 0, true, false, false, false, policies).await;
        assign(&pool, "u1", "r1", None).await;
        let rows = user_assigned_roles(&pool, "u1").await.expect("fetch");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].policies["pinLimit"]["value"], serde_json::json!(9));
    }
}
