//! announcements: db paths behind the announcements domain (upstream
//! `AnnouncementService.ts` + the announcements endpoints). Tables
//! `announcement` / `announcement_read` ship in the 0001 baseline (with the
//! (userId, announcementId) unique index IDX_924fa718), so no new migration
//! is needed.
//!
//! Timestamps: writes go through
//! `TIMESTAMP 'epoch' + $n::bigint * interval '1 millisecond'` (exact at the
//! millisecond precision upstream JS Dates carry); reads/comparisons map
//! timestamptz back with `(EXTRACT(EPOCH FROM col) * 1000)::bigint`, so the
//! now-parameter is a plain bigint and matches upstream (`autoArchiveAt >
//! now` / `<= now` on Date objects).

use crate::sqlx::PgPool;

/// `announcement` columns in AnnouncementEntityService.pack /
/// admin/announcements/list order (timestamptz → epoch ms).
const COLS: &str = r#""id",
(EXTRACT(EPOCH FROM "updatedAt") * 1000)::bigint AS "updated_at_ms",
"text", "title", "imageUrl" AS "image_url", "icon", "display",
"needConfirmationToRead" AS "need_confirmation_to_read",
"isActive" AS "is_active",
(EXTRACT(EPOCH FROM "autoArchiveAt") * 1000)::bigint AS "auto_archive_at_ms",
"forExistingUsers" AS "for_existing_users",
"silence", "userId" AS "user_id""#;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AnnouncementRow {
    pub id: String,
    pub updated_at_ms: Option<i64>,
    pub text: String,
    pub title: String,
    pub image_url: Option<String>,
    pub icon: String,
    pub display: String,
    pub need_confirmation_to_read: bool,
    pub is_active: bool,
    pub auto_archive_at_ms: Option<i64>,
    pub for_existing_users: bool,
    pub silence: bool,
    pub user_id: Option<String>,
}

/// The `createdAt` every announcement pack carries is the id's own timestamp
/// (`idService.parse(announcement.id).date`) — join across crates is the
/// caller's job; this is the first-registereds-user id probe used by the
/// moderator gate (see api::announcement, upstream SignupService L160
/// `if (meta.rootUserId == null) update({ rootUserId: account.id })`).
pub async fn first_registered_user_id(pool: &PgPool) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(r#"SELECT "id" FROM "user" ORDER BY "createdAt" ASC LIMIT 1"#)
        .fetch_optional(pool)
        .await
}

/// AnnouncementEntityService pack is createdAt-free; admin/announcements/list
/// L169 computes `isActive: announcement.isActive && (autoArchiveAt == null
/// || autoArchiveAt > now)` on top of the stored row.
pub fn computed_is_active(row: &AnnouncementRow, now_ms: i64) -> bool {
    row.is_active && row.auto_archive_at_ms.map(|t| t > now_ms).unwrap_or(true)
}

/// `AnnouncementService.create` insert (`announcementsRepository.insertOne`).
/// `is_active` is the endpoint's computed
/// `values.autoArchiveAt == null || values.autoArchiveAt > new Date()`.
#[allow(clippy::too_many_arguments)]
pub async fn insert_announcement(
    pool: &PgPool,
    id: &str,
    title: &str,
    text: &str,
    image_url: Option<&str>,
    icon: &str,
    display: &str,
    need_confirmation_to_read: bool,
    is_active: bool,
    auto_archive_at_ms: Option<i64>,
    for_existing_users: bool,
    silence: bool,
    user_id: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO "announcement"
            ("id", "updatedAt", "text", "title", "imageUrl", "icon", "display",
             "needConfirmationToRead", "isActive", "autoArchiveAt",
             "forExistingUsers", "silence", "userId")
        VALUES ($1, NULL, $2, $3, $4, $5, $6, $7, $8,
                TIMESTAMP 'epoch' + $9::bigint * interval '1 millisecond',
                $10, $11, $12)
        "#,
    )
    .bind(id)
    .bind(text)
    .bind(title)
    .bind(image_url)
    .bind(icon)
    .bind(display)
    .bind(need_confirmation_to_read)
    .bind(is_active)
    .bind(auto_archive_at_ms)
    .bind(for_existing_users)
    .bind(silence)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// `announcementsRepository.findOneBy` (a miss is the caller's
