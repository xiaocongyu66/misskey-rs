//! chat domain (upstream `server/api/endpoints/chat/*` +
//! `core/ChatService.ts` / `core/entities/ChatEntityService.ts`).
//!
//! Core slice: rooms (create/show/update/join/leave/joining/members), room
//! invitations (create/ignore) and the message plane (create-to-user,
//! create-to-room, room-timeline, show, delete, history). Db paths live in
//! `mk_db::chat` (all five chat tables ship in the 0001 baseline).
//!
//! DEVIATIONS (documented):
//! - `checkChatAvailability` (RoleService policy `chatAvailability`) is not
//!   re-evaluated here: DEFAULT_POLICIES carries `available`, and the role
//!   engine has no chat-specific test lane. Every endpoint behaves as
//!   read+write available.
//! - The unread markers (`newUserChatMessageExists:*` /
//!   `newRoomChatMessageExists:*`) are Redis keys upstream; this server has
//!   no Redis client, so `chat/history` packs `isRead: true` and the
//!   readRoomChatMessage (chat/messages/user-timeline) / readRoomChatMessage
//!   side effects are no-ops.
//! - Moderator overrides on show/members/message-show are not ported (the
//!   role engine has no isModerator probe on this path); owner/member/
//!   invitation scoping matches upstream for ordinary users.
//! - `chat/messages/react` / `unreact` store `"<userId>/<reaction>"` entries
//!   in the `reactions` array (ChatService.react/unreact). Access failures
//!   collapse to NO_SUCH_MESSAGE like upstream (existence is not disclosed).
//!   Custom `:name:` emojis are accepted without a local emoji-registry
//!   check; other unicode input is normalized through
//!   `social::normalize_reaction` (invalid input falls back to ❤ instead of
//!   upstream's generic 500).
//! - `chat/messages/search` mirrors `ChatService.searchMessages` (LOWER(text)
//!   LIKE with `sqlLikeEscape`); `chat/read-all` is a 204 no-op because the
//!   `newChatMessagesExists:*` markers exist only in Redis upstream.
//! - Chat streams/notifications (`publishChat*Stream`, push, 3s unread
//!   fanout) have no port-side consumer and stay out.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

use mk_db::chat::{
    ChatInvitationRow, ChatMembershipRow, ChatMessageRow, ChatRoomRow, MAX_ROOM_MEMBERS,
};

/// Endpoint names owned by this module (the chat dispatch arm in mod.rs).
pub(crate) const CHAT_ENDPOINTS: &[&str] = &[
    "chat/rooms/create",
    "chat/rooms/join",
    "chat/rooms/leave",
    "chat/rooms/joining",
    "chat/rooms/show",
    "chat/rooms/update",
    "chat/rooms/members",
    "chat/rooms/invitations/create",
    "chat/rooms/invitations/ignore",
    "chat/rooms/invitations/inbox",
    "chat/messages/create-to-user",
    "chat/messages/create-to-room",
    "chat/messages/room-timeline",
    "chat/messages/user-timeline",
    "chat/messages/show",
    "chat/messages/delete",
    "chat/messages/react",
    "chat/messages/unreact",
    "chat/messages/search",
    "chat/read-all",
    "chat/history",
];

pub(crate) fn handles(name: &str) -> bool {
    CHAT_ENDPOINTS.contains(&name)
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_chat", error = %e, "db error: {context}");
    api_err(ApiError::internal_error())
}

/// Meta error without httpStatusCode: kind=client → default 400.
fn meta_err(code: &str, id: &str, message: &str) -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        code,
        id,
        message,
    ))
}

fn invalid_param(param: &str, reason: &str) -> Response {
    api_err(ApiError::invalid_param(json!({
        "param": param,
        "reason": reason,
    })))
}

fn ok_json(v: Value) -> Response {
    (StatusCode::OK, Json(v)).into_response()
}

fn ok_empty() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

fn no_such_room(id: &str) -> Response {
    meta_err("NO_SUCH_ROOM", id, "No such room.")
}

fn no_such_message(id: &str) -> Response {
    meta_err("NO_SUCH_MESSAGE", id, "No such message.")
}

fn opt_str<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(|v| v.as_str())
}

fn required_id<'a>(body: &'a Value, key: &'a str) -> Result<&'a str, Response> {
    opt_str(body, key).ok_or_else(|| invalid_param(key, "required (misskey:id format)"))
}

/// `limit` integer 1..=100 with the endpoint-specific default.
fn limit_param(body: &Value, default: i64) -> Result<i64, Response> {
    match body.get("limit") {
        None | Some(Value::Null) => Ok(default),
        Some(v) => match v.as_i64() {
            Some(n) if (1..=100).contains(&n) => Ok(n),
            _ => Err(invalid_param(
                "limit",
                "must be an integer between 1 and 100",
            )),
        },
    }
}

/// `ps.sinceId ?? (ps.sinceDate ? gen(sinceDate) : null)` (same for until).
fn page_id(body: &Value, id_key: &str, date_key: &str) -> Result<Option<String>, Response> {
    if let Some(v) = body.get(id_key) {
        match v {
            Value::Null => {}
            Value::String(s) => return Ok(Some(s.clone())),
            _ => return Err(invalid_param(id_key, "expected a misskey:id string")),
        }
    }
    match body.get(date_key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_i64() {
            // falsy 0 → null (upstream `ps.sinceDate ? ... : null`)
            Some(0) => Ok(None),
            Some(ms) => Ok(Some(mk_types::id::gen_aid(ms))),
            None => Err(invalid_param(date_key, "expected an integer (epoch ms)")),
        },
    }
}

