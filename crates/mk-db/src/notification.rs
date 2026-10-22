//! Notification SQL (M2 truthful slice: mention / reply / renote / reaction /
//! follow). See `endpoints/i/notifications.ts` for endpoint semantics and
//! `core/NotificationService.ts` for create/read semantics (upstream moved the
//! store to Redis; the local schema keeps the pre-cleanup table — see
//! migrations/0002_notifications.sql).
//!
//! Self-heal: the pinned upstream e2e fixture (`test/utils.ts initTestDb`)
//! runs TypeORM synchronize+dropSchema against a schema where Notification is
//! a bare type (no entity), so the migration's `notification` table is
//! dropped OUT from under a live server between suite files
//! (`relation "notification" does not exist` / SQLSTATE 42P01 → every
//! i/notifications read 500'd — the whole mute.ts Notification cluster).
//! Every public fn therefore retriggers the idempotent 0002 DDL once on 42P01
//! and retries (ntf-mute).

use sqlx::PgPool;

pub struct NewNotification<'a> {
    pub id: &'a str,
    /// `notification_type_enum` value, e.g. "mention" / "reaction" / "follow".
    pub r#type: &'a str,
    /// Recipient (`notifieeId`).
    pub user_id: &'a str,
    /// Sender; `None` for notifier-less types (upstream `notifierId` nullable).
    pub notifier_id: Option<&'a str>,
    /// Note-bound types (mention/reply/renote/reaction) carry the source note.
    pub note_id: Option<&'a str>,
    /// `reaction` type only.
    pub reaction: Option<&'a str>,
}

/// Upstream `NotificationService.createNotification`-ish insert. Follower
/// self-notification suppression (= notifiee == notifier) lives at callers
/// upstream; insert_notification stays dumb.
pub async fn insert_notification(
    pool: &PgPool,
    n: &NewNotification<'_>,
) -> Result<(), sqlx::Error> {
    match insert_notification_once(pool, n).await {
        Err(e) if is_undefined_table(&e) => {
            ensure_table(pool).await?;
            insert_notification_once(pool, n).await
        }
        r => r,
    }
}

async fn insert_notification_once(
    pool: &PgPool,
    n: &NewNotification<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO "notification"
            ("id", "createdAt", "notifieeId", "notifierId", "type", "isRead", "noteId", "reaction")
        VALUES ($1, now(), $2, $3, $4::notification_type_enum, FALSE, $5, $6)
        "#,
    )
    .bind(n.id)
    .bind(n.user_id)
    .bind(n.notifier_id)
    .bind(n.r#type)
    .bind(n.note_id)
    .bind(n.reaction)
    .execute(pool)
    .await?;
    Ok(())
}

/// ntf-mute: `NotificationService.createNotification` CentralGate (the two
/// social notify entry points route through here so every reason shares ONE
/// muting drop):
/// - L110: notifiee === notifier → null (self-notification suppression),
/// - L111-114: the muting check is TYPE-AGNOSTIC and sits BEFORE the
///   recieveConfig ladder — a muted notifier notifies for NO reason
///   (mention/reply/renote/quote/reaction AND follow/receiveFollowRequest
///   alike; mute.ts Notification + Notification (Grouped) read exactly this).
/// Notifier-less rows (upstream allows them) bypass both gates, mirroring
/// `if (notifierId) {...}`. Returns `Ok(true)` when the row was inserted and
/// `Ok(false)` when the gate dropped it, so the push fan-out can sit behind
/// the exact same gate (upstream `createNotification` returns null before the
/// push tail).
pub async fn insert_if_not_muted(
    pool: &PgPool,
    n: &NewNotification<'_>,
) -> Result<bool, sqlx::Error> {
    if let Some(notifier_id) = n.notifier_id {
        if n.user_id == notifier_id {
            return Ok(false);
        }
        if crate::social::muting_exists(pool, n.user_id, notifier_id).await? {
            return Ok(false);
        }
    }
    insert_notification(pool, n).await.map(|()| true)
}

