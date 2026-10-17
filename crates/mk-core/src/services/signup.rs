//! Signup primitives ported from `core/SignupService.ts` and
//! `server/api/SignupApiService.ts`. DB writes live in mk-db; this module
//! carries validation, hashing, key generation, token generation, and the
//! fresh-user MeDetailed construction.

use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
use rsa::RsaPrivateKey;

use crate::packers::user::{
    BackupCodesStock, ChatScope, MeDetailed, OnlineStatus, VisibilityLevel,
};

/// `misc/secure-rndstr.ts` LU_CHARS (native user tokens use this alphabet).
pub const LU_CHARS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
/// `misc/secure-rndstr.ts` L_CHARS.
pub const L_CHARS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// Port of `generateNativeUserToken`: 16 chars of LU alphabet.
pub fn generate_native_user_token() -> String {
    secure_rndstr(16)
}

/// Port of `secureRndstr` (same byte contract: floor(rand/0xFF * len),
/// clamped to len-1 so the last char is never skipped).
pub fn secure_rndstr(length: usize) -> String {
    use rand::RngCore;
    let mut rng = rand::rngs::OsRng {};
    (0..length)
        .map(|_| {
            let mut b = [0u8; 1];
            rng.fill_bytes(&mut b);
            let idx = ((b[0] as usize * LU_CHARS.len()) / 255).min(LU_CHARS.len() - 1);
            LU_CHARS[idx] as char
        })
        .collect()
}

/// `UserEntityService.validateLocalUsername`: `/^\w{1,20}$/` (JS \w, ASCII).
pub fn validate_local_username(username: &str) -> bool {
    let n = username.chars().count();
    (1..=20).contains(&n)
        && username
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Node `generateKeyPair('rsa', { modulusLength: 2048 })` equivalent:
/// SPKI public PEM + PKCS#8 private PEM.
pub fn generate_rsa_keypair_pem() -> Result<(String, String), String> {
    let mut rng = rand::thread_rng();
    let key = RsaPrivateKey::new(&mut rng, 2048).map_err(|e| e.to_string())?;
    let public_pem = key
        .to_public_key()
        .to_public_key_pem(LineEnding::LF)
        .map_err(|e| e.to_string())?;
    let private_pem = key
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|e| e.to_string())?;
    Ok((public_pem, private_pem.to_string()))
}

