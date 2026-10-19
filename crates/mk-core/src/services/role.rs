//! Role/policy engine MVP — a truthful slice of upstream
//! `src/core/RoleService.ts`: `getUserPolicies`, `getUserBadgeRoles`, and the
//! `isModerator`/`isAdministrator` role checks, over the rows fetched by
//! [`mk_db::role::user_assigned_roles`].
//!
//! # Merge semantics (upstream `calc()`)
//! Base = `DEFAULT_POLICIES` merged with the instance `meta.policies` jsonb
//! (`{ ...DEFAULT_POLICIES, ...this.meta.policies }`) — see
//! [`UserPolicy::base_with_meta`]; [`UserPolicy::compute`] uses plain defaults
//! (no meta overrides).
//!
//! Per policy key, every assigned role contributes the entry
//! `role.policies[key]` = `{ priority, useDefault, value }`; a missing entry
//! counts as `{ priority: 0, useDefault: true }`. The highest priority tier
//! present wins (priority 2, else priority 1, else all), the base value is
//! substituted into every `useDefault: true` entry, and the tier is
//! aggregated with the key's combiner:
//! - booleans: any-true (`vs.some(v => v === true)`);
//! - numbers: max (`Math.max(...vs)`);
//! - `chatAvailability`: best of available > readonly > unavailable.
//! With no roles assigned the base value wins outright.
//!
//! # MVP scope and documented deviations
//! - Only DEFAULT_POLICY keys the user packer consumes (or will imminently)
//!   are mirrored (8 of upstream's 40); extend as packers adopt more.
//! - `isSilenced` is not a stored policy upstream: the packer derives
//!   `isSilenced: !policies.canPublicNote` ([`UserPolicy::is_silenced`]),
//!   and `canChat` derives `chatAvailability === 'available'`
//!   ([`UserPolicy::can_chat`]).
//! - Conditional roles (`role.target = 'conditional'`) are granted by
//!   `condFormula` evaluation — not implemented; only manual assignment rows
//!   are considered (under-privilege, never over).
//! - A malformed entry (`useDefault: false` with a wrong-typed `value`)
//!   falls back to the base value instead of upstream's `NaN`/`undefined`
//!   propagation.
//! - `maxFileSizeMb` aggregates as plain max; upstream additionally caps it
//!   at `Math.floor(config.maxFileSize / 1MB)` — apply the cap at call sites
//!   that have config.
//! - Root-user bypass (upstream `meta.rootUserId` in `isModerator`/
//!   `isAdministrator`) is the caller's check.
//! - `MeDetailed.policies` stays hardcoded `{}` in the packer until the full
//!   40-key set exists (an e2e deepStrictEqual on partial JSON would fail).
//!
//! # WIRING (next session — exact diff for user_pack.rs)
//! ```text
//! // once per user, next to the existing user/profile fetch:
//! let assigned = mk_db::role::user_assigned_roles(pool, &row.id).await?;
//! // then in pack_inline's `detailed(...)` call, replacing the literals:
//! let (badges, policy) = mk_core::services::role::badge_roles_and_policy(
//!     &assigned, /* viewer_is_moderator */ false);
//! // in the UserLite literal:  badge_roles: Some(badges),
//! // in the UserDetailedNotMe literal:
//! //   is_silenced: policy.is_silenced(),
//! //   can_chat: policy.can_chat(),
//! ```

use mk_db::role::AssignedRole;
use serde::Serialize;

/// Upstream `chatAvailability` policy values (serialized lowercase, matching
/// the misskey-js `RolePolicies` wire form).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatAvailability {
    Available,
    Readonly,
    Unavailable,
}

fn chat_from_str(s: &str) -> Option<ChatAvailability> {
    match s {
        "available" => Some(ChatAvailability::Available),
        "readonly" => Some(ChatAvailability::Readonly),
        "unavailable" => Some(ChatAvailability::Unavailable),
        _ => None,
    }
}

