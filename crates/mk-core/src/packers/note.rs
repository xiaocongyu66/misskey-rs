//! Note pack — M1 minimal slice.
//!
//! TODO(M2): replace the JSON assembly with a full `Packed<Note>` struct
//! (`src/models/json-schema/note.ts`, all 35 fields incl. reply/renote
//! recursion, poll, files, clips).

use mk_db::note::NoteRow;
use serde_json::{json, Value};

fn string_vec(v: &serde_json::Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// `NoteEntityService.pack` field presence mirrored for the M1 subset:
/// schema `optional: true` fields whose upstream value would be
/// `undefined` are ABSENT; everything else always serialized.
pub fn note_pack_min(
    row: &NoteRow,
    user_lite: Value,
    created_at_iso: String,
    mentions: &[String],
    tags: &[String],
    emojis: Value,
) -> Value {
    let visibility = match row.visibility.as_str() {
        "home" | "followers" | "specified" => row.visibility.clone(),
        _ => "public".to_string(),
    };

    let mut o = serde_json::Map::new();
    o.insert("id".into(), json!(row.id));
    o.insert("createdAt".into(), json!(created_at_iso));
    // deletedAt: note.deletedAt ?? undefined → absent (never set in M1)
    o.insert("text".into(), json!(row.text));
    if let Some(cw) = &row.cw {
        o.insert("cw".into(), json!(cw));
    }
    o.insert("userId".into(), json!(row.user_id));
    o.insert("user".into(), user_lite);
    if let Some(rid) = &row.reply_id {
        o.insert("replyId".into(), json!(rid));
    }
    if let Some(rid) = &row.renote_id {
        o.insert("renoteId".into(), json!(rid));
    }
    o.insert("visibility".into(), json!(visibility));
    o.insert("mentions".into(), json!(mentions));
    o.insert("visibleUserIds".into(), json!(Vec::<String>::new()));
    o.insert("fileIds".into(), json!(Vec::<String>::new()));
    o.insert("files".into(), json!(Vec::<Value>::new()));
    o.insert("tags".into(), json!(tags));
    if row.has_poll {
        o.insert("hasPoll".into(), json!(true));
        // poll body is M2
    }
    o.insert("localOnly".into(), json!(row.local_only));
    o.insert(
        "reactionAcceptance".into(),
        row.reaction_acceptance
            .as_str()
            .map(|s| json!(s))
            .unwrap_or(Value::Null),
    );
    o.insert("reactionEmojis".into(), json!({}));
    o.insert("emojis".into(), emojis);
    o.insert("reactions".into(), json!({}));
    o.insert("reactionCount".into(), json!(0));
    o.insert("renoteCount".into(), json!(0));
    o.insert("repliesCount".into(), json!(0));
    Value::Object(o)
}

/// Field-set contract for unit tests (e2e deepStrictEqual harness parity).
pub fn m1_note_fields() -> &'static [&'static str] {
    &[
        "id",
        "createdAt",
        "text",
        "userId",
        "user",
        "visibility",
        "mentions",
        "visibleUserIds",
        "fileIds",
        "files",
        "tags",
        "localOnly",
        "reactionAcceptance",
        "reactionEmojis",
        "emojis",
        "reactions",
        "reactionCount",
        "renoteCount",
        "repliesCount",
    ]
}

#[allow(dead_code)]
fn _tag_fixup(v: &Value) -> Vec<String> {
    string_vec(v)
}
