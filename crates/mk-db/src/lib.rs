//! PostgreSQL access: pool construction and migrations.
//! The only crate allowed to touch SQL.

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

pub use sqlx;

pub mod admin;
// announcements-agent: announcements domain (announcement/announcement_read
// live in the 0001 baseline; list/read/admin paths + the unread probe the
// MeDetailed lane will wire)
pub mod announcement;
// channels-agent: channels/* domain (channel/channel_following/channel_favorite
// tables + the channels/timeline note db path)
pub mod channel;
// delete-account: i/delete-account + admin/delete-account soft-delete flag
pub mod delete_account;
// i/update tail wave: drive files (upload), note pinings, personal memos
pub mod drive;
// note-agent: drive-file SQL for the note.ts slice (owned by-ids fetch +
// drive/files/update write), kept separate from the drive-arena agent's
// drive.rs
pub mod drive_note;
// freq-agent: users/get-frequently-replied-users + pinned-users db paths
pub mod freq;
// note-agent: meta row note-settings subset (sensitive/prohibited words +
// the deeplAuthKey read the translate gate consumes)
pub mod memo;
pub mod meta;
pub mod note;
pub mod notification;
// note-agent: poll + poll_vote (notes/create poll param + notes/polls/vote)
pub mod pin;
pub mod poll;
pub mod relation;
pub mod role;
// gate-roles: minimal admin/roles/create + assign SQL support
pub mod roles_lite;
// social-agent: social-graph writes (follow/mute/block/renote)
pub mod social;
// timelines arena: notes/timeline / notes/local-timeline / notes/hybrid-timeline
pub mod timelines;
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