/// The policy slice our packer consumes, post-merge. Serializes with the
/// upstream camelCase key names (a partial `RolePolicies` view).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UserPolicy {
    #[serde(rename = "canPublicNote")]
    pub can_public_note: bool,
    #[serde(rename = "chatAvailability")]
    pub chat_availability: ChatAvailability,
    #[serde(rename = "mentionLimit")]
    pub mention_limit: i64,
    #[serde(rename = "driveCapacityMb")]
    pub drive_capacity_mb: i64,
    #[serde(rename = "maxFileSizeMb")]
    pub max_file_size_mb: f64,
    #[serde(rename = "uploadableFileTypes")]
    pub uploadable_file_types: Vec<String>,
    #[serde(rename = "pinLimit")]
    pub pin_limit: i64,
    #[serde(rename = "antennaLimit")]
    pub antenna_limit: i64,
    #[serde(rename = "wordMuteLimit")]
    pub word_mute_limit: i64,
    /// notes/create word forcing + drive upload/update alwaysMarkNsfw.
    #[serde(rename = "alwaysMarkNsfw")]
    pub always_mark_nsfw: bool,
    /// notes/translate gate (default true).
    #[serde(rename = "canUseTranslator")]
    pub can_use_translator: bool,
}

/// Upstream `DEFAULT_POLICIES` for the mirrored keys.
impl UserPolicy {
    pub fn base() -> Self {
        Self {
            can_public_note: true,
            chat_availability: ChatAvailability::Available,
            mention_limit: 20,
            drive_capacity_mb: 100,
            max_file_size_mb: 30.0,
            uploadable_file_types: vec!["*/*".to_string()],
            pin_limit: 5,
            antenna_limit: 5,
            word_mute_limit: 200,
            always_mark_nsfw: false,
            can_use_translator: true,
        }
    }

    /// `basePolicies = { ...DEFAULT_POLICIES, ...meta.policies }` — instance
    /// `meta.policies` jsonb overrides per key.
    pub fn base_with_meta(meta_policies: &serde_json::Value) -> Self {
        let mut base = Self::base();
        let Some(obj) = meta_policies.as_object() else {
            return base;
        };
        if let Some(v) = obj.get("canPublicNote").and_then(|v| v.as_bool()) {
            base.can_public_note = v;
        }
        if let Some(v) = obj
            .get("chatAvailability")
            .and_then(|v| v.as_str())
            .and_then(chat_from_str)
        {
            base.chat_availability = v;
        }
        if let Some(v) = obj.get("mentionLimit").and_then(|v| v.as_i64()) {
            base.mention_limit = v;
        }
        if let Some(v) = obj.get("driveCapacityMb").and_then(|v| v.as_i64()) {
            base.drive_capacity_mb = v;
        }
        if let Some(v) = obj.get("maxFileSizeMb").and_then(|v| v.as_f64()) {
            base.max_file_size_mb = v;
        }
        if let Some(arr) = obj.get("uploadableFileTypes").and_then(|v| v.as_array()) {
            base.uploadable_file_types = arr
                .iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect();
        }
        if let Some(v) = obj.get("pinLimit").and_then(|v| v.as_i64()) {
            base.pin_limit = v;
        }
        if let Some(v) = obj.get("antennaLimit").and_then(|v| v.as_i64()) {
            base.antenna_limit = v;
        }
        if let Some(v) = obj.get("wordMuteLimit").and_then(|v| v.as_i64()) {
            base.word_mute_limit = v;
        }
        base
    }

    /// UserEntityService pack: `isSilenced: policies.canPublicNote ? false : true`.
    pub fn is_silenced(&self) -> bool {
        !self.can_public_note
    }

    /// UserEntityService pack: `canChat: policies.chatAvailability === 'available'`.
    pub fn can_chat(&self) -> bool {
        self.chat_availability == ChatAvailability::Available
    }

    /// `getUserPolicies(userId)` — merge `base` through the assigned roles'
    /// per-key priority tiers.
    pub fn from_base_and_roles(assigned: &[AssignedRole], base: &Self) -> Self {
        Self {
            uploadable_file_types: merge_types(
                assigned,
                &base.uploadable_file_types,
                "uploadableFileTypes",
            ),
            can_public_note: merge_any_true(assigned, base.can_public_note, "canPublicNote"),
            chat_availability: merge_chat(assigned, base.chat_availability, "chatAvailability"),
            mention_limit: merge_max(assigned, base.mention_limit, "mentionLimit"),
            drive_capacity_mb: merge_max(assigned, base.drive_capacity_mb, "driveCapacityMb"),
            max_file_size_mb: merge_f64(assigned, base.max_file_size_mb, "maxFileSizeMb"),
            pin_limit: merge_max(assigned, base.pin_limit, "pinLimit"),
            antenna_limit: merge_max(assigned, base.antenna_limit, "antennaLimit"),
            word_mute_limit: merge_max(assigned, base.word_mute_limit, "wordMuteLimit"),
            always_mark_nsfw: merge_any_true(assigned, base.always_mark_nsfw, "alwaysMarkNsfw"),
            can_use_translator: merge_any_true(
                assigned,
                base.can_use_translator,
                "canUseTranslator",
            ),
        }
    }

