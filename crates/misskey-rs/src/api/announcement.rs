//! announcements domain — ports of
//! `endpoints/announcements.ts`, `endpoints/announcements/show.ts`,
//! `endpoints/i/read-announcement.ts`,
//! `endpoints/admin/announcements/{create,list,update,delete}.ts` funneling
//! through `core/AnnouncementService.ts` +
//! `core/entities/AnnouncementEntityService.ts`. Self-contained module on the
//! delete_account/channel pattern: the `// announcements-agent` marked
//! dispatch arm in mod.rs (nosed in ahead of the admin/* catch-all so
//! `admin/announcements/*` shadows it) parses the body, extracts the token
//! and probes a declared credential; this file owns the requireCredential /
//! requireModerator gates and the endpoint bodies. Db paths live in
//! `mk_db::announcement` (tables ship in the 0001 baseline; no migration).
//!
//! NOTE on `i/read-all-unread-announcements`: the endpoint does not exist at
//! the pinned upstream (ea2c3aed) — the registry only carries
//! `i/read-announcement`; the unread feed surfaces through the MeDetailed
//! `unreadAnnouncements` pack keys (db path: `unread_for_user` below).
//!
//! Status mapping (`ApiCallService.#sendApiError`): the meta ApiErrors here
//! (NO_SUCH_ANNOUNCEMENT, INVALID_AUTO_ARCHIVE_AT) carry no httpStatusCode →
//! kind 'client' defaults to 400; void handlers answer 204 NO_CONTENT.
//!
//! MODERATOR GATE (documented deviation, admin.rs pattern): upstream tags the
//! requester root via `meta."rootUserId"`, which SignupService stamps on the
//! FIRST account (SignupService L160). The port's signup route does not write
//! meta yet, so root detection is `meta."rootUserId" == me.id` ||
//! `usernameLower == 'root'` || — the first-user proxy keeping the e2e
//! admins (first signup of a fresh test db) functional — "rootUserId unset
//! and me is the earliest registered user". Once signup stamps meta this
//! proxy goes inert; requireModerator role assignments round the gate out.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// Endpoint names owned by this module (all POST-only).
pub(crate) fn handles(name: &str) -> bool {
    matches!(
        name,
        "announcements"
            | "announcements/show"
            | "i/read-announcement"
            | "admin/announcements/create"
            | "admin/announcements/list"
            | "admin/announcements/update"
            | "admin/announcements/delete"
    )
}

/// Names behind requireCredential (everything else here is guest-ok and
/// resolves an optional me).
const REQUIRES_CREDENTIAL: &[&str] = &["i/read-announcement"];

/// Names behind requireModerator (admin/*, upstream meta.requireModerator).
const REQUIRES_MODERATOR: &[&str] = &[
    "admin/announcements/create",
    "admin/announcements/list",
    "admin/announcements/update",
    "admin/announcements/delete",
];

/// Entry from the announcements-agent dispatch arm in mod.rs (body
/// pre-parsed, token + credential-declared pre-extracted).
pub(crate) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
    credential_declared: bool,
) -> Response {
    let Some(pool) = &state.pool else {
        return internal();
    };
    let result: Check<Response> = async {
        let me_id = if REQUIRES_CREDENTIAL.contains(&name)
            || REQUIRES_MODERATOR.contains(&name)
            || token.is_some()
            || credential_declared
        {
            // One resolution for every shape: the requireCredential /
            // requireModerator gates and the guest-ok optional me alike
            // (a declared-but-invalid credential → 401, mirroring
            // #sendAuthenticationError on the public path).
            Some(
                super::social::require_checked_user(pool, token.as_deref(), credential_declared)
                    .await?,
            )
        } else {
            None
        };

        if REQUIRES_MODERATOR.contains(&name) {
            moderator_gate(pool, me_id.as_deref().unwrap()).await?;
        }

        match name {
            "announcements" => announcements_list(pool, body, me_id.as_deref()).await,
            "announcements/show" => announcements_show(pool, body, me_id.as_deref()).await,
            "i/read-announcement" => {
                i_read_announcement(pool, body, me_id.as_deref().unwrap()).await
            }
            "admin/announcements/create" => {
                admin_create(pool, body, me_id.as_deref().unwrap()).await
            }
            "admin/announcements/list" => admin_list(pool, body, me_id.as_deref().unwrap()).await,
            "admin/announcements/update" => {
                admin_update(pool, body, me_id.as_deref().unwrap()).await
            }
            "admin/announcements/delete" => {
                admin_delete(pool, body, me_id.as_deref().unwrap()).await
            }
            _ => Ok(api_err(ApiError::unknown_endpoint())),
        }
    }
    .await;
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

// --- shared shapes ---

type Check<T = Response> = Result<T, Response>;

/// Plain handler Error → ApiCallService.#onExecError → 500 INTERNAL_ERROR.
fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_announcements", error = %e, "db error: {context}");
    internal()
}