/// NO_SUCH_ANNOUNCEMENT: update/delete re-read the row).
pub async fn find_by_id(pool: &PgPool, id: &str) -> Result<Option<AnnouncementRow>, sqlx::Error> {
    sqlx::query_as::<_, AnnouncementRow>(&format!(
        r#"SELECT {COLS} FROM "announcement" WHERE "id" = $1"#
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// `endpoints/announcements.ts` list: personal (me) announcements on top of
/// the global ones (upstream Brackets `userId = meId OR userId IS NULL`),
/// the `isActive` switch driving the two archive-window halves (`isActive`
/// default true: `isActive AND (autoArchiveAt IS NULL OR autoArchiveAt >
/// now)`; false: the archived union). Returns `(row, forYou, isRead)`;
/// `isRead`/`forYou` are only meaningful with a me (the pack omits both for
/// guests, as upstream's `undefined` does in JSON).
///
/// Pagination mirrors QueryService.makePaginationQuery with ids already
/// normalized (sinceDate/untilDate fold into since/until id boundaries at the
/// api layer): `until` orders DESC, a lone `since` orders ASC, both DESC.
pub async fn list_public(
    pool: &PgPool,
    me_id: Option<&str>,
    is_active: bool,
    since_id: Option<&str>,
    until_id: Option<&str>,
    now_ms: i64,
    limit: i64,
) -> Result<Vec<(AnnouncementRow, bool, bool)>, sqlx::Error> {
    let me = me_id.unwrap_or("");
    // Upstream announcements.ts L60-71: the isActive switch drives two
    // different unions — actives keep the open archive window; "inactive" is
    // the OR-union of deactivated and past their autoArchiveAt.
    let filter = if is_active {
        r#"(a."isActive" AND (a."autoArchiveAt" IS NULL
               OR (EXTRACT(EPOCH FROM a."autoArchiveAt") * 1000)::bigint > $1))"#
    } else {
        r#"(NOT a."isActive" OR (a."autoArchiveAt" IS NOT NULL
               AND (EXTRACT(EPOCH FROM a."autoArchiveAt") * 1000)::bigint <= $1))"#
    };
    let sql = format!(
        r#"
        SELECT {COLS},
               (a."userId" = $3) AS "for_you",
               EXISTS(SELECT 1 FROM "announcement_read" r
                      WHERE r."announcementId" = a."id" AND r."userId" = $3) AS "is_read"
        FROM "announcement" a
        WHERE (a."userId" IS NULL OR a."userId" = $3)
          AND {filter}
          AND ($2::text IS NULL OR a."id" < $2)
          AND ($4::text IS NULL OR a."id" > $4)
        ORDER BY a."id" {}
        LIMIT $5
        "#,
        if until_id.is_none() && since_id.is_some() {
            "ASC"
        } else {
            "DESC"
        },
        filter = filter,
    );
    sqlx::query_as::<_, (AnnouncementRow, bool, bool)>(&sql)
        .bind(now_ms)
        .bind(until_id)
        .bind(me)
        .bind(since_id)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// `endpoints/admin/announcements/list.ts`: status filter (`active` default /
/// `archived` / `all`), the userId scope — a *present* param narrows to ONLY
/// that user's announcements while an absent one narrows to the global
/// (userId IS NULL) set, upstream L144-148 `if (ps.userId) … else … IS NULL`
/// — plus the shared pagination and the per-row read count
/// (`announcementReadsRepository.countBy`). Pagination has the same
/// normalized-boundary rule as list_public.
pub async fn list_admin(
    pool: &PgPool,
    status: &str,
    user_id: Option<&str>,
    since_id: Option<&str>,
    until_id: Option<&str>,
    now_ms: i64,
    limit: i64,
) -> Result<Vec<(AnnouncementRow, i64)>, sqlx::Error> {
    let scope = match status {
        "archived" => {
            r#"(NOT a."isActive" OR (a."autoArchiveAt" IS NOT NULL
                    AND (EXTRACT(EPOCH FROM a."autoArchiveAt") * 1000)::bigint <= $1))"#
        }
        "all" => "TRUE",
        // 'active' (paramDef default): the un-expired actives
        _ => {
            r#"(a."isActive" AND (a."autoArchiveAt" IS NULL
                    OR (EXTRACT(EPOCH FROM a."autoArchiveAt") * 1000)::bigint > $1))"#
        }
    };
    // $2 scope: present param = exactly that user (no global union), absent =
    // the global set. Both halves keep $2 referenced — a placeholder bound
    // but unreferenced in the statement text is a Postgres bind-count error.
    let user_scope = r#"(($2::text IS NOT NULL AND a."userId" = $2)
             OR ($2::text IS NULL AND a."userId" IS NULL))"#;
    let sql = format!(
        r#"
        SELECT {COLS},
               (SELECT count(*) FROM "announcement_read" r
                WHERE r."announcementId" = a."id")::bigint AS "reads"
        FROM "announcement" a
        WHERE ({scope})
          AND ({user_scope})
          AND ($3::text IS NULL OR a."id" < $3)
          AND ($4::text IS NULL OR a."id" > $4)
        ORDER BY a."id" {}
        LIMIT $5
        "#,
        if until_id.is_none() && since_id.is_some() {
            "ASC"
        } else {
            "DESC"
        },
        scope = scope,
        user_scope = user_scope,
    );
    sqlx::query_as::<_, (AnnouncementRow, i64)>(&sql)
        .bind(now_ms)
        .bind(user_id)
        .bind(until_id)
        .bind(since_id)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// One `AnnouncementService.update` write (endpoints/admin/announcements/
