//! `Packed<'Note>` assembly — `NoteEntityService.pack` (detail / lite).
//!
//! Field presence mirrors pack/packDetail: schema-optional fields whose
//! upstream value would be `undefined` are ABSENT. The M1 slice pinned
//! `mentions`/`tags`/`visibleUserIds`/`emojis`/`cw` as always-present for
//! the local-only note shapes producible at M1; that quirk is kept here
//! (upstream omits empty mentions/tags, non-specified visibleUserIds, null
//! cw, and emojis for local authors) until the e2e field-set fixture swap.
//!
//! Reacted-state inputs (`reactions` jsonb, myReaction) come straight from
//! `mk-db::note`; legacy reaction keys are converted like
//! `ReactionService.convertLegacyReaction(s)` (zero-count entries filtered).

use mk_db::note::NoteRow;
use serde_json::{Map, Value, json};

/// Inputs for one packed note. `reply`/`renote` are only Some when the row
/// has the corresponding id; `Some(Value::Null)` models upstream's
/// "JOINed but target missing" null resolution, `Some(v)` a packed child.
pub struct NotePackIn<'a> {
    pub row: &'a NoteRow,
    pub user_lite: Value,
    pub created_at: String,
    pub emojis: Value,
    pub reply: Option<Value>,
    pub renote: Option<Value>,
    pub my_reaction: Option<String>,
}

/// `ReactionService` legacy short-form table.
const LEGACY_REACTIONS: &[(&str, &str)] = &[
    ("like", "👍"),
    ("love", "❤"),
    ("laugh", "😆"),
    ("hmm", "🤔"),
    ("surprise", "😮"),
    ("congrats", "🎉"),
    ("angry", "💢"),
    ("confused", "😥"),
    ("rip", "😇"),
    ("pudding", "🍮"),
    ("star", "⭐"),
];

pub fn convert_legacy_reaction(reaction: &str) -> String {
    for (k, v) in LEGACY_REACTIONS {
        if *k == reaction {
            return (*v).to_string();
        }
    }
    reaction.to_string()
}

/// `convertLegacyReactions`: drop `count <= 0` entries, map legacy keys, merge.
pub fn packed_reactions(raw: &str) -> Value {
    let Ok(map) = serde_json::from_str::<Map<String, Value>>(raw) else {
        return json!({});
    };
    let mut out = Map::new();
    for (k, v) in map {
        let count = v.as_i64().unwrap_or(0);
        if count <= 0 {
            continue;
        }
        let key = convert_legacy_reaction(&k);
        let slot = out.entry(key).or_insert(json!(0));
        *slot = json!(slot.as_i64().unwrap_or(0) + count);
    }
    Value::Object(out)
}

pub fn reaction_count(reactions: &Value) -> i64 {
    reactions
        .as_object()
        .map(|m| m.values().filter_map(|v| v.as_i64()).sum())
        .unwrap_or(0)
}

/// Shared non-detail fields (pack with detail: false).
pub fn pack_note_lite(inp: &NotePackIn<'_>) -> Value {
    let row = inp.row;
    let visibility = match row.visibility.as_str() {
        "home" | "followers" | "specified" => row.visibility.clone(),
        _ => "public".to_string(),
    };

    let reactions = packed_reactions(&row.reactions);

    let mut o = Map::new();
    o.insert("id".into(), json!(row.id));
    o.insert("createdAt".into(), json!(inp.created_at));
    // deletedAt is not a packed field upstream
    o.insert("text".into(), json!(row.text));
    // M1 slice quirk: cw is null-only-when-unset upstream (always present);
    // the pinned M1 test keeps it absent-when-null.
    if let Some(cw) = &row.cw {
        o.insert("cw".into(), json!(cw));
    }
    o.insert("userId".into(), json!(row.user_id));
    o.insert("user".into(), inp.user_lite.clone());
    if let Some(rid) = &row.reply_id {
        o.insert("replyId".into(), json!(rid));
    }
    if let Some(rid) = &row.renote_id {
        o.insert("renoteId".into(), json!(rid));
    }
    o.insert("visibility".into(), json!(visibility));
    o.insert("mentions".into(), json!(row.mentions));
    // upstream: present only for 'specified'; M1 pin keeps it always (see above)
    o.insert("visibleUserIds".into(), json!(Vec::<String>::new()));
    o.insert("fileIds".into(), json!(row.file_ids));
    o.insert("files".into(), json!(Vec::<Value>::new()));
    o.insert("tags".into(), json!(row.tags));
    if row.has_poll {
        o.insert("hasPoll".into(), json!(true));
        // poll body is M2
    }
    o.insert("localOnly".into(), json!(row.local_only));
    o.insert(
        "reactionAcceptance".into(),
        row.reaction_acceptance
            .as_deref()
            .map(|s| json!(s))
            .unwrap_or(Value::Null),
    );
    if let Some(ch) = &row.channel_id {
        o.insert("channelId".into(), json!(ch));
    }
    o.insert("reactionEmojis".into(), json!({}));
    o.insert("emojis".into(), inp.emojis.clone());
    o.insert("reactions".into(), reactions.clone());
    o.insert(
        "reactionCount".into(),
        json!(
            reactions
                .as_object()
                .map(|m| m.values().filter_map(|v| v.as_i64()).sum::<i64>())
                .unwrap_or(0)
        ),
    );
    o.insert("renoteCount".into(), json!(row.renote_count));
    o.insert("repliesCount".into(), json!(row.replies_count));
    if let Some(uri) = &row.uri {
        o.insert("uri".into(), json!(uri));
    }
    if let Some(url) = &row.url {
        o.insert("url".into(), json!(url));
    }
    Value::Object(o)
}

/// `pack` with `detail: true`: + clippedCount, reply, renote, myReaction
/// (poll stays M2 — M1 inserts never set hasPoll).
pub fn pack_note_detail(inp: &NotePackIn<'_>) -> Value {
    let mut o = match pack_note_lite(inp) {
        Value::Object(map) => map,
        _ => unreachable!(),
    };
    o.insert("clippedCount".into(), json!(inp.row.clipped_count));
    if inp.row.reply_id.is_some() {
        // JOINed-but-missing resolves null upstream
        o.insert("reply".into(), inp.reply.clone().unwrap_or(Value::Null));
    }
    if inp.row.renote_id.is_some() {
        o.insert("renote".into(), inp.renote.clone().unwrap_or(Value::Null));
    }
    if let Some(mr) = &inp.my_reaction {
        o.insert("myReaction".into(), json!(mr));
    }
    Value::Object(o)
}