/// Meta error without httpStatusCode: kind 'client' → default 400.
fn meta_err(code: &str, id: &str, message: &str) -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        code,
        id,
        message,
    ))
}

fn no_such_announcement(id: &str) -> Response {
    // "No such announcement." ApiError metas (update/delete/show ids differ).
    meta_err("NO_SUCH_ANNOUNCEMENT", id, "No such announcement.")
}

fn invalid_auto_archive_at(id: &str) -> Response {
    meta_err("INVALID_AUTO_ARCHIVE_AT", id, "Invalid auto archive date.")
}

fn role_permission_denied() -> Response {
    api_err(ApiError::new(
        403,
        mk_types::api_error::ApiErrorKind::Permission,
        "ROLE_PERMISSION_DENIED",
        "d33d5333-db36-423d-a8f9-1a2b9549da41",
        "You are not assigned to a moderator role.",
    ))
}

fn invalid_param(param: &str, reason: &str) -> Response {
    api_err(ApiError::invalid_param(json!({
        "param": param,
        "reason": reason,
    })))
}

/// Root + moderator-role probe (module doc: the first-user proxy for the
/// unlanded SignupService meta stamp; admin.rs holds the sibling shape).
async fn is_root(pool: &mk_db::sqlx::PgPool, me: &mk_db::user::LocalUserRow) -> Check<bool> {
    let meta_root = mk_db::admin::root_user_id(pool)
        .await
        .map_err(|e| db_err("root_user_id[announcements]", e))?;
    if meta_root.as_deref() == Some(me.id.as_str()) || me.username_lower == "root" {
        return Ok(true);
    }
    if meta_root.is_none() {
        // SignupService stamp gap: the first registered account IS root.
        let first = mk_db::announcement::first_registered_user_id(pool)
            .await
            .map_err(|e| db_err("first_registered_user_id", e))?;
        if first.as_deref() == Some(me.id.as_str()) {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn moderator_gate(
    pool: &mk_db::sqlx::PgPool,
    me_id: &str,
) -> Check<mk_db::user::LocalUserRow> {
    let me = match mk_db::user::find_user_by_id(pool, me_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return Err(internal()),
        Err(e) => return Err(db_err("find_user_by_id[moderator_gate]", e)),
    };
    if !is_root(pool, &me).await? {
        let assigned = mk_db::role::user_assigned_roles(pool, me_id)
            .await
            .map_err(|e| db_err("user_assigned_roles[announcements]", e))?;
        if !mk_core::services::role::has_moderator_role(&assigned) {
            return Err(role_permission_denied());
        }
    }
    Ok(me)
}

// --- params ---

fn require_str(body: &Value, key: &str) -> Check<String> {
    match body.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        Some(Value::String(_)) => Err(invalid_param(key, "must NOT be empty")),
        None | Some(Value::Null) => Err(invalid_param(
            key,
            &format!("must have required property '{key}'"),
        )),
        Some(_) => Err(invalid_param(key, "must be string")),
    }
}

/// paramDef limit: integer 1..=100, default 10 (ajv usesDefaults).
fn parse_limit(body: &Value) -> Check<i64> {
    match body.get("limit") {
        None | Some(Value::Null) => Ok(10),
        Some(v) => match v.as_i64() {
            Some(n) if (1..=100).contains(&n) => Ok(n),
            _ => Err(invalid_param(
                "limit",
                "must be an integer between 1 and 100",
            )),
        },
    }
}

fn opt_id(body: &Value, key: &str) -> Check<Option<String>> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(invalid_param(key, "must be a string")),
    }
}

/// `{ type: 'integer' }` optional param (sinceDate/untilDate).
fn opt_int(body: &Value, key: &str) -> Check<Option<i64>> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_i64() {
            Some(n) => Ok(Some(n)),
            None => Err(invalid_param(key, "must be an integer")),
        },
    }
}

