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
