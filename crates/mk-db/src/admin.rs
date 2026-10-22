//! Admin-flow SQL (M2 slice, `endpoints/admin/*` ports).
//!
//! Suspend writes mirror `core/UserSuspendService.ts`'s DAO effects; the
//! id-set builders mirror `RoleService.getAdministratorIds/getModeratorIds`;
//! the listing query mirrors `endpoints/admin/show-users.ts`'s
//! state/origin/sort switch. Per-user role probes/policy merges live in
//! `mk-core/src/services/role.rs` (crate dep direction forbids using them
//! here); `crate::role::user_assigned_roles` is the shared fetch they all go
//! through.

use sqlx::PgPool;

use crate::user::{LOCAL_USER_COLS, LocalUserRow};

/// `UserSuspendService.suspend` DB writes: `usersRepository.update(id, {
/// isSuspended: true })` + the harness-visible `updatedAt = now()` bump, and
/// `postSuspend`'s follow-request purge in both directions (the AP Delete
/// deliver / unfollow queue jobs have no local DB footprint).
pub async fn suspend_user(pool: &PgPool, user_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "user" SET "isSuspended" = TRUE, "updatedAt" = now() WHERE "id" = $1"#)
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query(r#"DELETE FROM "follow_request" WHERE "followeeId" = $1"#)
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query(r#"DELETE FROM "follow_request" WHERE "followerId" = $1"#)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// `UserSuspendService.unsuspend`: upstream writes only `isSuspended = false`.
pub async fn unsuspend_user(pool: &PgPool, user_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "user" SET "isSuspended" = FALSE WHERE "id" = $1"#)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// `ModerationLogService.log(moderator, type, info)` — one `moderation_log`
/// row keyed by the moderator (`userId`) with the target data in `info`.
pub async fn insert_moderation_log(
    pool: &PgPool,
    id: &str,
    moderator_id: &str,
    log_type: &str,
    info: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO "moderation_log" ("id", "userId", "type", "info")
           VALUES ($1, $2, $3, $4::jsonb)"#,
    )
    .bind(id)
    .bind(moderator_id)
    .bind(log_type)
    .bind(serde_json::to_string(info).unwrap_or_else(|_| "null".to_string()))
    .execute(pool)
    .await?;
    Ok(())
}

/// `meta."rootUserId"` (SignupService stores the first signup there once a
/// meta row exists; a fresh instance has no row → None).
pub async fn root_user_id(pool: &PgPool) -> Result<Option<String>, sqlx::Error> {
    let v: Option<Option<String>> =
        sqlx::query_scalar(r#"SELECT "rootUserId" FROM "meta" LIMIT 1"#)
            .fetch_optional(pool)
            .await?;
    Ok(v.flatten())
}

/// SignupService L160: `if (this.meta.rootUserId == null)
/// await this.metaService.update({ rootUserId: account.id })` — the FIRST
/// signup of an instance becomes root. NULL-rooted or missing meta row →
/// stamp; an already-stamped row stays untouched (single meta row, id 'x',
/// matching update_meta_pinned_users).
pub async fn stamp_root_user_id_if_unset(pool: &PgPool, uid: &str) -> Result<(), sqlx::Error> {
    let updated = sqlx::query(r#"UPDATE "meta" SET "rootUserId" = $1 WHERE "rootUserId" IS NULL"#)
        .bind(uid)
        .execute(pool)
        .await?
        .rows_affected();
    if updated == 0 {
        sqlx::query(
            r#"INSERT INTO "meta" ("id", "rootUserId") VALUES ('x', $1)
            ON CONFLICT ("id") DO NOTHING"#,
        )
        .bind(uid)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// `meta."policies"` (MetaService.fetch → `basePolicies` override source for
/// policy computation; None when no meta row exists).
pub async fn meta_policies(pool: &PgPool) -> Result<Option<serde_json::Value>, sqlx::Error> {
    let v: Option<Option<serde_json::Value>> =
        sqlx::query_scalar(r#"SELECT "policies" FROM "meta" LIMIT 1"#)
            .fetch_optional(pool)
            .await?;
    Ok(v.flatten())
}

/// `RoleService.getAdministratorIds` (root is deliberately NOT included —
/// upstream carries a TODO saying so).
pub async fn administrator_user_ids(pool: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        r#"
        SELECT DISTINCT ra."userId"
        FROM "role_assignment" ra
        JOIN "role" r ON r."id" = ra."roleId"
        WHERE r."isAdministrator" = TRUE
          AND (ra."expiresAt" IS NULL OR ra."expiresAt" > now())
        ORDER BY 1
        "#,
    )
    .fetch_all(pool)
    .await
}

/// `RoleService.getModeratorIds({ includeAdmins })`.
pub async fn moderator_user_ids(
    pool: &PgPool,
    include_admins: bool,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        r#"
        SELECT DISTINCT ra."userId"
        FROM "role_assignment" ra
        JOIN "role" r ON r."id" = ra."roleId"
        WHERE (r."isModerator" = TRUE OR ($1 AND r."isAdministrator" = TRUE))
          AND (ra."expiresAt" IS NULL OR ra."expiresAt" > now())
        ORDER BY 1
        "#,
    )
    .bind(include_admins)
    .fetch_all(pool)
    .await
}

/// `RoleService.getUserAssigns(user.id)` — raw assignment rows;
/// admin/show-user maps `createdAt` out of the AID id and keeps `expiresAt`.
#[derive(Debug, sqlx::FromRow)]
pub struct RoleAssignRow {
    pub id: String,
    #[sqlx(rename = "roleId")]
    pub role_id: String,
    #[sqlx(rename = "expiresAtMs")]
    pub expires_at_ms: Option<i64>,
}

pub async fn user_role_assigns(
    pool: &PgPool,
    user_id: &str,
) -> Result<Vec<RoleAssignRow>, sqlx::Error> {
    sqlx::query_as::<_, RoleAssignRow>(
        r#"
        SELECT "id",
               "roleId",
               CASE
                 WHEN "expiresAt" IS NULL THEN NULL
                 ELSE (EXTRACT(EPOCH FROM "expiresAt") * 1000)::bigint
               END AS "expiresAtMs"
        FROM "role_assignment"
        WHERE "userId" = $1
        "#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// `endpoints/admin/show-users.ts` query matrix, parameterized like
/// `user::users_explore`. `role_ids` carries the precomputed
/// administrator/moderator id set for the role-based states (None = no role
/// filter); the handler short-circuits to `[]` when that set is empty.
pub struct AdminUserListQuery<'a> {
    pub sort: Option<&'a str>,
    pub state: Option<&'a str>,
    pub origin: Option<&'a str>,
    /// Bound as NULL (no filter) or the already-escaped `prefix%` pattern.
    pub username_pattern: Option<String>,
    pub hostname: Option<&'a str>,
    pub role_ids: Option<Vec<String>>,
    pub limit: i64,
    pub offset: i64,
}

fn sql_like_escape(q: &str) -> String {
    // misc/sql-like-escape.ts (a private mirror of the helper in user.rs,
    // which belongs to another agent's hunk)
    q.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// `sqlLikeEscape(username.toLowerCase()) + '%'`, or None without a username.
pub fn username_prefix_pattern(username: Option<&str>) -> Option<String> {
    username.map(|u| format!("{}%", sql_like_escape(&u.to_lowercase())))
}

pub async fn list_users_for_admin(
    pool: &PgPool,
    q: &AdminUserListQuery<'_>,
) -> Result<Vec<LocalUserRow>, sqlx::Error> {
    // Sort switch is verbatim from upstream (incl. the NULLS ordering of
    // updatedAt / lastActiveDate); `id` DES/ASC stands in for createdAt± and
    // the default ASC id order.
    let (col, dir, nulls) = match q.sort {
        Some("+follower") => ("followersCount", "DESC", ""),
        Some("-follower") => ("followersCount", "ASC", ""),
        Some("+createdAt") => ("id", "DESC", ""),
        Some("-createdAt") => ("id", "ASC", ""),
        Some("+updatedAt") => ("updatedAt", "DESC", " NULLS LAST"),
        Some("-updatedAt") => ("updatedAt", "ASC", " NULLS FIRST"),
        Some("+lastActiveDate") => ("lastActiveDate", "DESC", " NULLS LAST"),
        Some("-lastActiveDate") => ("lastActiveDate", "ASC", " NULLS FIRST"),
        _ => ("id", "ASC", ""),
    };

    let mut sql = format!(
        r#"SELECT {LOCAL_USER_COLS} FROM "user"
           WHERE ($1::varchar[] IS NULL OR "id" = ANY($1::varchar[]))
             AND ($2::text IS NULL OR "usernameLower" LIKE $2)
             AND ($3::text IS NULL OR "host" = lower($3))"#,
        LOCAL_USER_COLS = LOCAL_USER_COLS
    );
    sql.push_str(match q.state {
        Some("available") => r#" AND "isSuspended" = FALSE"#,
        Some("alive") => r#" AND "updatedAt" > now() - interval '5 days'"#,
        Some("suspended") => r#" AND "isSuspended" = TRUE"#,
        _ => "",
    });
    sql.push_str(match q.origin {
        Some("local") => r#" AND "host" IS NULL"#,
        Some("remote") => r#" AND "host" IS NOT NULL"#,
        _ => "",
    });
    sql.push_str(&format!(
        r#" ORDER BY "{col}" {dir}{nulls} LIMIT {limit} OFFSET {offset}"#,
        col = col,
        dir = dir,
        nulls = nulls,
        limit = q.limit,
        offset = q.offset
    ));

    sqlx::query_as::<_, LocalUserRow>(&sql)
        .bind(q.role_ids.clone())
        .bind(q.username_pattern.clone())
        .bind(q.hostname)
        .fetch_all(pool)
        .await
}

/// `user_profile."moderationNote"` (admin/show-user.ts `moderationNote ?? ''`).
/// Separated from `user::ProfileRow` because user.rs is another agent's hunk.
pub async fn moderation_note(pool: &PgPool, user_id: &str) -> Result<Option<String>, sqlx::Error> {
    let v: Option<Option<String>> =
        sqlx::query_scalar(r#"SELECT "moderationNote" FROM "user_profile" WHERE "userId" = $1"#)
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
    Ok(v.flatten())
}

/// ("user"."lastActiveDate" as epoch-ms, "isHibernated") for the
/// admin/show-user response (the M1 LocalUserRow carries neither).
pub async fn user_moderation_columns(
    pool: &PgPool,
    user_id: &str,
) -> Result<Option<(Option<i64>, bool)>, sqlx::Error> {
    let row: Option<(Option<i64>, bool)> = sqlx::query_as(
        r#"
        SELECT CASE
                 WHEN "lastActiveDate" IS NULL THEN NULL
                 ELSE (EXTRACT(EPOCH FROM "lastActiveDate") * 1000)::bigint
               END,
               "isHibernated"
        FROM "user" WHERE "id" = $1
        "#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// `signinsRepository.findBy({ userId })` (Signin ref items; most recent
/// first — upstream issues no ORDER BY, which leaves the order undefined).
#[derive(Debug, sqlx::FromRow)]
pub struct SigninRow {
    pub id: String,
    pub ip: String,
    pub headers: serde_json::Value,
    pub success: bool,
}

pub async fn user_signins(pool: &PgPool, user_id: &str) -> Result<Vec<SigninRow>, sqlx::Error> {
    sqlx::query_as::<_, SigninRow>(
        r#"SELECT "id", "ip", "headers", "success" FROM "signin" WHERE "userId" = $1 ORDER BY "id" DESC"#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// One meta row as a JSON object — `SELECT to_jsonb(m)`: every `meta`
/// column with its schema (camelCase) name, for the admin/meta facade
/// composition (MetaService.fetch(true); a fresh instance has no row → None
/// and the handler applies the schema/defaults).
pub async fn fetch_meta_row_json(pool: &PgPool) -> Result<Option<serde_json::Value>, sqlx::Error> {
    let v: Option<serde_json::Value> =
        sqlx::query_scalar(r#"SELECT to_jsonb(m) FROM "meta" m LIMIT 1"#)
            .fetch_optional(pool)
            .await?;
    Ok(v)
}

/// Honest M2-row counts for the stats facades. `local_only` = host IS NULL /
/// userHost IS NULL (the `original*` variants).
pub async fn count_users(pool: &PgPool, local_only: bool) -> Result<i64, sqlx::Error> {
    let cond = if local_only {
        r#" WHERE "host" IS NULL"#
    } else {
        ""
    };
    let v: i64 = sqlx::query_scalar(&format!(r#"SELECT count(*) FROM "user"{cond}"#))
        .fetch_one(pool)
        .await?;
    Ok(v)
}

pub async fn count_notes(pool: &PgPool, local_only: bool) -> Result<i64, sqlx::Error> {
    let cond = if local_only {
        r#" WHERE "userHost" IS NULL"#
    } else {
        ""
    };
    let v: i64 = sqlx::query_scalar(&format!(r#"SELECT count(*) FROM "note"{cond}"#))
        .fetch_one(pool)
        .await?;
    Ok(v)
}

pub async fn count_instances(pool: &PgPool) -> Result<i64, sqlx::Error> {
    let v: i64 = sqlx::query_scalar(r#"SELECT count(*) FROM "instance""#)
        .fetch_one(pool)
        .await?;
    Ok(v)
}

/// `db.query('SHOW server_version')` for admin/server-info's `psql` field.
pub async fn pg_server_version(pool: &PgPool) -> Result<String, sqlx::Error> {
    let v: String = sqlx::query_scalar("SHOW server_version")
        .fetch_one(pool)
        .await?;
    Ok(v)
}

// --- admin/ad/* (upstream endpoints/admin/ad/{create,list,update,delete}.ts) ---

/// One `ad` row with timestamptz columns normalized to epoch ms (the
/// `admin.rs` RoleAssignRow convention; `ms_to_iso` renders the response).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AdRow {
    pub id: String,
    pub url: String,
    #[sqlx(rename = "imageUrl")]
    pub image_url: String,
    pub place: String,
    pub priority: String,
    pub ratio: i32,
    pub memo: String,
    #[sqlx(rename = "isSensitive")]
    pub is_sensitive: bool,
    #[sqlx(rename = "dayOfWeek")]
    pub day_of_week: i32,
    #[sqlx(rename = "expiresAtMs")]
    pub expires_at_ms: i64,
    #[sqlx(rename = "startsAtMs")]
    pub starts_at_ms: i64,
}

const AD_COLUMNS: &str = r#"
    "id" AS "id",
    "url" AS "url",
    "imageUrl" AS "imageUrl",
    "place" AS "place",
    "priority" AS "priority",
    "ratio"::int AS "ratio",
    "memo" AS "memo",
    "isSensitive" AS "isSensitive",
    "dayOfWeek"::int AS "dayOfWeek",
    (EXTRACT(EPOCH FROM "expiresAt") * 1000)::bigint AS "expiresAtMs",
    (EXTRACT(EPOCH FROM "startsAt") * 1000)::bigint AS "startsAtMs"
"#;

/// `admin/ad/create.ts` insert (id from IdService, all columns required by
/// the paramDef). `expiresAt`/`startsAt` arrive as epoch ms.
pub struct NewAd {
    pub id: String,
    pub url: String,
    pub image_url: String,
    pub place: String,
    pub priority: String,
    pub ratio: i32,
    pub memo: String,
    pub is_sensitive: bool,
    pub day_of_week: i32,
    pub expires_at_ms: i64,
    pub starts_at_ms: i64,
}

pub async fn insert_ad(pool: &PgPool, ad: &NewAd) -> Result<AdRow, sqlx::Error> {
    let sql = format!(
        r#"INSERT INTO "ad" ("id", "expiresAt", "place", "priority", "url",
                             "imageUrl", "memo", "ratio", "startsAt", "dayOfWeek",
                             "isSensitive")
           VALUES ($1, to_timestamp($2::double precision / 1000), $3, $4, $5,
                   $6, $7, $8, to_timestamp($9::double precision / 1000), $10,
                   $11)
           RETURNING {AD_COLUMNS}"#
    );
    sqlx::query_as::<_, AdRow>(&sql)
        .bind(&ad.id)
        .bind(ad.expires_at_ms)
        .bind(&ad.place)
        .bind(&ad.priority)
        .bind(&ad.url)
        .bind(&ad.image_url)
        .bind(&ad.memo)
        .bind(ad.ratio)
        .bind(ad.starts_at_ms)
        .bind(ad.day_of_week)
        .bind(ad.is_sensitive)
        .fetch_one(pool)
        .await
}

pub async fn find_ad(pool: &PgPool, id: &str) -> Result<Option<AdRow>, sqlx::Error> {
    let sql = format!(r#"SELECT {AD_COLUMNS} FROM "ad" WHERE "id" = $1"#);
    sqlx::query_as::<_, AdRow>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// `admin/ad/list.ts` query inputs (`makePaginationQuery` ids + the nullable
/// `publishing` filter).
pub struct AdListQuery<'a> {
    pub since_id: Option<&'a str>,
    pub until_id: Option<&'a str>,
    pub publishing: Option<bool>,
    pub limit: i64,
}

/// `admin/ad/list.ts`: id pagination (`sinceId` only → ASC, else DESC) and
/// the publishing window. The false branch keeps TypeORM's
/// `.andWhere(A).orWhere(B)` precedence — the pagination predicates bind to
/// the A branch only (upstream parity).
pub async fn list_ads(pool: &PgPool, q: &AdListQuery<'_>) -> Result<Vec<AdRow>, sqlx::Error> {
    let mut qb = sqlx::QueryBuilder::new(format!(r#"SELECT {AD_COLUMNS} FROM "ad" WHERE TRUE"#));
    if let Some(since) = q.since_id {
        qb.push(r#" AND "id" > "#).push_bind(since);
    }
    if let Some(until) = q.until_id {
        qb.push(r#" AND "id" < "#).push_bind(until);
    }
    match q.publishing {
        Some(true) => {
            qb.push(r#" AND "expiresAt" > now() AND "startsAt" <= now()"#);
        }
        Some(false) => {
            qb.push(r#" AND "expiresAt" <= now() OR "startsAt" > now()"#);
        }
        None => {}
    }
    if q.since_id.is_some() && q.until_id.is_none() {
        qb.push(r#" ORDER BY "id" ASC LIMIT "#);
    } else {
        qb.push(r#" ORDER BY "id" DESC LIMIT "#);
    }
    qb.push_bind(q.limit);
    qb.build_query_as::<AdRow>().fetch_all(pool).await
}

/// Partial `AdService.update` write (absent fields keep their column value).
#[derive(Default)]
pub struct AdUpdate {
    pub url: Option<String>,
    pub image_url: Option<String>,
    pub place: Option<String>,
    pub priority: Option<String>,
    pub ratio: Option<i32>,
    pub memo: Option<String>,
    pub expires_at_ms: Option<i64>,
    pub starts_at_ms: Option<i64>,
    pub day_of_week: Option<i32>,
    pub is_sensitive: Option<bool>,
}

impl AdUpdate {
    pub fn is_empty(&self) -> bool {
        self.url.is_none()
            && self.image_url.is_none()
            && self.place.is_none()
            && self.priority.is_none()
            && self.ratio.is_none()
            && self.memo.is_none()
            && self.expires_at_ms.is_none()
            && self.starts_at_ms.is_none()
            && self.day_of_week.is_none()
            && self.is_sensitive.is_none()
    }
}

pub async fn update_ad(pool: &PgPool, id: &str, upd: &AdUpdate) -> Result<(), sqlx::Error> {
    if upd.is_empty() {
        // TypeORM `repository.update(id, {})` — nothing to write.
        return Ok(());
    }
    let mut qb = sqlx::QueryBuilder::new(r#"UPDATE "ad" SET "#);
    {
        let mut sep = qb.separated(", ");
        if let Some(v) = &upd.url {
            sep.push(r#""url" = "#).push_bind_unseparated(v);
        }
        if let Some(v) = &upd.image_url {
            sep.push(r#""imageUrl" = "#).push_bind_unseparated(v);
        }
        if let Some(v) = &upd.place {
            sep.push(r#""place" = "#).push_bind_unseparated(v);
        }
        if let Some(v) = &upd.priority {
            sep.push(r#""priority" = "#).push_bind_unseparated(v);
        }
        if let Some(v) = upd.ratio {
            sep.push(r#""ratio" = "#).push_bind_unseparated(v);
        }
        if let Some(v) = &upd.memo {
            sep.push(r#""memo" = "#).push_bind_unseparated(v);
        }
        if let Some(ms) = upd.expires_at_ms {
            sep.push(r#""expiresAt" = to_timestamp("#);
            sep.push_bind_unseparated(ms);
            sep.push_unseparated("::double precision / 1000)");
        }
        if let Some(ms) = upd.starts_at_ms {
            sep.push(r#""startsAt" = to_timestamp("#);
            sep.push_bind_unseparated(ms);
            sep.push_unseparated("::double precision / 1000)");
        }
        if let Some(v) = upd.day_of_week {
            sep.push(r#""dayOfWeek" = "#).push_bind_unseparated(v);
        }
        if let Some(v) = upd.is_sensitive {
            sep.push(r#""isSensitive" = "#).push_bind_unseparated(v);
        }
    }
    qb.push(r#" WHERE "id" = "#).push_bind(id);
    qb.build().execute(pool).await?;
    Ok(())
}

pub async fn delete_ad(pool: &PgPool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "ad" WHERE "id" = $1"#)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// `admin/accounts/find-by-email.ts`: `userProfilesRepository.findOneBy({
/// email })` — the email lives on user_profile, so the lookup returns the
/// owning user id (the endpoint then loads the user row).
pub async fn find_user_id_by_email(
    pool: &PgPool,
    email: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(r#"SELECT "userId" FROM "user_profile" WHERE "email" = $1 LIMIT 1"#)
        .bind(email)
        .fetch_optional(pool)
        .await
}
