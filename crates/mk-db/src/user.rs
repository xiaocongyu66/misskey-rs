//! User/account SQL. Mirrors `SignupService.signup`'s transaction: one step
//! inserts into `user`, `user_keypair`, `user_profile`, `used_username`.

use sqlx::PgPool;

pub struct NewUser<'a> {
    pub id: &'a str,
    pub username: &'a str,
    pub username_lower: &'a str,
    pub name: Option<&'a str>,
    /// `SignupService.signup`'s `host: toPunyNullable(host)` — NULL for local
    /// users, the folded host for the test-mode remote signup legs.
    pub host: Option<&'a str>,
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
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
    )
    .bind(u.id)
    .bind(u.username)
    .bind(u.username_lower)
    .bind(u.name)
    .bind(u.host)
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
    // tl-tail: AuthenticateService.ts L49-51 — the native-token lookup is
    // `findOneBy({ token })` with NO host filter, so the test-mode remote
    // signup users (signup({ host: genHost() }) legs) authenticate too.
    let id: Option<String> = sqlx::query_scalar(r#"SELECT "id" FROM "user" WHERE "token" = $1"#)
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

/// email-leg (upstream SignupService.ts): the signup email address and its
/// verification code land on user_profile ("email" + "emailVerifyCode",
/// 0001_init.sql user_profile block) for the verify-email endpoint to consume.
pub async fn set_email_verify_code(
    pool: &PgPool,
    user_id: &str,
    email: &str,
    code: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"UPDATE "user_profile" SET "email" = $1, "emailVerifyCode" = $2 WHERE "userId" = $3"#,
    )
    .bind(email)
    .bind(code)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// email-leg take(): matches `code` against the stored one; on a hit the code
/// is consumed and "emailVerified" flips true (upstream verify-email), on a
/// miss nothing changes and false comes back.
pub async fn take_email_verify_code(
    pool: &PgPool,
    user_id: &str,
    code: &str,
) -> Result<bool, sqlx::Error> {
    let out = sqlx::query(
        r#"UPDATE "user_profile" SET "emailVerifyCode" = NULL, "emailVerified" = TRUE
           WHERE "userId" = $1 AND "emailVerifyCode" = $2"#,
    )
    .bind(user_id)
    .bind(code)
    .execute(pool)
    .await?;
    Ok(out.rows_affected() > 0)
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
    // pack-fed me-view flag (UserEntityService.pack `isDeleted: user.isDeleted`)
    #[sqlx(rename = "isDeleted")]
    pub is_deleted: bool,
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
    // pack-fed counters/flags (upstream UserEntityService.pack reads
    // user.followersCount / followingCount / notesCount / isBot / isCat)
    #[sqlx(rename = "followersCount")]
    pub followers_count: i32,
    #[sqlx(rename = "followingCount")]
    pub following_count: i32,
    #[sqlx(rename = "notesCount")]
    pub notes_count: i32,
    #[sqlx(rename = "isBot")]
    pub is_bot: bool,
    #[sqlx(rename = "isCat")]
    pub is_cat: bool,
    // arena-move: the account-migration plane (upstream AccountMoveService)
    // fed into the UserDetailed pack's movedTo/alsoKnownAs. alsoKnownAs is
    // the raw TypeORM `simple-array` text (comma-joined URIs); the CSV codec
    // lives with the pack/endpoint layer (api::move_account).
    #[sqlx(rename = "movedToUri")]
    pub moved_to_uri: Option<String>,
    #[sqlx(rename = "alsoKnownAs")]
    pub also_known_as: Option<String>,
}

pub const LOCAL_USER_COLS: &str = r#""id", "username", "usernameLower", "name", "token", "isSuspended", "isDeleted", "isLocked", "isExplorable", "hideOnlineStatus", "chatScope", "avatarUrl", "avatarBlurhash", "avatarDecorations", "bannerUrl", "bannerBlurhash", "avatarId", "bannerId", "followersCount", "followingCount", "notesCount", "isBot", "isCat", "movedToUri", "alsoKnownAs""#;

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
    // varchar[]; drives twoFactorBackupCodesStock (UserEntityService.pack)
    #[sqlx(rename = "twoFactorBackupSecret")]
    pub two_factor_backup_secret: Option<Vec<String>>,
}

