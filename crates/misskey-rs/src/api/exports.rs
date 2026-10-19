//! arena-exports (upstream test/e2e/exports.ts = the export-clips suite):
//! the `i/export-clips` enqueue lane + the `exportClips` job processor + the
//! `/files/{accessKey}/{name}` byte route + the drive/files `{list,show,
//! delete}` endpoints the suite's beforeEach / pollFirstDriveFile drive.
//!
//! Upstream flows:
//! - `endpoints/i/export-clips.ts`: requireCredential, limit 1/day (the
//!   e2e never hits it — RateLimiterService disables itself unless
//!   NODE_ENV=production), void handler → 204 after
//!   `QueueService.createExportClipsJob` adds the `exportClips` db-queue job
//!   `{ type: 'exportClips', user: { id } }`.
//! - `queue/processors/ExportClipsProcessorService.ts`: stream a JSON array
//!   of the user's clips (`clip.id` ASC), each `{ id, name, description,
//!   lastClippedAt, clipNotes: [...] }`, clipNotes in `clipNote.id` ASC each
//!   `{ id, createdAt, note: {...author-lite} }` — then addFile the temp
//!   file as `clips-yyyy-MM-dd-HH-mm-ss.json` (force: true, ext: json) and
//!   createNotification('exportCompleted'). The clipNote loop runs
//!   QueryService.generateVisibilityQuery(me) (public/home, own,
//!   `me <@ visibleUserIds/mentions`, followers-of-author /
//!   followers-reply-to-me) + shouldHideNoteByTime over the author's
//!   makeNotesHiddenBefore. The exportCompleted notification is NOT ported
//!   (the notification_type_enum here predates the value and no acceptance
//!   leg reads it) — documented divergence, parallel to the delete_account
//!   purge-service note.
//! - `endpoints/drive/files.ts` / `files/show.ts` / `files/delete.ts`: the
//!   metadata endpoints (owner-scoped; moderator bypass unported — the
//!   attached_notes precedent). delete drops the blob bytes with the row
//!   (DriveService.deleteFile's storage half).
//!
//! Queue bundling: this rewrite brokers jobs through mk_queue (PG, FOR
//! UPDATE SKIP LOCKED) — spawn_export_worker wires the single `exportClips`
//! kind at boot (main.rs); a claimed job runs the processor inline, the
//! pollFirstDriveFile loop (100ms) racing mk_queue's 500ms poll cadence.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// The endpoint names routed into this module (single source of truth for
/// the arena-exports dispatch arm in mod.rs).
pub(crate) const EXPORT_ENDPOINTS: &[&str] = &[
    "i/export-clips",
    // the drive/files trio the exports.ts beforeEach + pollFirstDriveFile
    // exercise (upstream endpoints/drive/files{,/show,/delete}.ts)
    "drive/files",
    "drive/files/show",
    "drive/files/delete",
];

pub(crate) fn handles(name: &str) -> bool {
    EXPORT_ENDPOINTS.contains(&name)
}

/// Entry from the arena-exports dispatch arm in mod.rs (body pre-parsed,
/// token pre-extracted).
pub(crate) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    match name {
        "i/export-clips" => export_clips(pool, body, token.as_deref()).await,
        "drive/files" => files_list(pool, body, token.as_deref()).await,
        "drive/files/show" => files_show(pool, body, token.as_deref()).await,
        "drive/files/delete" => files_delete(pool, body, token.as_deref()).await,
        _ => api_err(ApiError::unknown_endpoint()),
    }
}

type Check<T = Response> = Result<T, Response>;

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_export", error = %e, "db error: {context}");
    internal()
}

fn meta_err(code: &'static str, id: &'static str, message: &'static str) -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        code,
        id,
        message,
    ))
}

fn schema_invalid(param: &str, reason: &str) -> Response {
    api_err(
        ApiError::new(
            400,
            mk_types::api_error::ApiErrorKind::Client,
            "INVALID_PARAM",
            "3d81ceae-475f-4600-b2a8-2bc116157532",
            "Invalid param.",
        )
        .with_info(json!({
            "param": param,
            "reason": reason,
        })),
    )
}

/// ajv `misskey:id` (endpoint-base.ts L20: plain alnum).
fn id_param<'a>(body: &'a Value, key: &str) -> Result<&'a str, Response> {
    match body.get(key) {
        Some(Value::String(s)) if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric()) => {
            Ok(s.as_str())
        }
        _ => Err(schema_invalid(key, "must be a misskey:id (alnum)")),
    }
}

