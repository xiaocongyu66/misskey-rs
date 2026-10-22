//! federation/* domain — db paths for the pinned upstream
//! `endpoints/federation/{instances,show-instance,stats,followers,following,
//! users}.ts`.
//!
//! Timestamps ride the workspace convention (`timestamptz` → epoch ms via
//! `EXTRACT(EPOCH ...)`); the endpoint layer turns them into ISO strings with
//! `mk_core::services::signup::ms_to_iso`. The `instance."suspensionState"`
//! enum is read through `::text` (sqlx carries no enum decoder, same as
//! `user_profile`'s visibility enums).
//!
//! `federation/update-remote-user` is intentionally absent: it drives
//! `ApPersonService.updatePerson` (an AP fetch lane) and has no db path.

use sqlx::{PgPool, Postgres, QueryBuilder};

use crate::ff_visibility::FollowingEdge;

const INSTANCE_COLS: &str = r#"
    "id",
    (EXTRACT(EPOCH FROM "firstRetrievedAt") * 1000)::bigint AS "firstRetrievedAtMs",
    "host",
    "usersCount",
    "notesCount",
    "followingCount",
    "followersCount",
    (CASE WHEN "latestRequestReceivedAt" IS NULL THEN NULL
          ELSE (EXTRACT(EPOCH FROM "latestRequestReceivedAt") * 1000)::bigint END)
        AS "latestRequestReceivedAtMs",
    "isNotResponding",
    "softwareName",
    "softwareVersion",
    "openRegistrations",
    "name",
    "description",
    "maintainerName",
    "maintainerEmail",
    (CASE WHEN "infoUpdatedAt" IS NULL THEN NULL
          ELSE (EXTRACT(EPOCH FROM "infoUpdatedAt") * 1000)::bigint END)
        AS "infoUpdatedAtMs",
    ("suspensionState"::text) AS "suspensionState",
    "iconUrl",
    "themeColor",
    "faviconUrl",
    "moderationNote"
"#;

/// One `instance` row (upstream `MiInstance`).
#[derive(Debug, sqlx::FromRow)]
pub struct InstanceRow {
    pub id: String,
    #[sqlx(rename = "firstRetrievedAtMs")]
    pub first_retrieved_at_ms: i64,
    pub host: String,
    #[sqlx(rename = "usersCount")]
    pub users_count: i32,
    #[sqlx(rename = "notesCount")]
    pub notes_count: i32,
    #[sqlx(rename = "followingCount")]
    pub following_count: i32,
    #[sqlx(rename = "followersCount")]
    pub followers_count: i32,
    #[sqlx(rename = "latestRequestReceivedAtMs")]
    pub latest_request_received_at_ms: Option<i64>,
    #[sqlx(rename = "isNotResponding")]
    pub is_not_responding: bool,
    #[sqlx(rename = "softwareName")]
    pub software_name: Option<String>,
    #[sqlx(rename = "softwareVersion")]
    pub software_version: Option<String>,
    #[sqlx(rename = "openRegistrations")]
    pub open_registrations: Option<bool>,
    pub name: Option<String>,
    pub description: Option<String>,
    #[sqlx(rename = "maintainerName")]
    pub maintainer_name: Option<String>,
    #[sqlx(rename = "maintainerEmail")]
    pub maintainer_email: Option<String>,
    #[sqlx(rename = "infoUpdatedAtMs")]
    pub info_updated_at_ms: Option<i64>,
    #[sqlx(rename = "suspensionState")]
    pub suspension_state: String,
    #[sqlx(rename = "iconUrl")]
    pub icon_url: Option<String>,
    #[sqlx(rename = "themeColor")]
    pub theme_color: Option<String>,
    #[sqlx(rename = "faviconUrl")]
    pub favicon_url: Option<String>,
    #[sqlx(rename = "moderationNote")]
    pub moderation_note: String,
}

