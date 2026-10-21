//! PostgreSQL access: pool construction and migrations.
//! The only crate allowed to touch SQL.

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

pub use sqlx;

pub mod admin;
// antennas-v2: antennas/* domain (antenna CRUD plane from the 0001 baseline +
// the 0004 antenna_note fanout projection; checkHitAntenna ladder + the
// antennas/notes feed page over the shared lists.rs fragment set)
pub mod antennas;
// announcements-agent: announcements domain (announcement/announcement_read
// live in the 0001 baseline; list/read/admin paths + the unread probe the
// MeDetailed lane will wire)
pub mod announcement;
// channels-agent: channels/* domain (channel/channel_following/channel_favorite
// tables + the channels/timeline note db path)
pub mod channel;
// chat-agent: chat/rooms/* + chat/messages/* + chat/history (chat_room /
// chat_message / chat_room_membership / chat_room_invitation / chat_approval
// all ship in the 0001 baseline)
pub mod chat;
// clips-agent: clips/* + users/clips domain (clip/clip_note/clip_favorite
// tables ship in the 0001 baseline; CRUD + notes + favorites db paths)
pub mod clips;
// flash-agent: flash/* domain (flash/flash_like tables ship in the 0001
// baseline; CRUD + likes + featured/search/my-likes db paths)
pub mod flash;
// delete-account: i/delete-account + admin/delete-account soft-delete flag
pub mod delete_account;
// two-factor arena: the TOTP lane (vendored std-only SHA-1/HMAC-SHA1/
// base32/TOTP + user_profile twoFactor* paths + the used-step burn
// UserAuthService.validateOtp rides)
pub mod two_factor;
// twofa-resume: the security-key lane (vendored std-only SHA-256/CBOR/P-256
// ECDSA + user_security_key paths + the redis challenge store parity —
// WebAuthnService.verifyRegistration/verifyAuthentication shape)
pub mod webauthn;
// i/update tail wave: drive files (upload), note pinings, personal memos
pub mod drive;
// admin-emoji: emoji table paths for the admin/emoji/* family (0001 baseline;
// no migration — the zip-import storage lane is not ported)
pub mod emoji;
// note-agent: drive-file SQL for the note.ts slice (owned by-ids fetch +
// drive/files/update write), kept separate from the drive-arena agent's
// drive.rs
pub mod drive_note;
// arena-exports: the export-job db plane (mk_export_blobs byte store + the
// ExportClipsProcessorService clip/clipNote reads + drive/files {list,show,
// delete} support queries)
pub mod export_blob;
// freq-agent: users/get-frequently-replied-users + pinned-users db paths
pub mod freq;
// arena-move: account-migration db legs (upstream AccountMoveService.ts) —
// the movedToUri/alsoKnownAs plane + postMoveProcess scans + the queued
// relationship (follow/unfollow) legs with the moved count-guard
pub mod move_account;
// ff-visibility arena: users/followers + users/following list edges + the
// meta."federation" patch/read the AP collections gate consumes
pub mod ff_visibility;
// ul-tl-agent: users/lists/* + notes/user-list-timeline domain (user_list /
// user_list_membership tables ship in the 0001 baseline)
pub mod lists;
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
// roles-v2: admin/roles/{show,list,users,update} + assign-expiry SQL support
pub mod roles;
// social-agent: social-graph writes (follow/mute/block/renote)
pub mod social;
// arena-api: MiAccessToken rows (api.ts miauth/gen-token mint + the app
// token scope gate the admin/get-index-stats legs ride)
pub mod access_token;
// timelines arena: notes/timeline / notes/local-timeline / notes/hybrid-timeline
pub mod timelines;
// arena-reversi: the reversi game domain (reversi_game + reversi_matching
// ship in the 0001 baseline; the matching table models the redis
// matchSpecific invitation zset on the legacy schema shape — no migration)
pub mod reversi;
// arena-synalio: system_webhook + abuse_report_notification_recipient +
// abuse_user_report db paths (0001 baseline; the synalio e2e webhook plane —
// no migration)
pub mod user;
pub mod webhook;
// charts arena: __chart__notes / __chart_day__notes commit + read paths
// (0001 baseline — hour/day tables ship with the span split; `date` is the
// span-normalized epoch second under a UNIQUE btree per table)
pub mod charts;
// push lane: sw_subscription rows (sw/register + sw/unregister writes; the
// VAPID assembly lives in mk-core::services::web_push)
pub mod sw;

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