pub const PROFILE_COLS: &str = r#""userId", "location", "birthday", "description", "lang", "fields", "url", "email", "emailVerified", "twoFactorEnabled", "password", "autoAcceptFollowed", "alwaysMarkNsfw", "carefulBot", "securityKeysAvailable", "usePasswordLessLogin", "injectFeaturedNote", "receiveAnnouncementEmail", "emailNotificationTypes", "publicReactions", "autoSensitive", "achievements", "loggedInDates", "preventAiLearning", "noCrawle", "verifiedLinks", "notificationRecieveConfig", "hardMutedWords", "mutedWords", "mutedInstances", ("followingVisibility"::text) AS "followingVisibility", ("followersVisibility"::text) AS "followersVisibility", "followedMessage", "followApprovalLocalSeconds", "followApprovalRemoteSeconds", "twoFactorBackupSecret""#;

pub struct ProfileUpdate {
    // "user" table columns
    pub name: Option<Option<String>>,
    pub is_locked: Option<bool>,
    pub is_explorable: Option<bool>,
    pub hide_online_status: Option<bool>,
    pub chat_scope: Option<String>,
    /// "user" table booleans (upstream i/update.ts
    /// `updates.isBot = ps.isBot` / `updates.isCat = ps.isCat`).
    pub is_bot: Option<bool>,
    pub is_cat: Option<bool>,
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
    /// follow-approval windows (i/update.ts paramDef: nullable int, 0..=30d).
    /// None → untouched; Some(None) → reset to NULL (initial state).
    pub follow_approval_local_seconds: Option<Option<i32>>,
    pub follow_approval_remote_seconds: Option<Option<i32>>,
}

/// One dynamically-built SET assignment value. Text/Json/Bool values become
/// bind parameters whose type PG infers from the assignment target; enum
/// values are used as inlined escaped literals (sqlx binds str params with
/// the TEXT type OID, which PG rejects against enum columns — 42804).
enum SetVal {
    Text(Option<String>),
    Bool(bool),
    /// int4 columns; None binds NULL (nullable-integer params such as
    /// followApprovalLocalSeconds — absent stays untouched, explicit null
    /// resets).
    Int(Option<i32>),
    JsonText(String),
}

pub async fn update_profile_subset(
    pool: &PgPool,
    user_id: &str,
    u: &ProfileUpdate,
) -> Result<(), sqlx::Error> {
    // --- "user" table: name / isLocked / isExplorable / hideOnlineStatus /
    // chatScope / isBot / isCat (upstream `updates`) ---
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
    if let Some(v) = u.is_bot {
        cols.push("\"isBot\"");
        vals.push(SetVal::Bool(v));
    }
    if let Some(v) = u.is_cat {
        cols.push("\"isCat\"");
        vals.push(SetVal::Bool(v));
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
                // user-table arm only carries Text/Bool (see the
                // "user_profile" loop below for Int/JsonText)
                SetVal::Int(_) | SetVal::JsonText(_) => unreachable!(),
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
    if let Some(v) = &u.follow_approval_local_seconds {
        vals.push(SetVal::Int(*v));
        sets.push(format!(
            "\"followApprovalLocalSeconds\" = ${}",
            vals.len() + 1
        ));
    }
    if let Some(v) = &u.follow_approval_remote_seconds {
        vals.push(SetVal::Int(*v));
        sets.push(format!(
            "\"followApprovalRemoteSeconds\" = ${}",
            vals.len() + 1
        ));
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
                SetVal::Int(o) => q.bind(*o),
                SetVal::JsonText(s) => q.bind(s),
            };
        }
        q.execute(pool).await?;
    }

    Ok(())
}

/// `i/update.ts` L377-411 avatar/banner drive-file swap: upstream writes the
/// denormal `avatarUrl`/`avatarBlurhash` (resp. banner) onto the SAME user
/// row as `avatarId`/`bannerId` (`updates` dict → `usersRepository.update`).
/// The pack then reads those columns (UserEntityService.pack L490/L536-537).
pub enum AvatarBannerOp {
    /// Param absent → column untouched (upstream `if (ps.avatarId)` guards).
    Untouched,
    /// `ps.avatarId === null` → id/url/blurhash all set to NULL.
    Clear,
    /// `ps.avatarId` truthy → validated drive file (id + public url + blurhash).
    Set {
        id: String,
        url: String,
        blurhash: Option<String>,
    },
}