    /// The common path: assigned roles merged over plain `DEFAULT_POLICIES`.
    pub fn compute(assigned: &[AssignedRole]) -> Self {
        Self::from_base_and_roles(assigned, &Self::base_with_meta(&serde_json::Value::Null))
    }
}

/// One role's policy entry: upstream `{ priority, useDefault, value }`.
/// A missing key (or a non-object) counts as `{ priority: 0, useDefault: true }`.
#[derive(Debug, Clone, Copy)]
struct PolicyEntry<'a> {
    priority: i64,
    use_default: bool,
    value: Option<&'a serde_json::Value>,
}

fn entry<'a>(role: &'a AssignedRole, key: &str) -> PolicyEntry<'a> {
    let Some(v) = role.policies.get(key) else {
        return PolicyEntry {
            priority: 0,
            use_default: true,
            value: None,
        };
    };
    let Some(o) = v.as_object() else {
        return PolicyEntry {
            priority: 0,
            use_default: true,
            value: None,
        };
    };
    PolicyEntry {
        priority: o.get("priority").and_then(|v| v.as_i64()).unwrap_or(0),
        use_default: o
            .get("useDefault")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        value: o.get("value"),
    }
}

/// Upstream `calc`'s tier pick: priority-2 entries if any, else priority-1,
/// else every entry (priority 0).
fn tier<'a>(assigned: &'a [AssignedRole], key: &str) -> Vec<PolicyEntry<'a>> {
    let entries: Vec<PolicyEntry<'a>> = assigned.iter().map(|r| entry(r, key)).collect();
    for p in [2, 1] {
        let t: Vec<PolicyEntry<'a>> = entries
            .iter()
            .filter(|e| e.priority == p)
            .copied()
            .collect();
        if !t.is_empty() {
            return t;
        }
    }
    entries
}

/// Entry value with the base substituted for `useDefault` (and for
/// malformed values — see module deviation notes).
fn entry_value<'a>(e: &'a PolicyEntry<'_>, base: &'a serde_json::Value) -> &'a serde_json::Value {
    if e.use_default {
        return base;
    }
    match e.value {
        Some(v) => v,
        None => base,
    }
}

/// uploadableFileTypes tier pick — array values REPLACE wholesale.
fn merge_types(assigned: &[AssignedRole], base: &[String], key: &str) -> Vec<String> {
    if assigned.is_empty() {
        return base.to_vec();
    }
    let base_json = serde_json::Value::Array(
        base.iter()
            .map(|s| serde_json::Value::String(s.clone()))
            .collect(),
    );
    let mut out = base.to_vec();
    for e in tier(assigned, key) {
        if let Some(arr) = entry_value(&e, &base_json).as_array() {
            out = arr
                .iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect();
        }
    }
    out
}

fn merge_f64(assigned: &[AssignedRole], base_val: f64, key: &str) -> f64 {
    if assigned.is_empty() {
        return base_val;
    }
    let base_json = serde_json::json!(base_val);
    // upstream calc(): `aggregate(policies.map(policy => policy.useDefault ?
    // basePolicies[name] : policy.value))` — the fold covers exactly the
    // tier's substituted values with NO base seed (Math.max(...vs)); a base
    // seed would let DEFAULT_POLICIES 30 mask a fractional override.
    tier(assigned, key)
        .iter()
        .map(|e| entry_value(e, &base_json).as_f64().unwrap_or(base_val))
        .fold(f64::NEG_INFINITY, f64::max)
}

fn merge_any_true(assigned: &[AssignedRole], base_val: bool, key: &str) -> bool {
    if assigned.is_empty() {
        return base_val;
    }
    let base_json = serde_json::Value::Bool(base_val);
    tier(assigned, key)
        .iter()
        .any(|e| entry_value(e, &base_json).as_bool().unwrap_or(base_val))
}

