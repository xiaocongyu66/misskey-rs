//! DB-row → packed-user mapping (M1 slice: local users, no relations, no
//! roles). See `core/entities/UserEntityService.ts` for upstream semantics.

use mk_db::role::AssignedRole;
use mk_db::user::{LocalUserRow, ProfileRow};

use crate::packers::user::{
    AvatarDecoration, BackupCodesStock, ChatScope, MeDetailed, OnlineStatus, RoleLiteValue,
    UserDetailedNotMe, UserField, VisibilityLevel,
};

#[derive(Clone, Copy)]
pub struct InstanceCtx<'a> {
    pub url: &'a str,
    pub host: &'a str,
}

/// Role-engine inputs threaded from the endpoint layer (upstream
/// `UserEntityService.pack` reads `roleService` per packed user):
/// public-role list, badge list, the merged-policy flags
/// (`isSilenced: !canPublicNote`, `canChat`), the me-view
/// `isAdmin`/`isModerator` and the `(isMe || iAmModerator)`
/// twoFactorEnabled block gate.
pub struct RolePack<'a> {
    /// Target user's assigned roles (`mk_db::role::user_assigned_roles`,
    /// non-expired, sorted `"displayOrder" DESC`).
    pub assigned: &'a [AssignedRole],
    /// `RoleService.isAdministrator(target)`'s root shortcut
    /// (meta.rootUserId === target.id — mirrored as `usernameLower='root'`).
    pub target_is_root: bool,
    /// `RoleService.isModerator(viewer)` — gates badge visibility plus the
    /// not-me twoFactor/usePasswordLessLogin/securityKeys block.
    pub viewer_is_moderator: bool,
}

/// Public assigned roles for the `roles` array (pack L563: filter isPublic,
/// sort `displayOrder` DESC, project the lite shape).
fn public_roles_value(rp: &RolePack<'_>) -> Vec<RoleLiteValue> {
    let mut out: Vec<RoleLiteValue> = rp
        .assigned
        .iter()
        .filter(|r| r.is_public)
        .map(|r| RoleLiteValue {
            id: r.id.clone(),
            name: r.name.clone(),
            color: r.color.clone(),
            icon_url: r.icon_url.clone(),
            description: r.description.clone(),
            is_moderator: r.is_moderator,
            is_administrator: r.is_administrator,
            display_order: r.display_order,
        })
        .collect();
    out.sort_by(|a, b| b.display_order.cmp(&a.display_order));
    out
}

fn fields_vec(v: &serde_json::Value) -> Vec<UserField> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|f| UserField {
                    name: f["name"].as_str().unwrap_or_default().to_string(),
                    value: f["value"].as_str().unwrap_or_default().to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn decorations(v: &serde_json::Value) -> Vec<AvatarDecoration> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|d| AvatarDecoration {
                    id: d["id"].as_str().unwrap_or_default().to_string(),
                    angle: match d.get("angle") {
                        Some(x) if !x.is_null() => Some(x.as_f64().unwrap_or_default()),
                        _ => None,
                    },
                    flip_h: match d.get("flipH") {
                        Some(x) if !x.is_null() => Some(x.as_bool().unwrap_or_default()),
                        _ => None,
                    },
                    url: d["url"].as_str().unwrap_or_default().to_string(),
                    offset_x: match d.get("offsetX") {
                        Some(x) if !x.is_null() => Some(x.as_f64().unwrap_or_default()),
                        _ => None,
                    },
                    offset_y: match d.get("offsetY") {
                        Some(x) if !x.is_null() => Some(x.as_f64().unwrap_or_default()),
                        _ => None,
                    },
                })
                .collect()
        })
        .unwrap_or_default()
}

