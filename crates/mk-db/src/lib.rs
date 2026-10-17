//! PostgreSQL access: pool construction and migrations.
//! The only crate allowed to touch SQL.

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

pub use sqlx;

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
    /// In CI (DATABASE_URL provided), verifies the committed baseline applies
    /// cleanly and is idempotent. Locally (no DATABASE_URL) it is a no-op.
    #[tokio::test]
    async fn baseline_migrations_apply() {
        let Ok(url) = std::env::var("DATABASE_URL") else {
            return;
        };
        let pool = connect(&url).await.expect("connect");
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