fn merge_max(assigned: &[AssignedRole], base_val: i64, key: &str) -> i64 {
    if assigned.is_empty() {
        return base_val;
    }
    let base_json = serde_json::Value::Number(base_val.into());
    tier(assigned, key)
        .iter()
        .map(|e| entry_value(e, &base_json).as_i64().unwrap_or(base_val))
        .max()
        .unwrap_or(base_val)
}

fn merge_chat(
    assigned: &[AssignedRole],
    base_val: ChatAvailability,
    key: &str,
) -> ChatAvailability {
    if assigned.is_empty() {
        return base_val;
    }
    let base_json = serde_json::Value::String(chat_str(base_val).to_string());
    let rank = |c: ChatAvailability| match c {
        ChatAvailability::Available => 2,
        ChatAvailability::Readonly => 1,
        ChatAvailability::Unavailable => 0,
    };
    tier(assigned, key)
        .iter()
        .map(|e| {
            entry_value(e, &base_json)
                .as_str()
                .and_then(chat_from_str)
                .unwrap_or(base_val)
        })
        .max_by_key(|c| rank(*c))
        .unwrap_or(base_val)
}

fn chat_str(c: ChatAvailability) -> &'static str {
    match c {
        ChatAvailability::Available => "available",
        ChatAvailability::Readonly => "readonly",
        ChatAvailability::Unavailable => "unavailable",
    }
}

/// Upstream `isModerator`'s role part
/// (`roles.some(r => r.isModerator || r.isAdministrator)`) — the root-user
/// bypass is the caller's check.
pub fn has_moderator_role(assigned: &[AssignedRole]) -> bool {
    assigned
        .iter()
        .any(|r| r.is_moderator || r.is_administrator)
}

/// Upstream `isAdministrator`'s role part
/// (`roles.some(r => r.isAdministrator)`) — the root-user bypass is the
/// caller's check.
pub fn has_administrator_role(assigned: &[AssignedRole]) -> bool {
    assigned.iter().any(|r| r.is_administrator)
}

/// Upstream badgeRoles slice (`getUserBadgeRoles` + the packer filter):
/// assigned roles with `asBadge`, visible when public or viewed by a
/// moderator (`rs.filter(r => r.isPublic || iAmModerator)`), sorted
/// `"displayOrder" DESC`.
pub fn badge_roles(
    assigned: &[AssignedRole],
    viewer_is_moderator: bool,
) -> Vec<crate::packers::user::BadgeRole> {
    let mut out: Vec<crate::packers::user::BadgeRole> = assigned
        .iter()
        .filter(|r| r.as_badge && (r.is_public || viewer_is_moderator))
        .map(|r| crate::packers::user::BadgeRole {
            name: r.name.clone(),
            icon_url: r.icon_url.clone(),
            display_order: r.display_order,
        })
        .collect();
    out.sort_by(|a, b| b.display_order.cmp(&a.display_order));
    out
}

