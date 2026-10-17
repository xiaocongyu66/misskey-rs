//! User/account SQL. Mirrors `SignupService.signup`'s transaction: one step
//! inserts into `user`, `user_keypair`, `user_profile`, `used_username`.

use sqlx::PgPool;

pub struct NewUser<'a> {
    pub id: &'a str,
    pub username: &'a str,
    pub username_lower: &'a str,
    pub name: Option<&'a str>,
    pub token: &'a str,
    pub password_hash: &'a str,
    pub public_key: &'a str,
    pub private_key: &'a str,
}

pub async fn insert_new_user(pool: &PgPool, u: &NewUser<'_>) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        r#"
        INSERT INTO "user" ("id", "username", "usernameLower", "name", "host", "token")
        VALUES ($1, $2, $3, $4, NULL, $5)
        "#,
    )
    .bind(u.id)
    .bind(u.username)
    .bind(u.username_lower)
    .bind(u.name)
    .bind(u.token)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        r#"
        INSERT INTO "user_keypair" ("userId", "publicKey", "privateKey")
        VALUES ($1, $2, $3)
        "#,
    )
    .bind(u.id)
    .bind(u.public_key)
    .bind(u.private_key)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        r#"
        INSERT INTO "user_profile" ("userId", "password", "autoAcceptFollowed", "userHost")
        VALUES ($1, $2, TRUE, NULL)
        "#,
    )
    .bind(u.id)
    .bind(u.password_hash)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        r#"
        INSERT INTO "used_username" ("username", "createdAt")
        VALUES ($1, now())
        "#,
    )
    .bind(u.username_lower)
    .execute(&mut *tx)
    .await?;

    tx.commit().await
}

pub async fn username_exists_local(
    pool: &PgPool,
    username_lower: &str,
) -> Result<bool, sqlx::Error> {
    let exists: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(SELECT 1 FROM "user" WHERE "usernameLower" = $1 AND "host" IS NULL)"#,
    )
    .bind(username_lower)
    .fetch_one(pool)
    .await?;
    Ok(exists)
}

