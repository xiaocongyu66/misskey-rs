//! roles-v2: the conditional-role engine + the Role-pack policy surface.
//! Upstream `src/core/RoleService.ts` (pinned ea2c3aed):
//! - `DEFAULT_POLICIES` — the full 39-key policy table the Role pack fills
//!   missing/foreign keys from (RoleEntityService.pack lines 47-56: clone,
//!   then for every DEFAULT key `policies[k] == null` gets
//!   `{ useDefault: true, priority: 0, value }`);
//! - `evalCond` — the RoleCondFormulaValue walk (and/or/not/roleAssignedTo,
//!   isLocal/isRemote/isSuspended/isLocked/isBot/isCat/isExplorable,
//!   createdLessThan/createdMoreThan, followers/following/notes
//!   LessThanOrEq/MoreThanOrEq; unknown types eval false inside the try);
//! - `getUserRoles` — assigned (manual, non-expired) roles first, then every
//!   conditional whose formula matches; `UserEntityService.pack` public
//!   projection filters `isPublic` and sorts `displayOrder` DESC;
//! - `getUserPolicies` canPublicNote feed for `isSilenced`
//!   (`!policies.canPublicNote`).
//!
//! # roles-v2 documented deviation (canPublicNote explicit-override)
//! Upstream's `calc` aggregates ALL role entries with any-true, so a
//! `{ priority: 0, useDefault: true }` entry (the conditional-role
//! default-policies fill the upstream test utils themselves send) votes the
//! DEFAULT `true` into `canPublicNote` even when another role explicitly
//! forces `false` — `isSilenced` would read false for a user carrying an
//! explicit silencing role. The roles.ts arena asserts the explicit
//! `useDefault: false` entries WIN (`sil.isSilenced === true`), so
//! [`can_public_note_effective`] first picks the priority tier as upstream,
//! then, if the tier carries any explicit (`useDefault: false`) entries,
//! aggregates ONLY those values. Absent explicit entries the merge
//! degenerates to the upstream any-true semantics — existing lanes that
//! never send explicit booleans are unaffected.

use std::sync::OnceLock;

use mk_db::role::AssignedRole;

pub fn default_policies_value() -> &'static serde_json::Value {
    static DEFAULTS: OnceLock<serde_json::Value> = OnceLock::new();
    DEFAULTS.get_or_init(|| {
        serde_json::json!({
            "gtlAvailable": true,
            "ltlAvailable": true,
            "canPublicNote": true,
            "mentionLimit": 20,
            "canInvite": false,
            "inviteLimit": 0,
            // 60 * 24 * 7
            "inviteLimitCycle": 10080,
            "inviteExpirationTime": 0,
            "canManageCustomEmojis": false,
            "canManageAvatarDecorations": false,
            "canSearchNotes": false,
            "canSearchUsers": true,
            "canUseTranslator": true,
            "canHideAds": false,
            "canCreateChannel": true,
            "driveCapacityMb": 100,
            "maxFileSizeMb": 30,
            "alwaysMarkNsfw": false,
            "canUpdateBioMedia": true,
            "pinLimit": 5,
            "antennaLimit": 5,
            "wordMuteLimit": 200,
            "webhookLimit": 3,
            "clipLimit": 10,
            "noteEachClipsLimit": 200,
            "userListLimit": 10,
            "userEachUserListsLimit": 50,
            "rateLimitFactor": 1,
            "avatarDecorationLimit": 1,
            "canImportAntennas": false,
            "canImportBlocking": false,
            "canImportFollowing": false,
            "canImportMuting": false,
            "canImportUserLists": false,
            "chatAvailability": "available",
            "uploadableFileTypes": [
                "text/*",
                "application/json",
                "image/*",
                "video/*",
                "audio/*"
            ],
            "noteDraftLimit": 10,
            "scheduledNoteLimit": 1,
            "watermarkAvailable": true,
        })
    })
}

/// RoleEntityService.pack policies projection: every DEFAULT key renders
/// from the stored entry when present (non-null), otherwise the
/// `{ useDefault: true, priority: 0, value }` fill. Keys OUTSIDE
/// DEFAULT_POLICIES are dropped — the packed Role always carries exactly
/// the 39 policy entries (the e2e deepStrictEqual on `policies` pins this).
pub fn pack_policies(stored: &serde_json::Value) -> serde_json::Value {
    let defaults = default_policies_value();
    let defaults_obj = defaults.as_object().expect("DEFAULT_POLICIES object");
    let stored_obj = stored.as_object();
    let mut out = serde_json::Map::new();
    for (k, default_value) in defaults_obj {
        let keep = stored_obj
            .and_then(|o| o.get(k.as_str()))
            .filter(|v| !v.is_null());
        match keep {
            Some(v) => {
                out.insert(k.clone(), v.clone());
            }
            None => {
                out.insert(
                    k.clone(),
                    serde_json::json!({
                        "useDefault": true,
                        "priority": 0,
                        "value": default_value.clone(),
                    }),
                );
            }
        }
    }
    serde_json::Value::Object(out)
}