/// PR-ready wiring hook: everything the user pack needs from the role engine
/// in one call — the packer-visible badge list and the merged policy block.
/// See the module docs for the exact `user_pack.rs` diff.
pub fn badge_roles_and_policy(
    assigned: &[AssignedRole],
    viewer_is_moderator: bool,
) -> (Vec<crate::packers::user::BadgeRole>, UserPolicy) {
    (
        badge_roles(assigned, viewer_is_moderator),
        UserPolicy::compute(assigned),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn role(id: &str, display_order: i64, policies: serde_json::Value) -> AssignedRole {
        AssignedRole {
            id: id.to_string(),
            name: format!("Role {id}"),
            is_public: true,
            is_moderator: false,
            is_administrator: false,
            as_badge: false,
            color: None,
            icon_url: None,
            description: String::new(),
            display_order,
            policies,
        }
    }

    /// `{ priority, useDefault, value }` entry as stored in `role.policies`.
    fn pe(value: serde_json::Value, priority: i64, use_default: bool) -> serde_json::Value {
        json!({ "priority": priority, "useDefault": use_default, "value": value })
    }

    #[test]
    fn no_roles_yields_base_defaults() {
        assert_eq!(UserPolicy::compute(&[]), UserPolicy::base());
        assert_eq!(UserPolicy::base().pin_limit, 5);
        assert_eq!(UserPolicy::base().drive_capacity_mb, 100);
        assert_eq!(UserPolicy::base().can_public_note, true);
        assert_eq!(
            UserPolicy::base().chat_availability,
            ChatAvailability::Available
        );
    }

    #[test]
    fn empty_role_policies_keep_base_defaults() {
        let roles = vec![role("r1", 0, json!({}))];
        assert_eq!(UserPolicy::compute(&roles), UserPolicy::base());
    }

    #[test]
    fn explicit_override_and_usedefault_flag() {
        // useDefault=false: the role's value replaces base.
        let roles = vec![role("r1", 0, json!({ "pinLimit": pe(json!(9), 0, false) }))];
        let p = UserPolicy::compute(&roles);
        assert_eq!(p.pin_limit, 9);
        assert_eq!(p.mention_limit, 20); // untouched key stays base

        // useDefault=true: the role's value field must be ignored.
        let roles = vec![role(
            "r1",
            0,
            json!({ "pinLimit": pe(json!(999), 0, true) }),
        )];
        assert_eq!(UserPolicy::compute(&roles).pin_limit, 5);

        // same shape for an any-true boolean key.
        let roles = vec![role(
            "r1",
            0,
            json!({ "canPublicNote": pe(json!(false), 0, false) }),
        )];
        let p = UserPolicy::compute(&roles);
        assert!(!p.can_public_note);
        assert!(p.is_silenced());
    }

    #[test]
    fn boolean_any_true_aggregation() {
        let r1 = role(
            "r1",
            0,
            json!({ "canPublicNote": pe(json!(false), 0, false) }),
        );
        let r2 = role(
            "r2",
            1,
            json!({ "canPublicNote": pe(json!(true), 0, false) }),
        );
        assert!(UserPolicy::compute(&[r1, r2]).can_public_note);
    }

    #[test]
    fn priority_tiers_two_beats_one_beats_zero() {
        // p0 says 100, p1 says 3 → the p1 tier wins → 3.
        let ra = role("rA", 0, json!({ "pinLimit": pe(json!(100), 0, false) }));
        let rb = role("rB", 1, json!({ "pinLimit": pe(json!(3), 1, false) }));
        assert_eq!(UserPolicy::compute(&[ra.clone(), rb]).pin_limit, 3);

        // a p2 entry beats both lower tiers regardless of their values.
        let rc = role("rC", 2, json!({ "pinLimit": pe(json!(7), 2, false) }));
        assert_eq!(UserPolicy::compute(&[rc, ra]).pin_limit, 7);
    }

    #[test]
    fn higher_tier_usedefault_falls_back_to_base() {
        // The p2 entry says "use the default" — the p0 override must lose
        // and the base default (5) wins.
        let roles = vec![
            role("rA", 0, json!({ "pinLimit": pe(json!(50), 0, false) })),
            role("rB", 1, json!({ "pinLimit": pe(json!(99), 2, true) })),
        ];
        assert_eq!(UserPolicy::compute(&roles).pin_limit, 5);
    }

    #[test]
    fn max_aggregation_within_tier() {
        let roles = vec![
            role(
                "r1",
                0,
                json!({ "driveCapacityMb": pe(json!(500), 0, false) }),
            ),
            role(
                "r2",
                1,
                json!({ "driveCapacityMb": pe(json!(5000), 0, false) }),
            ),
            role("r3", 2, json!({ "pinLimit": pe(json!(1), 0, false) })),
        ];
        assert_eq!(UserPolicy::compute(&roles).drive_capacity_mb, 5000);
    }

    #[test]
    fn chat_availability_best_wins() {
        let mk = |a: &str, b: &str| {
            vec![
                role(
                    "r1",
                    0,
                    json!({ "chatAvailability": pe(json!(a), 0, false) }),
                ),
                role(
                    "r2",
                    1,
                    json!({ "chatAvailability": pe(json!(b), 0, false) }),
                ),
            ]
        };
        let p = UserPolicy::compute(&mk("readonly", "unavailable"));
        assert_eq!(p.chat_availability, ChatAvailability::Readonly);
        assert!(!p.can_chat());

        let p = UserPolicy::compute(&mk("available", "unavailable"));
        assert_eq!(p.chat_availability, ChatAvailability::Available);
        assert!(p.can_chat());

        // single full-unavailable role disables chat.
        let p = UserPolicy::compute(&[role(
            "r1",
            0,
            json!({ "chatAvailability": pe(json!("unavailable"), 0, false) }),
        )]);
        assert!(!p.can_chat());
    }

    #[test]
    fn chat_role_usedefault_ignores_role_value() {
        let roles = vec![role(
            "r1",
            0,
            json!({ "chatAvailability": pe(json!("unavailable"), 0, true) }),
        )];
        let p = UserPolicy::compute(&roles);
        assert_eq!(p.chat_availability, ChatAvailability::Available);
    }

    #[test]
    fn base_with_meta_overrides_defaults() {
        let meta =
            json!({ "pinLimit": 50, "canPublicNote": false, "chatAvailability": "readonly" });
        let base = UserPolicy::base_with_meta(&meta);
        assert_eq!(base.pin_limit, 50);
        assert!(!base.can_public_note);
        assert_eq!(base.chat_availability, ChatAvailability::Readonly);
        // untouched keys stay at DEFAULT_POLICIES values.
        assert_eq!(base.mention_limit, 20);
        assert_eq!(base.drive_capacity_mb, 100);

        // null / non-object meta changes nothing.
        assert_eq!(
            UserPolicy::base_with_meta(&serde_json::Value::Null),
            UserPolicy::base()
        );
        assert_eq!(
            UserPolicy::base_with_meta(&json!([1, 2])),
            UserPolicy::base()
        );

        // base+roles compose: the role's explicit override (p0, 9) replaces
        // the meta-raised base (50) for that key.
        let base = UserPolicy::base_with_meta(&json!({ "pinLimit": 50 }));
        let roles = vec![role("r1", 0, json!({ "pinLimit": pe(json!(9), 0, false) }))];
        assert_eq!(UserPolicy::from_base_and_roles(&roles, &base).pin_limit, 9);
    }

    #[test]
    fn malformed_entries_fall_back_to_base() {
        // useDefault=false but a wrong-typed value → base (documented
        // deviation from upstream's NaN propagation).
        let roles = vec![role(
            "r1",
            0,
            json!({ "pinLimit": pe(json!("lots"), 0, false) }),
        )];
        assert_eq!(UserPolicy::compute(&roles).pin_limit, 5);
        // entry not an object at all → treated as missing (useDefault).
        let roles = vec![role("r1", 0, json!({ "pinLimit": 42 }))];
        assert_eq!(UserPolicy::compute(&roles).pin_limit, 5);
    }

    #[test]
    fn serializes_with_upstream_camelcase_keys() {
        let v = serde_json::to_value(UserPolicy::base()).unwrap();
        assert_eq!(v["canPublicNote"], json!(true));
        assert_eq!(v["chatAvailability"], json!("available"));
        assert_eq!(v["pinLimit"], json!(5));
    }

    #[test]
    fn moderator_and_administrator_flags() {
        let mut r = role("r1", 0, json!({}));
        assert!(!has_moderator_role(&[r.clone()]));
        r.is_moderator = true;
        assert!(has_moderator_role(&[r.clone()]));
        assert!(!has_administrator_role(&[r.clone()]));
        r.is_administrator = true;
        assert!(has_moderator_role(&[r.clone()]));
        assert!(has_administrator_role(&[r]));
    }

    #[test]
    fn badge_roles_filter_and_order() {
        let mut r_pub_badge = role("rb1", 1, json!({}));
        r_pub_badge.as_badge = true;
        let mut r_priv_badge = role("rb2", 5, json!({}));
        r_priv_badge.as_badge = true;
        r_priv_badge.is_public = false;
        let mut r_not_badge = role("rn", 99, json!({}));
        r_not_badge.as_badge = false;

        let assigned = vec![r_pub_badge, r_priv_badge, r_not_badge];
        // anonymous viewer: only the public badge, displayOrder DESC sort.
        let badges = badge_roles(&assigned, false);
        assert_eq!(badges.len(), 1);
        assert_eq!(badges[0].name, "Role rb1");

        // moderator viewer sees the private badge first (higher displayOrder).
        let badges = badge_roles(&assigned, true);
        let names: Vec<&str> = badges.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, vec!["Role rb2", "Role rb1"]);

        // two public badges sort display-order DESC.
        let mut lo = role("rb3", 2, json!({}));
        lo.as_badge = true;
        let mut hi = role("rb4", 8, json!({}));
        hi.as_badge = true;
        let badges = badge_roles(&[lo, hi], false);
        let orders: Vec<i64> = badges.iter().map(|b| b.display_order).collect();
        assert_eq!(orders, vec![8, 2]);
    }

    #[test]
    fn unit_services_with_role_beats_instance_default() {
        // unit-services — upstream RoleService.ts 'getUserPolicies with role':
        // instance meta.policies forces the key false, an assigned role
        // overrides true (useDefault: false, priority 0) → true. Mirrored on
        // canPublicNote (the boolean key our policy slice carries; upstream
        // uses canManageCustomEmojis, not mirrored here).
        let base = UserPolicy::base_with_meta(&json!({ "canPublicNote": false }));
        let roles = vec![role(
            "r1",
            0,
            json!({ "canPublicNote": pe(json!(true), 0, false) }),
        )];
        assert!(UserPolicy::from_base_and_roles(&roles, &base).can_public_note);
    }

    #[test]
    fn unit_services_role_with_no_policy_set() {
        // unit-services — upstream RoleService.ts 'getUserPolicies role with
        // no policy set': an empty-policies role rides the base value; the
        // pinLimit override (10) beats the meta default (5) under max
        // aggregation.
        let base = UserPolicy::base_with_meta(&json!({ "pinLimit": 5 }));
        let roles = vec![
            role("with", 0, json!({ "pinLimit": pe(json!(10), 0, false) })),
            role("without", 1, json!({})),
        ];
        assert_eq!(UserPolicy::from_base_and_roles(&roles, &base).pin_limit, 10);
    }

    #[tokio::test]
    async fn db_backed_no_roles_yields_base_and_role_override_merges() {
        if std::env::var("DATABASE_URL").is_err() {
            return;
        }
        let pool = mk_db::test_support::fresh_db("mkcore_role_policy").await;
        mk_db::run_migrations(&pool).await.expect("migrations");

        mk_db::sqlx::query(
            r#"INSERT INTO "user" ("id", "username", "usernameLower") VALUES ('u1', 'userone', 'userone')"#,
        )
        .execute(&pool)
        .await
        .expect("insert user");

        // No assignments → base policy defaults.
        let assigned = mk_db::role::user_assigned_roles(&pool, "u1").await.unwrap();
        assert!(assigned.is_empty());
        assert_eq!(UserPolicy::compute(&assigned), UserPolicy::base());

        // Role with empty policies jsonb → still base defaults.
        mk_db::sqlx::query(
            r#"
            INSERT INTO "role" ("id", "updatedAt", "name", "description", "lastUsedAt", "policies")
            VALUES ('rEmpty', now(), 'EmptyRole', 'empty', now(), '{}'::jsonb)
            "#,
        )
        .execute(&pool)
        .await
        .expect("insert role");
        mk_db::sqlx::query(
            r#"INSERT INTO "role_assignment" ("id", "userId", "roleId") VALUES ('a1', 'u1', 'rEmpty')"#,
        )
        .execute(&pool)
        .await
        .expect("insert assignment");
        let assigned = mk_db::role::user_assigned_roles(&pool, "u1").await.unwrap();
        assert_eq!(assigned.len(), 1);
        assert_eq!(UserPolicy::compute(&assigned), UserPolicy::base());

        // Role with a pinLimit override → merged result.
        mk_db::sqlx::query(
            r#"
            INSERT INTO "role" ("id", "updatedAt", "name", "description", "lastUsedAt", "policies")
            VALUES ('rPin', now(), 'Pinner', 'pins', now(), $1::jsonb)
            "#,
        )
        .bind(r#"{"pinLimit":{"priority":0,"useDefault":false,"value":9}}"#)
        .execute(&pool)
        .await
        .expect("insert role");
        mk_db::sqlx::query(
            r#"INSERT INTO "role_assignment" ("id", "userId", "roleId") VALUES ('a2', 'u1', 'rPin')"#,
        )
        .execute(&pool)
        .await
        .expect("insert assignment");
        let assigned = mk_db::role::user_assigned_roles(&pool, "u1").await.unwrap();
        assert_eq!(assigned.len(), 2, "displayOrder DESC puts rPin first");
        // also exercise the wiring hook end-to-end.
        let (badges, policy) = badge_roles_and_policy(&assigned, false);
        assert_eq!(policy.pin_limit, 9);
        assert_eq!(policy.mention_limit, 20);
        assert!(badges.is_empty()); // neither role is asBadge
    }
}