/// queue-min: pollEnded dedup probe — the at-least-once queue (plus the
/// per-note delayed job AND the hourly window scan both able to fire on the
/// same poll) must not double-notify; upstream BullMQ gets the same from one
/// delayed job per note + removeOnComplete. A checked-then-insert probe (not
/// a unique index) so the shared 42P01 self-heal insert path stays untouched.
pub async fn poll_ended_notified(
    pool: &PgPool,
    notifiee_id: &str,
    note_id: &str,
) -> Result<bool, sqlx::Error> {
    match poll_ended_notified_once(pool, notifiee_id, note_id).await {
        Err(e) if is_undefined_table(&e) => {
            ensure_table(pool).await?;
            poll_ended_notified_once(pool, notifiee_id, note_id).await
        }
        r => r,
    }
}

async fn poll_ended_notified_once(
    pool: &PgPool,
    notifiee_id: &str,
    note_id: &str,
) -> Result<bool, sqlx::Error> {
    let v: Option<i32> = sqlx::query_scalar(
        r#"
        SELECT 1 FROM "notification"
        WHERE "notifieeId" = $1 AND "type" = 'pollEnded' AND "noteId" = $2
        LIMIT 1
        "#,
    )
    .bind(notifiee_id)
    .bind(note_id)
    .fetch_optional(pool)
    .await?;
    Ok(v.is_some())
}

/// Columns carried by the M2 notification pack (createdAt is packed from the
/// aid timestamp at the pack layer, like notes).
pub const NOTIFICATION_COLS: &str = r#""id", ("type"::text) AS "type", ("isRead") AS "is_read", "noteId" AS "note_id", "notifierId" AS "notifier_id", "reaction""#;

#[derive(Debug, sqlx::FromRow)]
pub struct NotificationRow {
    pub id: String,
    pub r#type: String,
    pub is_read: bool,
    pub note_id: Option<String>,
    pub notifier_id: Option<String>,
    pub reaction: Option<String>,
}

/// `NotificationService.getNotifications` / `QueryService.makePaginationQuery`
/// pagination: `sinceId`-only → oldest-first (id ASC); otherwise newest-first
/// (id DESC). Both bounds are EXCLUSIVE (`(untilTime`, `(sinceTime` in
/// upstream; `misc/id.ts` aid ids compare monotonically as strings).
/// `unreadOnly` filters `isRead`; `include_types` (non-empty) filters
/// `"type" = ANY(...)`. upstream `following` filter not ported — see TODO in
/// the handler. (Upstream has NO read-side muting filter — the drop is
/// create-time only — so none here either.)
pub async fn list_notifications(
    pool: &PgPool,
    me_id: &str,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
    unread_only: bool,
    include_types: &[String],
) -> Result<Vec<NotificationRow>, sqlx::Error> {
    match list_notifications_once(
        pool,
        me_id,
        since_id,
        until_id,
        limit,
        unread_only,
        include_types,
    )
    .await
    {
        Err(e) if is_undefined_table(&e) => {
            ensure_table(pool).await?;
            list_notifications_once(
                pool,
                me_id,
                since_id,
                until_id,
                limit,
                unread_only,
                include_types,
            )
            .await
        }
        r => r,
    }
}

#[allow(clippy::too_many_arguments)]
async fn list_notifications_once(
    pool: &PgPool,
    me_id: &str,
    since_id: Option<&str>,
    until_id: Option<&str>,
    limit: i64,
    unread_only: bool,
    include_types: &[String],
) -> Result<Vec<NotificationRow>, sqlx::Error> {
    let ascending = since_id.is_some() && until_id.is_none();
    let order = if ascending { "ASC" } else { "DESC" };
    let sql = format!(
        r#"
        SELECT {NOTIFICATION_COLS} FROM "notification"
        WHERE "notifieeId" = $1
          AND ($2 = FALSE OR "isRead" = FALSE)
          AND ($3::text IS NULL OR "id" > $3)
          AND ($4::text IS NULL OR "id" < $4)
          AND ($5::text[] IS NULL OR ("type"::text) = ANY($5))
        ORDER BY "id" {order}
        LIMIT $6
        "#
    );
    let types: Option<Vec<String>> = if include_types.is_empty() {
        None
    } else {
        Some(include_types.to_vec())
    };
    Ok(sqlx::query_as::<_, NotificationRow>(&sql)
        .bind(me_id)
        .bind(unread_only)
        .bind(since_id)
        .bind(until_id)
        .bind(types)
        .bind(limit)
        .fetch_all(pool)
        .await?)
}