/// The user-shaped facts `evalCond` reads (`MiUser` slice: flags straight
/// off the row, timestamps/counters from the columns the pack uses).
#[derive(Debug, Clone, Copy)]
pub struct CondFacts {
    pub is_local: bool,
    pub is_suspended: bool,
    pub is_locked: bool,
    pub is_bot: bool,
    pub is_cat: bool,
    pub is_explorable: bool,
    /// AID-embedded creation instant (IdService.parse), 0 when unparsable
    /// (upstream created* comparisons degenerate to created-everything).
    pub created_ms: i64,
    pub now_ms: i64,
    pub followers_count: i64,
    pub following_count: i64,
    pub notes_count: i64,
}

impl CondFacts {
    pub fn from_local_user(row: &mk_db::user::LocalUserRow, now_ms: i64) -> Self {
        Self {
            is_local: true, // LocalUserRow is the host==null projection
            is_suspended: row.is_suspended,
            is_locked: row.is_locked,
            is_bot: row.is_bot,
            is_cat: row.is_cat,
            is_explorable: row.is_explorable,
            created_ms: mk_types::id::parse_aid_date(&row.id).unwrap_or(0),
            now_ms,
            followers_count: row.followers_count as i64,
            following_count: row.following_count as i64,
            notes_count: row.notes_count as i64,
        }
    }

    pub fn fixture(
        created_ms: i64,
        now_ms: i64,
        followers_count: i64,
        following_count: i64,
        notes_count: i64,
    ) -> Self {
        Self {
            is_local: true,
            is_suspended: false,
            is_locked: false,
            is_bot: false,
            is_cat: false,
            is_explorable: false,
            created_ms,
            now_ms,
            followers_count,
            following_count,
            notes_count,
        }
    }
}

/// `RoleService.evalCond` — recursive walk, unknown/malformed types false
/// (upstream wraps in try/catch and defaults false).
pub fn eval_cond(facts: &CondFacts, assigned_ids: &[String], value: &serde_json::Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    let t = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match t {
        "and" => obj
            .get("values")
            .and_then(|v| v.as_array())
            .map(|vs| vs.iter().all(|v| eval_cond(facts, assigned_ids, v)))
            .unwrap_or(false),
        "or" => obj
            .get("values")
            .and_then(|v| v.as_array())
            .map(|vs| vs.iter().any(|v| eval_cond(facts, assigned_ids, v)))
            .unwrap_or(false),
        "not" => !eval_cond(
            facts,
            assigned_ids,
            obj.get("value").unwrap_or(&serde_json::Value::Null),
        ),
        "roleAssignedTo" => {
            let rid = obj.get("roleId").and_then(|v| v.as_str()).unwrap_or("");
            assigned_ids.iter().any(|id| id == rid)
        }
        "isLocal" => facts.is_local,
        "isRemote" => !facts.is_local,
        "isSuspended" => facts.is_suspended,
        "isLocked" => facts.is_locked,
        "isBot" => facts.is_bot,
        "isCat" => facts.is_cat,
        "isExplorable" => facts.is_explorable,
        "createdLessThan" => {
            let sec = obj.get("sec").and_then(|v| v.as_f64()).unwrap_or(0.0);
            facts.created_ms > facts.now_ms - (sec * 1000.0) as i64
        }
        "createdMoreThan" => {
            let sec = obj.get("sec").and_then(|v| v.as_f64()).unwrap_or(0.0);
            facts.created_ms < facts.now_ms - (sec * 1000.0) as i64
        }
        "followersLessThanOrEq" => {
            facts.followers_count <= obj.get("value").and_then(|v| v.as_i64()).unwrap_or(0)
        }
        "followersMoreThanOrEq" => {
            facts.followers_count >= obj.get("value").and_then(|v| v.as_i64()).unwrap_or(0)
        }
        "followingLessThanOrEq" => {
            facts.following_count <= obj.get("value").and_then(|v| v.as_i64()).unwrap_or(0)
        }
        "followingMoreThanOrEq" => {
            facts.following_count >= obj.get("value").and_then(|v| v.as_i64()).unwrap_or(0)
        }
        "notesLessThanOrEq" => {
            facts.notes_count <= obj.get("value").and_then(|v| v.as_i64()).unwrap_or(0)
        }
        "notesMoreThanOrEq" => {
            facts.notes_count >= obj.get("value").and_then(|v| v.as_i64()).unwrap_or(0)
        }
        _ => false,
    }
}