fn opt_bool_param(body: &Value, key: &str) -> Check<Option<bool>> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(invalid_param(key, "expected a boolean")),
    }
}

fn opt_enum_param(body: &Value, key: &str, allowed: &[&str]) -> Check<Option<String>> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if allowed.contains(&s.as_str()) => Ok(Some(s.clone())),
        Some(_) => Err(invalid_param(
            key,
            &format!("expected one of: {}", allowed.join(", ")),
        )),
    }
}

/// QueryService.makePaginationQuery with the sinceDate/untilDate fold: the
/// date params turn into generated boundary ids (`idService.gen(date)`)
/// whenever the corresponding id param is absent (id params win).
fn pagination_bounds(body: &Value) -> Check<(Option<String>, Option<String>)> {
    let since_id = opt_id(body, "sinceId")?;
    let until_id = opt_id(body, "untilId")?;
    let since_date = opt_int(body, "sinceDate")?;
    let until_date = opt_int(body, "untilDate")?;
    let since = since_id.or_else(|| since_date.map(mk_types::id::gen_aid));
    let until = until_id.or_else(|| until_date.map(mk_types::id::gen_aid));
    Ok((since, until))
}

// --- packs ---

/// `AnnouncementEntityService.pack`: id, createdAt (id parse), updatedAt,
/// title, text, imageUrl, icon, display, forYou (userId === me?.id),
/// needConfirmationToRead, silence — isRead only resolves with a me (upstream
/// leaves it `undefined` otherwise, which JSON.stringify drops).
fn pack_announcement(
    row: mk_db::announcement::AnnouncementRow,
    me_id: Option<&str>,
    is_read: Option<bool>,
) -> Value {
    let created_at = mk_types::id::parse_aid_date(&row.id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default();
    let updated_at = row
        .updated_at_ms
        .map(mk_core::services::signup::ms_to_iso)
        .map(Value::String)
        .unwrap_or(Value::Null);
    let for_you = matches!(me_id, Some(m) if row.user_id.as_deref() == Some(m));
    let mut pack = json!({
        "id": row.id,
        "createdAt": created_at,
        "updatedAt": updated_at,
        "title": row.title,
        "text": row.text,
        "imageUrl": row.image_url,
        "icon": row.icon,
        "display": row.display,
        "forYou": for_you,
        "needConfirmationToRead": row.need_confirmation_to_read,
        "silence": row.silence,
    });
    let obj = pack.as_object_mut().unwrap();
    if let Some(is_read) = is_read {
        obj.insert("isRead".to_string(), json!(is_read));
    }
    pack
}

/// `admin/announcements/list` row shape: the stored columns plus the
/// COMPUTED isActive (`isActive && (autoArchiveAt == null || autoArchiveAt >
/// now)` — the archive queue's effect without a queue), autoArchiveAt as ISO
/// string/null and the per-row reads count.
fn pack_admin_row(row: mk_db::announcement::AnnouncementRow, reads: i64, now_ms: i64) -> Value {
    let created_at = mk_types::id::parse_aid_date(&row.id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default();
    let updated_at = row
        .updated_at_ms
        .map(mk_core::services::signup::ms_to_iso)
        .map(Value::String)
        .unwrap_or(Value::Null);
    let auto_archive_at = row
        .auto_archive_at_ms
        .map(mk_core::services::signup::ms_to_iso)
        .map(Value::String)
        .unwrap_or(Value::Null);
    json!({
        "id": row.id,
        "createdAt": created_at,
        "updatedAt": updated_at,
        "title": row.title,
        "text": row.text,
        "imageUrl": row.image_url,
        "icon": row.icon,
        "display": row.display,
        "isActive": mk_db::announcement::computed_is_active(&row, now_ms),
        "autoArchiveAt": auto_archive_at,
        "forExistingUsers": row.for_existing_users,
        "silence": row.silence,
        "needConfirmationToRead": row.need_confirmation_to_read,
        "userId": row.user_id,
        "reads": reads,
    })
}

// --- public endpoints ---

/// `POST /api/announcements` — meta list (requireCredential false): the
/// pagination, the isActive switch and the personal+global union are all in
/// `list_public`; guests pack without isRead/forYou resolution.
async fn announcements_list(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: Option<&str>,
) -> Check<Response> {
    let limit = parse_limit(body)?;
    let (since, until) = pagination_bounds(body)?;
    let is_active = opt_bool_param(body, "isActive")?.unwrap_or(true);
    let now = super::now_ms() as i64;
    let rows = mk_db::announcement::list_public(
        pool,
        me_id,
        is_active,
        since.as_deref(),
        until.as_deref(),
        now,
        limit,
    )
    .await
    .map_err(|e| db_err("list_public", e))?;
    let packed: Vec<Value> = rows
        .into_iter()
        .map(|(row, _for_you, is_read)| {
            // isRead rides only with a me; guests still resolve forYou=false.
            let read = me_id.map(|_| is_read);
            pack_announcement(row, me_id, read)
        })
        .collect();
    Ok(Json(packed).into_response())
}

/// `POST /api/announcements/show` — one pack; `getAnnouncement`
/// re-routes the EntityNotFoundError (personal announcement of someone else,
/// or a miss) into NO_SUCH_ANNOUNCEMENT.
async fn announcements_show(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: Option<&str>,
) -> Check<Response> {
    let announcement_id = require_str(body, "announcementId")?;
    let row = mk_db::announcement::find_by_id(pool, &announcement_id)
        .await
        .map_err(|e| db_err("find_by_id[show]", e))?;
    let Some(row) = row else {
        return Err(no_such_announcement("b57b5e1d-4f49-404a-9edb-46b00268f121"));
    };
    // `if (announcement.userId && (me == null || announcement.userId !== me.id))`
    if let Some(owner) = row.user_id.as_deref() {
        if me_id != Some(owner) {
            return Err(no_such_announcement("b57b5e1d-4f49-404a-9edb-46b00268f121"));
        }
    }
    let is_read = match me_id {
        Some(m) => Some(
            mk_db::announcement::is_read(pool, m, &row.id)
                .await
                .map_err(|e| db_err("is_read[show]", e))?,
        ),
        None => None,
    };
    let pack = pack_announcement(row, me_id, is_read);
    Ok(Json(pack).into_response())
}

// --- i/read-announcement ---

/// `POST /api/i/read-announcement` — AnnouncementService.read (read-row
/// insert + the personal-announcement deactivation); void → 204.
async fn i_read_announcement(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Check<Response> {
    let announcement_id = require_str(body, "announcementId")?;
    mk_db::announcement::read(pool, &mk_types::id::gen_aid_now(), me_id, &announcement_id)
        .await
        .map_err(|e| db_err("announcement::read", e))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// --- admin/announcements/* ---

/// `POST /api/admin/announcements/create` — requireModerator; the
/// INVALID_AUTO_ARCHIVE_AT pre-check (`<= Date.now()` or unparseable) runs
/// before the insert; response is the entity pack.
async fn admin_create(pool: &mk_db::sqlx::PgPool, body: &Value, me_id: &str) -> Check<Response> {
    let title = require_str(body, "title")?;
    let text = require_str(body, "text")?;
    // imageUrl is REQUIRED (may be null), then `ps.imageUrl || null`.
    let image_url_raw = match body.get("imageUrl") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            if s.is_empty() {
                None // '' || null → null
            } else {
                Some(s.clone())
            }
        }
        Some(_) => return Err(invalid_param("imageUrl", "must be string or null")),
    };
    let icon = opt_enum_param(body, "icon", &["info", "warning", "error", "success"])?
        .unwrap_or_else(|| "info".to_string());
    let display = opt_enum_param(body, "display", &["normal", "banner", "dialog"])?
        .unwrap_or_else(|| "normal".to_string());
    let for_existing_users = opt_bool_param(body, "forExistingUsers")?.unwrap_or(false);
    let silence = opt_bool_param(body, "silence")?.unwrap_or(false);
    let need_confirmation_to_read =
        opt_bool_param(body, "needConfirmationToRead")?.unwrap_or(false);
    let target_user = opt_id(body, "userId")?;
    let now = super::now_ms() as i64;
    let auto_archive_at_ms = match body.get("autoArchiveAt") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let Some(ms) = v.as_i64() else {
                return Err(invalid_param("autoArchiveAt", "must be an integer or null"));
            };
            // `ps.autoArchiveAt <= Date.now()` → INVALID_AUTO_ARCHIVE_AT
            if ms <= now {
                return Err(invalid_auto_archive_at(
                    "2a892bd5-487d-46a2-a5fe-3d85ad51defe",
                ));
            }
            Some(ms)
        }
    };
    // Service.create: `isActive: values.autoArchiveAt == null || > new Date()`
    let is_active = auto_archive_at_ms.map(|t| t > now).unwrap_or(true);

    let id = mk_types::id::gen_aid_now();
    mk_db::announcement::insert_announcement(
        pool,
        &id,
        &title,
        &text,
        image_url_raw.as_deref(),
        &icon,
        &display,
        need_confirmation_to_read,
        is_active,
        auto_archive_at_ms,
        for_existing_users,
        silence,
        target_user.as_deref(),
    )
    .await
    .map_err(|e| db_err("insert_announcement", e))?;
    let row = mk_db::announcement::find_by_id(pool, &id)
        .await
        .map_err(|e| db_err("find_by_id[created]", e))?
        .ok_or_else(internal)?;

    // Fire-and-forget moderation log (moderator present). The personal
    // variant looks the target user up (`findOneByOrFail` — a miss is a 500
    // after the insert, like upstream).
    let mut log_info = json!({
        "announcementId": row.id,
        "announcement": {
            "id": row.id,
            "title": row.title,
            "text": row.text,
            "isActive": row.is_active,
        },
    });
    if let Some(uid) = target_user.as_deref() {
        let user = mk_db::user::find_user_by_id(pool, uid)
            .await
            .map_err(|e| db_err("find_user_by_id[create target]", e))?;
        let Some(user) = user else {
            return Err(internal());
        };
        log_info["userId"] = json!(user.id);
        log_info["userUsername"] = json!(user.username);
        log_info["userHost"] = Value::Null;
        moderation_log(pool, me_id, "createUserAnnouncement", log_info).await;
    } else {
        moderation_log(pool, me_id, "createGlobalAnnouncement", log_info).await;
    }

    // The AnnouncementEntityService.pack seen by the moderator response
    // carries no me (`pack(announcement)`) → forYou false, isRead absent.
    Ok(Json(pack_announcement(row, None, None)).into_response())
}