fn ok_empty() -> Response {
    // upstream `send(reply, undefined)` → 204
    StatusCode::NO_CONTENT.into_response()
}

// ---------------------------------------------------------------------------
// i/export-clips — export-clips.ts: enqueue and answer 204
// ---------------------------------------------------------------------------

async fn export_clips(pool: &mk_db::sqlx::PgPool, body: &Value, token: Option<&str>) -> Response {
    let me_id = match super::require_credential_me_id(pool, token, "dbg_export").await {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    // QueueService.createExportClipsJob: dbQueue.add('exportClips',
    // { type: 'exportClips', user: { id } }). Nothing else rides the params
    // (paramDef {}); foreign fields ignore.
    let _ = body;
    let queue = mk_queue::QueueHandle::new(pool.clone());
    let payload = json!({ "type": "exportClips", "user": { "id": me_id } });
    if let Err(e) = queue.enqueue("exportClips", &payload).await {
        return db_err("export_clips enqueue", e);
    }
    ok_empty()
}

/// Boot wiring (main.rs): the in-process worker for the `exportClips` kind —
/// the PG-broker replacement for upstream's BullMQ db-queue consumer.
pub fn spawn_export_worker(pool: mk_db::sqlx::PgPool, base_url: String) {
    mk_queue::spawn_worker(
        pool.clone(),
        vec!["exportClips".to_string()],
        std::sync::Arc::new(move |_queue, job| {
            let pool = pool.clone();
            let base_url = base_url.clone();
            Box::pin(async move {
                let uid = job
                    .payload
                    .get("user")
                    .and_then(|u| u.get("id"))
                    .and_then(Value::as_str)
                    .map(|s| s.to_string());
                let Some(uid) = uid else {
                    return Err(format!("exportClips job {}: malformed payload", job.id));
                };
                run_export_job(&pool, &base_url, &uid).await
            })
        }),
    );
}

// ---------------------------------------------------------------------------
// the exportClips processor (ExportClipsProcessorService.process)
// ---------------------------------------------------------------------------

/// `generateVisibilityQuery(me)` as a Rust-side filter over the joined
/// clipNote rows: public/home | own | `me <@ visibleUserIds/mentions` |
/// followers-of-author / followers-reply addressed to me (QueryService.ts
/// L270-315; upstream folds the same bracket into the SQL).
fn clip_note_visible_for(
    row: &mk_db::export_blob::ExportClipNoteRow,
    me_id: &str,
    followees: &std::collections::HashSet<String>,
) -> bool {
    if row.visibility == "public" || row.visibility == "home" {
        return true;
    }
    if row.author_id == me_id {
        return true;
    }
    // `:meIdAsList <@ note.visibleUserIds / note.mentions`
    if row.visible_user_ids.iter().any(|u| u == me_id) || row.mentions.iter().any(|u| u == me_id) {
        return true;
    }
    if row.visibility == "followers"
        && (followees.contains(&row.author_id) || row.reply_user_id.as_deref() == Some(me_id))
    {
        return true;
    }
    false
}

/// `misc/should-hide-note-by-time.ts`: negative → relative seconds since
/// creation, positive → absolute unix seconds, null → never.
fn should_hide_note_by_time(hidden_before: Option<i32>, note_created_ms: i64, now_ms: i64) -> bool {
    let Some(hb) = hidden_before else {
        return false;
    };
    if hb <= 0 {
        let elapsed_seconds = (now_ms - note_created_ms) / 1000;
        elapsed_seconds >= hb.unsigned_abs() as i64
    } else {
        note_created_ms / 1000 <= hb as i64
    }
}

/// `dateFormat(new Date(), 'yyyy-MM-dd-HH-mm-ss')` (UTC — the same civil
/// conversion the ms_to_iso helper uses, piped to the dash components).
fn filename_timestamp_ms(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (h, mi, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    // Howard Hinnant's civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}-{h:02}-{mi:02}-{s:02}")
}

/// The full `exportClips` job: read → serialize → addFile. Errors are
/// strings for mk_queue's retry-with-backoff lane.
pub(crate) async fn run_export_job(
    pool: &mk_db::sqlx::PgPool,
    base_url: &str,
    uid: &str,
) -> Result<(), String> {
    // `usersRepository.findOneBy({ id })` — a deleted user silently finishes.
    let Some(_user) = mk_db::user::find_user_by_id(pool, uid)
        .await
        .map_err(|e| e.to_string())?
    else {
        tracing::warn!(target: "dbg_export", user = %uid, "exportClips: user gone; job finished noop");
        return Ok(());
    };

    let clips = mk_db::export_blob::export_clips_asc(pool, uid)
        .await
        .map_err(|e| e.to_string())?;
    let followees: std::collections::HashSet<String> = mk_db::social::followee_ids(pool, uid)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .collect();
    let now_ms = crate::api::now_ms() as i64;

    let mut out: Vec<Value> = Vec::with_capacity(clips.len());
    for clip in &clips {
        let notes = mk_db::export_blob::export_clip_notes(pool, &clip.id)
            .await
            .map_err(|e| e.to_string())?;
        let mut clip_notes: Vec<Value> = Vec::new();
        for row in notes {
            let Some(note_created_ms) = mk_types::id::parse_aid_date(&row.note_id) else {
                continue;
            };
            if !clip_note_visible_for(&row, uid, &followees) {
                continue;
            }
            if should_hide_note_by_time(row.make_notes_hidden_before, note_created_ms, now_ms) {
                continue;
            }
            // OneByOrFail poll (only when the note carries one — undefined
            // drops the JSON key, exactly like upstream's `poll: undefined`).
            let poll = if row.has_poll {
                match mk_db::poll::find_poll_by_note(pool, &row.note_id).await {
                    Ok(Some(p)) => Some(json!({
                        "noteId": p.note_id,
                        "expiresAt": p.expires_at_ms.map(mk_core::services::signup::ms_to_iso),
                        "multiple": p.multiple,
                        "choices": p.choices,
                        "votes": p.votes,
                    })),
                    Ok(None) => None, // dangling hasPoll; upstream fails hard
                    Err(e) => return Err(e.to_string()),
                }
            } else {
                None
            };
            let iso = |ms: Option<i64>| -> Option<String> {
                ms.map(mk_core::services::signup::ms_to_iso)
            };
            let mut note_v = json!({
                "id": row.note_id,
                "text": row.text,
                "createdAt": iso(mk_types::id::parse_aid_date(&row.note_id)),
                "fileIds": row.file_ids,
                "replyId": row.reply_id,
                "renoteId": row.renote_id,
                "cw": row.cw,
                "visibility": row.visibility,
                "visibleUserIds": row.visible_user_ids,
                "localOnly": row.local_only,
                "reactionAcceptance": row.reaction_acceptance,
                "uri": row.note_uri,
                "url": row.note_url,
                "user": {
                    "id": row.author_id,
                    "name": row.author_name,
                    "username": row.username,
                    "host": row.host,
                    "uri": row.author_uri,
                },
            });
            if let Some(p) = poll {
                note_v
                    .as_object_mut()
                    .expect("note_v object")
                    .insert("poll".to_string(), p);
            }
            clip_notes.push(json!({
                "id": row.clip_note_id,
                "createdAt": iso(mk_types::id::parse_aid_date(&row.clip_note_id)),
                "note": note_v,
            }));
        }
        out.push(json!({
            "id": clip.id,
            "name": clip.name,
            "description": clip.description,
            "lastClippedAt": clip.last_clipped_at_ms.map(mk_core::services::signup::ms_to_iso),
            "clipNotes": clip_notes,
        }));
    }

    let bytes = serde_json::to_vec(&Value::Array(out)).map_err(|e| e.to_string())?;
    // DriveService.addFile({ name: 'clips-...json', force: true, ext: 'json' })
    let file_name = format!("clips-{}.json", filename_timestamp_ms(now_ms));
    let access_key = mk_core::services::signup::secure_rndstr(32);
    let url = format!(
        "{}/files/{}/{}",
        base_url.trim_end_matches('/'),
        access_key,
        file_name
    );
    let row = mk_db::drive::DriveFileRow {
        id: mk_types::id::gen_aid_now(),
        user_id: Some(uid.to_string()),
        md5: super::drive::md5_hex(&bytes),
        name: file_name.clone(),
        file_type: "application/json".to_string(),
        size: bytes.len() as i32,
        // non-image uploads carry no blurhash (DriveService only computes it
        // on image mimes) and no webpublic transcode
        blurhash: None,
        url: url.clone(),
        webpublic_url: None,
        webpublic_type: None,
        properties: json!({}),
        comment: None,
        is_sensitive: false,
        folder_id: None,
    };
    mk_db::export_blob::put_blob(pool, &access_key, &bytes)
        .await
        .map_err(|e| e.to_string())?;
    mk_db::drive::insert_drive_file(pool, &row, &access_key)
        .await
        .map_err(|e| e.to_string())?;
    tracing::info!(target: "dbg_export", user = %uid, file = %row.id, "exportClips: exported");
    Ok(())
}

// ---------------------------------------------------------------------------
// /files/{accessKey}/{name} — the byte route (upstream FileServerService)
// ---------------------------------------------------------------------------

/// GET /files/{key}/{name}: stream the stored bytes under the drive row's
/// MIME; a missing blob 404s (the e2e fetches `file.url` from this server).
pub async fn files_serve(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(rest): axum::extract::Path<String>,
) -> Response {
    let Some(pool) = &state.pool else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    // /files/:key/:name — the key is the first segment (upstream serves by
    // key; the name only rides for the content disposition).
    let key = rest.split('/').next().unwrap_or_default();
    if key.is_empty() {
        return StatusCode::NOT_FOUND.into_response();
    }
    match mk_db::export_blob::blob_and_type(pool, key).await {
        Ok(Some((bytes, mime))) => {
            let mut resp = Response::builder()
                .status(StatusCode::OK)
                .header(axum::http::header::CONTENT_TYPE, mime)
                .body(axum::body::Body::from(bytes))
                .expect("static body response");
            if let Some(name) = rest.rsplit('/').next() {
                if let Ok(hv) =
                    axum::http::HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
                {
                    // upstream sets content-disposition
                    resp.headers_mut()
                        .insert(axum::http::header::CONTENT_DISPOSITION, hv);
                }
            }
            resp
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(target: "dbg_export", error = %e, "files_serve blob lookup");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// drive/files — files.ts (owner list; pagination + folderId/type/sort)
// ---------------------------------------------------------------------------

fn pager_ids(body: &Value) -> Check<(Option<String>, Option<String>)> {
    let id_or_date = |key: &str, date_key: &str| -> Result<Option<String>, Response> {
        match body.get(key) {
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(schema_invalid(key, "must be a misskey:id (alnum)")),
            None => Ok(body
                .get(date_key)
                .and_then(|v| v.as_i64())
                .filter(|&ms| ms != 0)
                .map(mk_types::id::gen_aid)),
        }
    };
    Ok((
        id_or_date("sinceId", "sinceDate")?,
        id_or_date("untilId", "untilDate")?,
    ))
}

fn limit_param(body: &Value) -> Result<i64, Response> {
    match body.get("limit") {
        None => Ok(10),
        Some(Value::Number(n)) => {
            let v = n
                .as_i64()
                .ok_or_else(|| schema_invalid("limit", "must be an integer"))?;
            if !(1..=100).contains(&v) {
                return Err(schema_invalid("limit", "must be >= 1 and <= 100"));
            }
            Ok(v)
        }
        Some(_) => Err(schema_invalid("limit", "must be an integer")),
    }
}

async fn files_list(pool: &mk_db::sqlx::PgPool, body: &Value, token: Option<&str>) -> Response {
    let me_id = match super::require_credential_me_id(pool, token, "dbg_export").await {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let limit = match limit_param(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let (since_id, until_id) = match pager_ids(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    // paramDef: `{ nullable: true, default: null }` — explicit null and
    // absence both take the `folderId IS NULL` arm.
    let folder_id = match body.get("folderId") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric()) => {
            Some(s.clone())
        }
        Some(_) => return schema_invalid("folderId", "must be a misskey:id (alnum) or null"),
    };
    let file_type = match body.get("type") {
        None | Some(Value::Null) => None,
        Some(Value::String(s))
            if s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '/' || c == '-' || c == '*') =>
        {
            Some(s.clone())
        }
        Some(_) => return schema_invalid("type", "must be a MIME pattern string or null"),
    };
    let sort = match body.get("sort") {
        None | Some(Value::Null) => None,
        Some(Value::String(s))
            if [
                "+createdAt",
                "-createdAt",
                "+name",
                "-name",
                "+size",
                "-size",
            ]
            .contains(&s.as_str()) =>
        {
            Some(s.clone())
        }
        Some(_) => return schema_invalid("sort", "must be one of±createdAt/±name/±size or null"),
    };
    let rows = match mk_db::export_blob::drive_files_page(
        pool,
        &me_id,
        folder_id.as_deref(),
        file_type.as_deref(),
        since_id.as_deref(),
        until_id.as_deref(),
        sort.as_deref(),
        limit,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return db_err("files_list page", e),
    };
    // packMany(files, { detail: false, self: true }) — every row is the
    // caller's (owner-scoped query), so self-mode url/properties.
    let out: Vec<Value> = rows
        .iter()
        .map(|r| super::drive::pack_drive_file(r, true))
        .collect();
    (StatusCode::OK, Json(json!(out))).into_response()
}

// ---------------------------------------------------------------------------
// drive/files/show — files/show.ts (fileId or url; owner gate)
// ---------------------------------------------------------------------------

async fn files_show(pool: &mk_db::sqlx::PgPool, body: &Value, token: Option<&str>) -> Response {
    let me_id = match super::require_credential_me_id(pool, token, "dbg_export").await {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    // paramDef anyOf: the fileId schema lists first, so a present string id
    // wins even when a url also rides.
    let file_row = if let Some(Value::String(file_id)) = body.get("fileId") {
        if file_id.is_empty() || !file_id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return schema_invalid("fileId", "must be a misskey:id (alnum)");
        }
        match mk_db::drive::find_drive_file(pool, file_id).await {
            Ok(r) => r,
            Err(e) => return db_err("files_show find", e),
        }
    } else if let Some(Value::String(url)) = body.get("url") {
        match mk_db::export_blob::find_file_by_any_url(pool, url).await {
            Ok(r) => r,
            Err(e) => return db_err("files_show by-url", e),
        }
    } else {
        return schema_invalid("fileId", "either fileId or url is required");
    };
    let Some(row) = file_row else {
        return meta_err(
            "NO_SUCH_FILE",
            "067bc436-2718-4795-b0fb-ecbe43949e31",
            "No such file.",
        );
    };
    // `!isModerator(me) && file.userId !== me.id` → ACCESS_DENIED (moderator
    // bypass unported, attached_notes precedent).
    if row.user_id.as_deref() != Some(me_id.as_str()) {
        return meta_err(
            "ACCESS_DENIED",
            "25b73c73-68b1-41d0-bad1-381cfdf6579f",
            "Access denied.",
        );
    }
    // pack(file, { detail: true, withUser: true, self: true })
    (
        StatusCode::OK,
        Json(super::drive::pack_drive_file(&row, true)),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// drive/files/delete — files/delete.ts (owner gate; blob bytes dropped)
// ---------------------------------------------------------------------------

async fn files_delete(pool: &mk_db::sqlx::PgPool, body: &Value, token: Option<&str>) -> Response {
    let me_id = match super::require_credential_me_id(pool, token, "dbg_export").await {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let file_id = match id_param(body, "fileId") {
        Ok(v) => v.to_string(),
        Err(resp) => return resp,
    };
    let row = match mk_db::drive::find_drive_file(pool, &file_id).await {
        Ok(r) => r,
        Err(e) => return db_err("files_delete find", e),
    };
    let Some(row) = row else {
        return meta_err(
            "NO_SUCH_FILE",
            "908939ec-e52b-4458-b395-1025195cea58",
            "No such file.",
        );
    };
    if row.user_id.as_deref() != Some(me_id.as_str()) {
        return meta_err(
            "ACCESS_DENIED",
            "5eb8d909-2540-4970-90b8-dd6f86088121",
            "Access denied.",
        );
    }
    // DriveService.deleteFile: storage half first (the accessKey we stored),
    // then the row; the driveFileDeleted main-stream event is not fanout
    // here (no acceptance leg watches it in this suite).
    match mk_db::export_blob::drive_file_access_key(pool, &file_id).await {
        Ok(Some(key)) => {
            if let Err(e) = mk_db::export_blob::delete_blob(pool, &key).await {
                return db_err("files_delete blob", e);
            }
        }
        Ok(None) => {}
        Err(e) => return db_err("files_delete access_key", e),
    }
    if let Err(e) = mk_db::export_blob::delete_drive_file(pool, &file_id).await {
        return db_err("files_delete row", e);
    }
    ok_empty()
}
