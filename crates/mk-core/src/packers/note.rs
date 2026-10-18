//! Note pack — `NoteEntityService.pack` field semantics for the note.ts
//! acceptance slice (field presence + Detail recursion).
//!
//! Upstream presence rules (Packed<'Note> optional:true fields are ABSENT
//! when upstream would emit `undefined`; plain `null` stays `null`):
//! - `text`/`cw` always present (cw null when unset).
//! - `replyId`/`renoteId` always present (null when unset).
//! - `mentions`/`tags` only when non-empty.
//! - `visibleUserIds` only for visibility='specified'.
//! - `fileIds`/`files` always (`files` = DriveFile pack detail:false).
//! - `channelId`/`uri`/`url` only when set.
//! - `hasPoll` only when true; the `poll` object is Detail-only.
//! - `emojis` only for remote authors (local → undefined upstream).
//! - Detail adds `clippedCount`, `reply` (detail:false recursion), `renote`
//!   (detail:true recursion), `poll`, and `myReaction` (self-view, only when
//!   the note carries reactions).

use mk_db::note::NoteRow;
use serde_json::{Value, json};

/// Draft of one `files` entry (DriveFileEntityService.pack, detail:false).
/// `createdAtIso` derives from the aid; storage columns feed the rest.
pub fn drive_file_pack(row: &mk_db::drive_note::NoteDriveFileRow, created_at_iso: String) -> Value {
    json!({
        "id": row.id,
        "createdAt": created_at_iso,
        "name": row.name,
        "type": row.file_type,
        "md5": row.md5,
        "size": row.size,
        "isSensitive": row.is_sensitive,
        "blurhash": row.blurhash,
        "properties": row.properties,
        "url": row.webpublic_url.clone().unwrap_or_else(|| row.url.clone()),
        // image thumbs point at the stored url; the rest stay null
        "thumbnailUrl": row.thumbnail_url.clone().or_else(|| {
            row.file_type.starts_with("image/")
                .then(|| row.webpublic_url.clone().unwrap_or_else(|| row.url.clone()))
        }),
        "comment": row.comment,
        "folderId": Value::Null,
        "folder": Value::Null,
        "userId": Value::Null,
        "user": Value::Null,
    })
}

/// `NoteEntityService.populatePoll`: `{ multiple, expiresAt, choices:[{
/// text, votes, isVoted }] }` with the viewer's votes flagged `isVoted`.
/// ` expiresAt` is `null` when the poll never expires.
pub fn poll_pack(
    poll: &mk_db::poll::PollRow,
    my_choices: &[i32],
    expires_at_iso: Option<String>,
) -> Value {
    let choices: Vec<Value> = poll
        .choices
        .iter()
        .enumerate()
        .map(|(i, text)| {
            json!({
                "text": text,
                "votes": poll.votes.get(i).copied().unwrap_or(0),
                "isVoted": my_choices.contains(&(i as i32)),
            })
        })
        .collect();
    json!({
        "multiple": poll.multiple,
        "expiresAt": expires_at_iso.map(Value::String).unwrap_or(Value::Null),
        "choices": choices,
    })
}

/// Assembled pack inputs for `note_pack_full` (the db-fetching pipeline in
/// api/mod.rs::pack_note fills these).
pub struct NotePackParts {
    /// `{ detail: true }` upstream default; the nested reply pack uses
    /// detail:false (drops the reply/renote/poll/myReaction detail block —
    /// `hasPoll` itself is a core field and stays).
    pub detail: bool,
    pub user_lite: Value,
    pub created_at_iso: String,
    /// Remote authors → `Some(map)` (empty `populatedEmojis`), local →
    /// `None` (field absent upstream).
    pub emojis: Option<Value>,
    pub files: Vec<Value>,
    pub poll: Option<Value>,
    /// Nested reply pack (Some(Value::Null) → replyId set, target missing).
    pub reply: Option<Value>,
    /// Nested renote pack (Some(Value::Null) → renoteId set, target missing).
    pub renote: Option<Value>,
    /// `myReaction` (Detail + me + non-empty reactions only).
    pub my_reaction: Option<Value>,
}