/// Fire-and-forget ModerationLogService.log — failures logged, never fatal.
async fn moderation_log(
    pool: &mk_db::sqlx::PgPool,
    moderator_id: &str,
    log_type: &str,
    info: Value,
) {
    if let Err(e) = mk_db::admin::insert_moderation_log(
        pool,
        &mk_types::id::gen_aid_now(),
        moderator_id,
        log_type,
        &info,
    )
    .await
    {
        tracing::error!(target: "dbg_announcements", error = %e, "insert_moderation_log");
    }
}

/// `POST /api/admin/announcements/list` — requireModerator; status filter +
/// the either-or userId scope + reads counts (all in `list_admin`).
async fn admin_list(pool: &mk_db::sqlx::PgPool, body: &Value, me_id: &str) -> Check<Response> {
    let limit = parse_limit(body)?;
    let (since, until) = pagination_bounds(body)?;
    let user_id = opt_id(body, "userId")?;
    let status = opt_enum_param(body, "status", &["all", "active", "archived"])?
        .unwrap_or_else(|| "active".to_string());
    let now = super::now_ms() as i64;
    let rows = mk_db::announcement::list_admin(
        pool,
        &status,
        user_id.as_deref(),
        since.as_deref(),
        until.as_deref(),
        now,
        limit,
    )
    .await
    .map_err(|e| db_err("list_admin", e))?;
    let _ = me_id; // the list is not me-scoped beyond the userId param
    let packed: Vec<Value> = rows
        .into_iter()
        .map(|(row, reads)| pack_admin_row(row, reads, now))
        .collect();
    Ok(Json(packed).into_response())
}