pub async fn used_username_exists(
    pool: &PgPool,
    username_lower: &str,
) -> Result<bool, sqlx::Error> {
    let exists: bool =
        sqlx::query_scalar(r#"SELECT EXISTS(SELECT 1 FROM "used_username" WHERE "username" = $1)"#)
            .bind(username_lower)
            .fetch_one(pool)
            .await?;
    Ok(exists)
}

pub async fn find_local_user_id_by_token(
    pool: &PgPool,
    token: &str,
) -> Result<Option<String>, sqlx::Error> {
    let id: Option<String> =
        sqlx::query_scalar(r#"SELECT "id" FROM "user" WHERE "token" = $1 AND "host" IS NULL"#)
            .bind(token)
            .fetch_optional(pool)
            .await?;
    Ok(id)
}

pub async fn touch_token_last_used(pool: &PgPool, token: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "user" SET "updatedAt" = now() WHERE "token" = $1"#)
        .bind(token)
        .execute(pool)
        .await?;
    Ok(())
}

/// endpoints/i.ts: `loggedInDates` gains today's date (`y/m/d`, once per day)
/// before the profile is packed.
pub async fn append_logged_in_date(
    pool: &PgPool,
    user_id: &str,
    today: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"UPDATE "user_profile" SET "loggedInDates" = array_append("loggedInDates", $1::varchar) WHERE "userId" = $2"#,
    )
    .bind(today)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// e2e harness reset: truncate every public table (incl. identities) but
/// keep the schema (and sqlx's migration bookkeeping state semantics: the
/// bookkeeping table is truncated too — migrations are already applied).
pub async fn truncate_all(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        DO $$ DECLARE r RECORD;
        BEGIN
            FOR r IN (SELECT tablename FROM pg_tables WHERE schemaname = 'public') LOOP
                EXECUTE 'TRUNCATE TABLE public.' || quote_ident(r.tablename) || ' RESTART IDENTITY CASCADE';
            END LOOP;
        END $$;
        "#,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Minimal `user` row for auth/pack flows.
#[derive(Debug, sqlx::FromRow)]
pub struct LocalUserRow {
    pub id: String,
    pub username: String,
    #[sqlx(rename = "usernameLower")]
    pub username_lower: String,
    pub name: Option<String>,
    pub token: String,
    #[sqlx(rename = "isSuspended")]
    pub is_suspended: bool,
    #[sqlx(rename = "isLocked")]
    pub is_locked: bool,
    #[sqlx(rename = "isExplorable")]
    pub is_explorable: bool,
    #[sqlx(rename = "hideOnlineStatus")]
    pub hide_online_status: bool,
    #[sqlx(rename = "chatScope")]
    pub chat_scope: String,
    #[sqlx(rename = "avatarUrl")]
    pub avatar_url: Option<String>,
    #[sqlx(rename = "avatarBlurhash")]
    pub avatar_blurhash: Option<String>,
    #[sqlx(rename = "avatarDecorations")]
    pub avatar_decorations: serde_json::Value,
    #[sqlx(rename = "bannerUrl")]
    pub banner_url: Option<String>,
    #[sqlx(rename = "bannerBlurhash")]
    pub banner_blurhash: Option<String>,
    #[sqlx(rename = "avatarId")]
    pub avatar_id: Option<String>,
    #[sqlx(rename = "bannerId")]
    pub banner_id: Option<String>,
}

pub const LOCAL_USER_COLS: &str = r#""id", "username", "usernameLower", "name", "token", "isSuspended", "isLocked", "isExplorable", "hideOnlineStatus", "chatScope", "avatarUrl", "avatarBlurhash", "avatarDecorations", "bannerUrl", "bannerBlurhash", "avatarId", "bannerId""#;

pub async fn find_local_user_by_username(
    pool: &PgPool,
    username_lower: &str,
) -> Result<Option<LocalUserRow>, sqlx::Error> {
    sqlx::query_as::<_, LocalUserRow>(&format!(
        r#"SELECT {} FROM "user" WHERE "usernameLower" = $1 AND "host" IS NULL"#,
        LOCAL_USER_COLS
    ))
    .bind(username_lower)
    .fetch_optional(pool)
    .await
}

pub async fn find_user_by_id(pool: &PgPool, id: &str) -> Result<Option<LocalUserRow>, sqlx::Error> {
    sqlx::query_as::<_, LocalUserRow>(&format!(
        r#"SELECT {} FROM "user" WHERE "id" = $1"#,
        LOCAL_USER_COLS
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
}

#[derive(Debug, sqlx::FromRow)]
pub struct ProfileRow {
    #[sqlx(rename = "userId")]
    pub user_id: String,
    pub location: Option<String>,
    pub birthday: Option<String>,
    pub description: Option<String>,
    pub lang: Option<String>,
    pub fields: serde_json::Value,
    pub url: Option<String>,
    pub email: Option<String>,
    #[sqlx(rename = "emailVerified")]
    pub email_verified: bool,
    #[sqlx(rename = "twoFactorEnabled")]
    pub two_factor_enabled: bool,
    pub password: Option<String>,
    #[sqlx(rename = "autoAcceptFollowed")]
    pub auto_accept_followed: bool,
    #[sqlx(rename = "alwaysMarkNsfw")]
    pub always_mark_nsfw: bool,
    #[sqlx(rename = "carefulBot")]
    pub careful_bot: bool,
    #[sqlx(rename = "securityKeysAvailable")]
    pub security_keys_available: bool,
    #[sqlx(rename = "usePasswordLessLogin")]
    pub use_password_less_login: bool,
    #[sqlx(rename = "injectFeaturedNote")]
    pub inject_featured_note: bool,
    #[sqlx(rename = "receiveAnnouncementEmail")]
    pub receive_announcement_email: bool,
    #[sqlx(rename = "emailNotificationTypes")]
    pub email_notification_types: serde_json::Value,
    #[sqlx(rename = "publicReactions")]
    pub public_reactions: bool,
    #[sqlx(rename = "autoSensitive")]
    pub auto_sensitive: bool,
    pub achievements: serde_json::Value,
    #[sqlx(rename = "loggedInDates")]
    pub logged_in_dates: Vec<String>,
    #[sqlx(rename = "preventAiLearning")]
    pub prevent_ai_learning: bool,
    #[sqlx(rename = "noCrawle")]
    pub no_crawle: bool,
    #[sqlx(rename = "verifiedLinks")]
    pub verified_links: Vec<String>,
    #[sqlx(rename = "notificationRecieveConfig")]
    pub notification_recieve_config: serde_json::Value,
    #[sqlx(rename = "hardMutedWords")]
    pub hard_muted_words: serde_json::Value,
    #[sqlx(rename = "mutedWords")]
    pub muted_words: serde_json::Value,
    #[sqlx(rename = "mutedInstances")]
    pub muted_instances: serde_json::Value,
    #[sqlx(rename = "followingVisibility")]
    pub following_visibility: String,
    #[sqlx(rename = "followersVisibility")]
    pub followers_visibility: String,
    #[sqlx(rename = "followedMessage")]
    pub followed_message: Option<String>,
    #[sqlx(rename = "followApprovalLocalSeconds")]
    pub follow_approval_local_seconds: Option<i32>,
    #[sqlx(rename = "followApprovalRemoteSeconds")]
    pub follow_approval_remote_seconds: Option<i32>,
}

pub const PROFILE_COLS: &str = r#""userId", "location", "birthday", "description", "lang", "fields", "url", "email", "emailVerified", "twoFactorEnabled", "password", "autoAcceptFollowed", "alwaysMarkNsfw", "carefulBot", "securityKeysAvailable", "usePasswordLessLogin", "injectFeaturedNote", "receiveAnnouncementEmail", "emailNotificationTypes", "publicReactions", "autoSensitive", "achievements", "loggedInDates", "preventAiLearning", "noCrawle", "verifiedLinks", "notificationRecieveConfig", "hardMutedWords", "mutedWords", "mutedInstances", ("followingVisibility"::text) AS "followingVisibility", ("followersVisibility"::text) AS "followersVisibility", "followedMessage", "followApprovalLocalSeconds", "followApprovalRemoteSeconds""#;

pub struct ProfileUpdate {
    // "user" table columns
    pub name: Option<Option<String>>,
    pub is_locked: Option<bool>,
    pub is_explorable: Option<bool>,
    pub hide_online_status: Option<bool>,
    pub chat_scope: Option<String>,
    // "user_profile" table columns
    pub description: Option<Option<String>>,
    pub location: Option<Option<String>>,
    pub birthday: Option<Option<String>>,
    pub lang: Option<Option<String>>,
    pub url: Option<Option<String>>,
    /// jsonb; stored via `$n::jsonb` from its JSON text.
    pub fields: Option<serde_json::Value>,
    pub no_crawle: Option<bool>,
    pub prevent_ai_learning: Option<bool>,
    pub inject_featured_note: Option<bool>,
    pub receive_announcement_email: Option<bool>,
    pub always_mark_nsfw: Option<bool>,
    pub auto_sensitive: Option<bool>,
    pub careful_bot: Option<bool>,
    pub auto_accept_followed: Option<bool>,
    pub public_reactions: Option<bool>,
    /// PG enum `user_profile_followingvisibility_enum`; the plain string value
    /// is bound and PG casts it on UPDATE (column type is the enum).
    pub following_visibility: Option<String>,
    /// PG enum `user_profile_followersvisibility_enum`; bound as plain string.
    pub followers_visibility: Option<String>,
    // e2e-parity subset additions (users.ts を書き換える family)
    pub followed_message: Option<Option<String>>,
    pub muted_words: Option<serde_json::Value>,
    pub hard_muted_words: Option<serde_json::Value>,
    pub muted_instances: Option<serde_json::Value>,
    pub email_notification_types: Option<serde_json::Value>,
    pub notification_recieve_config: Option<serde_json::Value>,
}

/// One dynamically-built SET assignment value. Text/Json/Bool values become
/// bind parameters whose type PG infers from the assignment target; enum
/// values are used as inlined escaped literals (sqlx binds str params with
/// the TEXT type OID, which PG rejects against enum columns — 42804).
enum SetVal {
    Text(Option<String>),
    Bool(bool),
    JsonText(String),
}

pub async fn update_profile_subset(
    pool: &PgPool,
    user_id: &str,
    u: &ProfileUpdate,
) -> Result<(), sqlx::Error> {
    // --- "user" table: name / isLocked / isExplorable / hideOnlineStatus /
    // chatScope (upstream `updates`) ---
    let mut cols: Vec<&'static str> = Vec::new();
    let mut vals: Vec<SetVal> = Vec::new();
    if let Some(v) = &u.name {
        cols.push("\"name\"");
        vals.push(SetVal::Text(v.clone()));
    }
    if let Some(v) = u.is_locked {
        cols.push("\"isLocked\"");
        vals.push(SetVal::Bool(v));
    }
    if let Some(v) = u.is_explorable {
        cols.push("\"isExplorable\"");
        vals.push(SetVal::Bool(v));
    }
    if let Some(v) = u.hide_online_status {
        cols.push("\"hideOnlineStatus\"");
        vals.push(SetVal::Bool(v));
    }
    if let Some(v) = &u.chat_scope {
        cols.push("\"chatScope\"");
        vals.push(SetVal::Text(Some(v.clone())));
    }
    if !cols.is_empty() {
        let sets: Vec<String> = cols
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{} = ${}", c, i + 2))
            .collect();
        let sql = format!(r#"UPDATE "user" SET {} WHERE "id" = $1"#, sets.join(", "));
        let mut q = sqlx::query(&sql).bind(user_id);
        for v in &vals {
            q = match v {
                SetVal::Text(t) => q.bind(t.as_ref()),
                SetVal::Bool(b) => q.bind(*b),
                SetVal::JsonText(_) => unreachable!(),
            };
        }
        q.execute(pool).await?;
    }

    // --- "user_profile" table (upstream `profileUpdates`) ---
    // `userId` is $1; every bound value gets the next $n in push order. Enum
    // columns take inline literals (see SetVal docs).
    let mut sets: Vec<String> = Vec::new();
    let mut vals: Vec<SetVal> = Vec::new();
    macro_rules! push_set {
        ($col:expr, bound $val:expr) => {{
            vals.push($val);
            sets.push(format!("{} = ${}", $col, vals.len() + 1));
        }};
        ($col:expr, lit $val:expr) => {{
            sets.push(format!("{} = '{}'", $col, $val.replace('\'', "''")));
        }};
    }
    if let Some(v) = &u.description {
        push_set!("\"description\"", bound SetVal::Text(v.clone()));
    }
    if let Some(v) = &u.location {
        push_set!("\"location\"", bound SetVal::Text(v.clone()));
    }
    if let Some(v) = &u.birthday {
        push_set!("\"birthday\"", bound SetVal::Text(v.clone()));
    }
    if let Some(v) = &u.lang {
        push_set!("\"lang\"", bound SetVal::Text(v.clone()));
    }
    if let Some(v) = &u.url {
        push_set!("\"url\"", bound SetVal::Text(v.clone()));
    }
    if let Some(v) = &u.fields {
        vals.push(SetVal::JsonText(
            serde_json::to_string(v).unwrap_or_else(|_| "null".to_string()),
        ));
        // ::text keeps the declared param type string; PG then does the
        // explicit text→jsonb I/O-conversion input.
        sets.push(format!("\"fields\" = ${}::jsonb", vals.len() + 1));
    }
    if let Some(v) = &u.followed_message {
        push_set!("\"followedMessage\"", bound SetVal::Text(v.clone()));
    }
    for (col, val) in [
        ("mutedWords", &u.muted_words),
        ("hardMutedWords", &u.hard_muted_words),
        ("mutedInstances", &u.muted_instances),
        ("emailNotificationTypes", &u.email_notification_types),
        ("notificationRecieveConfig", &u.notification_recieve_config),
    ] {
        if let Some(v) = val {
            vals.push(SetVal::JsonText(
                serde_json::to_string(v).unwrap_or_else(|_| "null".to_string()),
            ));
            sets.push(format!("\"{}\" = ${}::jsonb", col, vals.len() + 1));
        }
    }
    if let Some(v) = u.no_crawle {
        push_set!("\"noCrawle\"", bound SetVal::Bool(v));
    }
    if let Some(v) = u.prevent_ai_learning {
        push_set!("\"preventAiLearning\"", bound SetVal::Bool(v));
    }
    if let Some(v) = u.inject_featured_note {
        push_set!("\"injectFeaturedNote\"", bound SetVal::Bool(v));
    }
    if let Some(v) = u.receive_announcement_email {
        push_set!("\"receiveAnnouncementEmail\"", bound SetVal::Bool(v));
    }
    if let Some(v) = u.always_mark_nsfw {
        push_set!("\"alwaysMarkNsfw\"", bound SetVal::Bool(v));
    }
    if let Some(v) = u.auto_sensitive {
        push_set!("\"autoSensitive\"", bound SetVal::Bool(v));
    }
    if let Some(v) = u.careful_bot {
        push_set!("\"carefulBot\"", bound SetVal::Bool(v));
    }
    if let Some(v) = u.auto_accept_followed {
        push_set!("\"autoAcceptFollowed\"", bound SetVal::Bool(v));
    }
    if let Some(v) = u.public_reactions {
        push_set!("\"publicReactions\"", bound SetVal::Bool(v));
    }
    // PG enums (labels validated against the paramDef whitelist upstream of
    // this call; escaped here regardless — PG coerces the literal to the enum).
    if let Some(v) = &u.following_visibility {
        push_set!("\"followingVisibility\"", lit v.clone());
    }
    if let Some(v) = &u.followers_visibility {
        push_set!("\"followersVisibility\"", lit v.clone());
    }
    if !sets.is_empty() {
        let sql = format!(
            r#"UPDATE "user_profile" SET {} WHERE "userId" = $1"#,
            sets.join(", ")
        );
        let mut q = sqlx::query(&sql).bind(user_id);
        for v in &vals {
            q = match v {
                SetVal::Text(t) => q.bind(t.as_ref()),
                SetVal::Bool(b) => q.bind(*b),
                SetVal::JsonText(s) => q.bind(s),
            };
        }
        q.execute(pool).await?;
    }

    Ok(())
}

pub async fn find_profile(pool: &PgPool, user_id: &str) -> Result<ProfileRow, sqlx::Error> {
    // (column-array decode: Vec<String> is required for varchar[] columns)
    sqlx::query_as::<_, ProfileRow>(&format!(
        r#"SELECT {} FROM "user_profile" WHERE "userId" = $1"#,
        PROFILE_COLS
    ))
    .bind(user_id)
    .fetch_one(pool)
    .await
}

pub async fn insert_signin(
    pool: &PgPool,
    id: &str,
    user_id: &str,
    ip: &str,
    success: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query(r#"INSERT INTO "signin" ("id", "userId", "ip", "headers", "success") VALUES ($1, $2, $3, '{}'::jsonb, $4)"#)
        .bind(id)
        .bind(user_id)
        .bind(ip)
        .bind(success)
        .execute(pool)
        .await?;
    Ok(())
}

/// `UserSearchService.searchByUsernameAndHost` — no-login variant collapsed to
/// one query (fresh M2 instances hold no `updatedAt` rows, so the
/// active/inactive split degenerates to the same ordering; `muting` exclusion
/// is layered in by the caller when a me is present). Order: usernameLower ASC.
pub async fn search_users_by_username_and_host(
    pool: &PgPool,
    username: Option<&str>,
    host: Option<&str>,
    own_host: &str,
    limit: i64,
) -> Result<Vec<LocalUserRow>, sqlx::Error> {
    let pattern = format!(
        "{}%",
        username
            .unwrap_or("")
            .to_lowercase()
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    sqlx::query_as::<_, LocalUserRow>(&format!(
        r#"SELECT {} FROM "user"
           WHERE "usernameLower" LIKE $1
             AND "isSuspended" = FALSE
             AND (
                   $2 IS NULL
                   OR ($2 IN ('.', $3) AND "host" IS NULL)
                   OR "host" ILIKE $2
                 )
           ORDER BY "usernameLower" ASC
           LIMIT {}"#,
        LOCAL_USER_COLS, limit
    ))
    .bind(pattern)
    .bind(host)
    .bind(own_host)
    .fetch_all(pool)
    .await
}

fn sql_like_escape(q: &str) -> String {
    q.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// `UserSearchService.search` — name/username pass, then a description-pass
/// only when fewer than limit rows were found (exclusions + origins per pass).
pub async fn search_users(
    pool: &PgPool,
    query: &str,
    me_id: Option<&str>,
    origin: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<LocalUserRow>, sqlx::Error> {
    let like = format!("%{}%", sql_like_escape(query));
    let muted_clause = if me_id.is_some() {
        " AND \"user\".\"id\" NOT IN (SELECT \"muteeId\" FROM \"muting\" WHERE \"muterId\" = $5)"
    } else {
        ""
    };
    let origin_clause = match origin {
        Some("local") => " AND \"user\".\"host\" IS NULL",
        Some("remote") => " AND \"user\".\"host\" IS NOT NULL",
        _ => "",
    };

    // pass 1: name / username (a token that starts with '@' prefixes the
    // username match; any valid-local-username token also matches mid-string)
    let uname_sub: String = if query.starts_with('@') {
        sql_like_escape(query.trim_start_matches('@').to_lowercase().as_str())
    } else {
        sql_like_escape(query.to_lowercase().as_str())
    };
    let uname_pattern = if query.starts_with('@') {
        format!("{}%", uname_sub)
    } else {
        format!("%{}%", uname_sub)
    };

    let rows = sqlx::query_as::<_, LocalUserRow>(&format!(
        r#"SELECT {} FROM "user"
           WHERE ("name" ILIKE $1 OR "usernameLower" LIKE $2)
             AND ("user"."updatedAt" IS NULL OR "user"."updatedAt" > now() - interval '30 days')
             AND "isSuspended" = FALSE{}{}
           ORDER BY "updatedAt" DESC NULLS LAST
           LIMIT {} OFFSET {}"#,
        LOCAL_USER_COLS, muted_clause, origin_clause, limit, offset
    ))
    .bind(&like)
    .bind(&uname_pattern)
    .bind(me_id)
    .fetch_all(pool)
    .await?;

    if (rows.len() as i64) >= limit {
        return Ok(rows);
    }

    // pass 2: profile description, passive users left in (upstream keeps the
    // active gate; the offset is applied to the description pass independently)
    let desc_rows = sqlx::query_as::<_, LocalUserRow>(&format!(
        r#"SELECT {} FROM "user" LEFT JOIN "user_profile" ON "user"."id" = "user_profile"."userId"
           WHERE "user_profile"."description" ILIKE $1
             AND ("user"."updatedAt" IS NULL OR "user"."updatedAt" > now() - interval '30 days')
             AND "user"."isSuspended" = FALSE{}{}
           ORDER BY "user"."updatedAt" DESC NULLS LAST
           LIMIT {} OFFSET {}"#,
        LOCAL_USER_COLS, muted_clause, origin_clause, limit, offset
    ))
    .bind(&like)
    .bind(me_id)
    .fetch_all(pool)
    .await?;

    let mut seen = rows;
    for r in desc_rows {
        if !seen.iter().any(|x| x.id == r.id) {
            seen.push(r);
        }
    }
    Ok(seen)
}