/// `NotificationService.readAllNotification` (DB-era equivalent): marks every
/// unread notification of the user read.
pub async fn mark_as_read_all(pool: &PgPool, user_id: &str) -> Result<(), sqlx::Error> {
    match mark_as_read_all_once(pool, user_id).await {
        Err(e) if is_undefined_table(&e) => {
            ensure_table(pool).await?;
            mark_as_read_all_once(pool, user_id).await
        }
        r => r,
    }
}

async fn mark_as_read_all_once(pool: &PgPool, user_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"UPDATE "notification" SET "isRead" = TRUE WHERE "notifieeId" = $1 AND "isRead" = FALSE"#,
    )
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// `i` endpoint's `hasUnreadNotification` read (user pack wiring is co-owned —
/// see coordinator notes).
pub async fn has_unread(pool: &PgPool, user_id: &str) -> Result<bool, sqlx::Error> {
    match has_unread_once(pool, user_id).await {
        Err(e) if is_undefined_table(&e) => {
            ensure_table(pool).await?;
            has_unread_once(pool, user_id).await
        }
        r => r,
    }
}

async fn has_unread_once(pool: &PgPool, user_id: &str) -> Result<bool, sqlx::Error> {
    let v: Option<i32> = sqlx::query_scalar(
        r#"SELECT 1 FROM "notification" WHERE "notifieeId" = $1 AND "isRead" = FALSE LIMIT 1"#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(v.is_some())
}

/// SQLSTATE 42P01 — the fixture (TypeORM dropSchema) dropped the table under
/// a running server; see the module header.
pub(crate) fn is_undefined_table(e: &sqlx::Error) -> bool {
    matches!(
        e,
        sqlx::Error::Database(db) if db.code().as_deref() == Some("42P01")
    )
}

/// Idempotent replay of migrations/0002_notifications.sql (shape = the final
/// pre-drop upstream table, union of 1000000000000-Init /
/// 1585385921215-custom-notification / 1674118260469-achievement). The enum
/// type + PK/FK use swallow-duplicate DO blocks because Postgres lacks
/// IF NOT EXISTS for them; the second encounter is a no-op by construction.
pub async fn ensure_table(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        DO $do$ BEGIN
            CREATE TYPE public.notification_type_enum AS ENUM ('follow', 'mention', 'reply', 'renote', 'quote', 'note', 'reaction', 'pollVote', 'pollEnded', 'receiveFollowRequest', 'followRequestAccepted', 'groupInvited', 'achievementEarned', 'app', 'test');
        -- note-notify: migration 0005 ALTERs 'note' between 'quote' and
        -- 'reaction' (the NoteCreateService L806 'note' fanout). This
        -- self-heal path (42P01 → replay, normally after the e2e fixture's
        -- dropSchema swept the table out) must mint the FINAL enum shape in
        -- one go — here the CREATE TYPE runs without 0005.
        EXCEPTION WHEN duplicate_object THEN NULL; END $do$;
        "#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS public.notification (
            "id" character varying(32) NOT NULL,
            "createdAt" timestamp with time zone NOT NULL,
            "notifieeId" character varying(32) NOT NULL,
            "notifierId" character varying(32),
            "type" public.notification_type_enum NOT NULL,
            "isRead" boolean DEFAULT false NOT NULL,
            "noteId" character varying(32),
            "reaction" character varying(128),
            "choice" integer,
            "achievement" character varying(128),
            "customBody" character varying(2048),
            "customHeader" character varying(256),
            "customIcon" character varying(1024),
            "appAccessTokenId" character varying(32)
        )
        "#,
    )
    .execute(pool)
    .await?;
    for stmt in [
        r#"DO $do$ BEGIN ALTER TABLE ONLY public.notification ADD CONSTRAINT "PK_705b6c7cdf9b2c2ff7ac7872cb7" PRIMARY KEY ("id"); EXCEPTION WHEN duplicate_object THEN NULL; END $do$;"#,
        r#"CREATE INDEX IF NOT EXISTS "IDX_b11a5e627c41d4dc3170f1d370" ON public.notification ("createdAt")"#,
        r#"CREATE INDEX IF NOT EXISTS "IDX_3c601b70a1066d2c8b517094cb" ON public.notification ("notifieeId")"#,
        r#"CREATE INDEX IF NOT EXISTS "IDX_3b4e96eec8d36a8bbb9d02aa71" ON public.notification ("notifierId")"#,
        r#"CREATE INDEX IF NOT EXISTS "IDX_33f33cc8ef29d805a97ff4628b" ON public.notification ("type")"#,
        r#"CREATE INDEX IF NOT EXISTS "IDX_080ab397c379af09b9d2169e5b" ON public.notification ("isRead")"#,
        r#"DO $do$ BEGIN ALTER TABLE ONLY public.notification ADD CONSTRAINT "FK_3b4e96eec8d36a8bbb9d02aa710" FOREIGN KEY ("notifierId") REFERENCES public."user"("id") ON DELETE CASCADE ON UPDATE NO ACTION; EXCEPTION WHEN duplicate_object THEN NULL; END $do$;"#,
    ] {
        sqlx::query(stmt).execute(pool).await?;
    }
    // `notifications/test-notification`: the pinned 0001 enum predates the
    // 'test' value (upstream keeps notifications in Redis, so its migration
    // chain never needed it). ADD VALUE IF NOT EXISTS is idempotent.
    ensure_test_type(pool).await?;
    Ok(())
}