fn page_ids(body: &Value) -> Result<(Option<String>, Option<String>), Response> {
    Ok((
        page_id(body, "sinceId", "sinceDate")?,
        page_id(body, "untilId", "untilDate")?,
    ))
}

/// Optional string with maxLength; None for absent/null.
fn opt_text<'a>(body: &'a Value, key: &str, max: usize) -> Result<Option<&'a str>, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            if s.chars().count() > max {
                Err(invalid_param(key, "too long"))
            } else {
                Ok(Some(s.as_str()))
            }
        }
        Some(_) => Err(invalid_param(key, "expected a string")),
    }
}

fn opt_bool(body: &Value, key: &str, dflt: bool) -> Result<bool, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(dflt),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(invalid_param(key, "expected a boolean")),
    }
}

fn iso_from_id(id: &str) -> String {
    mk_types::id::parse_aid_date(id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default()
}

// ==== entity packs =========================================================

/// UserEntityService.pack (UserLite shape for the chat schemas).
async fn pack_user_lite(state: &AppState, pool: &mk_db::sqlx::PgPool, user_id: &str) -> Value {
    let row = match mk_db::user::find_user_by_id(pool, user_id).await {
        Ok(Some(r)) => r,
        _ => return json!({ "id": user_id }),
    };
    let profile = match mk_db::user::find_profile(pool, user_id).await {
        Ok(p) => p,
        Err(_) => return json!({ "id": user_id }),
    };
    let ctx = mk_core::services::user_pack::InstanceCtx {
        url: &state.config.url,
        host: &state.config.host(),
    };
    mk_core::services::user_pack::user_lite_value(&row, &profile, ctx)
}

/// ChatEntityService.packRoom.
async fn pack_room(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    room: &ChatRoomRow,
    me_id: &str,
) -> Result<Value, Response> {
    let owner = pack_user_lite(state, pool, &room.owner_id).await;
    let (is_muted, invitation_exists) = if room.owner_id == me_id {
        (false, false)
    } else {
        let membership = mk_db::chat::find_membership(pool, &room.id, me_id)
            .await
            .map_err(|e| db_err("pack_room membership", e))?;
        let invitation = mk_db::chat::find_invitation(pool, &room.id, me_id)
            .await
            .map_err(|e| db_err("pack_room invitation", e))?;
        (
            membership.map(|m| m.is_muted).unwrap_or(false),
            invitation.is_some(),
        )
    };
    Ok(json!({
        "id": room.id,
        "createdAt": iso_from_id(&room.id),
        "name": room.name,
        "description": room.description,
        "ownerId": room.owner_id,
        "owner": owner,
        "isMuted": is_muted,
        "invitationExists": invitation_exists,
    }))
}

/// ChatEntityService.packRoomMembership (populateUser/populateRoom flags).
async fn pack_membership(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    membership: &ChatMembershipRow,
    me_id: &str,
    populate_user: bool,
    populate_room: bool,
) -> Result<Value, Response> {
    let mut out = json!({
        "id": membership.id,
        "createdAt": iso_from_id(&membership.id),
        "userId": membership.user_id,
        "roomId": membership.room_id,
    });
    if populate_user {
        out["user"] = pack_user_lite(state, pool, &membership.user_id).await;
    }
    if populate_room {
        let room = mk_db::chat::find_room(pool, &membership.room_id)
            .await
            .map_err(|e| db_err("pack_membership room", e))?;
        if let Some(room) = room {
            out["room"] = pack_room(state, pool, &room, me_id).await?;
        }
    }
    Ok(out)
}

/// ChatEntityService.packRoomInvitation.
async fn pack_invitation(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    invitation: &ChatInvitationRow,
    me_id: &str,
) -> Result<Value, Response> {
    let room = mk_db::chat::find_room(pool, &invitation.room_id)
        .await
        .map_err(|e| db_err("pack_invitation room", e))?
        .ok_or_else(|| api_err(ApiError::internal_error()))?;
    let user = pack_user_lite(state, pool, &invitation.user_id).await;
    Ok(json!({
        "id": invitation.id,
        "createdAt": iso_from_id(&invitation.id),
        "userId": invitation.user_id,
        "user": user,
        "roomId": invitation.room_id,
        "room": pack_room(state, pool, &room, me_id).await?,
    }))
}

/// ChatEntityService.packMessageDetailed (also used by chat/history and
/// `drive/files/attached-chat-messages` — upstream packMessagesDetailed).
pub(crate) async fn pack_message_detailed(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    message: &ChatMessageRow,
    me_id: &str,
) -> Result<Value, Response> {
    let from_user = pack_user_lite(state, pool, &message.from_user_id).await;
    let mut out = json!({
        "id": message.id,
        "createdAt": iso_from_id(&message.id),
        "text": message.text,
        "fromUserId": message.from_user_id,
        "fromUser": from_user,
        "fileId": message.file_id,
        "file": pack_file(pool, message.file_id.as_deref()).await?,
        "reactions": pack_reactions(state, pool, &message.reactions, true).await?,
    });
    if let Some(to_user_id) = &message.to_user_id {
        out["toUserId"] = json!(to_user_id);
        out["toUser"] = pack_user_lite(state, pool, to_user_id).await;
    }
    if let Some(to_room_id) = &message.to_room_id {
        out["toRoomId"] = json!(to_room_id);
        let room = mk_db::chat::find_room(pool, to_room_id)
            .await
            .map_err(|e| db_err("pack_message room", e))?;
        if let Some(room) = room {
            out["toRoom"] = pack_room(state, pool, &room, me_id).await?;
        }
    }
    Ok(out)
}

/// ChatEntityService.packMessageLiteFor1on1 (reactions: [{ reaction }]).
async fn pack_message_lite_1on1(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    message: &ChatMessageRow,
) -> Result<Value, Response> {
    Ok(json!({
        "id": message.id,
        "createdAt": iso_from_id(&message.id),
        "text": message.text,
        "fromUserId": message.from_user_id,
        "toUserId": message.to_user_id,
        "fileId": message.file_id,
        "file": pack_file(pool, message.file_id.as_deref()).await?,
        "reactions": pack_reactions(state, pool, &message.reactions, false).await?,
    }))
}

/// ChatEntityService.packMessageLiteForRoom (fromUser + reactions with user).
async fn pack_message_lite_room(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    message: &ChatMessageRow,
) -> Result<Value, Response> {
    Ok(json!({
        "id": message.id,
        "createdAt": iso_from_id(&message.id),
        "text": message.text,
        "fromUserId": message.from_user_id,
        "fromUser": pack_user_lite(state, pool, &message.from_user_id).await,
        "toRoomId": message.to_room_id,
        "fileId": message.file_id,
        "file": pack_file(pool, message.file_id.as_deref()).await?,
        "reactions": pack_reactions(state, pool, &message.reactions, true).await?,
    }))
}

/// DriveFileEntityService.pack (non-self).
async fn pack_file(pool: &mk_db::sqlx::PgPool, file_id: Option<&str>) -> Result<Value, Response> {
    let Some(file_id) = file_id else {
        return Ok(Value::Null);
    };
    let row = mk_db::drive::find_drive_file(pool, file_id)
        .await
        .map_err(|e| db_err("pack_file", e))?;
    Ok(match row {
        Some(row) => crate::api::drive::pack_drive_file(&row, false),
        None => Value::Null,
    })
}

/// `reactions` column entries are `"<userId>/<reaction>"`.
async fn pack_reactions(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    reactions: &[String],
    with_user: bool,
) -> Result<Value, Response> {
    let mut out = Vec::with_capacity(reactions.len());
    for record in reactions {
        let (user_id, reaction) = record
            .split_once('/')
            .map(|(u, r)| (Some(u), r))
            .unwrap_or((None, record.as_str()));
        let mut entry = json!({ "reaction": reaction });
        if with_user {
            match user_id {
                Some(user_id) => entry["user"] = pack_user_lite(state, pool, user_id).await,
                None => entry["user"] = Value::Null,
            }
        }
        out.push(entry);
    }
    Ok(Value::Array(out))
}

// ==== entry ================================================================

/// Entry from the chat dispatch arm in mod.rs (body pre-parsed, token +
/// credential-declared pre-extracted).
pub(crate) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
    credential_declared: bool,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    // Every chat endpoint is requireCredential upstream.
    let me_id = match super::social::require_checked_user(
        pool,
        token.as_deref(),
        credential_declared,
    )
    .await
    {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let result: Result<Response, Response> = match name {
        "chat/rooms/create" => rooms_create(state, pool, body, &me_id).await,
        "chat/rooms/joining" => rooms_joining(state, pool, body, &me_id).await,
        "chat/rooms/join" => rooms_join(pool, body, &me_id).await,
        "chat/rooms/leave" => rooms_leave(pool, body, &me_id).await,
        "chat/rooms/show" => rooms_show(state, pool, body, &me_id).await,
        "chat/rooms/update" => rooms_update(state, pool, body, &me_id).await,
        "chat/rooms/members" => rooms_members(state, pool, body, &me_id).await,
        "chat/rooms/invitations/create" => invitations_create(state, pool, body, &me_id).await,
        "chat/rooms/invitations/ignore" => invitations_ignore(pool, body, &me_id).await,
        "chat/rooms/invitations/inbox" => invitations_inbox(state, pool, body, &me_id).await,
        "chat/messages/create-to-user" => messages_create_to_user(state, pool, body, &me_id).await,
        "chat/messages/create-to-room" => messages_create_to_room(state, pool, body, &me_id).await,
        "chat/messages/room-timeline" => messages_room_timeline(state, pool, body, &me_id).await,
        "chat/messages/user-timeline" => messages_user_timeline(state, pool, body, &me_id).await,
        "chat/messages/show" => messages_show(state, pool, body, &me_id).await,
        "chat/messages/delete" => messages_delete(pool, body, &me_id).await,
        "chat/messages/react" => messages_react(pool, body, &me_id).await,
        "chat/messages/unreact" => messages_unreact(pool, body, &me_id).await,
        "chat/messages/search" => messages_search(state, pool, body, &me_id).await,
        "chat/read-all" => Ok(ok_empty()),
        "chat/history" => chat_history(state, pool, body, &me_id).await,
        _ => Ok(api_err(ApiError::unknown_endpoint())),
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

// ==== rooms ================================================================

/// chat/rooms/create — ChatService.createRoom.
async fn rooms_create(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let name = match body.get("name") {
        Some(Value::String(s)) if !s.is_empty() && s.chars().count() <= 256 => s.as_str(),
        Some(Value::String(_)) => return Err(invalid_param("name", "must be 1..256 characters")),
        _ => return Err(invalid_param("name", "required (1..256 characters)")),
    };
    let description = opt_text(body, "description", 1024)?.unwrap_or("");
    let id = mk_types::id::gen_aid_now();
    mk_db::chat::insert_room(pool, &id, me_id, name, description)
        .await
        .map_err(|e| db_err("rooms_create insert", e))?;
    let room = mk_db::chat::find_room(pool, &id)
        .await
        .map_err(|e| db_err("rooms_create refetch", e))?
        .ok_or_else(|| api_err(ApiError::internal_error()))?;
    Ok(ok_json(pack_room(state, pool, &room, me_id).await?))
}

/// chat/rooms/joining — ChatService.getMyMemberships (my rooms).
async fn rooms_joining(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let limit = limit_param(body, 30)?;
    let (since_id, until_id) = page_ids(body)?;
    let rows = mk_db::chat::memberships_for_user(
        pool,
        me_id,
        limit,
        since_id.as_deref(),
        until_id.as_deref(),
    )
    .await
    .map_err(|e| db_err("rooms_joining", e))?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(pack_membership(state, pool, row, me_id, false, true).await?);
    }
    Ok(ok_json(Value::Array(out)))
}

/// chat/rooms/join — ChatService.joinToRoom (invitation required).
async fn rooms_join(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let room_id = required_id(body, "roomId")?;
    let room = mk_db::chat::find_room(pool, room_id)
        .await
        .map_err(|e| db_err("rooms_join find_room", e))?
        .ok_or_else(|| no_such_room("84416476-5ce8-4a2c-b568-9569f1b10733"))?;
    if room.owner_id == me_id
        || mk_db::chat::membership_exists(pool, room_id, me_id)
            .await
            .map_err(|e| db_err("rooms_join membership", e))?
    {
        // upstream would trip the (userId, roomId) unique index; the port
        // treats a repeated join as idempotent.
        return Ok(ok_empty());
    }
    let invitation = mk_db::chat::find_invitation(pool, room_id, me_id)
        .await
        .map_err(|e| db_err("rooms_join invitation", e))?
        .ok_or_else(|| no_such_room("84416476-5ce8-4a2c-b568-9569f1b10733"))?;
    let count = mk_db::chat::count_memberships(pool, room_id)
        .await
        .map_err(|e| db_err("rooms_join count", e))?;
    if count >= MAX_ROOM_MEMBERS {
        return Err(meta_err(
            "ROOM_FULL",
            "b16d31a1-2c53-4e1d-9f7a-3a4f8d2e6b90",
            "The room is full.",
        ));
    }
    mk_db::chat::insert_membership(pool, &mk_types::id::gen_aid_now(), room_id, me_id)
        .await
        .map_err(|e| db_err("rooms_join insert", e))?;
    mk_db::chat::delete_invitation(pool, &invitation.id)
        .await
        .map_err(|e| db_err("rooms_join delete invitation", e))?;
    Ok(ok_empty())
}

/// chat/rooms/leave — ChatService.leaveRoom.
async fn rooms_leave(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let room_id = required_id(body, "roomId")?;
    mk_db::chat::find_room(pool, room_id)
        .await
        .map_err(|e| db_err("rooms_leave find_room", e))?
        .ok_or_else(|| no_such_room("cb7f3179-50e8-4389-8c30-dbe2650a67c9"))?;
    let removed = mk_db::chat::delete_membership(pool, room_id, me_id)
        .await
        .map_err(|e| db_err("rooms_leave delete", e))?;
    if removed == 0 {
        // owner has no membership row upstream → findOneByOrFail throws; the
        // endpoint only defines NO_SUCH_ROOM.
        return Err(no_such_room("cb7f3179-50e8-4389-8c30-dbe2650a67c9"));
    }
    Ok(ok_empty())
}

/// chat/rooms/show — pack when owner / member / invited.
async fn rooms_show(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let room_id = required_id(body, "roomId")?;
    let room = mk_db::chat::find_room(pool, room_id)
        .await
        .map_err(|e| db_err("rooms_show find_room", e))?
        .ok_or_else(|| no_such_room("857ae02f-8759-4d20-9adb-6e95fffe4fd7"))?;
    let visible = room.owner_id == me_id
        || mk_db::chat::membership_exists(pool, room_id, me_id)
            .await
            .map_err(|e| db_err("rooms_show membership", e))?
        || mk_db::chat::find_invitation(pool, room_id, me_id)
            .await
            .map_err(|e| db_err("rooms_show invitation", e))?
            .is_some();
    if !visible {
        return Err(no_such_room("857ae02f-8759-4d20-9adb-6e95fffe4fd7"));
    }
    Ok(ok_json(pack_room(state, pool, &room, me_id).await?))
}

/// chat/rooms/update — owner-scoped (findMyRoomById).
async fn rooms_update(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let room_id = required_id(body, "roomId")?;
    let name = opt_text(body, "name", 256)?;
    let description = opt_text(body, "description", 1024)?;
    mk_db::chat::find_owned_room(pool, room_id, me_id)
        .await
        .map_err(|e| db_err("rooms_update find_owned", e))?
        .ok_or_else(|| no_such_room("fcdb0f92-bda6-47f9-bd05-343e0e020932"))?;
    let room = mk_db::chat::update_room(pool, room_id, name, description)
        .await
        .map_err(|e| db_err("rooms_update update", e))?
        .ok_or_else(|| no_such_room("fcdb0f92-bda6-47f9-bd05-343e0e020932"))?;
    Ok(ok_json(pack_room(state, pool, &room, me_id).await?))
}

/// chat/rooms/members — member-scoped, populateUser.
async fn rooms_members(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let room_id = required_id(body, "roomId")?;
    let limit = limit_param(body, 30)?;
    let (since_id, until_id) = page_ids(body)?;
    let room = mk_db::chat::find_room(pool, room_id)
        .await
        .map_err(|e| db_err("rooms_members find_room", e))?
        .ok_or_else(|| no_such_room("7b9fe84c-eafc-4d21-bf89-485458ed2c18"))?;
    let is_member = room.owner_id == me_id
        || mk_db::chat::membership_exists(pool, room_id, me_id)
            .await
            .map_err(|e| db_err("rooms_members membership", e))?;
    if !is_member {
        return Err(no_such_room("7b9fe84c-eafc-4d21-bf89-485458ed2c18"));
    }
    let rows = mk_db::chat::memberships_for_room(
        pool,
        room_id,
        limit,
        since_id.as_deref(),
        until_id.as_deref(),
    )
    .await
    .map_err(|e| db_err("rooms_members list", e))?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(pack_membership(state, pool, row, me_id, true, false).await?);
    }
    Ok(ok_json(Value::Array(out)))
}

// ==== invitations ==========================================================

/// chat/rooms/invitations/create — owner-scoped ChatService.createRoomInvitation.
async fn invitations_create(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let room_id = required_id(body, "roomId")?;
    let user_id = required_id(body, "userId")?;
    let room = mk_db::chat::find_owned_room(pool, room_id, me_id)
        .await
        .map_err(|e| db_err("invitations_create find_owned", e))?
        .ok_or_else(|| no_such_room("916f9507-49ba-4e90-b57f-1fd4deaa47a5"))?;
    if user_id == me_id {
        return Err(meta_err(
            "RECIPIENT_IS_YOURSELF",
            "17e2ba79-e22a-4cbc-bf91-d327643f4a7e",
            "You can not send a message to yourself.",
        ));
    }
    mk_db::user::find_user_by_id(pool, user_id)
        .await
        .map_err(|e| db_err("invitations_create find_user", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_USER",
                "11795c64-40ea-4198-b06e-3c873ed9039d",
                "No such user.",
            )
        })?;
    if mk_db::chat::membership_exists(pool, room_id, user_id)
        .await
        .map_err(|e| db_err("invitations_create membership", e))?
    {
        return Err(meta_err(
            "ALREADY_MEMBER",
            "2a8f2f74-4d3e-4f6b-8a0e-6a2f5b8f3c11",
            "That user is already a member of the room.",
        ));
    }
    if mk_db::chat::find_invitation(pool, room_id, user_id)
        .await
        .map_err(|e| db_err("invitations_create existing", e))?
        .is_some()
    {
        return Err(meta_err(
            "ALREADY_INVITED",
            "4b1e7d24-6f0a-4c9d-9e3a-1d7c2b5a8f42",
            "That user has already been invited to the room.",
        ));
    }
    let count = mk_db::chat::count_memberships(pool, room_id)
        .await
        .map_err(|e| db_err("invitations_create count", e))?;
    if count >= MAX_ROOM_MEMBERS {
        return Err(meta_err(
            "ROOM_FULL",
            "b16d31a1-2c53-4e1d-9f7a-3a4f8d2e6b90",
            "The room is full.",
        ));
    }
    let id = mk_types::id::gen_aid_now();
    mk_db::chat::insert_invitation(pool, &id, &room.id, user_id)
        .await
        .map_err(|e| db_err("invitations_create insert", e))?;
    let invitation = mk_db::chat::find_invitation(pool, &room.id, user_id)
        .await
        .map_err(|e| db_err("invitations_create refetch", e))?
        .ok_or_else(|| api_err(ApiError::internal_error()))?;
    Ok(ok_json(
        pack_invitation(state, pool, &invitation, me_id).await?,
    ))
}

