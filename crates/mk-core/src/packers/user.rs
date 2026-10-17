//! Packed user responses (`Packed<UserLite|UserDetailedNotMe|MeDetailed>`).
//!
//! Field-by-field port of `src/models/json-schema/user.ts`. Presence rules
//! are load-bearing: e2e asserts exact field sets via deepStrictEqual.

use mk_types::presence::{Nullable, Omit};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AvatarDecoration {
    pub id: String,
    pub angle: Omit<f64>,
    #[serde(rename = "flipH")]
    pub flip_h: Omit<bool>,
    pub url: String,
    #[serde(rename = "offsetX")]
    pub offset_x: Omit<f64>,
    #[serde(rename = "offsetY")]
    pub offset_y: Omit<f64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FollowerInstance {
    pub name: Nullable<String>,
    #[serde(rename = "softwareName")]
    pub software_name: Nullable<String>,
    #[serde(rename = "softwareVersion")]
    pub software_version: Nullable<String>,
    #[serde(rename = "iconUrl")]
    pub icon_url: Nullable<String>,
    #[serde(rename = "faviconUrl")]
    pub favicon_url: Nullable<String>,
    #[serde(rename = "themeColor")]
    pub theme_color: Nullable<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BadgeRole {
    pub name: String,
    #[serde(rename = "iconUrl")]
    pub icon_url: Nullable<String>,
    #[serde(rename = "displayOrder")]
    pub display_order: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UserField {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RoleLiteValue {
    pub id: String,
    pub name: String,
    #[serde(rename = "color")]
    pub color: Nullable<String>,
    #[serde(rename = "iconUrl")]
    pub icon_url: Nullable<String>,
    pub description: String,
    #[serde(rename = "isModerator")]
    pub is_moderator: bool,
    #[serde(rename = "isAdministrator")]
    pub is_administrator: bool,
    #[serde(rename = "displayOrder")]
    pub display_order: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UserLite {
    pub id: String,
    pub name: Nullable<String>,
    pub username: String,
    pub host: Nullable<String>,
    #[serde(rename = "avatarUrl")]
    pub avatar_url: String,
    #[serde(rename = "avatarBlurhash")]
    pub avatar_blurhash: Nullable<String>,
    #[serde(rename = "avatarDecorations")]
    pub avatar_decorations: Vec<AvatarDecoration>,
    #[serde(rename = "isBot")]
    pub is_bot: Omit<bool>,
    #[serde(rename = "isCat")]
    pub is_cat: Omit<bool>,
    #[serde(rename = "requireSigninToViewContents")]
    pub require_signin_to_view_contents: Omit<bool>,
    #[serde(rename = "makeNotesFollowersOnlyBefore")]
    pub make_notes_followers_only_before: Omit<Nullable<i32>>,
    #[serde(rename = "makeNotesHiddenBefore")]
    pub make_notes_hidden_before: Omit<Nullable<i32>>,
    pub instance: Omit<FollowerInstance>,
    pub emojis: std::collections::BTreeMap<String, String>,
    #[serde(rename = "onlineStatus")]
    pub online_status: OnlineStatus,
    #[serde(rename = "badgeRoles")]
    pub badge_roles: Omit<Vec<BadgeRole>>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum OnlineStatus {
    Unknown,
    Online,
    Active,
    Offline,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UserDetailedNotMe {
    #[serde(flatten)]
    pub lite_fields: UserLite,
    pub url: Nullable<String>,
    pub uri: Nullable<String>,
    #[serde(rename = "movedTo")]
    pub moved_to: Nullable<String>,
    #[serde(rename = "alsoKnownAs")]
    pub also_known_as: Nullable<Vec<String>>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: Nullable<String>,
    #[serde(rename = "lastFetchedAt")]
    pub last_fetched_at: Nullable<String>,
    #[serde(rename = "bannerUrl")]
    pub banner_url: Nullable<String>,
    #[serde(rename = "bannerBlurhash")]
    pub banner_blurhash: Nullable<String>,
    #[serde(rename = "isLocked")]
    pub is_locked: bool,
    #[serde(rename = "isSilenced")]
    pub is_silenced: bool,
    #[serde(rename = "isSuspended")]
    pub is_suspended: bool,
    pub description: Nullable<String>,
    pub location: Nullable<String>,
    pub birthday: Nullable<String>,
    pub lang: Nullable<String>,
    pub fields: Vec<UserField>,
    #[serde(rename = "verifiedLinks")]
    pub verified_links: Vec<String>,
    #[serde(rename = "followersCount")]
    pub followers_count: i64,
    #[serde(rename = "followingCount")]
    pub following_count: i64,
    #[serde(rename = "notesCount")]
    pub notes_count: i64,
    #[serde(rename = "pinnedNoteIds")]
    pub pinned_note_ids: Vec<String>,
    #[serde(rename = "pinnedNotes")]
    pub pinned_notes: Vec<serde_json::Value>,
    #[serde(rename = "pinnedPageId")]
    pub pinned_page_id: Nullable<String>,
    #[serde(rename = "pinnedPage")]
    pub pinned_page: Nullable<serde_json::Value>,
    #[serde(rename = "publicReactions")]
    pub public_reactions: bool,
    #[serde(rename = "followingVisibility")]
    pub following_visibility: VisibilityLevel,
    #[serde(rename = "followersVisibility")]
    pub followers_visibility: VisibilityLevel,
    #[serde(rename = "chatScope")]
    pub chat_scope: ChatScope,
    #[serde(rename = "canChat")]
    pub can_chat: bool,
    pub roles: Vec<RoleLiteValue>,
    #[serde(rename = "followedMessage")]
    pub followed_message: Omit<Nullable<String>>,
    pub memo: Nullable<String>,
    #[serde(rename = "moderationNote")]
    pub moderation_note: Omit<String>,
    // Overridden with always-present fields when packed as MeDetailed.
    #[serde(rename = "twoFactorEnabled")]
    pub two_factor_enabled: Omit<bool>,
    #[serde(rename = "usePasswordLessLogin")]
    pub use_password_less_login: Omit<bool>,
    #[serde(rename = "securityKeys")]
    pub security_keys: Omit<bool>,
    // Relation flags (present only when a me parameter is supplied).
    #[serde(rename = "isFollowing")]
    pub is_following: Omit<bool>,
    #[serde(rename = "isFollowed")]
    pub is_followed: Omit<bool>,
    #[serde(rename = "hasPendingFollowRequestFromYou")]
    pub has_pending_follow_request_from_you: Omit<bool>,
    #[serde(rename = "hasPendingFollowRequestToYou")]
    pub has_pending_follow_request_to_you: Omit<bool>,
    #[serde(rename = "isBlocking")]
    pub is_blocking: Omit<bool>,
    #[serde(rename = "isBlocked")]
    pub is_blocked: Omit<bool>,
    #[serde(rename = "isMuted")]
    pub is_muted: Omit<bool>,
    #[serde(rename = "isRenoteMuted")]
    pub is_renote_muted: Omit<bool>,
    pub notify: Omit<String>,
    #[serde(rename = "withReplies")]
    pub with_replies: Omit<bool>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum VisibilityLevel {
    Public,
    Followers,
    Private,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ChatScope {
    Everyone,
    Following,
    Followers,
    Mutual,
    None_,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MeDetailed {
    #[serde(flatten)]
    pub detailed: UserDetailedNotMe,
    #[serde(rename = "avatarId")]
    pub avatar_id: Nullable<String>,
    #[serde(rename = "bannerId")]
    pub banner_id: Nullable<String>,
    // followedMessage loses its optional flag in the Me view.
    #[serde(rename = "followedMessage")]
    pub followed_message_always: Nullable<String>,
    #[serde(rename = "isModerator")]
    pub is_moderator: bool,
    #[serde(rename = "isAdmin")]
    pub is_admin: bool,
    #[serde(rename = "injectFeaturedNote")]
    pub inject_featured_note: bool,
    #[serde(rename = "receiveAnnouncementEmail")]
    pub receive_announcement_email: bool,
    #[serde(rename = "alwaysMarkNsfw")]
    pub always_mark_nsfw: bool,
    #[serde(rename = "autoSensitive")]
    pub auto_sensitive: bool,
    #[serde(rename = "carefulBot")]
    pub careful_bot: bool,
    #[serde(rename = "autoAcceptFollowed")]
    pub auto_accept_followed: bool,
    #[serde(rename = "followApprovalLocalSeconds")]
    pub follow_approval_local_seconds: Nullable<i32>,
    #[serde(rename = "followApprovalRemoteSeconds")]
    pub follow_approval_remote_seconds: Nullable<i32>,
    #[serde(rename = "noCrawle")]
    pub no_crawle: bool,
    #[serde(rename = "preventAiLearning")]
    pub prevent_ai_learning: bool,
    #[serde(rename = "isExplorable")]
    pub is_explorable: bool,
    #[serde(rename = "isDeleted")]
    pub is_deleted: bool,
    #[serde(rename = "twoFactorBackupCodesStock")]
    pub two_factor_backup_codes_stock: BackupCodesStock,
    #[serde(rename = "hideOnlineStatus")]
    pub hide_online_status: bool,
    #[serde(rename = "hasUnreadSpecifiedNotes")]
    pub has_unread_specified_notes: bool,
    #[serde(rename = "hasUnreadMentions")]
    pub has_unread_mentions: bool,
    #[serde(rename = "hasUnreadAnnouncement")]
    pub has_unread_announcement: bool,
    #[serde(rename = "unreadAnnouncements")]
    pub unread_announcements: Vec<serde_json::Value>,
    #[serde(rename = "hasUnreadAntenna")]
    pub has_unread_antenna: bool,
    #[serde(rename = "hasUnreadChannel")]
    pub has_unread_channel: bool,
    #[serde(rename = "hasUnreadChatMessages")]
    pub has_unread_chat_messages: bool,
    #[serde(rename = "hasUnreadNotification")]
    pub has_unread_notification: bool,
    #[serde(rename = "hasPendingReceivedFollowRequest")]
    pub has_pending_received_follow_request: bool,
    #[serde(rename = "unreadNotificationsCount")]
    pub unread_notifications_count: i64,
    #[serde(rename = "mutedWords")]
    pub muted_words: Vec<Vec<String>>,
    #[serde(rename = "hardMutedWords")]
    pub hard_muted_words: Vec<Vec<String>>,
    #[serde(rename = "mutedInstances")]
    pub muted_instances: Vec<String>,
    #[serde(rename = "notificationRecieveConfig")]
    pub notification_recieve_config: serde_json::Value,
    #[serde(rename = "emailNotificationTypes")]
    pub email_notification_types: Vec<String>,
    pub achievements: Vec<serde_json::Value>,
    #[serde(rename = "loggedInDays")]
    pub logged_in_days: i64,
    pub policies: serde_json::Value,
    #[serde(rename = "twoFactorEnabled")]
    pub two_factor_enabled_always: bool,
    #[serde(rename = "usePasswordLessLogin")]
    pub use_password_less_login_always: bool,
    #[serde(rename = "securityKeys")]
    pub security_keys_always: bool,
    // Secrets (includeSecrets only).
    pub email: Omit<Nullable<String>>,
    #[serde(rename = "emailVerified")]
    pub email_verified: Omit<Nullable<bool>>,
    #[serde(rename = "securityKeysList")]
    pub security_keys_list: Omit<Vec<serde_json::Value>>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum BackupCodesStock {
    Full,
    Partial,
    None_,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_lite() -> UserLite {
        UserLite {
            id: "9abc1234defg".to_string(),
            name: None,
            username: "alice".to_string(),
            host: None,
            avatar_url: "http://x/identicon/alice".to_string(),
            avatar_blurhash: None,
            avatar_decorations: vec![],
            is_bot: Omit(None),
            is_cat: Omit(None),
            require_signin_to_view_contents: Omit(Some(false)),
            make_notes_followers_only_before: Omit(None),
            make_notes_hidden_before: Omit(None),
            instance: Omit(None),
            emojis: Default::default(),
            online_status: OnlineStatus::Unknown,
            badge_roles: Omit(None),
        }
    }

    #[test]
    fn lite_field_presence_matches_schema() {
        let v = serde_json::to_value(&sample_lite()).unwrap();
        let expected_keys: Vec<&str> = vec![
            "id",
            "name",
            "username",
            "host",
            "avatarUrl",
            "avatarBlurhash",
            "avatarDecorations",
            "requireSigninToViewContents",
            "emojis",
            "onlineStatus",
        ];
        let mut got = v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        got.sort();
        let mut expected_keys = expected_keys;
        expected_keys.sort();
        assert_eq!(got, expected_keys);
        assert_eq!(v["name"], json!(null));
        assert_eq!(v["host"], json!(null));
    }
}
