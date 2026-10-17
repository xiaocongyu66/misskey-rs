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

pub const PROFILE_COLS: &str = r#""userId", "location", "birthday", "description", "lang", "fields", "url", "email", "emailVerified", "twoFactorEnabled", "password", "autoAcceptFollowed", "alwaysMarkNsfw", "carefulBot", "securityKeysAvailable", "usePasswordLessLogin", "injectFeaturedNote", "receiveAnnouncementEmail", "emailNotificationTypes", "publicReactions", "autoSensitive", "achievements", "loggedInDates", "preventAiLearning", "notificationRecieveConfig", "hardMutedWords", "mutedWords", "mutedInstances", ("followingVisibility"::text AS "followingVisibility"), ("followersVisibility"::text AS "followersVisibility"), "followedMessage", "followApprovalLocalSeconds", "followApprovalRemoteSeconds""#;

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
