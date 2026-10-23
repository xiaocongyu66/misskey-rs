//! chat: rooms / memberships / invitations / messages
//! (upstream core/ChatService.ts + endpoints/chat/*).
//!
//! Tables ship in the 0001 baseline — no migration here:
//! - `chat_room` (id, name, ownerId, description, isArchived)
//! - `chat_message` (id, fromUserId, toUserId, toRoomId, text, uri, reads,
//!   fileId, reactions) — 1on1 rows carry `toUserId`, room rows `toRoomId`
//! - `chat_room_membership` (id, userId, roomId, isMuted) — the owner is NOT
//!   a membership row upstream (ChatService concatenates the owner manually)
//! - `chat_room_invitation` (id, userId, roomId, ignored)
//! - `chat_approval` (id, userId, otherId) — the "I approved this 1on1
//!   partner" row ChatService.createMessageToUser inserts
//!
//! Aid ids sort monotonically as plain strings, so sinceId/untilId paging is
//! a string compare; bounds are EXCLUSIVE (`makePaginationQuery`), sinceId-only
//! → oldest-first ASC, otherwise newest-first DESC. `createdAt` is derived
//! from the aid prefix by the api layer (no column upstream).

use sqlx::PgPool;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ChatRoomRow {
    pub id: String,
    pub name: String,
    #[sqlx(rename = "ownerId")]
    pub owner_id: String,
    pub description: String,
    #[sqlx(rename = "isArchived")]
    pub is_archived: bool,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ChatMessageRow {
    pub id: String,
    #[sqlx(rename = "fromUserId")]
    pub from_user_id: String,
    #[sqlx(rename = "toUserId")]
    pub to_user_id: Option<String>,
    #[sqlx(rename = "toRoomId")]
    pub to_room_id: Option<String>,
    pub text: Option<String>,
    pub uri: Option<String>,
    pub reads: Vec<String>,
    #[sqlx(rename = "fileId")]
    pub file_id: Option<String>,
    pub reactions: Vec<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ChatMembershipRow {
    pub id: String,
    #[sqlx(rename = "userId")]
    pub user_id: String,
    #[sqlx(rename = "roomId")]
    pub room_id: String,
    #[sqlx(rename = "isMuted")]
    pub is_muted: bool,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ChatInvitationRow {
    pub id: String,
    #[sqlx(rename = "userId")]
    pub user_id: String,
    #[sqlx(rename = "roomId")]
    pub room_id: String,
    pub ignored: bool,
}

const ROOM_COLS: &str = r#""id", "name", "ownerId", "description", "isArchived""#;

const MESSAGE_COLS: &str =
    r#""id", "fromUserId", "toUserId", "toRoomId", "text", "uri", "reads", "fileId", "reactions""#;

const MEMBERSHIP_COLS: &str = r#""id", "userId", "roomId", "isMuted""#;

const INVITATION_COLS: &str = r#""id", "userId", "roomId", "ignored""#;

/// Upstream `MAX_ROOM_MEMBERS` (ChatService.ts).
pub const MAX_ROOM_MEMBERS: i64 = 50;

// ==== rooms ================================================================

/// `chatRoomsRepository.findOne({ where: { id }, relations: { owner: true } })`
/// — the owner relation is packed from the user table by the api layer.
pub async fn find_room(pool: &PgPool, room_id: &str) -> Result<Option<ChatRoomRow>, sqlx::Error> {
    sqlx::query_as::<_, ChatRoomRow>(&format!(
        r#"SELECT {ROOM_COLS} FROM "chat_room" WHERE "id" = $1"#
    ))
    .bind(room_id)
    .fetch_optional(pool)
    .await
}

/// `chatRoomsRepository.findOneBy({ id, ownerId })` (findMyRoomById).
pub async fn find_owned_room(
    pool: &PgPool,
    room_id: &str,
    owner_id: &str,
) -> Result<Option<ChatRoomRow>, sqlx::Error> {
    sqlx::query_as::<_, ChatRoomRow>(&format!(
        r#"SELECT {ROOM_COLS} FROM "chat_room" WHERE "id" = $1 AND "ownerId" = $2"#
    ))
    .bind(room_id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await
}

/// ChatService.createRoom — description defaults to '' upstream.
pub async fn insert_room(
    pool: &PgPool,
    id: &str,
    owner_id: &str,
    name: &str,
    description: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO "chat_room" ("id", "name", "ownerId", "description")
           VALUES ($1, $2, $3, $4)"#,
    )
    .bind(id)
    .bind(name)
    .bind(owner_id)
    .bind(description)
    .execute(pool)
    .await
    .map(|_| ())
}

/// ChatService.updateRoom — `UPDATE ... SET params WHERE id` (undefined keys
/// are omitted upstream; NULL binds leave the column untouched here).
pub async fn update_room(
    pool: &PgPool,
    room_id: &str,
    name: Option<&str>,
    description: Option<&str>,
) -> Result<Option<ChatRoomRow>, sqlx::Error> {
    sqlx::query_as::<_, ChatRoomRow>(&format!(
        r#"UPDATE "chat_room"
           SET "name" = COALESCE($2, "name"),
               "description" = COALESCE($3, "description")
           WHERE "id" = $1
           RETURNING {ROOM_COLS}"#
    ))
    .bind(room_id)
    .bind(name)
    .bind(description)
    .fetch_optional(pool)
    .await
}

// ==== memberships ==========================================================

/// `chatRoomMembershipsRepository.findOneBy({ roomId, userId })`.
pub async fn find_membership(
    pool: &PgPool,
    room_id: &str,
    user_id: &str,
) -> Result<Option<ChatMembershipRow>, sqlx::Error> {
    sqlx::query_as::<_, ChatMembershipRow>(&format!(
        r#"SELECT {MEMBERSHIP_COLS} FROM "chat_room_membership"
           WHERE "roomId" = $1 AND "userId" = $2"#
    ))
    .bind(room_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

pub async fn membership_exists(
    pool: &PgPool,
    room_id: &str,
    user_id: &str,
) -> Result<bool, sqlx::Error> {
    let n: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "chat_room_membership"
           WHERE "roomId" = $1 AND "userId" = $2"#,
    )
    .bind(room_id)
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

/// `chatRoomMembershipsRepository.countBy({ roomId })` (MAX_ROOM_MEMBERS gate).
pub async fn count_memberships(pool: &PgPool, room_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(r#"SELECT count(*) FROM "chat_room_membership" WHERE "roomId" = $1"#)
        .bind(room_id)
        .fetch_one(pool)
        .await
}

pub async fn insert_membership(
    pool: &PgPool,
    id: &str,
    room_id: &str,
    user_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO "chat_room_membership" ("id", "roomId", "userId")
           VALUES ($1, $2, $3)"#,
    )
    .bind(id)
    .bind(room_id)
    .bind(user_id)
    .execute(pool)
    .await
    .map(|_| ())
}

pub async fn delete_membership(
    pool: &PgPool,
    room_id: &str,
    user_id: &str,
) -> Result<u64, sqlx::Error> {
    sqlx::query(r#"DELETE FROM "chat_room_membership" WHERE "roomId" = $1 AND "userId" = $2"#)
        .bind(room_id)
        .bind(user_id)
        .execute(pool)
        .await
        .map(|r| r.rows_affected())
}

/// ChatService.muteRoom — flip `isMuted` on the caller's membership row.
/// Upstream's `findOneByOrFail({ roomId, userId })` throws a non-ApiError
/// when no membership exists (owners included); the api layer maps that to
/// INTERNAL_ERROR. Returns rows_affected so the caller can do the same.
pub async fn set_membership_muted(
    pool: &PgPool,
    room_id: &str,
    user_id: &str,
    muted: bool,
) -> Result<u64, sqlx::Error> {
    sqlx::query(
        r#"UPDATE "chat_room_membership" SET "isMuted" = $3
           WHERE "roomId" = $1 AND "userId" = $2"#,
    )
    .bind(room_id)
    .bind(user_id)
    .bind(muted)
    .execute(pool)
    .await
    .map(|r| r.rows_affected())
}

/// ChatService.getMyMemberships (paged).
pub async fn memberships_for_user(
    pool: &PgPool,
    user_id: &str,
    limit: i64,
    since_id: Option<&str>,
    until_id: Option<&str>,
) -> Result<Vec<ChatMembershipRow>, sqlx::Error> {
    let base =
        format!(r#"SELECT {MEMBERSHIP_COLS} FROM "chat_room_membership" WHERE "userId" = $1"#);
    paged(pool, &base, user_id, limit, since_id, until_id).await
}

/// ChatService.getRoomMembershipsWithPagination (paged).
pub async fn memberships_for_room(
    pool: &PgPool,
    room_id: &str,
    limit: i64,
    since_id: Option<&str>,
    until_id: Option<&str>,
) -> Result<Vec<ChatMembershipRow>, sqlx::Error> {
    let base =
        format!(r#"SELECT {MEMBERSHIP_COLS} FROM "chat_room_membership" WHERE "roomId" = $1"#);
    paged(pool, &base, room_id, limit, since_id, until_id).await
}

/// Shared `makePaginationQuery` shape: 4 exclusive-bound branches.
async fn paged(
    pool: &PgPool,
    base: &str,
    key: &str,
    limit: i64,
    since_id: Option<&str>,
    until_id: Option<&str>,
) -> Result<Vec<ChatMembershipRow>, sqlx::Error> {
    let sql = match (since_id, until_id) {
        (Some(_), Some(_)) => {
            format!(r#"{base} AND "id" > $2 AND "id" < $3 ORDER BY "id" DESC LIMIT $4"#)
        }
        (Some(_), None) => format!(r#"{base} AND "id" > $2 ORDER BY "id" ASC LIMIT $3"#),
        (None, Some(_)) => format!(r#"{base} AND "id" < $2 ORDER BY "id" DESC LIMIT $3"#),
        (None, None) => format!(r#"{base} ORDER BY "id" DESC LIMIT $2"#),
    };
    let mut q = sqlx::query_as::<_, ChatMembershipRow>(&sql).bind(key);
    match (since_id, until_id) {
        (Some(s), Some(u)) => {
            q = q.bind(s).bind(u).bind(limit);
        }
        (Some(s), None) | (None, Some(s)) => {
            q = q.bind(s).bind(limit);
        }
        (None, None) => {
            q = q.bind(limit);
        }
    }
    q.fetch_all(pool).await
}

// ==== invitations ==========================================================

/// `chatRoomInvitationsRepository.findOneBy({ roomId, userId })`.
pub async fn find_invitation(
    pool: &PgPool,
    room_id: &str,
    user_id: &str,
) -> Result<Option<ChatInvitationRow>, sqlx::Error> {
    sqlx::query_as::<_, ChatInvitationRow>(&format!(
        r#"SELECT {INVITATION_COLS} FROM "chat_room_invitation"
           WHERE "roomId" = $1 AND "userId" = $2"#
    ))
    .bind(room_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

pub async fn insert_invitation(
    pool: &PgPool,
    id: &str,
    room_id: &str,
    user_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO "chat_room_invitation" ("id", "roomId", "userId")
           VALUES ($1, $2, $3)"#,
    )
    .bind(id)
    .bind(room_id)
    .bind(user_id)
    .execute(pool)
    .await
    .map(|_| ())
}

pub async fn delete_invitation(pool: &PgPool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "chat_room_invitation" WHERE "id" = $1"#)
        .bind(id)
        .execute(pool)
        .await
        .map(|_| ())
}

/// ChatService.ignoreRoomInvitation — `update(id, { ignored: true })`.
pub async fn ignore_invitation(pool: &PgPool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "chat_room_invitation" SET "ignored" = TRUE WHERE "id" = $1"#)
        .bind(id)
        .execute(pool)
        .await
        .map(|_| ())
}

/// ChatService.getReceivedRoomInvitationsWithPagination (ignored = FALSE).
pub async fn invitations_received(
    pool: &PgPool,
    user_id: &str,
    limit: i64,
    since_id: Option<&str>,
    until_id: Option<&str>,
) -> Result<Vec<ChatInvitationRow>, sqlx::Error> {
    let base = format!(
        r#"SELECT {INVITATION_COLS} FROM "chat_room_invitation"
           WHERE "userId" = $1 AND "ignored" = FALSE"#
    );
    paged_invitations(pool, &base, user_id, limit, since_id, until_id).await
}

/// ChatService.getSentRoomInvitationsWithPagination.
pub async fn invitations_sent(
    pool: &PgPool,
    room_id: &str,
    limit: i64,
    since_id: Option<&str>,
    until_id: Option<&str>,
) -> Result<Vec<ChatInvitationRow>, sqlx::Error> {
    let base =
        format!(r#"SELECT {INVITATION_COLS} FROM "chat_room_invitation" WHERE "roomId" = $1"#);
    paged_invitations(pool, &base, room_id, limit, since_id, until_id).await
}

async fn paged_invitations(
    pool: &PgPool,
    base: &str,
    key: &str,
    limit: i64,
    since_id: Option<&str>,
    until_id: Option<&str>,
) -> Result<Vec<ChatInvitationRow>, sqlx::Error> {
    let sql = match (since_id, until_id) {
        (Some(_), Some(_)) => {
            format!(r#"{base} AND "id" > $2 AND "id" < $3 ORDER BY "id" DESC LIMIT $4"#)
        }
        (Some(_), None) => format!(r#"{base} AND "id" > $2 ORDER BY "id" ASC LIMIT $3"#),
        (None, Some(_)) => format!(r#"{base} AND "id" < $2 ORDER BY "id" DESC LIMIT $3"#),
        (None, None) => format!(r#"{base} ORDER BY "id" DESC LIMIT $2"#),
    };
    let mut q = sqlx::query_as::<_, ChatInvitationRow>(&sql).bind(key);
    match (since_id, until_id) {
        (Some(s), Some(u)) => {
            q = q.bind(s).bind(u).bind(limit);
        }
        (Some(s), None) | (None, Some(s)) => {
            q = q.bind(s).bind(limit);
        }
        (None, None) => {
            q = q.bind(limit);
        }
    }
    q.fetch_all(pool).await
}

// ==== approvals ============================================================

/// The `(userId, otherId)` pair probe ChatService.createMessageToUser uses to
/// decide whether either side already approved the 1on1 conversation.
pub async fn approval_exists(
    pool: &PgPool,
    user_id: &str,
    other_id: &str,
) -> Result<bool, sqlx::Error> {
    let n: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "chat_approval"
           WHERE "userId" = $1 AND "otherId" = $2"#,
    )
    .bind(user_id)
    .bind(other_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

pub async fn insert_approval(
    pool: &PgPool,
    id: &str,
    user_id: &str,
    other_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO "chat_approval" ("id", "userId", "otherId")
           VALUES ($1, $2, $3) ON CONFLICT DO NOTHING"#,
    )
    .bind(id)
    .bind(user_id)
    .bind(other_id)
    .execute(pool)
    .await
    .map(|_| ())
}

// ==== messages =============================================================

pub struct NewChatMessage<'a> {
    pub id: &'a str,
    pub from_user_id: &'a str,
    pub to_user_id: Option<&'a str>,
    pub to_room_id: Option<&'a str>,
    pub text: Option<&'a str>,
    pub file_id: Option<&'a str>,
}

pub async fn insert_message(pool: &PgPool, m: &NewChatMessage<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO "chat_message"
             ("id", "fromUserId", "toUserId", "toRoomId", "text", "fileId")
           VALUES ($1, $2, $3, $4, $5, $6)"#,
    )
    .bind(m.id)
    .bind(m.from_user_id)
    .bind(m.to_user_id)
    .bind(m.to_room_id)
    .bind(m.text)
    .bind(m.file_id)
    .execute(pool)
    .await
    .map(|_| ())
}

pub async fn find_message(
    pool: &PgPool,
    message_id: &str,
) -> Result<Option<ChatMessageRow>, sqlx::Error> {
    sqlx::query_as::<_, ChatMessageRow>(&format!(
        r#"SELECT {MESSAGE_COLS} FROM "chat_message" WHERE "id" = $1"#
    ))
    .bind(message_id)
    .fetch_optional(pool)
    .await
}

/// `findMyMessageById` — messages/delete only deletes the sender's rows.
pub async fn find_message_from_user(
    pool: &PgPool,
    message_id: &str,
    from_user_id: &str,
) -> Result<Option<ChatMessageRow>, sqlx::Error> {
    sqlx::query_as::<_, ChatMessageRow>(&format!(
        r#"SELECT {MESSAGE_COLS} FROM "chat_message"
           WHERE "id" = $1 AND "fromUserId" = $2"#
    ))
    .bind(message_id)
    .bind(from_user_id)
    .fetch_optional(pool)
    .await
}

pub async fn delete_message(pool: &PgPool, message_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "chat_message" WHERE "id" = $1"#)
        .bind(message_id)
        .execute(pool)
        .await
        .map(|_| ())
}

/// ChatService.roomTimeline (paged, newest-first default).
pub async fn room_messages(
    pool: &PgPool,
    room_id: &str,
    limit: i64,
    since_id: Option<&str>,
    until_id: Option<&str>,
) -> Result<Vec<ChatMessageRow>, sqlx::Error> {
    let base = format!(r#"SELECT {MESSAGE_COLS} FROM "chat_message" WHERE "toRoomId" = $1"#);
    paged_messages(pool, &base, room_id, limit, since_id, until_id).await
}

/// `drive/files/attached-chat-messages` (endpoints/drive/files/attached-
/// chat-messages.ts): `makePaginationQuery(message).andWhere('message.fileId
/// = :fileId')` — the same paged shape as room_messages keyed on fileId.
pub async fn file_messages(
    pool: &PgPool,
    file_id: &str,
    limit: i64,
    since_id: Option<&str>,
    until_id: Option<&str>,
) -> Result<Vec<ChatMessageRow>, sqlx::Error> {
    let base = format!(r#"SELECT {MESSAGE_COLS} FROM "chat_message" WHERE "fileId" = $1"#);
    paged_messages(pool, &base, file_id, limit, since_id, until_id).await
}

async fn paged_messages(
    pool: &PgPool,
    base: &str,
    key: &str,
    limit: i64,
    since_id: Option<&str>,
    until_id: Option<&str>,
) -> Result<Vec<ChatMessageRow>, sqlx::Error> {
    let sql = match (since_id, until_id) {
        (Some(_), Some(_)) => {
            format!(r#"{base} AND "id" > $2 AND "id" < $3 ORDER BY "id" DESC LIMIT $4"#)
        }
        (Some(_), None) => format!(r#"{base} AND "id" > $2 ORDER BY "id" ASC LIMIT $3"#),
        (None, Some(_)) => format!(r#"{base} AND "id" < $2 ORDER BY "id" DESC LIMIT $3"#),
        (None, None) => format!(r#"{base} ORDER BY "id" DESC LIMIT $2"#),
    };
    let mut q = sqlx::query_as::<_, ChatMessageRow>(&sql).bind(key);
    match (since_id, until_id) {
        (Some(s), Some(u)) => {
            q = q.bind(s).bind(u).bind(limit);
        }
        (Some(s), None) | (None, Some(s)) => {
            q = q.bind(s).bind(limit);
        }
        (None, None) => {
            q = q.bind(limit);
        }
    }
    q.fetch_all(pool).await
}

/// ChatService.userHistory — newest message per distinct 1on1 counterpart,
/// muted users excluded, then the whole set ordered newest-first.
pub async fn user_history(
    pool: &PgPool,
    user_id: &str,
    limit: i64,
) -> Result<Vec<ChatMessageRow>, sqlx::Error> {
    sqlx::query_as::<_, ChatMessageRow>(&format!(
        r#"SELECT * FROM (
             SELECT DISTINCT ON (CASE WHEN "fromUserId" = $1 THEN "toUserId" ELSE "fromUserId" END)
               {MESSAGE_COLS}
             FROM "chat_message"
             WHERE ("fromUserId" = $1 OR "toUserId" = $1)
               AND "toRoomId" IS NULL
               AND "fromUserId" NOT IN (SELECT "muteeId" FROM "muting" WHERE "muterId" = $1)
               AND ("toUserId" IS NULL OR "toUserId" NOT IN (SELECT "muteeId" FROM "muting" WHERE "muterId" = $1))
             ORDER BY CASE WHEN "fromUserId" = $1 THEN "toUserId" ELSE "fromUserId" END, "id" DESC
           ) t ORDER BY "id" DESC LIMIT $2"#
    ))
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// ChatService.userTimeline — 1on1 messages between me and one other user in
/// both directions, paged (same makePaginationQuery shape as room_messages).
pub async fn user_timeline(
    pool: &PgPool,
    me_id: &str,
    other_id: &str,
    limit: i64,
    since_id: Option<&str>,
    until_id: Option<&str>,
) -> Result<Vec<ChatMessageRow>, sqlx::Error> {
    let base = format!(
        r#"SELECT {MESSAGE_COLS} FROM "chat_message"
           WHERE (("fromUserId" = $1 AND "toUserId" = $2)
               OR ("fromUserId" = $2 AND "toUserId" = $1))"#
    );
    let sql = match (since_id, until_id) {
        (Some(_), Some(_)) => {
            format!(r#"{base} AND "id" > $3 AND "id" < $4 ORDER BY "id" DESC LIMIT $5"#)
        }
        (Some(_), None) => format!(r#"{base} AND "id" > $3 ORDER BY "id" ASC LIMIT $4"#),
        (None, Some(_)) => format!(r#"{base} AND "id" < $3 ORDER BY "id" DESC LIMIT $4"#),
        (None, None) => format!(r#"{base} ORDER BY "id" DESC LIMIT $3"#),
    };
    let mut q = sqlx::query_as::<_, ChatMessageRow>(&sql)
        .bind(me_id)
        .bind(other_id);
    match (since_id, until_id) {
        (Some(s), Some(u)) => {
            q = q.bind(s).bind(u).bind(limit);
        }
        (Some(s), None) | (None, Some(s)) => {
            q = q.bind(s).bind(limit);
        }
        (None, None) => {
            q = q.bind(limit);
        }
    }
    q.fetch_all(pool).await
}

/// ChatService.deleteRoom — upstream relies on `onDelete: 'CASCADE'` FKs
/// (membership/invitation/message → room) that this schema does not carry,
/// so the dependent rows are deleted explicitly; the Redis unread-key
/// cleanup is a no-op (see the api chat module header).
pub async fn delete_room(pool: &PgPool, room_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "chat_message" WHERE "toRoomId" = $1"#)
        .bind(room_id)
        .execute(pool)
        .await?;
    sqlx::query(r#"DELETE FROM "chat_room_invitation" WHERE "roomId" = $1"#)
        .bind(room_id)
        .execute(pool)
        .await?;
    sqlx::query(r#"DELETE FROM "chat_room_membership" WHERE "roomId" = $1"#)
        .bind(room_id)
        .execute(pool)
        .await?;
    sqlx::query(r#"DELETE FROM "chat_room" WHERE "id" = $1"#)
        .bind(room_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// ChatService.roomHistory — newest message per joined/owned room.
pub async fn room_history(
    pool: &PgPool,
    user_id: &str,
    limit: i64,
) -> Result<Vec<ChatMessageRow>, sqlx::Error> {
    sqlx::query_as::<_, ChatMessageRow>(&format!(
        r#"SELECT * FROM (
             SELECT DISTINCT ON ("toRoomId") {MESSAGE_COLS}
             FROM "chat_message"
             WHERE "toRoomId" IN (
               SELECT "roomId" FROM "chat_room_membership" WHERE "userId" = $1
               UNION
               SELECT "id" FROM "chat_room" WHERE "ownerId" = $1
             )
             ORDER BY "toRoomId", "id" DESC
           ) t ORDER BY "id" DESC LIMIT $2"#
    ))
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

// ==== reactions + search (chat/messages/{react,unreact,search}) ====

/// ChatService.react: append the `{userId}/{reaction}` pair (deduped).
pub async fn add_reaction(pool: &PgPool, message_id: &str, pair: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"UPDATE "chat_message" SET "reactions" = array_append("reactions", $2)
           WHERE "id" = $1 AND NOT ($2 = ANY("reactions"))"#,
    )
    .bind(message_id)
    .bind(pair)
    .execute(pool)
    .await?;
    Ok(())
}

/// ChatService.unreact: `array_remove`.
pub async fn remove_reaction(
    pool: &PgPool,
    message_id: &str,
    pair: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"UPDATE "chat_message" SET "reactions" = array_remove("reactions", $2)
           WHERE "id" = $1"#,
    )
    .bind(message_id)
    .bind(pair)
    .execute(pool)
    .await?;
    Ok(())
}

/// ChatService.searchMessages: LOWER(text) LIKE over messages the caller can
/// see (own 1on1 side, or a room they belong to), newest first.
pub async fn search_messages(
    pool: &PgPool,
    me_id: &str,
    query: &str,
    limit: i64,
    user_id: Option<&str>,
    room_id: Option<&str>,
) -> Result<Vec<ChatMessageRow>, sqlx::Error> {
    sqlx::query_as::<_, ChatMessageRow>(&format!(
        r#"SELECT {MESSAGE_COLS} FROM "chat_message" m
           WHERE (
                 m."fromUserId" = $1
                 OR m."toUserId" = $1
                 OR m."toRoomId" IN (
                     SELECT "roomId" FROM "chat_room_membership" WHERE "userId" = $1
                 )
                 OR m."toRoomId" IN (
                     SELECT "id" FROM "chat_room" WHERE "ownerId" = $1
                 )
               )
             AND m."text" IS NOT NULL
             AND lower(m."text") LIKE '%' || lower($2) || '%'
             AND ($3::varchar IS NULL OR m."fromUserId" = $3 OR m."toUserId" = $3)
             AND ($4::varchar IS NULL OR m."toRoomId" = $4)
           ORDER BY m."id" DESC
           LIMIT $5"#
    ))
    .bind(me_id)
    .bind(query)
    .bind(user_id)
    .bind(room_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}