pub fn hash_password(password: &str) -> Result<String, String> {
    bcrypt::hash(password, 8).map_err(|e| e.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> Result<bool, String> {
    bcrypt::verify(password, hash).map_err(|e| e.to_string())
}

/// Values produced by `SignupService.signup` that the DB layer persists.
pub struct SignupOutcome {
    pub user_id: String,
    pub username: String,
    pub username_lower: String,
    pub name: Option<String>,
    pub token: String,
    pub password_hash: String,
    pub public_key: String,
    pub private_key: String,
}

impl SignupOutcome {
    /// JS `new Date(id timestamp).toISOString()` for the derived createdAt.
    pub fn created_at_iso(&self) -> String {
        mk_types::id::parse_aid_date(&self.user_id)
            .map(ms_to_iso)
            .unwrap_or_default()
    }
}

/// The non-DB work of signup. Error strings mirror upstream failure codes.
pub fn build_signup(
    username: &str,
    password: &str,
    name: Option<&str>,
) -> Result<SignupOutcome, &'static str> {
    if !validate_local_username(username) {
        return Err("INVALID_USERNAME");
    }
    let token = generate_native_user_token();
    let password_hash = hash_password(password).map_err(|_| "HASH_FAILED")?;
    let (public_key, private_key) = generate_rsa_keypair_pem().map_err(|_| "KEYGEN_FAILED")?;
    let user_id = mk_types::id::gen_aid_now();
    Ok(SignupOutcome {
        user_id: user_id.clone(),
        username: username.to_string(),
        username_lower: username.to_lowercase(),
        name: name.map(|s| s.to_string()),
        token,
        password_hash,
        public_key,
        private_key,
    })
}

/// `UserEntityService.pack(user, me, { schema: 'MeDetailed', includeSecrets })`
/// for a just-created local user; every field carries upstream defaults
/// (profile column defaults + DEFAULT_POLICIES-derived computeds).
pub fn me_detailed_fresh(
    outcome: &SignupOutcome,
    instance_url: &str,
    instance_host: &str,
    include_secrets: bool,
) -> MeDetailed {
    let url_base = instance_url.trim_end_matches('/');
    let avatar_url = format!(
        "{}/identicon/{}@{}",
        url_base, outcome.username_lower, instance_host
    );

    let lite = crate::packers::user::UserLite {
        id: outcome.user_id.clone(),
        name: outcome.name.clone(),
        username: outcome.username.clone(),
        host: None,
        avatar_url,
        avatar_blurhash: None,
        avatar_decorations: Vec::new(),
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

    let detailed = crate::packers::user::UserDetailedNotMe {
        lite_fields: lite,
        url: None, // profile.url default null
        uri: None, // local user without uri
        moved_to: None,
        also_known_as: None,
        created_at: outcome.created_at_iso(),
        updated_at: None,
        last_fetched_at: None,
        banner_url: None,
        banner_blurhash: None,
        is_locked: false,
        is_silenced: false, // !canPublicNote, default policy true
        is_suspended: false,
        description: None,
        location: None,
        birthday: None,
        lang: None,
        fields: Vec::new(),
        verified_links: Vec::new(),
        followers_count: 0,
        following_count: 0,
        notes_count: 0,
        pinned_note_ids: Vec::new(),
        pinned_notes: Vec::new(),
        pinned_page_id: None,
        pinned_page: None,
        public_reactions: true, // local → profile default true
        following_visibility: VisibilityLevel::Public,
        followers_visibility: VisibilityLevel::Public,
        chat_scope: ChatScope::Mutual,
        can_chat: true, // chatAvailability default 'available'
        roles: Vec::new(),
        followed_message: None,
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
    };

    MeDetailed {
        detailed,
        avatar_id: None,
        banner_id: None,
        followed_message_always: None,
        is_moderator: false,
        is_admin: false,
        inject_featured_note: true,       // profile column default
        receive_announcement_email: true, // profile column default
        always_mark_nsfw: false,
        auto_sensitive: false,
        careful_bot: false,
        auto_accept_followed: true, // set explicitly by SignupService
        follow_approval_local_seconds: None,
        follow_approval_remote_seconds: None,
        no_crawle: false,
        prevent_ai_learning: true, // profile column default
        is_explorable: true,       // user column default
        is_deleted: false,
        // profile.twoFactorBackupSecret is null for a fresh user (pack L603)
        two_factor_backup_codes_stock: BackupCodesStock::None_,
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
        muted_words: Vec::new(),
        hard_muted_words: Vec::new(),
        muted_instances: Vec::new(),
        notification_recieve_config: serde_json::json!({}),
        email_notification_types: vec!["follow".to_string(), "receiveFollowRequest".to_string()],
        achievements: Vec::new(),
        logged_in_days: 0,
        policies: serde_json::json!({}),
        two_factor_enabled_always: false,
        use_password_less_login_always: false,
        security_keys_always: false,
        email: if include_secrets { Some(None) } else { None },
        email_verified: if include_secrets {
            Some(Some(false))
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

/// JS `Date.prototype.toISOString()` — millisecond RFC3339 with Z suffix.
pub fn ms_to_iso(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    // days_from_civil inverse (Howard Hinnant's civil_from_days).
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let secs_of_day = secs.rem_euclid(86_400);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y,
        m,
        d,
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
        millis
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_shape() {
        let t = generate_native_user_token();
        assert_eq!(t.len(), 16);
        assert!(t.chars().all(|c| LU_CHARS.contains(&(c as u8))));
        // native-token detection matches misc/token.ts isNativeUserToken
        assert_eq!(t.len() == 16, true);
    }

    #[test]
    fn username_validation() {
        assert!(validate_local_username("alice"));
        assert!(validate_local_username("a"));
        assert!(validate_local_username("01234567890123456789")); // 20
        assert!(!validate_local_username(""));
        assert!(!validate_local_username("012345678901234567890")); // 21
        assert!(!validate_local_username("bad name"));
        assert!(!validate_local_username("日本語"));
    }

    #[test]
    fn iso_conversion() {
        assert_eq!(ms_to_iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(ms_to_iso(1_700_000_000_123), "2023-11-14T22:13:20.123Z");
    }

    #[test]
    fn signup_builds_and_hashes() {
        let o = build_signup("alice", "test", None).unwrap();
        assert_eq!(o.username_lower, "alice");
        assert_eq!(o.token.len(), 16);
        assert!(o.public_key.starts_with("-----BEGIN PUBLIC KEY-----"));
        assert!(o.private_key.starts_with("-----BEGIN PRIVATE KEY-----"));
        assert!(verify_password("test", &o.password_hash).unwrap());
        assert!(!verify_password("wrong", &o.password_hash).unwrap());
    }
}