async fn ensure_test_type(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(r#"ALTER TYPE public.notification_type_enum ADD VALUE IF NOT EXISTS 'test'"#)
        .execute(pool)
        .await?;
    Ok(())
}

/// `notifications/create` — `NotificationService.createNotification('app')`
/// with the custom body/header/icon + appAccessTokenId columns (the 0002
/// table carries the 1585385921215-custom-notification shape).
pub async fn insert_app_notification(
    pool: &PgPool,
    id: &str,
    user_id: &str,
    custom_body: &str,
    custom_header: Option<&str>,
    custom_icon: Option<&str>,
    app_access_token_id: Option<&str>,
) -> Result<(), sqlx::Error> {
    match insert_app_notification_once(
        pool,
        id,
        user_id,
        custom_body,
        custom_header,
        custom_icon,
        app_access_token_id,
    )
    .await
    {
        Err(e) if is_undefined_table(&e) => {
            ensure_table(pool).await?;
            insert_app_notification_once(
                pool,
                id,
                user_id,
                custom_body,
                custom_header,
                custom_icon,
                app_access_token_id,
            )
            .await
        }
        r => r,
    }
}

async fn insert_app_notification_once(
    pool: &PgPool,
    id: &str,
    user_id: &str,
    custom_body: &str,
    custom_header: Option<&str>,
    custom_icon: Option<&str>,
    app_access_token_id: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO "notification"
            ("id", "createdAt", "notifieeId", "type", "isRead",
             "customBody", "customHeader", "customIcon", "appAccessTokenId")
        VALUES ($1, now(), $2, 'app'::notification_type_enum, FALSE, $3, $4, $5, $6)
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(custom_body)
    .bind(custom_header)
    .bind(custom_icon)
    .bind(app_access_token_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// `notifications/test-notification` — `createNotification('test')`: a
/// notifier-less row (pack shape is `{id, createdAt, type}`).
pub async fn insert_test_notification(
    pool: &PgPool,
    id: &str,
    user_id: &str,
) -> Result<(), sqlx::Error> {
    match insert_test_notification_once(pool, id, user_id).await {
        Err(e) if is_undefined_table(&e) => {
            ensure_table(pool).await?;
            insert_test_notification_once(pool, id, user_id).await
        }
        r => r,
    }
}

async fn insert_test_notification_once(
    pool: &PgPool,
    id: &str,
    user_id: &str,
) -> Result<(), sqlx::Error> {
    ensure_test_type(pool).await?;
    sqlx::query(
        r#"
        INSERT INTO "notification" ("id", "createdAt", "notifieeId", "type", "isRead")
        VALUES ($1, now(), $2, 'test'::notification_type_enum, FALSE)
        "#,
    )
    .bind(id)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}