/// update.ts → `announcementsRepository.update`). `updatedAt` is stamped and
/// `imageUrl` is ALWAYS written (`ps.imageUrl || null` — the update
/// endpoint's fallthrough clears imageUrl whenever the params omit it);
/// every other column is written only when the params carried it (TypeORM
/// skips `undefined` in the SET clause). Bind order == placeholder order:
/// the SET fragments are numbered sequentially as each optional is bound.
/// `auto_archive_at_ms`: None → absent (keep), Some(None) → cleared,
/// Some(Some(ms)) → set.
#[allow(clippy::too_many_arguments)]
pub async fn update_announcement(
    pool: &PgPool,
    id: &str,
    now_ms: i64,
    image_url: Option<&str>,
    title: Option<&str>,
    text: Option<&str>,
    icon: Option<&str>,
    display: Option<&str>,
    for_existing_users: Option<bool>,
    silence: Option<bool>,
    need_confirmation_to_read: Option<bool>,
    is_active: Option<bool>,
    auto_archive_at_ms: Option<Option<i64>>,
) -> Result<(), sqlx::Error> {
    // $1 updatedAt, $2 imageUrl (both unconditional) — announced up front so
    // the optional numbering below starts at 3.
    let mut sets: Vec<String> = vec![
        "\"updatedAt\" = TIMESTAMP 'epoch' + $1::bigint * interval '1 millisecond'".to_string(),
        "\"imageUrl\" = $2".to_string(),
    ];
    let mut n = 2;
    let mut title_n = 0;
    let mut text_n = 0;
    let mut icon_n = 0;
    let mut display_n = 0;
    let mut foe_n = 0;
    let mut silence_n = 0;
    let mut need_n = 0;
    let mut active_n = 0;
    let mut archive_n = 0;
    if title.is_some() {
        n += 1;
        title_n = n;
        sets.push(format!("\"title\" = ${n}"));
    }
    if text.is_some() {
        n += 1;
        text_n = n;
        sets.push(format!("\"text\" = ${n}"));
    }
    if icon.is_some() {
        n += 1;
        icon_n = n;
        sets.push(format!("\"icon\" = ${n}"));
    }
    if display.is_some() {
        n += 1;
        display_n = n;
        sets.push(format!("\"display\" = ${n}"));
    }
    if for_existing_users.is_some() {
        n += 1;
        foe_n = n;
        sets.push(format!("\"forExistingUsers\" = ${n}"));
    }
    if silence.is_some() {
        n += 1;
        silence_n = n;
        sets.push(format!("\"silence\" = ${n}"));
    }
    if need_confirmation_to_read.is_some() {
        n += 1;
        need_n = n;
        sets.push(format!("\"needConfirmationToRead\" = ${n}"));
    }
    if is_active.is_some() {
        n += 1;
        active_n = n;
        sets.push(format!("\"isActive\" = ${n}"));
    }
    if auto_archive_at_ms.is_some() {
        n += 1;
        archive_n = n;
        sets.push(format!(
            "\"autoArchiveAt\" = TIMESTAMP 'epoch' + ${n}::bigint * interval '1 millisecond'"
        ));
    }
    n += 1;
    let id_n = n;
    let sql = format!(
        "UPDATE \"announcement\" SET {} WHERE \"id\" = ${id_n}",
        sets.join(", ")
    );

    let mut q = sqlx::query(&sql).bind(now_ms).bind(image_url);
    if title_n != 0 {
        q = q.bind(title);
    }
    if text_n != 0 {
        q = q.bind(text);
    }
    if icon_n != 0 {
        q = q.bind(icon);
    }
    if display_n != 0 {
        q = q.bind(display);
    }
    if foe_n != 0 {
        q = q.bind(for_existing_users);
    }
    if silence_n != 0 {
        q = q.bind(silence);
    }
    if need_n != 0 {
        q = q.bind(need_confirmation_to_read);
    }
    if active_n != 0 {
        q = q.bind(is_active);
    }
    if archive_n != 0 {
        q = q.bind(auto_archive_at_ms.flatten());
    }
    q.bind(id).execute(pool).await?;
    Ok(())
}