fn string_vec(v: &serde_json::Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn string_vec_vec(v: &serde_json::Value) -> Vec<Vec<String>> {
    v.as_array()
        .map(|a| a.iter().map(|x| string_vec(x)).collect())
        .unwrap_or_default()
}

/// `getOnlineStatus`: local fresh user → 'unknown' (no lastActiveDate col in
/// M1 auth rows; hideOnlineStatus col absent locally → never hidden).
fn online_status(_row: &LocalUserRow) -> OnlineStatus {
    OnlineStatus::Unknown
}

fn avatar_url(row: &LocalUserRow, ctx: InstanceCtx<'_>) -> String {
    match &row.avatar_url {
        Some(u) if !u.is_empty() => u.clone(),
        _ => {
            let base = ctx.url.trim_end_matches('/');
            format!("{}/identicon/{}@{}", base, row.username_lower, ctx.host)
        }
    }
}

/// followersCount/followingCount visibility gate (UserEntityService.pack
/// ~L465-473): public always shows the real count; `followers` shows it for
/// me or a follower; `private` only for me; otherwise the pack emits 0
/// (`followersCount ?? 0`). M1 has no moderator/moderator-role concept, so
/// the iAmModerator term is always false.
fn gated_count(
    real: i32,
    visibility: VisibilityLevel,
    me_view: bool,
    relation_is_following: bool,
) -> i64 {
    if me_view
        || visibility == VisibilityLevel::Public
        || (visibility == VisibilityLevel::Followers && relation_is_following)
    {
        real as i64
    } else {
        0
    }
}

fn detailed(
    row: &LocalUserRow,
    profile: &ProfileRow,
    ctx: InstanceCtx<'_>,
    me_view: bool,
    relation: Option<&mk_db::relation::UserRelation>,
    roles: Option<&RolePack<'_>>,
) -> UserDetailedNotMe {
    let created_at = mk_types::id::parse_aid_date(&row.id).map(crate::services::signup::ms_to_iso);
    let relation_is_following = relation.as_ref().map(|r| r.is_following).unwrap_or(false);
    // Role engine: badges for the lite block, public roles + merged policy
    // flags for the detailed block (badge_roles_and_policy = the wiring hook).
    let (badge_roles, public_roles, policy) = match roles {
        Some(rp) => {
            let (badges, policy) =
                crate::services::role::badge_roles_and_policy(rp.assigned, rp.viewer_is_moderator);
            (badges, public_roles_value(rp), policy)
        }
        None => (
            Vec::new(),
            Vec::new(),
            crate::services::role::UserPolicy::base(),
        ),
    };
    // pack L577: `...(isDetailed && (isMe || iAmModerator) ? { twoFactorEnabled,
    // usePasswordLessLogin, securityKeys } : {})` — the not-me block appears
    // only for moderator viewers.
    let show_two_factor = me_view || roles.map(|rp| rp.viewer_is_moderator).unwrap_or(false);
    UserDetailedNotMe {
        lite_fields: crate::packers::user::UserLite {
            id: row.id.clone(),
            name: row.name.clone(),
            username: row.username.clone(),
            host: None,
            avatar_url: avatar_url(row, ctx),
            avatar_blurhash: row.avatar_blurhash.clone(),
            avatar_decorations: decorations(&row.avatar_decorations),
            is_bot: Some(row.is_bot), // user.isBot, always present
            is_cat: Some(row.is_cat), // user.isCat, always present
            require_signin_to_view_contents: None, // upstream: ===false ? undefined : true
            make_notes_followers_only_before: None, // ?? undefined → DB null omits
            make_notes_hidden_before: None, // ?? undefined → DB null omits
            instance: None,
            emojis: Default::default(),
            online_status: online_status(row),
            badge_roles: Some(badge_roles),
        },
        url: profile.url.clone(),
        uri: None, // local user has no uri
        moved_to: None,
        also_known_as: None,
        created_at: created_at.unwrap_or_default(),
        updated_at: None,
        last_fetched_at: None,
        banner_url: if row.banner_id.is_none() {
            None
        } else {
            row.banner_url.clone()
        },
        banner_blurhash: if row.banner_id.is_none() {
            None
        } else {
            row.banner_blurhash.clone()
        },
        is_locked: row.is_locked,
        is_silenced: policy.is_silenced(), // !canPublicNote (merged policies)
        is_suspended: row.is_suspended,
        description: profile.description.clone(),
        location: profile.location.clone(),
        birthday: profile.birthday.clone(),
        lang: profile.lang.clone(),
        fields: fields_vec(&profile.fields),
        verified_links: profile.verified_links.clone(), // profile.verifiedLinks
        followers_count: gated_count(
            row.followers_count,
            visibility(&profile.followers_visibility),
            me_view,
            relation_is_following,
        ),
        following_count: gated_count(
            row.following_count,
            visibility(&profile.following_visibility),
            me_view,
            relation_is_following,
        ),
        notes_count: row.notes_count as i64, // user.notesCount
        pinned_note_ids: Vec::new(),
        pinned_notes: Vec::new(),
        pinned_page_id: None,
        pinned_page: None,
        public_reactions: profile.public_reactions, // local user
        following_visibility: visibility(&profile.following_visibility),
        followers_visibility: visibility(&profile.followers_visibility),
        chat_scope: chat_scope(&row.chat_scope),
        can_chat: policy.can_chat(), // chatAvailability === 'available'
        roles: public_roles,
        followed_message: relation
            .as_ref()
            .filter(|r| r.is_following)
            .map(|_| profile.followed_message.clone()),
        memo: None,
        moderation_note: None,
        two_factor_enabled: if show_two_factor {
            Some(profile.two_factor_enabled)
        } else {
            None
        },
        use_password_less_login: if show_two_factor {
            Some(profile.use_password_less_login)
        } else {
            None
        },
        security_keys: if show_two_factor {
            Some(profile.security_keys_available)
        } else {
            None
        },
        // `...(relation ? {...} : {})` in upstream pack: relation fields
        // exist only when a me view targets another user.
        is_following: relation.as_ref().map(|r| r.is_following),
        is_followed: relation.as_ref().map(|r| r.is_followed),
        has_pending_follow_request_from_you: relation
            .as_ref()
            .map(|r| r.has_pending_follow_request_from_you),
        has_pending_follow_request_to_you: relation
            .as_ref()
            .map(|r| r.has_pending_follow_request_to_you),
        is_blocking: relation.as_ref().map(|r| r.is_blocking),
        is_blocked: relation.as_ref().map(|r| r.is_blocked),
        is_muted: relation.as_ref().map(|r| r.is_muted),
        is_renote_muted: relation.as_ref().map(|r| r.is_renote_muted),
        notify: relation
            .as_ref()
            .map(|r| r.notify.clone().unwrap_or_else(|| "none".to_string())),
        with_replies: relation.as_ref().map(|r| r.with_replies.unwrap_or(false)),
    }
}

fn visibility(v: &str) -> VisibilityLevel {
    match v {
        "followers" => VisibilityLevel::Followers,
        "private" => VisibilityLevel::Private,
        _ => VisibilityLevel::Public,
    }
}

fn chat_scope(v: &str) -> ChatScope {
    match v {
        "everyone" => ChatScope::Everyone,
        "following" => ChatScope::Following,
        "followers" => ChatScope::Followers,
        "none" => ChatScope::None_,
        _ => ChatScope::Mutual,
    }
}

fn backup_stock(two_factor_backup_secret_len: Option<usize>) -> BackupCodesStock {
    match two_factor_backup_secret_len {
        Some(5) => BackupCodesStock::Full,
        Some(n) if n > 0 => BackupCodesStock::Partial,
        _ => BackupCodesStock::None_,
    }
}

/// One entrypoint for both DetailedNotMe (is_me=false) and MeDetailed
/// (is_me=true) responses; returns serde_json::Value.
///
/// Secrets (`email`/`emailVerified`/`securityKeysList`) follow the upstream
/// opts.includeSecrets switch (UserEntityService.pack L626): it is an
/// endpoint-level choice, not a property of the me view — upstream
/// `i`/`i/update` pack with includeSecrets: true, while `users/show` keeps
/// the default false even when the viewer is the target. Use
/// `pack_inline_opts` for the false case.
pub fn pack_inline(
    row: &LocalUserRow,
    profile: &ProfileRow,
    is_me: bool,
    relation: Option<&mk_db::relation::UserRelation>,
    ctx: InstanceCtx<'_>,
) -> serde_json::Value {
    pack_inline_opts(row, profile, is_me, relation, ctx, true)
}

pub fn pack_inline_opts(
    row: &LocalUserRow,
    profile: &ProfileRow,
    is_me: bool,
    relation: Option<&mk_db::relation::UserRelation>,
    ctx: InstanceCtx<'_>,
    include_secrets: bool,
) -> serde_json::Value {
    pack_inline_with_roles(row, profile, is_me, relation, ctx, include_secrets, None)
}

/// Roles-aware variant (endpoints/users paths): `roles` carries the target's
/// assigned roles + viewer/root flags; `None` degrades to the pack above
/// (empty role lists, default policies).
pub fn pack_inline_with_roles(
    row: &LocalUserRow,
    profile: &ProfileRow,
    is_me: bool,
    relation: Option<&mk_db::relation::UserRelation>,
    ctx: InstanceCtx<'_>,
    include_secrets: bool,
    roles: Option<&RolePack<'_>>,
) -> serde_json::Value {
    if !is_me {
        let d = detailed(row, profile, ctx, false, relation, roles);
        return serde_json::to_value(&d).unwrap_or(serde_json::json!({}));
    }
    // me view: no relation block (upstream relation is computed only for
    // other-target views)
    let me = detailed_as_me(row, profile, ctx, include_secrets, roles);
    serde_json::to_value(&me).unwrap_or(serde_json::json!({}))
}

/// Me variant of the pack (detailed() + Me-only fields).
fn detailed_as_me(
    row: &LocalUserRow,
    profile: &ProfileRow,
    ctx: InstanceCtx<'_>,
    include_secrets: bool,
    roles: Option<&RolePack<'_>>,
) -> MeDetailed {
    let d = detailed(row, profile, ctx, true, None, roles);
    let ach = profile.achievements.as_array().cloned().unwrap_or_default();
    // pack L464: `isModerator/isAdmin = isMe && isDetailed ? roleService.… :
    // undefined` — RoleService.isModerator = root || assigned moderator/admin
    // roles; isAdministrator = root || assigned administrator roles.
    let (is_moderator, is_admin) = match roles {
        Some(rp) => (
            rp.target_is_root || crate::services::role::has_moderator_role(rp.assigned),
            rp.target_is_root || crate::services::role::has_administrator_role(rp.assigned),
        ),
        None => (false, false),
    };
    MeDetailed {
        detailed: crate::packers::user::UserDetailedNotMe {
            two_factor_enabled: Some(profile.two_factor_enabled),
            use_password_less_login: Some(profile.use_password_less_login),
            security_keys: Some(profile.security_keys_available),
            ..d
        },
        avatar_id: row.avatar_id.clone(),
        banner_id: row.banner_id.clone(),
        followed_message_always: profile.followed_message.clone(),
        is_moderator,
        is_admin,
        inject_featured_note: profile.inject_featured_note,
        receive_announcement_email: profile.receive_announcement_email,
        always_mark_nsfw: profile.always_mark_nsfw,
        auto_sensitive: profile.auto_sensitive,
        careful_bot: profile.careful_bot,
        auto_accept_followed: profile.auto_accept_followed,
        follow_approval_local_seconds: profile.follow_approval_local_seconds,
        follow_approval_remote_seconds: profile.follow_approval_remote_seconds,
        no_crawle: profile.no_crawle, // profile.noCrawle (pack L599)
        prevent_ai_learning: profile.prevent_ai_learning,
        is_explorable: row.is_explorable,
        is_deleted: false,
        // profile.twoFactorBackupSecret?.length (pack L603)
        two_factor_backup_codes_stock: backup_stock(
            profile
                .two_factor_backup_secret
                .as_ref()
                .map(|codes| codes.len()),
        ),
        hide_online_status: row.hide_online_status,
        has_unread_specified_notes: false,
        has_unread_mentions: false,
        has_unread_announcement: false,
        unread_announcements: Vec::new(),
        has_unread_antenna: false,
        has_unread_channel: false,
        has_unread_chat_messages: false,
        has_unread_notification: false,
        has_pending_received_follow_request: false,
        unread_notifications_count: 0,
        muted_words: string_vec_vec(&profile.muted_words),
        hard_muted_words: string_vec_vec(&profile.hard_muted_words),
        muted_instances: string_vec(&profile.muted_instances),
        notification_recieve_config: profile.notification_recieve_config.clone(),
        email_notification_types: string_vec(&profile.email_notification_types),
        achievements: ach,
        logged_in_days: profile.logged_in_dates.len() as i64,
        policies: serde_json::json!({}),
        two_factor_enabled_always: profile.two_factor_enabled,
        use_password_less_login_always: profile.use_password_less_login,
        security_keys_always: profile.security_keys_available,
        // Secret fields, gated on opts.includeSecrets (upstream is a
        // per-endpoint switch; see pack_inline_opts).
        email: if include_secrets {
            Some(profile.email.clone())
        } else {
            None
        },
        email_verified: if include_secrets {
            Some(Some(profile.email_verified))
        } else {
            None
        },
        security_keys_list: if include_secrets {
            Some(Vec::new())
        } else {
            None
        },
    }
}

/// Lite view of a local user used inside note packs (`pack`, lite: true).
pub fn user_lite_value(
    row: &LocalUserRow,
    _profile: &ProfileRow,
    ctx: InstanceCtx<'_>,
) -> serde_json::Value {
    let _ = _profile;
    user_lite_value_with_roles(row, ctx, None)
}

/// Roles-aware UserLite (`schema: 'UserLite'` results, e2e users/search
/// `detail: false`): only `badgeRoles` carries the role engine (pack L517).
pub fn user_lite_value_with_roles(
    row: &LocalUserRow,
    ctx: InstanceCtx<'_>,
    roles: Option<&RolePack<'_>>,
) -> serde_json::Value {
    use crate::packers::user::UserLite;
    let badge_roles = match roles {
        Some(rp) => crate::services::role::badge_roles(rp.assigned, rp.viewer_is_moderator),
        None => Vec::new(),
    };
    let lite = UserLite {
        id: row.id.clone(),
        name: row.name.clone(),
        username: row.username.clone(),
        host: None,
        avatar_url: avatar_url(row, ctx),
        avatar_blurhash: row.avatar_blurhash.clone(),
        avatar_decorations: decorations(&row.avatar_decorations),
        is_bot: Some(row.is_bot),               // user.isBot, always present
        is_cat: Some(row.is_cat),               // user.isCat, always present
        require_signin_to_view_contents: None,  // upstream: ===false ? undefined : true
        make_notes_followers_only_before: None, // ?? undefined → DB null omits
        make_notes_hidden_before: None,         // ?? undefined → DB null omits
        instance: None,
        emojis: Default::default(),
        online_status: OnlineStatus::Unknown,
        badge_roles: Some(badge_roles),
    };
    serde_json::to_value(&lite).unwrap_or(serde_json::json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packers::user::BackupCodesStock;
    use serde_json::json;

    fn row() -> LocalUserRow {
        LocalUserRow {
            id: "9testuser0001".to_string(),
            username: "TestUser".to_string(),
            username_lower: "testuser".to_string(),
            name: None,
            token: "0123456789abcdef".to_string(),
            is_suspended: false,
            is_locked: false,
            is_explorable: true,
            hide_online_status: false,
            chat_scope: "mutual".to_string(),
            avatar_url: None,
            avatar_blurhash: None,
            avatar_decorations: json!([]),
            banner_url: None,
            banner_blurhash: None,
            avatar_id: None,
            banner_id: None,
            followers_count: 7,
            following_count: 5,
            notes_count: 3,
            is_bot: false,
            is_cat: false,
        }
    }

    fn profile() -> ProfileRow {
        ProfileRow {
            user_id: "9testuser0001".to_string(),
            location: None,
            birthday: None,
            description: None,
            lang: None,
            fields: json!([]),
            url: None,
            email: Some("t@example.com".to_string()),
            email_verified: false,
            two_factor_enabled: false,
            password: None,
            auto_accept_followed: true,
            always_mark_nsfw: false,
            careful_bot: false,
            security_keys_available: false,
            use_password_less_login: false,
            inject_featured_note: true,
            receive_announcement_email: true,
            email_notification_types: json!(["follow"]),
            public_reactions: true,
            auto_sensitive: false,
            achievements: json!([]),
            logged_in_dates: vec![],
            prevent_ai_learning: true,
            no_crawle: true,
            verified_links: vec!["https://example.com".to_string()],
            notification_recieve_config: json!({}),
            hard_muted_words: json!([]),
            muted_words: json!([]),
            muted_instances: json!([]),
            following_visibility: "public".to_string(),
            followers_visibility: "public".to_string(),
            followed_message: None,
            follow_approval_local_seconds: None,
            follow_approval_remote_seconds: None,
            two_factor_backup_secret: None,
        }
    }

    fn ctx() -> InstanceCtx<'static> {
        InstanceCtx {
            url: "http://127.0.0.1:3000",
            host: "127.0.0.1:3000",
        }
    }

    #[test]
    fn gated_count_follows_visibility_rules() {
        let r = row();
        // public: everyone sees the real count
        assert_eq!(
            gated_count(r.followers_count, VisibilityLevel::Public, false, false),
            7
        );
        // private: only the me view
        assert_eq!(
            gated_count(r.followers_count, VisibilityLevel::Private, false, false),
            0
        );
        assert_eq!(
            gated_count(r.followers_count, VisibilityLevel::Private, true, false),
            7
        );
        // followers: me or an actual follower
        assert_eq!(
            gated_count(r.followers_count, VisibilityLevel::Followers, false, false),
            0
        );
        assert_eq!(
            gated_count(r.followers_count, VisibilityLevel::Followers, false, true),
            7
        );
    }

    #[test]
    fn backup_stock_levels_mirror_upstream() {
        // UserEntityService.pack L603: length === 5 ? 'full' : length > 0 ?
        // 'partial' : 'none'
        assert_eq!(backup_stock(None), BackupCodesStock::None_);
        assert_eq!(backup_stock(Some(0)), BackupCodesStock::None_);
        assert_eq!(backup_stock(Some(3)), BackupCodesStock::Partial);
        assert_eq!(backup_stock(Some(5)), BackupCodesStock::Full);
    }

    #[test]
    fn me_pack_gates_secrets_on_include_secrets() {
        let ctx = ctx();
        // users/show packs self without secrets (upstream default false)
        let no_secrets = pack_inline_opts(&row(), &profile(), true, None, ctx, false);
        assert!(no_secrets.get("email").is_none());
        assert!(no_secrets.get("emailVerified").is_none());
        assert!(no_secrets.get("securityKeysList").is_none());
        // i/i-update pack with includeSecrets: true
        let secrets = pack_inline_opts(&row(), &profile(), true, None, ctx, true);
        assert_eq!(secrets["email"], json!("t@example.com"));
        assert_eq!(secrets["emailVerified"], json!(false));
        assert_eq!(secrets["securityKeysList"], json!([]));
        // twoFactorBackupCodesStock serializes with the upstream label
        assert_eq!(secrets["twoFactorBackupCodesStock"], json!("none"));
    }

    #[test]
    fn detailed_pack_wires_user_and_profile_columns() {
        let no_secrets = pack_inline(&row(), &profile(), false, None, ctx());
        // user row: isBot/isCat exist and feed the pack (always present)
        assert_eq!(no_secrets["isBot"], json!(false));
        assert_eq!(no_secrets["isCat"], json!(false));
        // user_profile row: verifiedLinks + noCrawle no longer hardcoded
        assert_eq!(no_secrets["verifiedLinks"], json!(["https://example.com"]));
        // counts: public visibility keeps the real user-row counter
        assert_eq!(no_secrets["followersCount"], json!(7));
        assert_eq!(no_secrets["followingCount"], json!(5));
        assert_eq!(no_secrets["notesCount"], json!(3));
        // no secrets leak on the not-me shape
        assert!(no_secrets.get("email").is_none());
    }
}