/// chat/rooms/invitations/ignore (decline) — ChatService.ignoreRoomInvitation.
async fn invitations_ignore(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let room_id = required_id(body, "roomId")?;
    mk_db::chat::find_room(pool, room_id)
        .await
        .map_err(|e| db_err("invitations_ignore find_room", e))?
        .ok_or_else(|| no_such_room("5130557e-5a11-4cfb-9cc5-fe60cda5de0d"))?;
    let invitation = mk_db::chat::find_invitation(pool, room_id, me_id)
        .await
        .map_err(|e| db_err("invitations_ignore find", e))?
        .ok_or_else(|| no_such_room("5130557e-5a11-4cfb-9cc5-fe60cda5de0d"))?;
    mk_db::chat::ignore_invitation(pool, &invitation.id)
        .await
        .map_err(|e| db_err("invitations_ignore update", e))?;
    Ok(ok_empty())
}

/// chat/rooms/invitations/inbox — received invitations (not ignored).
async fn invitations_inbox(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let limit = limit_param(body, 30)?;
    let (since_id, until_id) = page_ids(body)?;
    let rows = mk_db::chat::invitations_received(
        pool,
        me_id,
        limit,
        since_id.as_deref(),
        until_id.as_deref(),
    )
    .await
    .map_err(|e| db_err("invitations_inbox", e))?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(pack_invitation(state, pool, row, me_id).await?);
    }
    Ok(ok_json(Value::Array(out)))
}

