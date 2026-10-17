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
