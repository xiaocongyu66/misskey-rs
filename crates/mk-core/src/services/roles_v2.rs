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

    // ------------------------------------------------------------------------
    // unit-services: 1:1 port of upstream test/unit/RoleService.ts
    // (describe('RoleService') — engine slice this file owns).
    //
    // Upstream test → parity site map:
    // - getUserAssigns (expiry matrix)        → mk-db/src/role.rs tests
    //   (`expired_excluded...` pins no-expiry / future / past assignments).
    // - getUserPolicies                        → mk-core/src/services/role.rs
    //   tests (instance-default / with-role / priority / no-policy-set legs).
    // - assign + roleAssigned notification     → SKIPPED here: the
    //   notification leg is not modeled (mk-core notification_pack.rs marks
    //   roleAssigned deferred) and the e2e census (e2e-harness/roles.ts)
    //   never demands it — upstream `assign describe`'s assignment-exists
    //   half is the dedup row in mk-db/src/roles.rs (`assign_role_expiring`).
    // - getModeratorIds / getAdministratorIds  → SKIPPED here: no equivalent
    //   moderator-id query anywhere in the tree and no e2e demand (harness
    //   roles.ts pins pack flags, not id listings) — nothing to wire without
    //   dead code.
    // ------------------------------------------------------------------------

    /// CondFacts shaped after an upstream `createUser(...)` args combo.
    fn facts(created_s_ago: i64, bot: bool, cat: bool, suspended: bool, now_ms: i64) -> CondFacts {
        let mut f = CondFacts::fixture(now_ms - created_s_ago * 1000, now_ms, 0, 0, 0);
        f.is_bot = bot;
        f.is_cat = cat;
        f.is_suspended = suspended;
        f
    }

    /// `createConditionalRole({ type })` / `createConditionalRole(formula)`.
    fn formula(t: &str) -> serde_json::Value {
        serde_json::json!({ "type": t })
    }

    fn a_cond_role(id: &str, cond_formula: serde_json::Value) -> mk_db::roles::CondRole {
        mk_db::roles::CondRole {
            id: id.to_string(),
            name: format!("[conditional] {id}"),
            is_public: true,
            is_moderator: false,
            is_administrator: false,
            as_badge: false,
            color: None,
            icon_url: None,
            description: String::new(),
            display_order: 0,
            policies: serde_json::json!({}),
            target: "conditional".to_string(),
            cond_formula,
        }
    }

    /// `createRole` (manual row) — only the fields `effective_roles` reads.
    fn a_manual_role(id: &str) -> AssignedRole {
        AssignedRole {
            id: id.to_string(),
            name: id.to_string(),
            is_public: true,
            is_moderator: false,
            is_administrator: false,
            as_badge: false,
            color: None,
            icon_url: None,
            description: String::new(),
            display_order: 0,
            policies: serde_json::json!({}),
        }
    }

    fn holds(roles: &[AssignedRole], id: &str) -> bool {
        roles.iter().any(|r| r.id == id)
    }

    /// `getUserRoles` against the pure fixture tables.
    fn roles_for(
        all: &[mk_db::roles::CondRole],
        assigns: &[AssignedRole],
        f: &CondFacts,
    ) -> Vec<AssignedRole> {
        effective_roles(all, assigns, f)
    }

    /// Counter-only variant (followers/following/notes bound tests).
    fn counts(followers: i64, following: i64, notes: i64) -> CondFacts {
        let mut f = facts(60, false, false, false, NOW);
        f.followers_count = followers;
        f.following_count = following;
        f.notes_count = notes;
        f
    }

    const NOW: i64 = 1_700_000_000_000;

    #[test]
    fn conditional_role_and() {
        // upstream '～かつ～': u1 bot-only / u2 cat-only / u3 bot+cat /
        // u4 neither (suspended) vs role4 = isBot AND isCat.
        let role1 = formula("isBot");
        let role2 = formula("isCat");
        let role4 = serde_json::json!({ "type": "and", "values": [role1, role2] });
        let u1 = facts(60, true, false, false, NOW);
        let u2 = facts(60, false, true, false, NOW);
        let u3 = facts(60, true, true, false, NOW);
        let u4 = facts(60, false, false, true, NOW);
        assert!(!eval_cond(&u1, &[], &role4));
        assert!(!eval_cond(&u2, &[], &role4));
        assert!(eval_cond(&u3, &[], &role4));
        assert!(!eval_cond(&u4, &[], &role4));
    }

    #[test]
    fn conditional_role_or() {
        // upstream '～または～' — same users vs isBot OR isCat.
        let v1 = formula("isBot");
        let v2 = formula("isCat");
        let role4 = serde_json::json!({ "type": "or", "values": [v1, v2] });
        let u1 = facts(60, true, false, false, NOW);
        let u2 = facts(60, false, true, false, NOW);
        let u3 = facts(60, true, true, false, NOW);
        let u4 = facts(60, false, false, true, NOW);
        assert!(eval_cond(&u1, &[], &role4));
        assert!(eval_cond(&u2, &[], &role4));
        assert!(eval_cond(&u3, &[], &role4));
        assert!(!eval_cond(&u4, &[], &role4));
    }

    #[test]
    fn conditional_role_not() {
        // upstream '～ではない' — u1 bot / u2 cat / u3 bot+cat vs NOT isBot.
        let role4 = serde_json::json!({ "type": "not", "value": formula("isBot") });
        assert!(!eval_cond(&facts(60, true, false, false, NOW), &[], &role4));
        assert!(eval_cond(&facts(60, false, true, false, NOW), &[], &role4));
        assert!(!eval_cond(&facts(60, true, true, false, NOW), &[], &role4));
    }

    #[test]
    fn conditional_role_assigned_to_manual() {
        // upstream 'マニュアルロールにアサイン済み' — the conditional
        // roleAssignedTo(manual) fires only for a user holding the manual
        // role, and getUserRoles keeps BOTH rows (manual + conditional).
        let manual = a_manual_role("rmanual");
        let cond = a_cond_role(
            "rcond",
            serde_json::json!({ "type": "roleAssignedTo", "roleId": "rmanual" }),
        );
        let all = [cond];
        let f = facts(60, false, false, false, NOW);
        // user1: no assignments
        let u1roles = roles_for(&all, &[], &f);
        assert!(!holds(&u1roles, "rcond"));
        // user2: assigned the manual role
        let u2roles = roles_for(&all, &[manual.clone()], &f);
        assert!(holds(&u2roles, "rmanual"));
        assert!(holds(&u2roles, "rcond"));
    }

    #[test]
    fn conditional_role_is_local_is_remote() {
        // upstream 'ローカルユーザのみ' / 'リモートユーザのみ'
        let local = formula("isLocal");
        let remote = formula("isRemote");
        let mut u1 = facts(60, false, false, false, NOW);
        assert!(eval_cond(&u1, &[], &local));
        assert!(!eval_cond(&u1, &[], &remote));
        u1.is_local = false; // host = 'example.com'
        assert!(!eval_cond(&u1, &[], &local));
        assert!(eval_cond(&u1, &[], &remote));
    }

    #[test]
    fn conditional_role_flag_types() {
        // upstream 'サスペンド済みユーザである' / '鍵アカウントユーザである' /
        // 'botユーザである' / '猫である' / '「ユーザを見つけやすくする」が
        // 有効なアカウント' — one off/on pair per single-flag formula.
        for t in ["isSuspended", "isLocked", "isBot", "isCat", "isExplorable"] {
            let f = formula(t);
            let mut off = facts(60, false, false, false, NOW);
            assert!(!eval_cond(&off, &[], &f), "{t} off must not match");
            match t {
                "isSuspended" => off.is_suspended = true,
                "isLocked" => off.is_locked = true,
                "isBot" => off.is_bot = true,
                "isCat" => off.is_cat = true,
                _ => off.is_explorable = true,
            }
            assert!(eval_cond(&off, &[], &f), "{t} on must match");
        }
    }

    #[test]
    fn conditional_role_created_less_than() {
        // upstream 'ユーザが作成されてから指定期間経過した'
        // (createdLessThan sec:300) — createdAt after (now - sec*1000):
        // created 5:01 ago → false, 5:00 → false (strict), 4:59 → true.
        let role = serde_json::json!({ "type": "createdLessThan", "sec": 300 });
        assert!(!eval_cond(
            &facts(301, false, false, false, NOW),
            &[],
            &role
        ));
        assert!(!eval_cond(
            &facts(300, false, false, false, NOW),
            &[],
            &role
        ));
        assert!(eval_cond(&facts(299, false, false, false, NOW), &[], &role));
    }

    #[test]
    fn conditional_role_created_more_than() {
        // upstream 'ユーザが作成されてから指定期間経っていない'
        // (createdMoreThan sec:300).
        let role = serde_json::json!({ "type": "createdMoreThan", "sec": 300 });
        assert!(eval_cond(&facts(301, false, false, false, NOW), &[], &role));
        assert!(!eval_cond(
            &facts(300, false, false, false, NOW),
            &[],
            &role
        ));
        assert!(!eval_cond(
            &facts(299, false, false, false, NOW),
            &[],
            &role
        ));
    }

    #[test]
    fn conditional_role_count_bounds() {
        // upstream 'フォロワー数が指定値以下/以上', 'フォロー数が…',
        // 'ノート数が…' — 99/100/101 against value 100 (also 10 for notes).
        let f_le = serde_json::json!({ "type": "followersLessThanOrEq", "value": 100 });
        let f_ge = serde_json::json!({ "type": "followersMoreThanOrEq", "value": 100 });
        let g_le = serde_json::json!({ "type": "followingLessThanOrEq", "value": 100 });
        let g_ge = serde_json::json!({ "type": "followingMoreThanOrEq", "value": 100 });
        let n_le = serde_json::json!({ "type": "notesLessThanOrEq", "value": 10 });
        let n_ge = serde_json::json!({ "type": "notesMoreThanOrEq", "value": 10 });
        let lo = counts(99, 99, 9);
        let eq = counts(100, 100, 10);
        let hi = counts(101, 101, 11);
        let ev = |f: &CondFacts, c: &serde_json::Value| eval_cond(f, &[], c);
        assert!(ev(&lo, &f_le) && ev(&eq, &f_le) && !ev(&hi, &f_le));
        assert!(!ev(&lo, &f_ge) && ev(&eq, &f_ge) && ev(&hi, &f_ge));
        assert!(ev(&lo, &g_le) && ev(&eq, &g_le) && !ev(&hi, &g_le));
        assert!(!ev(&lo, &g_ge) && ev(&eq, &g_ge) && ev(&hi, &g_ge));
        assert!(ev(&lo, &n_le) && ev(&eq, &n_le) && !ev(&hi, &n_le));
        assert!(!ev(&lo, &n_ge) && ev(&eq, &n_ge) && ev(&hi, &n_ge));
    }

    #[test]
    fn get_user_roles_merges_manual_and_conditional() {
        // upstream 'getUserRoles アサインされたロールとコンディショナル
        // ロールの両方が取得できる' — manual assignment + matching conditional
        // formula both land in the list.
        let cond = a_cond_role("rcond", formula("isBot"));
        let all = [cond];
        let f = facts(60, true, false, false, NOW);
        let roles = roles_for(&all, &[a_manual_role("rmanual")], &f);
        assert!(holds(&roles, "rmanual"));
        assert!(holds(&roles, "rcond"));
    }

    #[test]
    fn get_user_badge_roles_conditional() {
        // upstream 'getUserBadgeRoles コンディショナルなバッジロールが条件
        // 一致で返る' + 'roleAssignedTo 条件のバッジロール: アサイン有無で
        // 変化する' — only asBadge conditional roles surface, gated on the
        // formula. (Expiry of manual badge assignments is the assignment
        // fetch, pinned at mk-db/src/role.rs `expired_excluded...`.)
        let mk = |id: &str, badge: bool, cf: serde_json::Value| {
            let mut r = a_cond_role(id, cf);
            r.as_badge = badge;
            r
        };
        let cond_badge = mk("condbadge", true, formula("isBot"));
        let cond_plain = mk("condplain", false, formula("isBot"));
        let manual = a_manual_role("rmanual");
        let assigned_badge = mk(
            "assignedbadge",
            true,
            serde_json::json!({ "type": "roleAssignedTo", "roleId": "rmanual" }),
        );
        let all = [cond_badge, cond_plain, assigned_badge];
        // isBot user with no assignments: bot-conditional badge only.
        let roles = roles_for(&all, &[], &facts(60, true, false, false, NOW));
        assert_eq!(badge_ids(&roles), vec!["condbadge".to_string()]);
        // user2 assigned the manual role: the roleAssignedTo badge fires too.
        let roles = roles_for(&all, &[manual], &facts(60, false, false, false, NOW));
        assert_eq!(badge_ids(&roles), vec!["assignedbadge".to_string()]);
    }

    /// The getUserBadgeRoles asBadge projection (ids for assertion purposes).
    fn badge_ids(roles: &[AssignedRole]) -> Vec<String> {
        roles
            .iter()
            .filter(|r| r.as_badge)
            .map(|r| r.id.clone())
            .collect()
    }

    /// upstream 'getUserRoles' against real tables: manual + conditional rows
    /// merge, and an EXPIRED assignment never reaches the engine (the
    /// getUserAssigns expiry filter lives in `mk_db::role::user_assigned_roles`).
    #[tokio::test]
    async fn db_user_effective_roles_merges_and_expires() {
        if std::env::var("DATABASE_URL").is_err() {
            return;
        }
        let pool = mk_db::test_support::fresh_db("mkcore_roles_v2").await;
        mk_db::run_migrations(&pool).await.expect("migrations");

        mk_db::sqlx::query(
            r#"INSERT INTO "user" ("id", "username", "usernameLower", "isBot", "token")
               VALUES ('u1', 'botuser', 'botuser', TRUE, '0123456789abcdef')"#,
        )
        .execute(&pool)
        .await
        .expect("insert user");

        mk_db::sqlx::query(
            r#"INSERT INTO "role" ("id", "updatedAt", "name", "description", "lastUsedAt")
               VALUES ('rManual', now(), 'manual role', '', now())"#,
        )
        .execute(&pool)
        .await
        .expect("insert manual role");
        mk_db::sqlx::query(
            r#"INSERT INTO "role" ("id", "updatedAt", "name", "description", "lastUsedAt",
                                   "target", "condFormula")
               VALUES ('rCondBot', now(), '[conditional] isBot', '', now(), 'conditional', $1::jsonb)"#,
        )
        .bind(r#"{"type":"isBot"}"#)
        .execute(&pool)
        .await
        .expect("insert conditional role");
        mk_db::sqlx::query(
            r#"INSERT INTO "role" ("id", "updatedAt", "name", "description", "lastUsedAt")
               VALUES ('rExpiredAssign', now(), 'expired', '', now())"#,
        )
        .execute(&pool)
        .await
        .expect("insert expired-assignment role");

        // live manual + live conditional + EXPIRED assignment
        mk_db::sqlx::query(
            r#"INSERT INTO "role_assignment" ("id", "userId", "roleId")
               VALUES ('a1', 'u1', 'rManual'), ('a2', 'u1', 'rExpiredAssign')"#,
        )
        .execute(&pool)
        .await
        .expect("insert assignments");
        mk_db::sqlx::query(
            r#"UPDATE "role_assignment" SET "expiresAt" = now() - interval '1 second'
               WHERE "id" = 'a2'"#,
        )
        .execute(&pool)
        .await
        .expect("expire a2");

        let row = mk_db::user::find_user_by_id(&pool, "u1")
            .await
            .expect("find user")
            .expect("user row");
        let roles = user_effective_roles(&pool, &row).await.expect("effective");

        // upstream 'getUserRoles 両方が取得できる' + 'getUserAssigns 期限切れ'
        assert!(holds(&roles, "rManual"), "live manual assignment kept");
        assert!(holds(&roles, "rCondBot"), "matching conditional granted");
        assert!(
            !holds(&roles, "rExpiredAssign"),
            "expired assignment dropped"
        );
    }
}
