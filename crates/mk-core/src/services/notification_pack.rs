//! DB-row → packed-notification mapping (M2 truthful slice).
//!
//! Port of `core/entities/NotificationEntityService.ts` `#packInternal` for
//! the types this slice can produce NOW: `mention` / `reply` / `renote` /
//! `reaction` / `follow`. Packed shape per type:
//! - always: `id`, `createdAt` (ISO), `type`
//! - types with a notifier (all five): `userId` (= notifierId) + `user`
//!   (Packed<'UserLite'> — packed by the M1 `user_pack` pipeline at the
//!   fetch site; deleted users drop the notification upstream)
//! - note-bound types (mention/reply/renote/reaction): `note`
//!   (Packed<'Note'> — `note_pack_min` subset, TODO(M2+): upstream packs
//!   `detail: true` with reply/renote recursion/files/poll)
//! - `reaction`: + `reaction` (emoji string)
//! Deferred: roleAssigned/achievement/app/grouped payloads, `renote`
//! targetNoteId.

use mk_db::notification::NotificationRow;
use serde_json::{Value, json};

/// Upstream `NOTE_REQUIRED_NOTIFICATION_TYPES` (non-grouped, typed SOURCE
/// set): these must carry `note` or the notification is not shown.
pub const NOTE_BOUND_TYPES: &[&str] = &[
    "note",
    "mention",
    "reply",
    "renote",
    "quote",
    "reaction",
    "pollEnded",
];

pub fn is_note_bound(r#type: &str) -> bool {
    NOTE_BOUND_TYPES.contains(&r#type)
}

/// Assemble one `Packed<'Notification'>` subset. `notifier_lite` / `note`
/// omitted when their pack is `None` (upstream drops undefined fields; a
/// missing note/user means the CALLER skips the notification entirely).
pub fn pack_notification(
    row: &NotificationRow,
    notifier_lite: Option<Value>,
    note: Option<Value>,
    created_at_iso: String,
) -> Value {
    let mut o = serde_json::Map::new();
    o.insert("id".into(), json!(row.id));
    o.insert("createdAt".into(), json!(created_at_iso));
    o.insert("type".into(), json!(row.r#type));
    if let Some(notifier_id) = &row.notifier_id {
        o.insert("userId".into(), json!(notifier_id));
        if let Some(lite) = notifier_lite {
            o.insert("user".into(), lite);
        }
    }
    if let Some(note) = note {
        o.insert("note".into(), note);
    }
    if row.r#type == "reaction" {
        o.insert("reaction".into(), json!(row.reaction));
    }
    Value::Object(o)
}
