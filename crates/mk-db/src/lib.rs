//! PostgreSQL access: pool construction and migrations.
//! The only crate allowed to touch SQL.

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

pub use sqlx;

pub mod user;

pub async fn connect(url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new().max_connections(20).connect(url).await
}

/// Runs the embedded migrations (baseline `0001_init.sql` is generated in CI
/// by `tools/gen-baseline.sh` from an upstream TypeORM migration run).
pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("./migrations").run(pool).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In CI (DATABASE_URL provided), verifies the committed baseline applies
    /// cleanly and is idempotent. Locally (no DATABASE_URL) it is a no-op.
    #[tokio::test]
    async fn baseline_migrations_apply() {
        if std::env::var("DATABASE_URL").is_err() {
            return;
        }
        let pool = super::test_support::fresh_db("mkdb_baseline").await;
        run_migrations(&pool).await.expect("baseline migrations");
        run_migrations(&pool).await.expect("idempotent rerun");
        let tables: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public'",
        )
        .fetch_one(&pool)
        .await
        .expect("table count");
        assert!(
            tables > 100,
            "expected the full misskey schema, got {tables}"
        );
    }
}

/// Per-test isolated database helper (CI: tests run in parallel binaries
/// against one PG server; each test creates its own database).
#[doc(hidden)]
pub mod test_support {
    use super::*;
    use sqlx::postgres::PgConnectOptions;

    pub async fn fresh_db(db_suffix: &str) -> PgPool {
        let base = std::env::var("DATABASE_URL").unwrap_or_default();
        assert!(!base.is_empty(), "DATABASE_URL required for db tests");

        let admin_url = swap_db(&base, "postgres");
        let admin = connect(&admin_url).await.expect("connect admin db");
        sqlx::query(&format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            db_suffix
        ))
        .execute(&admin)
        .await
        .expect("drop stale test db");
        sqlx::query(&format!("CREATE DATABASE {}", db_suffix))
            .execute(&admin)
            .await
            .expect("create test db");
        admin.close().await;

        connect(&swap_db(&base, db_suffix))
            .await
            .expect("connect test db")
    }

    fn swap_db(url: &str, db: &str) -> String {
        let (prefix, _) = url.rsplit_once('/').unwrap_or((url, ""));
        format!("{}/{}", prefix, db)
    }

    #[allow(dead_code)]
    fn _opts(_: PgConnectOptions) {}
}
