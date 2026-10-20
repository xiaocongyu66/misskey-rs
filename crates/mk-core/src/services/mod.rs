//! Services ported from `src/core/`.

// email4: EmailSender seam (upstream EmailService.sendEmail) + Noop/SMTP
// senders. Deliberately unwired this slice — seam + contract tests only.
pub mod email;
pub mod misc;
// arena-move: account-migration pure helpers — Acct.parse parity, the
// TypeORM `simple-array` alsoKnownAs codec and local-user-URI math
// (UserEntityService.genLocalUserUri + ApPersonService.fetchPerson's local
// shortcut). Shared by the UserDetailed pack and api::move_account.
pub mod move_account;
pub mod note_pack;
// push-send: PushNotificationService.pushNotification delivery leg
// (list_by_user → type filter → vapid_jwt + minimal TCP POST; 404/410 prune).
pub mod notification_pack;
pub mod push_deliver;
pub mod role;
// roles-v2: conditional-role engine (evalCond/getUserRoles) + Role-pack
// policy surface (DEFAULT_POLICIES fill / explicit canPublicNote override)
pub mod roles_v2;
pub mod search_bridge;
pub mod signup;
pub mod user_pack;

pub use signup::{
    SignupOutcome, build_signup, generate_native_user_token, hash_password, me_detailed_fresh,
    verify_password,
};
pub mod web_push;