/// Full `NoteEntityService.pack(note, me, { detail: true })`.
pub fn note_pack_full(row: &NoteRow, parts: &NotePackParts) -> Value {
    let mut o = serde_json::Map::new();
    o.insert("id".into(), json!(row.id));
    o.insert("createdAt".into(), json!(parts.created_at_iso));
    o.insert("userId".into(), json!(row.user_id));
    o.insert("user".into(), parts.user_lite.clone());
    o.insert("text".into(), json!(row.text));
    // upstream pack: optional fields are ABSENT when unset (e2e exact-field
    // sets) — cw/replyId/renoteId/channelId only when set.
    if let Some(cw) = &row.cw {
        o.insert("cw".into(), json!(cw));
    }
    if let Some(reply_id) = &row.reply_id {
        o.insert("replyId".into(), json!(reply_id));
    }
    if let Some(renote_id) = &row.renote_id {
        o.insert("renoteId".into(), json!(renote_id));
    }
    o.insert(
        "visibility".into(),
        json!(match row.visibility.as_str() {
            "home" | "followers" | "specified" => row.visibility.clone(),
            _ => "public".to_string(),
        }),
    );
    o.insert("localOnly".into(), json!(row.local_only));
    o.insert(
        "reactionAcceptance".into(),
        row.reaction_acceptance
            .as_deref()
            .map(|s| json!(s))
            .unwrap_or(Value::Null),
    );
    if row.visibility == "specified" {
        o.insert("visibleUserIds".into(), json!(row.visible_user_ids));
    }
    o.insert("renoteCount".into(), json!(row.renote_count));
    o.insert("repliesCount".into(), json!(row.replies_count));
    let reactions: Value = serde_json::from_str(&row.reactions).unwrap_or_else(|_| json!({}));
    let reaction_count = reactions
        .as_object()
        .map(|m| m.values().filter_map(|v| v.as_i64()).sum::<i64>())
        .unwrap_or(0);
    o.insert("reactionCount".into(), json!(reaction_count));
    o.insert("reactions".into(), reactions);
    o.insert("reactionEmojis".into(), json!({}));
    if let Some(emojis) = &parts.emojis {
        o.insert("emojis".into(), emojis.clone());
    }
    if !row.tags.is_empty() {
        o.insert("tags".into(), json!(row.tags));
    }
    o.insert("fileIds".into(), json!(row.file_ids));
    o.insert("files".into(), json!(parts.files));
    if !row.mentions.is_empty() {
        o.insert("mentions".into(), json!(row.mentions));
    }
    if row.has_poll {
        o.insert("hasPoll".into(), json!(true));
    }
    if let Some(uri) = &row.uri {
        o.insert("uri".into(), json!(uri));
    }
    if let Some(url) = &row.url {
        o.insert("url".into(), json!(url));
    }
    // channelId only when set (channels-agent: notes/create { channelId }
    // arena; upstream `...(note.channelId != null ? { channelId } : {})`).
    if let Some(channel_id) = &row.channel_id {
        o.insert("channelId".into(), json!(channel_id));
    }
    // Detail block (`opts.detail`):
    if parts.detail {
        if row.clipped_count > 0 {
            o.insert("clippedCount".into(), json!(row.clipped_count));
        }
        if let Some(reply) = &parts.reply {
            o.insert("reply".into(), reply.clone());
        }
        if let Some(renote) = &parts.renote {
            o.insert("renote".into(), renote.clone());
        }
        if let Some(poll) = &parts.poll {
            o.insert("poll".into(), poll.clone());
        }
        if let Some(my_reaction) = &parts.my_reaction {
            o.insert("myReaction".into(), my_reaction.clone());
        }
    }
    Value::Object(o)
}

/// Legacy entry point shared by the co-agent timelines / notifications /
/// freq paths (no db recursion: no nested reply/renote, no poll body, no
/// files). Presence rules mirror `note_pack_full`.
pub fn note_pack_min(
    row: &NoteRow,
    user_lite: Value,
    created_at_iso: String,
    mentions: &[String],
    tags: &[String],
    emojis: Value,
) -> Value {
    let _ = (mentions, tags);
    note_pack_full(
        row,
        &NotePackParts {
            detail: true,
            user_lite,
            created_at_iso,
            emojis: (!emojis.is_null()).then_some(emojis),
            files: Vec::new(),
            poll: None,
            reply: None,
            renote: None,
            my_reaction: None,
        },
    )
}
