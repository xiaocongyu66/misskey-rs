//! PostgreSQL access: pool construction and migrations.
//! The only crate allowed to touch SQL.

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

pub use sqlx;

// app-auth: `app` / `auth_session` / app-scoped `access_token` paths
// (app/*, my/apps, i/apps, auth/session/*, auth/accept, i/regenerate-token,
// i/change-password; all three tables ship in the 0001 baseline).
pub mod app;

pub mod admin;
// admin-abuse: the admin/abuse-* family paths (report list filters + resolve
// resolvedAs + forward, and the notification-recipient CRUD) over the 0001
// baseline tables; the synalio webhook plane keeps its minimal queries in
// webhook.rs.
pub mod abuse;
// antennas-v2: antennas/* domain (antenna CRUD plane from the 0001 baseline +
// the 0004 antenna_note fanout projection; checkHitAntenna ladder + the
// antennas/notes feed page over the shared lists.rs fragment set)
pub mod antennas;
// announcements-agent: announcements domain (announcement/announcement_read
// live in the 0001 baseline; list/read/admin paths + the unread probe the
// MeDetailed lane will wire)
pub mod announcement;
// avatar-decorations: get-avatar-decorations + admin/avatar-decorations/*
// (the avatar_decoration table ships in the 0001 baseline; role-visibility
// filtering reads the role table)
pub mod avatar_decoration;
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
// gallery-agent: gallery/* domain (gallery_post/gallery_like tables ship in
// the 0001 baseline; CRUD + likes + featured/popular/list db paths)
pub mod gallery;
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
// federation/* domain: instance list/show/stats + host-filtered
// followers/following/users reads (upstream endpoints/federation/*.ts; the
// AP-fetch `update-remote-user` lane is out of scope)
pub mod federation;
// ul-tl-agent: users/lists/* + notes/user-list-timeline domain (user_list /
// user_list_membership tables ship in the 0001 baseline)
pub mod lists;
// note-agent: meta row note-settings subset (sensitive/prohibited words +
// the deeplAuthKey read the translate gate consumes)
pub mod memo;
pub mod meta;
pub mod note;
pub mod notification;
// pages-agent: page + page_like (endpoints/pages/*; 0001 baseline)
pub mod page;
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
// following-agent: following/invalidate + following/requests/list reads
// (the create/delete/accept/cancel writes live in social.rs)
pub mod following;
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
// admin-relays: the `relay` table paths (admin/relays/{list,add,remove};
// 0001 baseline — id/inbox/status, UNIQUE btree on inbox; no migration)
pub mod relay;
// invite-promo: registration_ticket paths (invite/* + admin/invite/*) and
// promo_note/promo_read paths (promo/read + admin/promo/*); all three tables
// ship in the 0001 baseline — no migration.
pub mod invite;
pub mod promo;
// i-registry arena: registry_item paths behind i/registry/* (key stored
// `/`-joined, scope varchar[]; 0001 baseline — no migration)
pub mod registry;
// i-registry arena: signin rows behind i/signin-history (0001 baseline —
// no migration; createdAt derived from the AID id)
pub mod signin;
// hashtags arena: hashtag counter reads + `__chart__hashtag` trend fallback
// (both tables ship in the 0001 baseline — no migration)
pub mod hashtag;
// auth arena: auth_session + app join behind auth/session/show (0001
// baseline — no migration)
pub mod auth;

pub async fn connect(url: &str) -> Result<PgPool, sqlx::Error> {
    // A migration DDL that waits on a lock must fail visibly instead of
    // sitting forever: CI wedged for 30+ minutes inside the 0001 DDL with the
    // advisory lock already acquired. 60s is well above normal lock waits.
    PgPoolOptions::new()
        .max_connections(20)
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                sqlx::query("SET lock_timeout = '60000ms'")
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect(url)
        .await
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

    /// One migrated database per PG server; every test clones it, which is
    /// milliseconds instead of running the 40k-line 0001 DDL 200+ times.
    const TEMPLATE_DB: &str = "mk_schema_template";
    const TEMPLATE_LOCK: i64 = 0x6d6b_746d_706c_0001;

    pub async fn fresh_db(db_suffix: &str) -> PgPool {
        let base = std::env::var("DATABASE_URL").unwrap_or_default();
        assert!(!base.is_empty(), "DATABASE_URL required for db tests");

        let admin_url = swap_db(&base, "postgres");
        let admin = connect(&admin_url).await.expect("connect admin db");
        ensure_template(&base, &admin).await;
        sqlx::query(&format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            db_suffix
        ))
        .execute(&admin)
        .await
        .expect("drop stale test db");
        sqlx::query(&format!(
            "CREATE DATABASE {} TEMPLATE {}",
            db_suffix, TEMPLATE_DB
        ))
        .execute(&admin)
        .await
        .expect("clone template db");
        admin.close().await;

        connect(&swap_db(&base, db_suffix))
            .await
            .expect("connect test db")
    }

    async fn template_exists(admin: &PgPool) -> bool {
        sqlx::query_scalar::<_, i32>("SELECT 1 FROM pg_database WHERE datname = $1")
            .bind(TEMPLATE_DB)
            .fetch_optional(admin)
            .await
            .expect("probe template db")
            .is_some()
    }

    async fn ensure_template(base: &str, admin: &PgPool) {
        if template_exists(admin).await {
            return;
        }
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(TEMPLATE_LOCK)
            .execute(admin)
            .await
            .expect("template lock");
        // Re-check under the lock: a sibling test binary may have built it
        // between our probe and the lock.
        if !template_exists(admin).await {
            sqlx::query(&format!("CREATE DATABASE {}", TEMPLATE_DB))
                .execute(admin)
                .await
                .expect("create template db");
            let tpl = connect(&swap_db(base, TEMPLATE_DB))
                .await
                .expect("connect template db");
            super::run_migrations(&tpl).await.expect("migrate template");
            // CREATE DATABASE ... TEMPLATE refuses to run while the source
            // still has connections, so close before returning.
            tpl.close().await;
        }
        let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(TEMPLATE_LOCK)
            .execute(admin)
            .await;
    }

    fn swap_db(url: &str, db: &str) -> String {
        let (prefix, _) = url.rsplit_once('/').unwrap_or((url, ""));
        format!("{}/{}", prefix, db)
    }

    #[allow(dead_code)]
    fn _opts(_: PgConnectOptions) {}
}