/// The `meta` host lists + software-suspension config the instance pack and
/// the `blocked`/`silenced` filters consume. Missing meta row → empty lists.
#[derive(Debug, Default)]
pub struct MetaHostLists {
    pub blocked_hosts: Vec<String>,
    pub silenced_hosts: Vec<String>,
    pub media_silenced_hosts: Vec<String>,
    /// `deliverSuspendedSoftware` jsonb ([{ software, versionRange }]).
    pub deliver_suspended_software: serde_json::Value,
}

/// MetaService.fetch equivalent for the columns this lane reads.
pub async fn meta_host_lists(pool: &PgPool) -> Result<MetaHostLists, sqlx::Error> {
    let row = sqlx::query_as::<_, (Vec<String>, Vec<String>, Vec<String>, serde_json::Value)>(
        r#"
        SELECT "blockedHosts", "silencedHosts", "mediaSilencedHosts",
               "deliverSuspendedSoftware"
        FROM "meta" ORDER BY "id" DESC LIMIT 1
        "#,
    )
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        Some((blocked, silenced, media_silenced, suspended)) => MetaHostLists {
            blocked_hosts: blocked,
            silenced_hosts: silenced,
            media_silenced_hosts: media_silenced,
            deliver_suspended_software: suspended,
        },
        None => MetaHostLists {
            deliver_suspended_software: serde_json::json!([]),
            ..Default::default()
        },
    })
}

/// `ORDER BY` for the `federation/instances` sort enum (instances.ts switch).
fn sort_clause(sort: Option<&str>) -> &'static str {
    match sort {
        Some("+pubSub") => r#""followingCount" DESC, "followersCount" DESC"#,
        Some("-pubSub") => r#""followingCount" ASC, "followersCount" ASC"#,
        Some("+notes") => r#""notesCount" DESC"#,
        Some("-notes") => r#""notesCount" ASC"#,
        Some("+users") => r#""usersCount" DESC"#,
        Some("-users") => r#""usersCount" ASC"#,
        Some("+following") => r#""followingCount" DESC"#,
        Some("-following") => r#""followingCount" ASC"#,
        Some("+followers") => r#""followersCount" DESC"#,
        Some("-followers") => r#""followersCount" ASC"#,
        Some("+firstRetrievedAt") => r#""firstRetrievedAt" DESC"#,
        Some("-firstRetrievedAt") => r#""firstRetrievedAt" ASC"#,
        Some("+latestRequestReceivedAt") => r#""latestRequestReceivedAt" DESC NULLS LAST"#,
        Some("-latestRequestReceivedAt") => r#""latestRequestReceivedAt" ASC NULLS FIRST"#,
        _ => r#""id" DESC"#,
    }
}

/// `federation/instances` filters (instances.ts L82-160).
pub struct InstanceListQuery<'a> {
    /// Already LIKE-escaped + lowercased, `%...%` wrapped (None/empty = no filter).
    pub host_like: Option<&'a str>,
    pub blocked: Option<bool>,
    pub blocked_hosts: &'a [String],
    pub not_responding: Option<bool>,
    pub suspended: Option<bool>,
    pub silenced: Option<bool>,
    pub silenced_hosts: &'a [String],
    pub federating: Option<bool>,
    pub subscribing: Option<bool>,
    pub publishing: Option<bool>,
    pub sort: Option<&'a str>,
    pub limit: i64,
    pub offset: i64,
}

