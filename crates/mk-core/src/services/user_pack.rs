//! DB-row → packed-user mapping (M1 slice: local users, no relations, no
//! roles). See `core/entities/UserEntityService.ts` for upstream semantics.

use mk_db::user::{LocalUserRow, ProfileRow};

use crate::packers::user::{
    AvatarDecoration, BackupCodesStock, ChatScope, MeDetailed, OnlineStatus, UserDetailedNotMe,
    UserField, VisibilityLevel,
};

#[derive(Clone, Copy)]
pub struct InstanceCtx<'a> {
    pub url: &'a str,
    pub host: &'a str,
}

fn opt_json_string(v: &serde_json::Value) -> Option<String> {
    v.as_str().map(|s| s.to_string())
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

fn detailed(row: &LocalUserRow, profile: &ProfileRow, ctx: InstanceCtx<'_>) -> UserDetailedNotMe {
    let created_at = mk_types::id::parse_aid_date(&row.id).map(crate::services::signup::ms_to_iso);
    UserDetailedNotMe {
        lite_fields: crate::packers::user::UserLite {
            id: row.id.clone(),
            name: row.name.clone(),
            username: row.username.clone(),
            host: None,
            avatar_url: avatar_url(row, ctx),
            avatar_blurhash: row.avatar_blurhash.clone(),
            avatar_decorations: decorations(&row.avatar_decorations),
            is_bot: Some(false),
            is_cat: Some(false),
            require_signin_to_view_contents: None, // upstream: ===false ? undefined : true
            make_notes_followers_only_before: None, // ?? undefined → DB null omits
            make_notes_hidden_before: None,        // ?? undefined → DB null omits
            instance: None,
            emojis: Default::default(),
            online_status: online_status(row),
            badge_roles: Some(Vec::new()),
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
        is_silenced: false, // !canPublicNote, default policies
        is_suspended: row.is_suspended,
        description: profile.description.clone(),
        location: profile.location.clone(),
        birthday: profile.birthday.clone(),
        lang: profile.lang.clone(),
        fields: fields_vec(&profile.fields),
        verified_links: Vec::new(),
        followers_count: 0,
        following_count: 0,
        notes_count: 0,
        pinned_note_ids: Vec::new(),
        pinned_notes: Vec::new(),
        pinned_page_id: None,
        pinned_page: None,
        public_reactions: profile.public_reactions, // local user
        following_visibility: visibility(&profile.following_visibility),
        followers_visibility: visibility(&profile.followers_visibility),
        chat_scope: chat_scope(&row.chat_scope),
        can_chat: true,
        roles: Vec::new(),
        followed_message: Some(profile.followed_message.clone()),
        memo: None,
        moderation_note: None,
        two_factor_enabled: None,
        use_password_less_login: None,
        security_keys: None,
        is_following: None,
        is_followed: None,
        has_pending_follow_request_from_you: None,
        has_pending_follow_request_to_you: None,
        is_blocking: None,
        is_blocked: None,
        is_muted: None,
        is_renote_muted: None,
        notify: None,
        with_replies: None,
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

/// `pack(user, me, { schema: 'MeDetailed', includeSecrets: iAmOwner })`.
pub fn pack_show(
    row: &LocalUserRow,
    profile: &ProfileRow,
    me_id: Option<&str>,
    ctx: InstanceCtx<'_>,
) -> serde_json::Value {
    let is_me = me_id.map(|me| me == row.id).unwrap_or(false);
    if !is_me {
        let d = detailed(row, profile, ctx);
        return serde_json::to_value(&d).unwrap_or(serde_json::json!({}));
    }

    let d = detailed(row, profile, ctx);
    let ach: Vec<serde_json::Value> = profile.achievements.as_array().cloned().unwrap_or_default();
    let me = MeDetailed {
        detailed: crate::packers::user::UserDetailedNotMe {
            two_factor_enabled: Some(profile.two_factor_enabled),
            use_password_less_login: Some(profile.use_password_less_login),
            security_keys: Some(profile.security_keys_available),
            ..d
        },
        avatar_id: row.avatar_id.clone(),
        banner_id: row.banner_id.clone(),
        followed_message_always: profile.followed_message.clone(),
        is_moderator: false,
        is_admin: false,
        inject_featured_note: profile.inject_featured_note,
        receive_announcement_email: profile.receive_announcement_email,
        always_mark_nsfw: profile.always_mark_nsfw,
        auto_sensitive: profile.auto_sensitive,
        careful_bot: profile.careful_bot,
        auto_accept_followed: profile.auto_accept_followed,
        follow_approval_local_seconds: profile.follow_approval_local_seconds,
        follow_approval_remote_seconds: profile.follow_approval_remote_seconds,
        no_crawle: false,
        prevent_ai_learning: profile.prevent_ai_learning,
        is_explorable: row.is_explorable,
        is_deleted: false,
        two_factor_backup_codes_stock: backup_stock(None), // no backup secret col read yet
        hide_online_status: false,
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
        email: Some(profile.email.clone()),
        email_verified: Some(Some(profile.email_verified)),
        security_keys_list: Some(Vec::new()),
    };
    serde_json::to_value(&me).unwrap_or(serde_json::json!({}))
}

#[allow(dead_code)]
fn _silence(_: fn(&serde_json::Value) -> Option<String>) -> Option<String> {
    opt_json_string(&serde_json::json!(null))
}

/// One entrypoint for both DetailedNotMe (is_me=false) and MeDetailed
/// (is_me=true) responses; returns serde_json::Value.
pub fn pack_inline(
    row: &LocalUserRow,
    profile: &ProfileRow,
    is_me: bool,
    ctx: InstanceCtx<'_>,
) -> serde_json::Value {
    if !is_me {
        let d = detailed(row, profile, ctx);
        return serde_json::to_value(&d).unwrap_or(serde_json::json!({}));
    }
    let me = detailed_as_me(row, profile, ctx);
    serde_json::to_value(&me).unwrap_or(serde_json::json!({}))
}

/// Me variant of the pack (detailed() + Me-only fields).
fn detailed_as_me(row: &LocalUserRow, profile: &ProfileRow, ctx: InstanceCtx<'_>) -> MeDetailed {
    let d = detailed(row, profile, ctx);
    let ach = profile.achievements.as_array().cloned().unwrap_or_default();
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
        is_moderator: false,
        is_admin: false,
        inject_featured_note: profile.inject_featured_note,
        receive_announcement_email: profile.receive_announcement_email,
        always_mark_nsfw: profile.always_mark_nsfw,
        auto_sensitive: profile.auto_sensitive,
        careful_bot: profile.careful_bot,
        auto_accept_followed: profile.auto_accept_followed,
        follow_approval_local_seconds: profile.follow_approval_local_seconds,
        follow_approval_remote_seconds: profile.follow_approval_remote_seconds,
        no_crawle: false,
        prevent_ai_learning: profile.prevent_ai_learning,
        is_explorable: row.is_explorable,
        is_deleted: false,
        two_factor_backup_codes_stock: backup_stock(None),
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
        email: Some(profile.email.clone()),
        email_verified: Some(Some(profile.email_verified)),
        security_keys_list: Some(Vec::new()),
    }
}

/// Lite view of a local user used inside note packs (`pack`, lite: true).
pub fn user_lite_value(
    row: &LocalUserRow,
    _profile: &ProfileRow,
    ctx: InstanceCtx<'_>,
) -> serde_json::Value {
    let _ = _profile;
    use crate::packers::user::UserLite;
    let lite = UserLite {
        id: row.id.clone(),
        name: row.name.clone(),
        username: row.username.clone(),
        host: None,
        avatar_url: avatar_url(row, ctx),
        avatar_blurhash: row.avatar_blurhash.clone(),
        avatar_decorations: decorations(&row.avatar_decorations),
        is_bot: Some(false),
        is_cat: Some(false),
        require_signin_to_view_contents: None, // upstream: ===false ? undefined : true
        make_notes_followers_only_before: None, // ?? undefined → DB null omits
        make_notes_hidden_before: None,        // ?? undefined → DB null omits
        instance: None,
        emojis: Default::default(),
        online_status: OnlineStatus::Unknown,
        badge_roles: Some(Vec::new()),
    };
    serde_json::to_value(&lite).unwrap_or(serde_json::json!({}))
}