// ==== messages =============================================================

/// `text` paramDef: nullable string maxLength 2000 (absent/null → None).
fn text_param(body: &Value) -> Result<Option<&str>, Response> {
    match body.get("text") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            if s.chars().count() > 2000 {
                Err(invalid_param("text", "too long"))
            } else {
                Ok(Some(s.as_str()))
            }
        }
        Some(_) => Err(invalid_param("text", "expected a string")),
    }
}

/// `driveFilesRepository.findOneBy({ id, userId: me.id })` for `fileId`.
async fn checked_file(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
    no_such_file_id: &str,
) -> Result<Option<String>, Response> {
    let Some(file_id) = opt_str(body, "fileId") else {
        return Ok(None);
    };
    let file = mk_db::drive::find_drive_file(pool, file_id)
        .await
        .map_err(|e| db_err("checked_file", e))?;
    match file {
        Some(row) if row.user_id.as_deref() == Some(me_id) => Ok(Some(file_id.to_string())),
        _ => Err(meta_err("NO_SUCH_FILE", no_such_file_id, "No such file.")),
    }
}

fn content_required(id: &str) -> Response {
    meta_err(
        "CONTENT_REQUIRED",
        id,
        "Content required. You need to set text or fileId.",
    )
}

/// ChatService.createMessageToUser — approvals / chatScope / block gates,
/// then the 1on1 row + the approval row ChatService inserts on send.
async fn messages_create_to_user(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let text = text_param(body)?;
    let file_id = checked_file(pool, body, me_id, "4372b8e2-185d-4146-8749-2f68864a3e5f").await?;
    if text.is_none() && file_id.is_none() {
        return Err(content_required("25587321-b0e6-449c-9239-f8925092942c"));
    }
    let to_user_id = required_id(body, "toUserId")?;
    if to_user_id == me_id {
        return Err(meta_err(
            "RECIPIENT_IS_YOURSELF",
            "17e2ba79-e22a-4cbc-bf91-d327643f4a7e",
            "You can not send a message to yourself.",
        ));
    }
    let to_user = mk_db::user::find_user_by_id(pool, to_user_id)
        .await
        .map_err(|e| db_err("create_to_user find_user", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_USER",
                "11795c64-40ea-4198-b06e-3c873ed9039d",
                "No such user.",
            )
        })?;

    let other_approved_me = mk_db::chat::approval_exists(pool, to_user_id, me_id)
        .await
        .map_err(|e| db_err("create_to_user approval other", e))?;
    let i_approved_other = mk_db::chat::approval_exists(pool, me_id, to_user_id)
        .await
        .map_err(|e| db_err("create_to_user approval me", e))?;
    if !other_approved_me {
        let allowed = match to_user.chat_scope.as_str() {
            "none" => false,
            // upstream names the follower direction from the sender side
            "followers" => mk_db::social::following_exists(pool, me_id, to_user_id)
                .await
                .map_err(|e| db_err("create_to_user followers", e))?,
            "following" => mk_db::social::following_exists(pool, to_user_id, me_id)
                .await
                .map_err(|e| db_err("create_to_user following", e))?,
            "mutual" => {
                mk_db::social::following_exists(pool, me_id, to_user_id)
                    .await
                    .map_err(|e| db_err("create_to_user mutual a", e))?
                    && mk_db::social::following_exists(pool, to_user_id, me_id)
                        .await
                        .map_err(|e| db_err("create_to_user mutual b", e))?
            }
            _ => true,
        };
        if !allowed {
            return Err(meta_err(
                "CANNOT_CHAT",
                "6f7d3a90-6d8b-4c7e-b1a2-3c4d5e6f7a8b",
                "You cannot chat with this user.",
            ));
        }
    }
    if mk_db::social::blocking_exists(pool, to_user_id, me_id)
        .await
        .map_err(|e| db_err("create_to_user blocked", e))?
    {
        return Err(meta_err(
            "YOU_HAVE_BEEN_BLOCKED",
            "c15a5199-7422-4968-941a-2a462c478f7d",
            "You cannot send a message because you have been blocked by this user.",
        ));
    }

    // `params.text ? trim : null` — empty strings collapse to null.
    let text = text.map(|t| t.trim()).filter(|t| !t.is_empty());
    let id = mk_types::id::gen_aid_now();
    mk_db::chat::insert_message(
        pool,
        &mk_db::chat::NewChatMessage {
            id: &id,
            from_user_id: me_id,
            to_user_id: Some(to_user_id),
            to_room_id: None,
            text,
            file_id: file_id.as_deref(),
        },
    )
    .await
    .map_err(|e| db_err("create_to_user insert", e))?;
    if !i_approved_other {
        mk_db::chat::insert_approval(pool, &mk_types::id::gen_aid_now(), me_id, to_user_id)
            .await
            .map_err(|e| db_err("create_to_user approval insert", e))?;
    }
    let message = mk_db::chat::find_message(pool, &id)
        .await
        .map_err(|e| db_err("create_to_user refetch", e))?
        .ok_or_else(|| api_err(ApiError::internal_error()))?;
    Ok(ok_json(
        pack_message_lite_1on1(state, pool, &message).await?,
    ))
}