/// `POST /api/admin/announcements/update` — requireModerator; the
/// INVALID_AUTO_ARCHIVE_AT check is laxer than create (`< 0` only — the e2e
/// archives by setting a past date); void → 204.
async fn admin_update(pool: &mk_db::sqlx::PgPool, body: &Value, me_id: &str) -> Check<Response> {
    let id = require_str(body, "id")?;
    let now = super::now_ms() as i64;
    let auto_archive_at_ms = match body.get("autoArchiveAt") {
        None => None,                    // absent → column untouched
        Some(Value::Null) => Some(None), // null → cleared
        Some(v) => {
            let Some(ms) = v.as_i64() else {
                return Err(invalid_param("autoArchiveAt", "must be an integer or null"));
            };
            // `ps.autoArchiveAt < 0 || NaN` → INVALID_AUTO_ARCHIVE_AT
            if ms < 0 {
                return Err(invalid_auto_archive_at(
                    "01b83d7b-2fd5-4d7c-86c4-d03144d16355",
                ));
            }
            Some(Some(ms))
        }
    };
    // imageUrl is ALWAYS written (`ps.imageUrl || null`): an absent param
    // collapses to null, an explicit null too, '' collapses to null.
    let image_url: Option<String> = match body.get("imageUrl") {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    };
    let title = opt_str_ne(body, "title", "title")?;
    let text = opt_str_ne(body, "text", "text")?;
    let icon = opt_enum_param(body, "icon", &["info", "warning", "error", "success"])?;
    let display = opt_enum_param(body, "display", &["normal", "banner", "dialog"])?;
    let for_existing_users = opt_bool_param(body, "forExistingUsers")?;
    let silence = opt_bool_param(body, "silence")?;
    let need_confirmation_to_read = opt_bool_param(body, "needConfirmationToRead")?;
    let is_active = opt_bool_param(body, "isActive")?;

    let before = mk_db::announcement::find_by_id(pool, &id)
        .await
        .map_err(|e| db_err("find_by_id[update]", e))?;
    let Some(before) = before else {
        return Err(no_such_announcement("d3aae5a7-6372-4cb4-b61c-f511ffc2d7cc"));
    };

    mk_db::announcement::update_announcement(
        pool,
        &id,
        now,
        image_url.as_deref(),
        title.as_deref(),
        text.as_deref(),
        icon.as_deref(),
        display.as_deref(),
        for_existing_users,
        silence,
        need_confirmation_to_read,
        is_active,
        auto_archive_at_ms,
    )
    .await
    .map_err(|e| db_err("update_announcement", e))?;

    let after = mk_db::announcement::find_by_id(pool, &id)
        .await
        .map_err(|e| db_err("find_by_id[updated]", e))?;

    // ModerationLogService.log (fire-and-forget), global vs user variant.
    let compact = |tag: &str, r: &Option<mk_db::announcement::AnnouncementRow>| {
        r.as_ref()
            .map(|row| {
                json!({
                    "id": row.id,
                    "isActive": row.is_active,
                    "autoArchiveAt": row.auto_archive_at_ms,
                    "stage": tag,
                })
            })
            .unwrap_or(Value::Null)
    };
    let mut log_info = json!({
        "announcementId": before.id,
        "before": compact("before", &Some(before.clone())),
        "after": compact("after", &after),
    });
    if before.user_id.is_some() {
        log_info["userId"] = json!(before.user_id);
        moderation_log(pool, me_id, "updateUserAnnouncement", log_info).await;
    } else {
        moderation_log(pool, me_id, "updateGlobalAnnouncement", log_info).await;
    }

    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Optional non-empty string param (minLength 1), `undefined` == absent.
fn opt_str_ne(body: &Value, key: &str, label: &str) -> Check<Option<String>> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
        Some(_) => Err(invalid_param(label, "must be a non-empty string")),
    }
}

/// `POST /api/admin/announcements/delete` — requireModerator; row delete +
/// the moderation log; void → 204.
async fn admin_delete(pool: &mk_db::sqlx::PgPool, body: &Value, me_id: &str) -> Check<Response> {
    let id = require_str(body, "id")?;
    let announcement = mk_db::announcement::find_by_id(pool, &id)
        .await
        .map_err(|e| db_err("find_by_id[delete]", e))?;
    let Some(announcement) = announcement else {
        return Err(no_such_announcement("ecad8040-a276-4e85-bda9-015a708d291e"));
    };

    mk_db::announcement::delete_announcement(pool, &id)
        .await
        .map_err(|e| db_err("delete_announcement", e))?;

    let mut log_info = json!({
        "announcementId": announcement.id,
        "announcement": {
            "id": announcement.id,
            "title": announcement.title,
            "isActive": announcement.is_active,
        },
    });
    if announcement.user_id.is_some() {
        log_info["userId"] = json!(announcement.user_id);
        moderation_log(pool, me_id, "deleteUserAnnouncement", log_info).await;
    } else {
        moderation_log(pool, me_id, "deleteGlobalAnnouncement", log_info).await;
    }

    Ok(StatusCode::NO_CONTENT.into_response())
}
