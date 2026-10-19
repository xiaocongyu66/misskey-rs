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

/// `ReactionService.decodeReaction` (upstream decodeCustomEmojiRegexp
/// `^:([\w+-]+)(?:@([\w.-]+))?:$`): a well-formed custom-emoji token
/// canonicalizes to `:name@host:` with the host defaulting to `.` (the
/// local instance) when omitted; anything else rides through untouched.
fn decode_custom_emoji_reaction(reaction: &str) -> String {
    let Some(body) = reaction.strip_prefix(':').and_then(|s| s.strip_suffix(':')) else {
        return reaction.to_string();
    };
    let (name, host) = match body.split_once('@') {
        Some((name, host)) => (name, Some(host)),
        None => (body, None),
    };
    let is_word = |s: &str, extra: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || extra.contains(c))
    };
    let host_ok = host.map(|h| is_word(h, ".-")).unwrap_or(true);
    if !is_word(name, "+-") || !host_ok {
        return reaction.to_string();
    }
    format!(":{name}@{}:", host.unwrap_or("."))
}

/// `convertLegacyReactions`: drop `count <= 0` entries, map legacy keys, merge.
/// Each key rides `convertLegacyReaction` — the custom-emoji DECODE happens
/// first (`:name:` → `:name@.:`), then the bare legacy short-form table.
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
        let key = convert_legacy_reaction(&decode_custom_emoji_reaction(&k));
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

#[cfg(test)]
mod tests {
    use super::*;

    // unit-services — upstream test/unit/ReactionService.ts
    // describe('convertLegacyReactions'), 1:1 with the upstream test names.

    #[test]
    fn empty_input_untouched() {
        // upstream '空の入力に対しては何もしない'
        assert_eq!(packed_reactions("{}"), json!({}));
        // non-JSON storage degenerates to the empty map (no legacy walk)
        assert_eq!(packed_reactions("null"), json!({}));
    }

    #[test]
    fn unicode_emoji_reactions_untouched() {
        // upstream 'Unicode絵文字リアクションを変換してしまわない'
        assert_eq!(
            packed_reactions(r#"{"👍": 1, "🍮": 2}"#),
            json!({ "👍": 1, "🍮": 2 })
        );
    }

    #[test]
    fn custom_emoji_reactions_untouched() {
        // upstream 'カスタム絵文字リアクションを変換してしまわない' — the
        // decoded host-present forms ride convertLegacyReaction unchanged
        // (the legacy table keys are the bare short forms only).
        assert_eq!(
            packed_reactions(r#"{":like@.:": 1, ":pudding@example.tld:": 2}"#),
            json!({ ":like@.:": 1, ":pudding@example.tld:": 2 })
        );
    }

    #[test]
    fn legacy_string_reactions_converted() {
        // upstream '文字列によるレガシーなリアクションを変換する'
        assert_eq!(
            packed_reactions(r#"{"like": 1, "pudding": 2}"#),
            json!({ "👍": 1, "🍮": 2 })
        );
    }
    #[test]
    fn host_omitted_custom_emoji_converted() {
        // upstream 'host部分が省略されたレガシーなカスタム絵文字リアクションを
        // 変換する' — decodeReaction defaults the missing host to '.'.
        assert_eq!(
            packed_reactions(r#"{":custom_emoji:": 1}"#),
            json!({ ":custom_emoji@.:": 1 })
        );
    }

    #[test]
    fn zero_count_entries_dropped() {
        // upstream '「0個のリアクション」情報を削除する'
        assert_eq!(packed_reactions(r#"{"angry": 0}"#), json!({}));
    }

    #[test]
    fn decoded_collision_merges_counts() {
        // upstream 'host部分の有無によりデコードすると同じ表記になるカスタム
        // 絵文字リアクションの個数情報を正しく足し合わせる'
        assert_eq!(
            packed_reactions(r#"{":custom_emoji:": 1, ":custom_emoji@.:": 2}"#),
            json!({ ":custom_emoji@.:": 3 })
        );
    }

    #[test]
    fn decode_rejects_non_word_tokens() {
        // decodeCustomEmojiRegexp is anchored: name [\w+-]+, host [\w.-]+.
        // Anything else rides through (then falls off the legacy table).
        assert_eq!(decode_custom_emoji_reaction(":a b:"), ":a b:");
        assert_eq!(decode_custom_emoji_reaction(":a@b@c:"), ":a@b@c:");
        assert_eq!(decode_custom_emoji_reaction("@.@"), "@.@");
        // no closing colon — not a custom-emoji token
        assert_eq!(decode_custom_emoji_reaction(":like@.:."), ":like@.:.");
        // host-omitted form decodes with the local-host default
        assert_eq!(decode_custom_emoji_reaction(":like:"), ":like@.:");
    }
}