/// ChatService.createMessageToRoom — membership gate + room message row.
async fn messages_create_to_room(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let to_room_id = required_id(body, "toRoomId")?;
    let room = mk_db::chat::find_room(pool, to_room_id)
        .await
        .map_err(|e| db_err("create_to_room find_room", e))?
        .ok_or_else(|| no_such_room("8098520d-2da5-4e8f-8ee1-df78b55a4ec6"))?;
    let text = text_param(body)?;
    let file_id = checked_file(pool, body, me_id, "b6accbd3-1d7b-4d9f-bdb7-eb185bac06db").await?;
    if text.is_none() && file_id.is_none() {
        return Err(content_required("340517b7-6d04-42c0-bac1-37ee804e3594"));
    }
    let is_member = room.owner_id == me_id
        || mk_db::chat::membership_exists(pool, to_room_id, me_id)
            .await
            .map_err(|e| db_err("create_to_room membership", e))?;
    if !is_member {
        return Err(no_such_room("8098520d-2da5-4e8f-8ee1-df78b55a4ec6"));
    }
    let text = text.map(|t| t.trim()).filter(|t| !t.is_empty());
    let id = mk_types::id::gen_aid_now();
    mk_db::chat::insert_message(
        pool,
        &mk_db::chat::NewChatMessage {
            id: &id,
            from_user_id: me_id,
            to_user_id: None,
            to_room_id: Some(to_room_id),
            text,
            file_id: file_id.as_deref(),
        },
    )
    .await
    .map_err(|e| db_err("create_to_room insert", e))?;
    let message = mk_db::chat::find_message(pool, &id)
        .await
        .map_err(|e| db_err("create_to_room refetch", e))?
        .ok_or_else(|| api_err(ApiError::internal_error()))?;
    Ok(ok_json(
        pack_message_lite_room(state, pool, &message).await?,
    ))
}