/// `AnnouncementService.delete` row write (the moderation log is the api
/// layer's concern).
pub async fn delete_announcement(pool: &PgPool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "announcement" WHERE "id" = $1"#)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// `AnnouncementService.read` writes: insert the read row (the
/// (userId, announcementId) unique index makes duplicates silent through DO
/// NOTHING), then flip the read announcement's `isActive` off when it was a
/// personal one (`if (announcement.userId === user.id) → isActive: false`).
/// Like upstream's try/catch around `announcementReadsRepository.insert`
/// (dup key *and* FK miss both land there → plain return), an insert error
/// ends the read silently with the deactivation skipped.
pub async fn read(
    pool: &PgPool,
    id: &str,
    user_id: &str,
    announcement_id: &str,
) -> Result<(), sqlx::Error> {
    let ok = sqlx::query(
        r#"
        INSERT INTO "announcement_read" ("id", "userId", "announcementId")
        VALUES ($1, $2, $3)
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(announcement_id)
    .execute(pool)
    .await;
    if ok.is_err() {
        // upstream `catch (_) { return; }` — the deactivation never runs
        return Ok(());
    }
    sqlx::query(
        r#"UPDATE "announcement" SET "isActive" = FALSE WHERE "id" = $1 AND "userId" = $2"#,
    )
    .bind(announcement_id)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// `AnnouncementService.getUnreadAnnouncements` verbatim: active (with the
/// auto-archive window), not silenced, global-or-personal, not
/// for-existing-users-only past this account (`announcement.id > userId`),
/// and not yet read. Feeds the MeDetailed
/// `unreadAnnouncements`/`hasUnreadAnnouncement` keys (the pack side lands
/// with the MeDetailed lane; this is its db path).
pub async fn unread_for_user(
    pool: &PgPool,
    user_id: &str,
    now_ms: i64,
) -> Result<Vec<AnnouncementRow>, sqlx::Error> {
    let sql = format!(
        r#"
        SELECT {COLS}
        FROM "announcement" a
        WHERE a."isActive"
          AND (a."autoArchiveAt" IS NULL
               OR (EXTRACT(EPOCH FROM a."autoArchiveAt") * 1000)::bigint > $2)
          AND a."silence" = FALSE
          AND (a."userId" = $1 OR a."userId" IS NULL)
          AND (a."forExistingUsers" = FALSE OR a."id" > $1)
          AND a."id" NOT IN (
              SELECT r."announcementId" FROM "announcement_read" r
              WHERE r."userId" = $1)
        "#,
    );
    sqlx::query_as::<_, AnnouncementRow>(&sql)
        .bind(user_id)
        .bind(now_ms)
        .fetch_all(pool)
        .await
}

/// `AnnouncementService.getAnnouncement`'s read probe: the me view passes
/// `{ ...announcement, isRead: read !== null }` to the pack.
pub async fn is_read(
    pool: &PgPool,
    user_id: &str,
    announcement_id: &str,
) -> Result<bool, sqlx::Error> {
    let v: Option<i32> = sqlx::query_scalar(
        r#"SELECT 1 FROM "announcement_read" WHERE "userId" = $1 AND "announcementId" = $2"#,
    )
    .bind(user_id)
    .bind(announcement_id)
    .fetch_optional(pool)
    .await?;
    Ok(v.is_some())
}