pub async fn update_avatar_banner(
    pool: &PgPool,
    user_id: &str,
    avatar: AvatarBannerOp,
    banner: AvatarBannerOp,
) -> Result<(), sqlx::Error> {
    let mut sets: Vec<String> = Vec::new();
    let mut binds: Vec<Option<String>> = Vec::new();
    for (op, id_col, url_col, blur_col) in [
        (
            &avatar,
            "\"avatarId\"",
            "\"avatarUrl\"",
            "\"avatarBlurhash\"",
        ),
        (
            &banner,
            "\"bannerId\"",
            "\"bannerUrl\"",
            "\"bannerBlurhash\"",
        ),
    ] {
        let triple: Option<[Option<String>; 3]> = match op {
            AvatarBannerOp::Untouched => None,
            AvatarBannerOp::Clear => Some([None, None, None]),
            AvatarBannerOp::Set { id, url, blurhash } => {
                Some([Some(id.clone()), Some(url.clone()), blurhash.clone()])
            }
        };
        if let Some([id, url, blur]) = triple {
            for (col, val) in [(id_col, id), (url_col, url), (blur_col, blur)] {
                binds.push(val);
                sets.push(format!("{col} = ${}", binds.len() + 1));
            }
        }
    }
    if sets.is_empty() {
        return Ok(());
    }
    let sql = format!(r#"UPDATE "user" SET {} WHERE "id" = $1"#, sets.join(", "));
    let mut q = sqlx::query(&sql).bind(user_id);
    for v in &binds {
        // url/blurhash columns are varchar; no empty strings here (drive rows
        // carry non-empty urls, image files real-or-placeholder blurhashes).
        q = q.bind(v.as_ref());
    }
    q.execute(pool).await?;
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

/// `UserSearchService.searchByUsernameAndHost` — priority-leg search
/// (upstream `src/core/UserSearchService.ts`): for a logged-in me the legs are
/// 1. followees with `updatedAt > now-30d`,
/// 2. followees with `updatedAt IS NULL OR <= now-30d`,
/// 3. non-followees with `updatedAt > now-30d`,
/// 4. non-followees with `updatedAt <= now-30d` (NULL excluded! — the
///    no-activity local-user gate: a user without notes and without a
///    follow edge to the viewer matches no leg, mirroring the e2e
///    "ノートなしは検索に載らない" case),
/// and for anonymous callers two legs: `updatedAt IS NULL OR > now-30d`,
/// then `<= now-30d`. Each leg is limited to `limit - collected` and ordered
/// `usernameLower ASC`; ids are deduped across legs preserving leg order.
pub async fn search_users_by_username_and_host(
    pool: &PgPool,
    username: Option<&str>,
    host: Option<&str>,
    own_host: &str,
    me_id: Option<&str>,
    limit: i64,
) -> Result<Vec<LocalUserRow>, sqlx::Error> {
    // parent site filter: `usernameLower` is interpolated after sql-like
    // escaping (like upstream sqlLikeEscape); `me` stays a bound parameter.
    let pattern = username
        .filter(|u| !u.is_empty())
        .map(|u| format!("{}%", sql_like_escape(&u.to_lowercase())));
    // upstream: `if (params.host)` truthy — `null`/absent means no host filter;
    // `.` or the local host mean local rows.
    let pattern_host = host.filter(|h| !h.is_empty()).map(|h| {
        if h == "." || h.eq_ignore_ascii_case(own_host) {
            None // local
        } else {
            Some(format!("{}%", sql_like_escape(&h.to_lowercase())))
        }
    });

    // (follow-leg kind per viewer state) — the updated-at gate differs by leg:
    // upstream buildSearchUserQueries: followee-inactive keeps
    // `updatedAt IS NULL OR <= threshold`; non-followee legs keep only non-NULL
    // rows either side of the threshold (a user without notes and without a
    // follow edge to the viewer matches NO leg — the e2e
    // "ローカルでノートなしは検索に載らない" gate); the anonymous pair is
    // (`updatedAt IS NULL OR > threshold`) then `<= threshold`.
    let legs: Vec<(&str, bool)> = match me_id {
        Some(_) => vec![
            ("following", true),
            ("following", false),
            ("not-following", true),
            ("not-following", false),
        ],
        None => vec![("anonymous", true), ("anonymous", false)],
    };

    let mut seen: Vec<LocalUserRow> = Vec::new();
    for (follow_kind, active) in legs {
        if seen.len() as i64 >= limit {
            break;
        }
        let remaining = limit - seen.len() as i64;
        let follow_clause = match (me_id, follow_kind) {
            (Some(_), "following") => {
                r#" AND ("id" IN (SELECT "followeeId" FROM "following" WHERE "followerId" = $1))"#
            }
            (Some(_), "not-following") => {
                r#" AND ("id" NOT IN (SELECT "followeeId" FROM "following" WHERE "followerId" = $1))"#
            }
            _ => "",
        };
        let gate = match (follow_kind, active) {
            ("anonymous", true) => {
                r#"("updatedAt" IS NULL OR "updatedAt" > now() - interval '30 days')"#
            }
            (_, true) => {
                r#"("updatedAt" IS NOT NULL AND "updatedAt" > now() - interval '30 days')"#
            }
            ("not-following", false) => {
                r#"("updatedAt" IS NOT NULL AND "updatedAt" <= now() - interval '30 days')"#
            }
            // following-inactive / anonymous-inactive (and any other kind):
            // NULL updatedAt rows live in the inactive leg only for these.
            _ => r#"("updatedAt" IS NULL OR "updatedAt" <= now() - interval '30 days')"#,
        };
        let mut where_ = format!(r#""isSuspended" = FALSE AND ({gate})"#);
        if let Some(p) = &pattern {
            where_.push_str(&format!(r#" AND ("usernameLower" LIKE '{p}')"#));
        }
        match &pattern_host {
            Some(None) => where_.push_str(r#" AND ("host" IS NULL)"#),
            Some(Some(p)) => where_.push_str(&format!(r#" AND "host" ILIKE '{p}'"#)),
            None => {}
        }

        let sql = format!(
            r#"SELECT {LOCAL_USER_COLS} FROM "user"
               WHERE {where_}{follow_clause}
               ORDER BY "usernameLower" ASC
               LIMIT {remaining}"#
        );
        let rows = match me_id {
            Some(me) => {
                sqlx::query_as::<_, LocalUserRow>(&sql)
                    .bind(me)
                    .fetch_all(pool)
                    .await?
            }
            None => {
                sqlx::query_as::<_, LocalUserRow>(&sql)
                    .fetch_all(pool)
                    .await?
            }
        };
        for r in rows {
            if !seen.iter().any(|x| x.id == r.id) {
                seen.push(r);
            }
            if seen.len() as i64 >= limit {
                break;
            }
        }
    }
    Ok(seen)
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
    // [shared-gate repair] the mute-exclusion clause is shared by two passes
    // whose bind positions differ (pass 1: like=$1/uname=$2/me=$3; pass 2:
    // like=$1/me=$2) — a fixed $5 left sparse params and PG's
    // "could not determine data type of parameter $4". Take the slot number.
    let muted_clause = |slot: &str| {
        if me_id.is_some() {
            format!(
                " AND \"user\".\"id\" NOT IN (SELECT \"muteeId\" FROM \"muting\" WHERE \"muterId\" = {slot})"
            )
        } else {
            String::new()
        }
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
        LOCAL_USER_COLS,
        muted_clause("$3"),
        origin_clause,
        limit,
        offset
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
        LOCAL_USER_COLS,
        muted_clause("$2"),
        origin_clause,
        limit,
        offset
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

/// `endpoints/hashtags/users.ts`: tag membership via array containment.
/// The queried tag is normalized like upstream `normalizeForSearch`
/// (NFKC + lowercase; we apply lowercase, matching how tags are stored —
/// `extract_hashtags` lowercases on the note path). Sort signs mirror
/// upstream's switch, including `+updatedAt`/`-updatedAt` ordering by the
/// `updatedAt` column (PG default NULLS placement, like TypeORM's plain
/// ORDER BY). `state: alive` is upstream's plain `updatedAt > now - 5d` —
/// NULL updatedAt fails that comparison, so those rows are excluded.
/// `origin` defaults to 'local' per the upstream paramDef.
pub async fn users_by_tag(
    pool: &PgPool,
    tag: &str,
    sort: &str,
    state: Option<&str>,
    origin: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<LocalUserRow>, sqlx::Error> {
    let (col, dir) = match sort {
        "+follower" => ("followersCount", "DESC"),
        "-follower" => ("followersCount", "ASC"),
        "+createdAt" => ("id", "DESC"),
        "-createdAt" => ("id", "ASC"),
        "+updatedAt" => ("updatedAt", "DESC"),
        "-updatedAt" => ("updatedAt", "ASC"),
        _ => ("id", "DESC"),
    };
    let tag = tag.to_lowercase();
    let mut sql = format!(
        r#"SELECT {} FROM "user"
           WHERE "tags" @> ARRAY[$1::varchar]
             AND "isSuspended" = FALSE"#,
        LOCAL_USER_COLS
    );
    if state == Some("alive") {
        sql.push_str(r#" AND "user"."updatedAt" > now() - interval '5 days'"#);
    }
    sql.push_str(match origin {
        Some("remote") => r#" AND "host" IS NOT NULL"#,
        Some("combined") => "",
        // paramDef default: origin: 'local'
        _ => r#" AND "host" IS NULL"#,
    });
    sql.push_str(&format!(
        r#" ORDER BY "{}" {} LIMIT {} OFFSET {}"#,
        col, dir, limit, offset
    ));
    sqlx::query_as::<_, LocalUserRow>(&sql)
        .bind(tag)
        .fetch_all(pool)
        .await
}

/// `endpoints/users.ts` (explore list). Mirrors the sort/state/origin switch,
/// `isExplorable` + not-suspended gates, hostname exact match, and the
/// nested-per-user exclusions for muted/blocking relations when a me is set.
pub async fn users_explore(
    pool: &PgPool,
    sort: Option<&str>,
    state: Option<&str>,
    origin: Option<&str>,
    hostname: Option<&str>,
    me_id: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<LocalUserRow>, sqlx::Error> {
    let (col, dir) = match sort {
        Some("+follower") => ("followersCount", "DESC"),
        Some("-follower") => ("followersCount", "ASC"),
        Some("+createdAt") => ("id", "DESC"),
        Some("-createdAt") => ("id", "ASC"),
        Some("+updatedAt") | Some("-updatedAt") => (
            "updatedAt",
            if sort == Some("+updatedAt") {
                "DESC"
            } else {
                "ASC"
            },
        ),
        _ => ("id", "ASC"),
    };
    let updated_at_gate = matches!(sort, Some("+updatedAt") | Some("-updatedAt"));
    let mut sql = format!(
        r#"SELECT {} FROM "user"
           WHERE "isExplorable" = TRUE
             AND "isSuspended" = FALSE"#,
        LOCAL_USER_COLS
    );
    if updated_at_gate {
        sql.push_str(r#" AND "user"."updatedAt" IS NOT NULL"#);
    }
    if state == Some("alive") {
        sql.push_str(r#" AND "user"."updatedAt" > now() - interval '5 days'"#);
    }
    sql.push_str(match origin {
        Some("local") => r#" AND "host" IS NULL"#,
        Some("remote") => r#" AND "host" IS NOT NULL"#,
        _ => "",
    });
    // $1 = hostname (null ⇒ no filter), $2 = me (null ⇒ no exclusions);
    // both bound, never interpolated — hostname is user input.
    sql.push_str(r#" AND ($1::text IS NULL OR "host" = lower($1))"#);
    // generateMutedUserQueryForUsers + generateBlockQueryForUsers
    // (QueryService.ts): mute-exclusions plus BOTH blocking directions —
    // users the viewer blocks AND users who block the viewer are excluded
    // (e2e "ブロックしてきているユーザーが含まれる" excludes:true).
    sql.push_str(
        r#" AND ($2::text IS NULL OR "user"."id" NOT IN
              (SELECT "muteeId" FROM "muting" WHERE "muterId" = $2))"#,
    );
    sql.push_str(
        r#" AND ($2::text IS NULL OR "user"."id" NOT IN
              (SELECT "blockeeId" FROM "blocking" WHERE "blockerId" = $2))"#,
    );
    sql.push_str(
        r#" AND ($2::text IS NULL OR "user"."id" NOT IN
              (SELECT "blockerId" FROM "blocking" WHERE "blockeeId" = $2))"#,
    );
    sql.push_str(&format!(
        r#" ORDER BY "{}" {} LIMIT {} OFFSET {}"#,
        col, dir, limit, offset
    ));
    sqlx::query_as::<_, LocalUserRow>(&sql)
        .bind(hostname)
        .bind(me_id)
        .fetch_all(pool)
        .await
}

/// Password-reset email lane (upstream `request-reset-password.ts` +
/// `reset-password.ts`). The 0001 baseline ships a dedicated
/// `password_reset_request` table (`id`, `token`, `"userId"` — no timestamp
/// column), so the mission's "no column → in-process TTL map" fallback is
/// NOT needed: the code is persisted here. Upstream derives the 30-minute
/// TTL from the AID-encoded creation time, so the row id is AID-shaped
/// (8 base36 chars of ms-since-2000 + 2 noise chars).
const RESET_TTL_MS: i64 = 30 * 60 * 1000;
const TIME_2000_MS: i64 = 946_684_800_000;

fn reset_base36(mut v: u64, width: usize) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = vec![b'0'; width];
    for slot in out.iter_mut().rev() {
        *slot = DIGITS[(v % 36) as usize];
        v /= 36;
    }
    String::from_utf8(out).unwrap_or_default()
}

/// AID-shaped id for the reset row (`mk-types::id::gen_aid` shape without
/// pulling the crate into mk-db).
fn reset_aid(now_ms: i64) -> String {
    let elapsed = if now_ms > TIME_2000_MS {
        (now_ms - TIME_2000_MS) as u64
    } else {
        0
    };
    format!("{}00", reset_base36(elapsed, 8))
}

fn reset_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Stores a fresh reset code for `user_id`, dropping any previous request so
/// only the newest code is live (upstream allows several rows but only the
/// token from the last email is usable in practice).
pub async fn set_password_reset_code(
    pool: &PgPool,
    user_id: &str,
    code: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "password_reset_request" WHERE "userId" = $1"#)
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query(
        r#"INSERT INTO "password_reset_request" ("id", "token", "userId") VALUES ($1, $2, $3)"#,
    )
    .bind(reset_aid(reset_now_ms()))
    .bind(code)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Resolves the user a reset token was issued for (`reset-password.ts`
/// `findOneByOrFail({ token })`). None → unknown token.
pub async fn password_reset_user_id(
    pool: &PgPool,
    code: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        r#"SELECT "userId" FROM "password_reset_request" WHERE "token" = $1 LIMIT 1"#,
    )
    .bind(code)
    .fetch_optional(pool)
    .await
}

/// Consumes the code when it matches `user_id` and is younger than the
/// upstream 30-minute window; stale or unknown codes are purged and report
/// false. The `id >= cutoff` predicate is the TTL check (AID time prefix).
pub async fn take_password_reset_code(
    pool: &PgPool,
    user_id: &str,
    code: &str,
) -> Result<bool, sqlx::Error> {
    let cutoff = reset_aid(reset_now_ms().saturating_sub(RESET_TTL_MS));
    let hit: Option<String> = sqlx::query_scalar(
        r#"DELETE FROM "password_reset_request"
           WHERE "userId" = $1 AND "token" = $2 AND "id" >= $3
           RETURNING "id""#,
    )
    .bind(user_id)
    .bind(code)
    .bind(&cutoff)
    .fetch_optional(pool)
    .await?;
    if hit.is_none() {
        sqlx::query(r#"DELETE FROM "password_reset_request" WHERE "userId" = $1 AND "token" = $2"#)
            .bind(user_id)
            .bind(code)
            .execute(pool)
            .await?;
    }
    Ok(hit.is_some())
}

/// `reset-password.ts`: `userProfilesRepository.update(userId, { password })`.
pub async fn set_password_hash(
    pool: &PgPool,
    user_id: &str,
    password_hash: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "user_profile" SET "password" = $2 WHERE "userId" = $1"#)
        .bind(user_id)
        .bind(password_hash)
        .execute(pool)
        .await?;
    Ok(())
}

/// `i/update.ts` L483-531: description/body hashtags merge into `user.tags`
/// (normalized upstream-side; our extractor lowercases; caller caps at 32).
pub async fn merge_user_tags(
    pool: &PgPool,
    user_id: &str,
    tags: &[String],
) -> Result<(), sqlx::Error> {
    for tag in tags.iter().take(32) {
        sqlx::query(
            r#"UPDATE "user" SET "tags" = array_append("tags", $2)
               WHERE "id" = $1 AND NOT ($2 = ANY("tags"))"#,
        )
        .bind(user_id)
        .bind(tag)
        .execute(pool)
        .await?;
    }
    Ok(())
}