/// chat/messages/room-timeline — member-scoped paged timeline.
async fn messages_room_timeline(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let room_id = required_id(body, "roomId")?;
    let limit = limit_param(body, 10)?;
    let (since_id, until_id) = page_ids(body)?;
    let room = mk_db::chat::find_room(pool, room_id)
        .await
        .map_err(|e| db_err("room_timeline find_room", e))?
        .ok_or_else(|| no_such_room("c4d9f88c-9270-4632-b032-6ed8cee36f7f"))?;
    let is_member = room.owner_id == me_id
        || mk_db::chat::membership_exists(pool, room_id, me_id)
            .await
            .map_err(|e| db_err("room_timeline membership", e))?;
    if !is_member {
        return Err(no_such_room("c4d9f88c-9270-4632-b032-6ed8cee36f7f"));
    }
    let rows = mk_db::chat::room_messages(
        pool,
        room_id,
        limit,
        since_id.as_deref(),
        until_id.as_deref(),
    )
    .await
    .map_err(|e| db_err("room_timeline", e))?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(pack_message_lite_room(state, pool, row).await?);
    }
    Ok(ok_json(Value::Array(out)))
}

/// chat/messages/user-timeline — the 1on1 history with one user in both
/// directions (ChatService.userTimeline), packed LiteFor1on1. The trailing
/// readUserChatMessage side effect is a Redis no-op (see module header).
async fn messages_user_timeline(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let user_id = required_id(body, "userId")?;
    let limit = limit_param(body, 10)?;
    let (since_id, until_id) = page_ids(body)?;
    mk_db::user::find_user_by_id(pool, user_id)
        .await
        .map_err(|e| db_err("user_timeline find_user", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_USER",
                "11795c64-40ea-4198-b06e-3c873ed9039d",
                "No such user.",
            )
        })?;
    let rows = mk_db::chat::user_timeline(
        pool,
        me_id,
        user_id,
        limit,
        since_id.as_deref(),
        until_id.as_deref(),
    )
    .await
    .map_err(|e| db_err("user_timeline", e))?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(pack_message_lite_1on1(state, pool, row).await?);
    }
    Ok(ok_json(Value::Array(out)))
}

