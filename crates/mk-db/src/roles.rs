//! roles-v2: SQL support for the rest of the `admin/roles/*` family
//! (upstream `endpoints/admin/roles/{show,list,users,update}.ts` +
//! `RoleService.assign` with expiry, `src/core/RoleService.ts`).
//! Self-contained on the `roles_lite` pattern:
//! - `fetch_cond_roles` — every role row with `target`/`condFormula` for the
//!   conditional-eval engine in `mk_core::services::roles_v2`;
//! - `list_role_packs` / `find_role_pack` — the Role pack inputs with
//!   `updatedAt`/epoch-ms (`timestamptz` → `(EXTRACT(EPOCH ...) * 1000)`,
//!   the `admin.rs` RoleAssignRow convention) and the non-expired
//!   `usersCount` (`RoleEntityService.pack` counts live assignments only);
//! - `assign_role_expiring` — `RoleService.assign`'s dedup: a live row
//!   (never-expires or future `expiresAt`) → `Ok(false)`
//!   (AlreadyAssignedError), an expired row is swept and refreshed,
//!   `expiresAt` bound as epoch-ms through `to_timestamp($4 / 1000.0)`;
//! - `role_assignments_page` — the `admin/roles/users` join
//!   (`makePaginationQuery` default order `id DESC`, expired rows hidden);
//! - `update_role` — `RoleService.update`: partial column update
//!   (`updatedAt` = now), list order `lastUsedAt` DESC.

use sqlx::PgPool;

/// Every role row for the conditional-eval pass. Mirrors the
/// `getUserRoles` fetch shape (`rolesRepository.findBy({})`), carrying the
/// `AssignedRole` columns plus `target`/`condFormula`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CondRole {
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
    pub target: String,
    #[sqlx(rename = "condFormula")]
    pub cond_formula: serde_json::Value,
}

pub async fn fetch_cond_roles(pool: &PgPool) -> Result<Vec<CondRole>, sqlx::Error> {
    sqlx::query_as::<_, CondRole>(
        r#"
        SELECT "id" AS "id",
               "name" AS "name",
               "isPublic" AS "isPublic",
               "isModerator" AS "isModerator",
               "isAdministrator" AS "isAdministrator",
               "asBadge" AS "asBadge",
               "color" AS "color",
               "iconUrl" AS "iconUrl",
               "description" AS "description",
               "displayOrder"::bigint AS "displayOrder",
               "policies" AS "policies",
               "target"::text AS "target",
               "condFormula" AS "condFormula"
        FROM "role"
        "#,
    )
    .fetch_all(pool)
    .await
}

/// Role pack inputs for `admin/roles/{show,list}`. `updatedAtMs` formats
/// through `ms_to_iso` (IdService/ISO parity — `timestamptz::text` is NOT
/// ISO parseable); `usersCount` counts non-expired assignments
/// (RoleEntityService.pack's Brackets clause).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RolePackRow {
    pub id: String,
    pub name: String,
    pub description: String,
    pub color: Option<String>,
    #[sqlx(rename = "iconUrl")]
    pub icon_url: Option<String>,
    pub target: String,
    #[sqlx(rename = "condFormula")]
    pub cond_formula: serde_json::Value,
    #[sqlx(rename = "isPublic")]
    pub is_public: bool,
    #[sqlx(rename = "isAdministrator")]
    pub is_administrator: bool,
    #[sqlx(rename = "isModerator")]
    pub is_moderator: bool,
    #[sqlx(rename = "isExplorable")]
    pub is_explorable: bool,
    #[sqlx(rename = "asBadge")]
    pub as_badge: bool,
    #[sqlx(rename = "preserveAssignmentOnMoveAccount")]
    pub preserve_assignment_on_move_account: bool,
    #[sqlx(rename = "canEditMembersByModerator")]
    pub can_edit_members_by_moderator: bool,
    #[sqlx(rename = "displayOrder")]
    pub display_order: i64,
    pub policies: serde_json::Value,
    #[sqlx(rename = "updatedAtMs")]
    pub updated_at_ms: i64,
    #[sqlx(rename = "usersCount")]
    pub users_count: i64,
}