/// `InstancesRepository.createQueryBuilder` + the instances.ts predicate
/// chain, paginated by limit/offset.
pub async fn list_instances(
    pool: &PgPool,
    q: &InstanceListQuery<'_>,
) -> Result<Vec<InstanceRow>, sqlx::Error> {
    // silenced=true against an empty meta list short-circuits to [] upstream.
    if q.silenced == Some(true) && q.silenced_hosts.is_empty() {
        return Ok(Vec::new());
    }

    let mut qb = QueryBuilder::<Postgres>::new(format!(
        r#"SELECT {INSTANCE_COLS} FROM "instance" WHERE TRUE"#
    ));

    if let Some(pattern) = q.host_like {
        qb.push(r#" AND "host" LIKE "#)
            .push_bind(pattern.to_string());
    }

    match q.blocked {
        Some(true) if q.blocked_hosts.is_empty() => {
            qb.push(" AND 1=0");
        }
        Some(true) => {
            qb.push(r#" AND "host" = ANY("#)
                .push_bind(q.blocked_hosts.to_vec())
                .push(")");
        }
        Some(false) if q.blocked_hosts.is_empty() => {
            qb.push(" AND 1=1");
        }
        Some(false) => {
            qb.push(r#" AND NOT ("host" = ANY("#)
                .push_bind(q.blocked_hosts.to_vec())
                .push("))");
        }
        None => {}
    }

    if let Some(v) = q.not_responding {
        qb.push(r#" AND "isNotResponding" = "#).push_bind(v);
    }

    match q.suspended {
        Some(true) => {
            qb.push(r#" AND "suspensionState" != 'none'"#);
        }
        Some(false) => {
            qb.push(r#" AND "suspensionState" = 'none'"#);
        }
        None => {}
    }

    match q.silenced {
        Some(true) => {
            qb.push(r#" AND "host" = ANY("#)
                .push_bind(q.silenced_hosts.to_vec())
                .push(")");
        }
        Some(false) if !q.silenced_hosts.is_empty() => {
            qb.push(r#" AND NOT ("host" = ANY("#)
                .push_bind(q.silenced_hosts.to_vec())
                .push("))");
        }
        _ => {}
    }

    if let Some(v) = q.federating {
        if v {
            qb.push(r#" AND (("followingCount" > 0) OR ("followersCount" > 0))"#);
        } else {
            qb.push(r#" AND (("followingCount" = 0) AND ("followersCount" = 0))"#);
        }
    }
    if let Some(v) = q.subscribing {
        qb.push(if v {
            r#" AND "followersCount" > 0"#
        } else {
            r#" AND "followersCount" = 0"#
        });
    }
    if let Some(v) = q.publishing {
        qb.push(if v {
            r#" AND "followingCount" > 0"#
        } else {
            r#" AND "followingCount" = 0"#
        });
    }

    qb.push(format!(" ORDER BY {}", sort_clause(q.sort)));
    qb.push(" LIMIT ").push_bind(q.limit);
    qb.push(" OFFSET ").push_bind(q.offset);

    qb.build_query_as::<InstanceRow>().fetch_all(pool).await
}

/// `federation/show-instance`: `findOneBy({ host })` (host already puny-folded
/// by the endpoint layer).
pub async fn find_instance_by_host(
    pool: &PgPool,
    host: &str,
) -> Result<Option<InstanceRow>, sqlx::Error> {
    sqlx::query_as::<_, InstanceRow>(&format!(
        r#"SELECT {INSTANCE_COLS} FROM "instance" WHERE "host" = $1"#
    ))
    .bind(host)
    .fetch_optional(pool)
    .await
}

/// `federation/stats` read side.
pub struct FederationStats {
    pub top_sub: Vec<InstanceRow>,
    pub top_pub: Vec<InstanceRow>,
    /// `followingsRepository.count({ followeeHost: Not(IsNull()) })`.
    pub all_sub_count: i64,
    /// `followingsRepository.count({ followerHost: Not(IsNull()) })`.
    pub all_pub_count: i64,
}

/// stats.ts: top instances by followers/following + the remote-edge totals.
pub async fn stats(pool: &PgPool, limit: i64) -> Result<FederationStats, sqlx::Error> {
    let top_sub = sqlx::query_as::<_, InstanceRow>(&format!(
        r#"SELECT {INSTANCE_COLS} FROM "instance"
           WHERE "followersCount" > 0
           ORDER BY "followersCount" DESC LIMIT $1"#
    ))
    .bind(limit)
    .fetch_all(pool)
    .await?;
    let top_pub = sqlx::query_as::<_, InstanceRow>(&format!(
        r#"SELECT {INSTANCE_COLS} FROM "instance"
           WHERE "followingCount" > 0
           ORDER BY "followingCount" DESC LIMIT $1"#
    ))
    .bind(limit)
    .fetch_all(pool)
    .await?;
    let all_sub_count: i64 =
        sqlx::query_scalar(r#"SELECT count(*) FROM "following" WHERE "followeeHost" IS NOT NULL"#)
            .fetch_one(pool)
            .await?;
    let all_pub_count: i64 =
        sqlx::query_scalar(r#"SELECT count(*) FROM "following" WHERE "followerHost" IS NOT NULL"#)
            .fetch_one(pool)
            .await?;
    Ok(FederationStats {
        top_sub,
        top_pub,
        all_sub_count,
        all_pub_count,
    })
}

/// `federation/{followers,following}`: following rows whose denormalized
/// `followeeHost` / `followerHost` matches, with the
/// `QueryService.generateFollowingRelationVisibilityQuery` gate for
/// non-moderators (owner profile public, self, or followers-only + me follows
/// the owner). `followers=true` = followers endpoint (owner = followeeId).
#[allow(clippy::too_many_arguments)]
pub async fn list_edges_by_host(
    pool: &PgPool,
    followers: bool,
    host: &str,
    from_id: Option<&str>,
    until_id: Option<&str>,
    desc: bool,
    limit: u32,
    me_id: Option<&str>,
    moderator: bool,
) -> Result<Vec<FollowingEdge>, sqlx::Error> {
    let host_col = if followers {
        "followeeHost"
    } else {
        "followerHost"
    };
    let owner_col = if followers {
        "followeeId"
    } else {
        "followerId"
    };
    let vis_col = if followers {
        "followersVisibility"
    } else {
        "followingVisibility"
    };
    let order = if desc { "DESC" } else { "ASC" };

    let mut sql = format!(
        r#"
        SELECT f."id", f."followerId", f."followeeId"
        FROM "following" f
        INNER JOIN "user_profile" ownerProfile
            ON ownerProfile."userId" = f."{owner_col}"
        WHERE f."{host_col}" = $1
          AND ($2::varchar IS NULL OR f."id" > $2)
          AND ($3::varchar IS NULL OR f."id" < $3)
        "#
    );
    if !moderator {
        match me_id {
            None => sql.push_str(&format!(r#" AND ownerProfile."{vis_col}" = 'public'"#)),
            Some(_) => sql.push_str(&format!(
                r#" AND (ownerProfile."{vis_col}" = 'public'
                         OR f."{owner_col}" = $5
                         OR (ownerProfile."{vis_col}" = 'followers'
                             AND f."{owner_col}" IN (
                                 SELECT "followeeId" FROM "following"
                                 WHERE "followerId" = $5)))"#
            )),
        }
    }
    sql.push_str(&format!(r#" ORDER BY f."id" {order} LIMIT $4"#));

    let mut q = sqlx::query_as::<_, FollowingEdge>(&sql)
        .bind(host)
        .bind(from_id)
        .bind(until_id)
        .bind(i64::from(limit));
    if !moderator && me_id.is_some() {
        q = q.bind(me_id);
    }
    q.fetch_all(pool).await
}

/// `federation/users`: user ids with `host = :host`, paginated by the same
/// makePaginationQuery bounds.
pub async fn list_user_ids_by_host(
    pool: &PgPool,
    host: &str,
    from_id: Option<&str>,
    until_id: Option<&str>,
    desc: bool,
    limit: u32,
) -> Result<Vec<String>, sqlx::Error> {
    let order = if desc { "DESC" } else { "ASC" };
    sqlx::query_scalar(&format!(
        r#"
        SELECT "id" FROM "user"
        WHERE "host" = $1
          AND ($2::varchar IS NULL OR "id" > $2)
          AND ($3::varchar IS NULL OR "id" < $3)
        ORDER BY "id" {order}
        LIMIT $4
        "#
    ))
    .bind(host)
    .bind(from_id)
    .bind(until_id)
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await
}