/// chat/messages/show — sender/recipient only (upstream has no room-member
/// fallback on this endpoint).
async fn messages_show(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let message_id = required_id(body, "messageId")?;
    let message = mk_db::chat::find_message(pool, message_id)
        .await
        .map_err(|e| db_err("messages_show find", e))?
        .ok_or_else(|| no_such_message("3710865b-1848-4da9-8d61-cfed15510b93"))?;
    if message.from_user_id != me_id && message.to_user_id.as_deref() != Some(me_id) {
        return Err(no_such_message("3710865b-1848-4da9-8d61-cfed15510b93"));
    }
    Ok(ok_json(
        pack_message_detailed(state, pool, &message, me_id).await?,
    ))
}

/// chat/messages/delete — sender-scoped.
async fn messages_delete(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let message_id = required_id(body, "messageId")?;
    let message = mk_db::chat::find_message_from_user(pool, message_id, me_id)
        .await
        .map_err(|e| db_err("messages_delete find", e))?
        .ok_or_else(|| no_such_message("36b67f0e-66a6-414b-83df-992a55294f17"))?;
    mk_db::chat::delete_message(pool, &message.id)
        .await
        .map_err(|e| db_err("messages_delete", e))?;
    Ok(ok_empty())
}

/// ChatService.react/unreact reaction normalization: a custom `:name:` /
/// `:name@.:` shortcode is stored verbatim (the port has no chat emoji
/// registry to validate it against); anything else goes through the shared
/// note-reaction normalizer (ZWJ kept, \ufe0f stripped).
fn chat_reaction(input: &str) -> String {
    if let Some(inner) = input.strip_prefix(':').and_then(|s| s.strip_suffix(':')) {
        let name = inner.strip_suffix("@.").unwrap_or(inner);
        if !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '+' | '-'))
        {
            return format!(":{name}:");
        }
    }
    super::social::normalize_reaction(input)
}