const ROLE_PACK_COLUMNS: &str = r#"
               "id", "name", "description", "color", "iconUrl",
               "target"::text AS "target", "condFormula",
               "isPublic", "isAdministrator", "isModerator", "isExplorable",
               "asBadge", "preserveAssignmentOnMoveAccount",
               "canEditMembersByModerator", "displayOrder"::bigint AS "displayOrder",
               "policies",
               (EXTRACT(EPOCH FROM "updatedAt") * 1000)::bigint AS "updatedAtMs",
               (SELECT count(*) FROM "role_assignment" a
                 WHERE a."roleId" = role."id"
                   AND (a."expiresAt" IS NULL OR a."expiresAt" > now())) AS "usersCount"
"#;

/// `rolesRepository.find({ order: { lastUsedAt: 'DESC' } })` — the
/// admin/roles/list ordering.
pub async fn list_role_packs(pool: &PgPool) -> Result<Vec<RolePackRow>, sqlx::Error> {
    let sql = format!(
        r#"SELECT {} FROM "role" role ORDER BY role."lastUsedAt" DESC"#,
        ROLE_PACK_COLUMNS
    );
    sqlx::query_as::<_, RolePackRow>(&sql).fetch_all(pool).await
}

pub async fn find_role_pack(pool: &PgPool, id: &str) -> Result<Option<RolePackRow>, sqlx::Error> {
    let sql = format!(
        r#"SELECT {} FROM "role" role WHERE role."id" = $1"#,
        ROLE_PACK_COLUMNS
    );
    sqlx::query_as::<_, RolePackRow>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// One `role_assignment` row for `admin/roles/users`; `expiresAtMs` renders
/// back as an ISO `expiresAt` (or null).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AssignmentRow {
    pub id: String,
    #[sqlx(rename = "userId")]
    pub user_id: String,
    #[sqlx(rename = "expiresAtMs")]
    pub expires_at_ms: Option<i64>,
}