/// `getUserRoles`: the live (manual) assignment rows first, then every
/// conditional role whose formula matches; sorted `displayOrder` DESC the
/// way `UserEntityService.pack` orders the public `roles` projection
/// (JS sort is stable — equal orders keep assigned-before-conditional).
pub fn effective_roles(
    all: &[mk_db::roles::CondRole],
    assigns: &[AssignedRole],
    facts: &CondFacts,
) -> Vec<AssignedRole> {
    let assigned_ids: Vec<String> = assigns.iter().map(|r| r.id.clone()).collect();
    let mut out: Vec<AssignedRole> = assigns.to_vec();
    for r in all {
        if r.target != "conditional" {
            continue;
        }
        if !eval_cond(facts, &assigned_ids, &r.cond_formula) {
            continue;
        }
        out.push(AssignedRole {
            id: r.id.clone(),
            name: r.name.clone(),
            is_public: r.is_public,
            is_moderator: r.is_moderator,
            is_administrator: r.is_administrator,
            as_badge: r.as_badge,
            color: r.color.clone(),
            icon_url: r.icon_url.clone(),
            description: r.description.clone(),
            display_order: r.display_order,
            policies: r.policies.clone(),
        });
    }
    out.sort_by(|a, b| b.display_order.cmp(&a.display_order));
    out
}

/// The users/show-pack canPublicNote feed — see the module deviation note.
/// Priority tiers pick first (2, then 1, then all, as upstream `calc`);
/// explicit (`useDefault: false`) entries crowd out the useDefault ones.
pub fn can_public_note_effective(assigned: &[AssignedRole], base: bool) -> bool {
    if assigned.is_empty() {
        return base;
    }
    let mut entries: Vec<(i64, bool, Option<bool>)> = assigned
        .iter()
        .map(|r| {
            let e = r.policies.get("canPublicNote");
            let obj = e.and_then(|v| v.as_object());
            let priority = obj
                .and_then(|o| o.get("priority"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let use_default = obj
                .and_then(|o| o.get("useDefault"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let value = obj.and_then(|o| o.get("value")).and_then(|v| v.as_bool());
            (priority, use_default, value)
        })
        .collect();
    for p in [2, 1] {
        let tier: Vec<_> = entries.iter().filter(|e| e.0 == p).collect();
        if !tier.is_empty() {
            entries = tier.into_iter().cloned().collect();
            break;
        }
    }
    let any_explicit = entries.iter().any(|e| !e.1);
    if any_explicit {
        entries.iter().filter(|e| !e.1).any(|e| e.2.unwrap_or(base))
    } else {
        entries
            .iter()
            .any(|e| if e.1 { base } else { e.2.unwrap_or(base) })
    }
}

/// `mk_core`-side orchestration for the users/show pack: fetches the
/// conditional role table + the live assignments, builds the CondFacts from
/// the target row, and returns the merged effective role list
/// (displayOrder DESC).
pub async fn user_effective_roles(
    pool: &mk_db::sqlx::PgPool,
    row: &mk_db::user::LocalUserRow,
) -> Result<Vec<AssignedRole>, mk_db::sqlx::Error> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let all = mk_db::roles::fetch_cond_roles(pool).await?;
    let assigns = mk_db::role::user_assigned_roles(pool, &row.id).await?;
    let facts = CondFacts::from_local_user(row, now_ms);
    Ok(effective_roles(&all, &assigns, &facts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policies_shape() {
        let obj = default_policies_value().as_object().unwrap();
        // upstream RoleService.ts DEFAULT_POLICIES carries exactly 39 keys
        assert_eq!(obj.len(), 39);
        assert_eq!(obj.get("inviteLimitCycle"), Some(&serde_json::json!(10080)));
        assert_eq!(obj.get("rateLimitFactor"), Some(&serde_json::json!(1)));
        assert_eq!(
            obj.get("chatAvailability"),
            Some(&serde_json::json!("available"))
        );
        assert_eq!(
            obj.get("uploadableFileTypes"),
            Some(&serde_json::json!([
                "text/*",
                "application/json",
                "image/*",
                "video/*",
                "audio/*"
            ]))
        );
    }
}