/// Shared react/unreact access gate: message must exist and, for room
/// messages, the caller must own or belong to the room. Failures collapse to
/// the endpoint's NO_SUCH_MESSAGE (upstream hides existence via
/// ChatMessageAccessError).
async fn reaction_target(
    pool: &mk_db::sqlx::PgPool,
    message_id: &str,
    me_id: &str,
    error_id: &str,
) -> Result<ChatMessageRow, Response> {
    let message = mk_db::chat::find_message(pool, message_id)
        .await
        .map_err(|e| db_err("reaction find_message", e))?
        .ok_or_else(|| no_such_message(error_id))?;
    if let Some(room_id) = &message.to_room_id {
        let room = mk_db::chat::find_room(pool, room_id)
            .await
            .map_err(|e| db_err("reaction find_room", e))?
            .ok_or_else(|| no_such_message(error_id))?;
        let member = room.owner_id == me_id
            || mk_db::chat::membership_exists(pool, room_id, me_id)
                .await
                .map_err(|e| db_err("reaction membership", e))?;
        if !member {
            return Err(no_such_message(error_id));
        }
    }
    Ok(message)
}

/// chat/messages/react — ChatService.react (`array_append`).
async fn messages_react(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    const ERR: &str = "9b5839b9-0ba0-4351-8c35-37082093d200";
    let message_id = required_id(body, "messageId")?;
    let reaction =
        opt_str(body, "reaction").ok_or_else(|| invalid_param("reaction", "required"))?;
    let message = reaction_target(pool, message_id, me_id, ERR).await?;
    if message.from_user_id == me_id {
        return Err(no_such_message(ERR));
    }
    if message.to_room_id.is_none() && message.to_user_id.as_deref() != Some(me_id) {
        return Err(no_such_message(ERR));
    }
    if message.reactions.len() >= 100 {
        // MAX_REACTIONS_PER_MESSAGE (ChatService.ts L35) — upstream throws a
        // generic Error, which the API layer turns into INTERNAL_ERROR.
        return Err(api_err(ApiError::internal_error()));
    }
    let pair = format!("{me_id}/{}", chat_reaction(reaction));
    mk_db::chat::add_reaction(pool, &message.id, &pair)
        .await
        .map_err(|e| db_err("messages_react", e))?;
    Ok(ok_empty())
}

/// chat/messages/unreact — ChatService.unreact (`array_remove`); unlike
/// react this does not forbid the sender's own message.
async fn messages_unreact(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    const ERR: &str = "c39ea42f-e3ca-428a-ad57-390e0a711595";
    let message_id = required_id(body, "messageId")?;
    let reaction =
        opt_str(body, "reaction").ok_or_else(|| invalid_param("reaction", "required"))?;
    let message = reaction_target(pool, message_id, me_id, ERR).await?;
    if message.to_room_id.is_none()
        && message.from_user_id != me_id
        && message.to_user_id.as_deref() != Some(me_id)
    {
        return Err(no_such_message(ERR));
    }
    let pair = format!("{me_id}/{}", chat_reaction(reaction));
    mk_db::chat::remove_reaction(pool, &message.id, &pair)
        .await
        .map_err(|e| db_err("messages_unreact", e))?;
    Ok(ok_empty())
}

/// chat/messages/search — ChatService.searchMessages (LOWER(text) LIKE).
async fn messages_search(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    const NO_SUCH_ROOM: &str = "460b3669-81b0-4dc9-a997-44442141bf83";
    let query = match opt_str(body, "query") {
        Some(q) if !q.is_empty() && q.chars().count() <= 256 => q,
        Some(_) => return Err(invalid_param("query", "must be 1..256 characters")),
        None => return Err(invalid_param("query", "required (1..256 characters)")),
    };
    let limit = limit_param(body, 10)?;
    let user_id = opt_str(body, "userId");
    let room_id = opt_str(body, "roomId");
    if let Some(room_id) = room_id {
        let room = mk_db::chat::find_room(pool, room_id)
            .await
            .map_err(|e| db_err("messages_search find_room", e))?
            .ok_or_else(|| no_such_room(NO_SUCH_ROOM))?;
        let member = room.owner_id == me_id
            || mk_db::chat::membership_exists(pool, room_id, me_id)
                .await
                .map_err(|e| db_err("messages_search membership", e))?;
        if !member {
            return Err(no_such_room(NO_SUCH_ROOM));
        }
    }
    let rows = mk_db::chat::search_messages(pool, me_id, query, limit, user_id, room_id)
        .await
        .map_err(|e| db_err("messages_search", e))?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(pack_message_detailed(state, pool, row, me_id).await?);
    }
    Ok(ok_json(Value::Array(out)))
}

/// chat/history — latest per 1on1 counterpart (room: false) or per joined
/// room (room: true), packed detailed. `isRead` is true because the Redis
/// unread markers have no port-side store.
async fn chat_history(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let limit = limit_param(body, 10)?;
    let room = opt_bool(body, "room", false)?;
    let rows = if room {
        mk_db::chat::room_history(pool, me_id, limit).await
    } else {
        mk_db::chat::user_history(pool, me_id, limit).await
    }
    .map_err(|e| db_err("chat_history", e))?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let mut packed = pack_message_detailed(state, pool, row, me_id).await?;
        packed["isRead"] = json!(true);
        out.push(packed);
    }
    Ok(ok_json(Value::Array(out)))
}