/// `makePaginationQuery` default (id DESC) + the role filter + the
/// non-expired Brackets clause; `since_id`/`until_id` mirror the
/// sinceId/untilId bounds.
pub async fn role_assignments_page(
    pool: &PgPool,
    role_id: &str,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
) -> Result<Vec<AssignmentRow>, sqlx::Error> {
    sqlx::query_as::<_, AssignmentRow>(
        r#"
        SELECT a."id" AS "id",
               a."userId" AS "userId",
               CASE
                 WHEN a."expiresAt" IS NULL THEN NULL
                 ELSE (EXTRACT(EPOCH FROM a."expiresAt") * 1000)::bigint
               END AS "expiresAtMs"
        FROM "role_assignment" a
        WHERE a."roleId" = $1
          AND (a."expiresAt" IS NULL OR a."expiresAt" > now())
          AND ($2::varchar IS NULL OR a."id" > $2)
          AND ($3::varchar IS NULL OR a."id" < $3)
        ORDER BY a."id" DESC
        LIMIT $4
        "#,
    )
    .bind(role_id)
    .bind(since_id)
    .bind(until_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// `RoleService.assign` with an expiry: dedup on the (roleId, userId) pair
/// where a LIVE row (no expiry / future expiry) → `Ok(false)`
/// (AlreadyAssignedError — the private error surfaces as the 500 shape); an
/// expired row is deleted then re-inserted. `expires_ms` (epoch ms) binds
/// through `to_timestamp($4 / 1000.0)` (NULL → never expires). Bumps the
/// role's `lastUsedAt`.
pub async fn assign_role_expiring(
    pool: &PgPool,
    id: &str,
    user_id: &str,
    role_id: &str,
    expires_ms: Option<i64>,
) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let live: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM "role_assignment"
               WHERE "roleId" = $1 AND "userId" = $2
                 AND ("expiresAt" IS NULL OR "expiresAt" > now())
           )"#,
    )
    .bind(role_id)
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await?;
    if live {
        tx.rollback().await.ok();
        return Ok(false);
    }
    // upstream sweeps an expired row before re-inserting
    sqlx::query(r#"DELETE FROM "role_assignment" WHERE "roleId" = $1 AND "userId" = $2"#)
        .bind(role_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        r#"INSERT INTO "role_assignment" ("id", "userId", "roleId", "expiresAt")
           VALUES ($1, $2, $3, to_timestamp($4 / 1000.0))"#,
    )
    .bind(id)
    .bind(user_id)
    .bind(role_id)
    .bind(expires_ms)
    .execute(&mut *tx)
    .await?;
    sqlx::query(r#"UPDATE "role" SET "lastUsedAt" = now() WHERE "id" = $1"#)
        .bind(role_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

/// `RoleService.update` — only the provided fields change (`paramDef` is
/// supplied-minus-roleId optional); `updatedAt` always restamps. Built as a
/// dynamic SET over the typed `RoleUpdate` options.
#[derive(Debug, Default, Clone)]
pub struct RoleUpdate {
    pub name: Option<String>,
    pub description: Option<String>,
    /// `Option<Option<String>>`: outer None = param absent (keep), inner
    /// None = explicit null from the wire (set NULL).
    pub color: Option<Option<String>>,
    pub icon_url: Option<Option<String>>,
    pub target: Option<String>,
    pub cond_formula: Option<serde_json::Value>,
    pub is_public: Option<bool>,
    pub is_moderator: Option<bool>,
    pub is_administrator: Option<bool>,
    pub is_explorable: Option<bool>,
    pub as_badge: Option<bool>,
    pub preserve_assignment_on_move_account: Option<bool>,
    pub can_edit_members_by_moderator: Option<bool>,
    pub display_order: Option<i64>,
    pub policies: Option<serde_json::Value>,
}

pub async fn update_role(pool: &PgPool, id: &str, upd: &RoleUpdate) -> Result<(), sqlx::Error> {
    use sqlx::QueryBuilder;
    let mut qb = QueryBuilder::new(r#"UPDATE "role" SET "#);
    let mut first = true;
    macro_rules! set_col {
        ($col:literal, $val:expr) => {
            set_col!($col, $val, "")
        };
        ($col:literal, $val:expr, $cast:literal) => {
            if !first {
                qb.push(", ");
            }
            first = false;
            // the cast suffix rides directly on the generated placeholder
            // ($N::...) so enum/text columns bind like the roles_lite
            // INSERT's $9::text::public.role_target_enum
            qb.push(concat!($col, " = ")).push_bind($val).push($cast);
        };
    }
    if let Some(v) = &upd.name {
        set_col!("\"name\"", v);
    }
    if let Some(v) = &upd.description {
        set_col!("\"description\"", v);
    }
    if let Some(v) = &upd.color {
        set_col!("\"color\"", v);
    }
    if let Some(v) = &upd.icon_url {
        set_col!("\"iconUrl\"", v);
    }
    if let Some(v) = &upd.target {
        set_col!("\"target\"", v, "::text::public.role_target_enum");
    }
    if let Some(v) = &upd.cond_formula {
        set_col!("\"condFormula\"", v);
    }
    if let Some(v) = &upd.is_public {
        set_col!("\"isPublic\"", v);
    }
    if let Some(v) = &upd.is_moderator {
        set_col!("\"isModerator\"", v);
    }
    if let Some(v) = &upd.is_administrator {
        set_col!("\"isAdministrator\"", v);
    }
    if let Some(v) = &upd.is_explorable {
        set_col!("\"isExplorable\"", v);
    }
    if let Some(v) = &upd.as_badge {
        set_col!("\"asBadge\"", v);
    }
    if let Some(v) = &upd.preserve_assignment_on_move_account {
        set_col!("\"preserveAssignmentOnMoveAccount\"", v);
    }
    if let Some(v) = &upd.can_edit_members_by_moderator {
        set_col!("\"canEditMembersByModerator\"", v);
    }
    if let Some(v) = &upd.display_order {
        set_col!("\"displayOrder\"", *v as i32);
    }
    if let Some(v) = &upd.policies {
        set_col!("\"policies\"", v);
    }
    if first {
        // nothing supplied — upstream still runs update() (no-op + timestamps)
        qb.push("\"updatedAt\" = now()");
    } else {
        qb.push(", \"updatedAt\" = now()");
    }
    qb.push(" WHERE \"id\" = ").push_bind(id);
    qb.build().execute(pool).await?;
    Ok(())
}
