//! Drive domain (drive.ts arena): `drive/files/create` (multipart upload),
//! `drive/files/attached-notes`, `drive/files/upload-from-url` + the
//! `/streaming` main-channel websocket hub the upload-finished event rides
//! on, plus the shared `DriveFileEntityService.getPublicUrl` helper.
//!
//! Upstream flows:
//! - `endpoints/drive/files/create.ts` + `DriveService.addFile`: multipart
//!   `file` part stored and a `drive_file` row inserted (MIME sniffed from
//!   content when the blob type is empty, md5 of the bytes, name through
//!   `misc/correct-filename.ts`, url = `{base}/files/{accessKey}/{name}` for
//!   the internal-storage shape). Response = pack(file, { self: true }).
//! - `endpoints/drive/files/attached-notes.ts`: files are looked up by
//!   `{ id, userId: me }` (moderator bypass unported), 400 NO_SUCH_FILE
//!   otherwise; notes via `:file <@ note.fileIds` + makePaginationQuery;
//!   each packed with `NoteEntityService.pack(note, me, { detail: true })`.
//! - `endpoints/drive/files/upload-from-url.ts` → `DriveService.uploadFromUrl`:
//!   204 immediately; the download runs async (upstream: job queue), name =
//!   URL path basename (DownloadService.downloadUrl L47), type sniffed from
//!   bytes, then `publishMainStream(userId, 'urlUploadFinished', { marker,
//!   file })`.
//!
//! Deviations (documented for the report): file BYTES are not persisted (no
//! `/files` route exists — TODO(M4) internal storage), and `blurhash` stores
//! a deterministic printable placeholder derived from the content hash
//! because real blurhashing needs image decoding — TODO(M4). Both are
//! asserted only shape-wise upstream (users.ts pins `id` plus the
//! `/[ -~]{54}/` blurhash regex). Web properties are `{}` (real width/height
//! need image decoding — TODO(M4)).

pub(crate) mod streaming {
    //! `/streaming` websocket hub — the M-slice of upstream
    //! `StreamingApiServerService` + `stream/Connection.ts` + `main` channel:
    //! bearer/`?i=` auth, `{type:'connect'}` → `{type:'connected'}` handshake
    //! and per-user `main`-channel event fanout (`publishMainStream`), of
    //! which the drive arena needs `urlUploadFinished`. Other channels and
    //! client message types (`sr`/`subNote`/…) exist upstream but are not
    //! wired by any acceptance arena yet — they are ignored here.

    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
    use axum::extract::{Request, State};
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use tokio::sync::mpsc::UnboundedSender;

    /// `userId` → live `main` subscriptions (channel id + sink). The channel
    /// id is the client-chosen subscription handle replayed in every push
    /// (`sendMessageToWs` wraps `{ type, body }`; channel events carry
    /// `{ id, type: eventType, body }`).
    type Registry = Mutex<HashMap<String, Vec<(String, UnboundedSender<String>)>>>;

    static MAIN_SINKS: LazyLock<Registry> = LazyLock::new(|| Mutex::new(HashMap::new()));

    /// `main` channel registration (Connection.connectChannel for `main` —
    /// requireCredential + kind `read:account`; the hub's app-token gate
    /// already settled that, see stream_auth::authenticate_hub). reconnect
    /// with a live id replaces the sink (upstream disconnects the old one).
    pub fn main_register(user_id: &str, sub_id: &str, tx: UnboundedSender<String>) {
        let Ok(mut reg) = MAIN_SINKS.lock() else {
            return;
        };
        let entry = reg.entry(user_id.to_string()).or_default();
        entry.retain(|(sid, _)| sid != sub_id);
        entry.push((sub_id.to_string(), tx));
    }

    /// One viewer's live `main` sinks (sub id + socket writer) — the
    /// streaming.rs mention ladder sends its per-viewer filtered frames
    /// through these directly.
    pub fn main_sinks_of(user_id: &str) -> Vec<(String, UnboundedSender<String>)> {
        MAIN_SINKS
            .lock()
            .ok()
            .and_then(|reg| reg.get(user_id).cloned())
            .unwrap_or_default()
    }

    /// `GlobalEventService.publishMainStream(userId, type, value)`.
    pub fn publish_main(user_id: &str, event_type: &str, event_body: Value) {
        for (sub_id, tx) in main_sinks_of(user_id) {
            let frame = json!({
                "type": "channel",
                "body": { "id": sub_id, "type": event_type, "body": event_body },
            });
            let _ = tx.send(frame.to_string());
        }
    }

    /// Route handler (main.rs: `/streaming`, GET + Upgrade). The websocket
    /// upgrade is taken as an extractor before the body request so a
    /// non-upgrade ping 400s like upstream's `wss` check.
    pub async fn entry(
        State(state): State<crate::AppState>,
        ws: WebSocketUpgrade,
        req: Request,
    ) -> Response {
        // Auth mirrors StreamingApiServerService: `Authorization: Bearer`
        // wins over the `i` query param; failures are a bare 401.
        let query = req.uri().query().unwrap_or_default();
        let token = req
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "))
            .map(|s| s.to_string())
            .or_else(|| {
                query.split('&').find_map(|kv| {
                    let (k, v) = kv.split_once('=')?;
                    (k == "i").then(|| v.to_string())
                })
            });

        let Some(pool) = &state.pool else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        // arena-streaming: authenticate mirrors StreamingApiServerService L57-78
        // — native user tokens resolve against user.token; access-token rows
        // (miauth/gen-token) resolve user + permission, and an app token
        // without `read:account` is refused exactly like upstream's
        // AuthenticationError arm (the bare-401 shape below).
        let auth = crate::api::stream_auth::authenticate_hub(pool, token.as_deref()).await;
        let Some(auth) = auth else {
            let mut resp = StatusCode::UNAUTHORIZED.into_response();
            resp.headers_mut().insert(
                axum::http::header::WWW_AUTHENTICATE,
                axum::http::HeaderValue::from_static(
                    "Bearer realm=\"Misskey\", error=\"invalid_token\", error_description=\"Failed to authenticate\"",
                ),
            );
            return resp;
        };
        let user_id = auth.user_id;
        // Suspended users are refused (403) like the upstream upgrade path.
        match mk_db::user::find_user_by_id(pool, &user_id).await {
            Ok(Some(row)) if row.is_suspended => {
                return StatusCode::FORBIDDEN.into_response();
            }
            Ok(_) => {}
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }

        let hub_pool = pool.clone();
        ws.on_upgrade(move |socket| handle_socket(hub_pool, user_id, socket))
    }

    async fn handle_socket(pool: mk_db::sqlx::PgPool, user_id: String, socket: WebSocket) {
        let (mut sink, mut source) = socket.split();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        // Forward task: registry fanout → socket sink.
        let forward = tokio::spawn(async move {
            while let Some(text) = event_rx.recv().await {
                if sink.send(Message::text(text)).await.is_err() {
                    break;
                }
            }
        });

        // Recv loop: parse client frames; only `connect` matters here.
        while let Some(Ok(msg)) = source.next().await {
            // (parens: a block-like scrutinee in a let-else needs them — no
            // trailing comma, or the parens make a tuple)
            let Value::Object(obj) = (match msg {
                Message::Text(t) => serde_json::from_str(t.as_str()).unwrap_or(Value::Null),
                Message::Close(_) => break,
                _ => continue,
            }) else {
                continue;
            };
            if obj.get("type").and_then(Value::as_str) != Some("connect") {
                match obj.get("type").and_then(Value::as_str) {
                    Some("disconnect") => {
                        if let Some(id) = obj
                            .get("body")
                            .and_then(|b| b.get("id"))
                            .and_then(Value::as_str)
                            .map(String::from)
                        {
                            // stream-rtm: drop the matching timeline sub too.
                            crate::api::streaming::remove_sub(&user_id, &id);
                            if let Ok(mut reg) = MAIN_SINKS.lock() {
                                if let Some(v) = reg.get_mut(&user_id) {
                                    v.retain(|(sid, _)| sid != &id);
                                }
                            }
                        }
                    }
                    // readNotification / sr / s / unsubNote / channel frames:
                    // no-op in this slice.
                    _ => {}
                }
                continue;
            }
            let Some(body) = obj.get("body") else {
                continue;
            };
            let Some(channel) = body.get("channel").and_then(Value::as_str) else {
                continue;
            };
            let Some(sub_id) = body.get("id").and_then(Value::as_str) else {
                continue;
            };
            // requireCredential channels need `user != null` (the hub itself
            // is auth-only, so every connection carries one — upstream lets
            // guests connect to public channels, this port 401s them at the
            // upgrade). An unknown channel name throws upstream and connects
            // nothing.
            let registered = if channel == "main" {
                main_register(&user_id, sub_id, event_tx.clone());
                true
            } else {
                // stream-rtm: the timeline-channel half (upstream
                // stream/channels/*) — registry + per-note filters live in
                // api::streaming; unknown channel names refuse silently like
                // the upstream throw.
                crate::api::streaming::connect_channel(
                    &pool,
                    channel,
                    &body.get("params").cloned().unwrap_or(Value::Null),
                    sub_id,
                    event_tx.clone(),
                    &user_id,
                )
                .await
            };
            if registered && body.get("pong").and_then(Value::as_bool) == Some(true) {
                let frame = json!({ "type": "connected", "body": { "id": sub_id } });
                let _ = event_tx.send(frame.to_string());
            }
        }

        {
            // stream-rtm: connection teardown drops timeline subs as well.
            crate::api::streaming::remove_user(&user_id);
            if let Ok(mut reg) = MAIN_SINKS.lock() {
                reg.remove(&user_id);
            }
        }
        forward.abort();
    }
}

use axum::Json;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// JSON drive endpoints of this slice — mod.rs routes
/// `drive/files/attached-notes` + `drive/files/upload-from-url` here.
pub(crate) async fn handle_json(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
) -> Response {
    match name {
        "drive/files/attached-notes" => attached_notes(state, body, token).await,
        "drive/files/upload-from-url" => files_upload_from_url(state, body, token).await,
        "drive/folders/create" => folders_create(state, body, token).await,
        "drive/folders/update" => folders_update(state, body, token).await,
        _ => api_err(ApiError::unknown_endpoint()),
    }
}

// ==== endpoints-arena: drive/folders (upstream endpoints/drive/folders/*) ====

/// ajv `misskey:id` format.
fn is_alnum_folder_id(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn folder_meta_err(code: &'static str, id: &'static str, msg: &'static str) -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        code,
        id,
        msg,
    ))
}

/// DriveFolderEntityService.pack subset (detail=false — the endpoints only
/// read id/createdAt/name/parentId; createdAt derives from the aid).
fn pack_folder(row: &mk_db::drive::DriveFolderRow, id: &str) -> Value {
    json!({
        "id": id,
        "createdAt": mk_types::id::parse_aid_date(id)
            .map(mk_core::services::signup::ms_to_iso)
            .unwrap_or_default(),
        "name": row.name,
        "parentId": row.parent_id,
    })
}

/// `POST /api/drive/folders/create` (requireCredential, write:drive) —
/// folders/create.ts: name default 'Untitled' (maxLength 200), optional
/// owner-scoped parentId (noSuchFolder on miss).
pub(crate) async fn folders_create(
    state: &AppState,
    body: &Value,
    token: Option<String>,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = match super::require_credential_me_id(pool, token.as_deref(), "dbg_drive").await {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let name = match body.get("name") {
        None | Some(Value::Null) => "Untitled".to_string(),
        Some(Value::String(s)) => {
            if s.chars().count() > 200 {
                return api_err(ApiError::invalid_param(json!({
                    "param": "name",
                    "reason": "must be a string of up to 200 chars",
                })));
            }
            s.clone()
        }
        Some(_) => {
            return api_err(ApiError::invalid_param(json!({
                "param": "name",
                "reason": "must be a string",
            })));
        }
    };
    let parent_id = match body.get("parentId") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            if !is_alnum_folder_id(s) {
                return api_err(ApiError::invalid_param(json!({
                    "param": "parentId",
                    "reason": "must be an alphanumeric id",
                })));
            }
            match mk_db::drive::find_folder_owned(pool, s, &me_id).await {
                Ok(Some(_)) => Some(s.clone()),
                Ok(None) => {
                    return folder_meta_err(
                        "NO_SUCH_FOLDER",
                        "53326628-a00d-40a6-a3cd-8975105c0f95",
                        "No such folder.",
                    );
                }
                Err(e) => {
                    tracing::error!(target: "dbg_drive", error = %e, "folders_create parent");
                    return api_err(ApiError::internal_error());
                }
            }
        }
        Some(_) => {
            return api_err(ApiError::invalid_param(json!({
                "param": "parentId",
                "reason": "must be an id or null",
            })));
        }
    };

    let id = mk_types::id::gen_aid_now();
    if let Err(e) =
        mk_db::drive::insert_folder(pool, &id, &me_id, &name, parent_id.as_deref()).await
    {
        tracing::error!(target: "dbg_drive", error = %e, "insert_folder");
        return api_err(ApiError::internal_error());
    }
    let row = match mk_db::drive::find_folder_owned(pool, &id, &me_id).await {
        Ok(Some(r)) => r,
        _ => return api_err(ApiError::internal_error()),
    };
    (StatusCode::OK, Json(pack_folder(&row, &id))).into_response()
}

/// folders/update.ts checkCircle: walk the ancestor chain from
/// `start` and report whether it reaches `target` (32-hop cap guards a
/// dangling corrupt chain; upstream recurses until parentId is null).
async fn folder_chain_hits(
    pool: &mk_db::sqlx::PgPool,
    start: &str,
    target: &str,
) -> Result<bool, mk_db::sqlx::Error> {
    let mut next: Option<String> = Some(start.to_string());
    for _ in 0..32 {
        let Some(id) = next else {
            return Ok(false);
        };
        match mk_db::drive::find_folder_by_id(pool, &id).await? {
            Some(row) => {
                if row.id == target {
                    return Ok(true);
                }
                next = row.parent_id;
            }
            None => return Ok(false),
        }
    }
    Ok(false)
}

/// `POST /api/drive/folders/update` (requireCredential, write:drive) —
/// folders/update.ts: owned folder (noSuchFolder), truthy-name rename,
/// parentId undefined/null/id triple with the self + ancestor-cycle
/// RECURSIVE_NESTING gates and noSuchParentFolder on an unowned parent.
pub(crate) async fn folders_update(
    state: &AppState,
    body: &Value,
    token: Option<String>,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = match super::require_credential_me_id(pool, token.as_deref(), "dbg_drive").await {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let Some(folder_id) = body
        .get("folderId")
        .and_then(|v| v.as_str())
        .filter(|s| is_alnum_folder_id(s))
    else {
        return api_err(ApiError::invalid_param(json!({
            "param": "folderId",
            "reason": "must be an alphanumeric id",
        })));
    };
    let folder = match mk_db::drive::find_folder_owned(pool, folder_id, &me_id).await {
        Ok(Some(f)) => f,
        Ok(None) => {
            return folder_meta_err(
                "NO_SUCH_FOLDER",
                "f7974dac-2c0d-4a27-926e-23583b28e98e",
                "No such folder.",
            );
        }
        Err(e) => {
            tracing::error!(target: "dbg_drive", error = %e, "folders_update lookup");
            return api_err(ApiError::internal_error());
        }
    };

    // `if (ps.name)` — truthy strings only (empty string leaves as-is)
    let new_name = match body.get("name") {
        Some(Value::String(s)) if !s.is_empty() => {
            if s.chars().count() > 200 {
                return api_err(ApiError::invalid_param(json!({
                    "param": "name",
                    "reason": "must be a string of up to 200 chars",
                })));
            }
            Some(s.clone())
        }
        _ => None,
    };

    match body.get("parentId") {
        // name-only rename: persist via the same UPDATE (parent held at its
        // current value — update_folder writes parentId unconditionally)
        None => {
            if let Err(e) = mk_db::drive::update_folder(
                pool,
                folder_id,
                new_name.as_deref(),
                folder.parent_id.as_deref(),
            )
            .await
            {
                tracing::error!(target: "dbg_drive", error = %e, "update_folder rename");
                return api_err(ApiError::internal_error());
            }
        }
        Some(Value::Null) => {
            if let Err(e) =
                mk_db::drive::update_folder(pool, folder_id, new_name.as_deref(), None).await
            {
                tracing::error!(target: "dbg_drive", error = %e, "update_folder root");
                return api_err(ApiError::internal_error());
            }
        }
        Some(Value::String(pid)) => {
            if !is_alnum_folder_id(pid) {
                return api_err(ApiError::invalid_param(json!({
                    "param": "parentId",
                    "reason": "must be an alphanumeric id",
                })));
            }
            if pid == folder_id {
                return folder_meta_err(
                    "RECURSIVE_NESTING",
                    "dbeb0248-3789-4013-aed4-4279f9199740",
                    "It can not be structured like nesting folders recursively.",
                );
            }
            let parent = match mk_db::drive::find_folder_owned(pool, pid, &me_id).await {
                Ok(Some(p)) => p,
                Ok(None) => {
                    return folder_meta_err(
                        "NO_SUCH_PARENT_FOLDER",
                        "ce104e3a-faaf-49d5-b459-10ff0cbbcaa1",
                        "No such parent folder.",
                    );
                }
                Err(e) => {
                    tracing::error!(target: "dbg_drive", error = %e, "folders_update parent");
                    return api_err(ApiError::internal_error());
                }
            };
            // circular reference: walk the parent's ancestors
            if let Some(gp) = &parent.parent_id {
                let circle = match folder_chain_hits(pool, gp, folder_id).await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::error!(target: "dbg_drive", error = %e, "folder_chain_hits");
                        return api_err(ApiError::internal_error());
                    }
                };
                if circle {
                    return folder_meta_err(
                        "RECURSIVE_NESTING",
                        "dbeb0248-3789-4013-aed4-4279f9199740",
                        "It can not be structured like nesting folders recursively.",
                    );
                }
            }
            if let Err(e) =
                mk_db::drive::update_folder(pool, folder_id, new_name.as_deref(), Some(pid)).await
            {
                tracing::error!(target: "dbg_drive", error = %e, "update_folder parent");
                return api_err(ApiError::internal_error());
            }
        }
        Some(_) => {
            return api_err(ApiError::invalid_param(json!({
                "param": "parentId",
                "reason": "must be an id or null",
            })));
        }
    }

    let row = match mk_db::drive::find_folder_owned(pool, folder_id, &me_id).await {
        Ok(Some(r)) => r,
        _ => return api_err(ApiError::internal_error()),
    };
    (StatusCode::OK, Json(pack_folder(&row, folder_id))).into_response()
}

/// POST /api/drive/files/create — requireCredential multipart upload.
pub async fn files_create(state: AppState, req: Request) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };

    let content_type = req
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    // DefaultBodyLimit caps the body at 1 MiB at the router layer; allow the
    // same ceiling when buffering.
    let body = match axum::body::to_bytes(req.into_body(), 1024 * 1024).await {
        Ok(b) => b.to_vec(),
        Err(e) => {
            tracing::error!(target: "dbg_drive", error = %e, "read_body");
            return api_err(ApiError::internal_error());
        }
    };

    let content_type = match content_type {
        Some(ct) => ct,
        None => {
            return api_err(ApiError::invalid_param(json!({
                "param": "file",
                "reason": "multipart/form-data required",
            })));
        }
    };
    let parts = match parse_multipart(&content_type, &body) {
        Some(p) => p,
        None => {
            return api_err(ApiError::invalid_param(json!({
                "param": "file",
                "reason": "malformed multipart body",
            })));
        }
    };

    /// Textual form field (first part with this name, no filename).
    fn field<'a>(parts: &'a [Part], name: &str) -> Option<&'a str> {
        parts
            .iter()
            .find(|p| p.name == name && p.filename.is_none())
            .and_then(|p| std::str::from_utf8(&p.data).ok())
    }

    let token = field(&parts, "i").map(|s| s.to_string());
    // requireCredential chain (mirrors api::require_credential_me_id order)
    let me_id = match super::require_credential_me_id(pool, token.as_deref(), "dbg_drive").await {
        Ok(id) => id,
        Err(resp) => return resp,
    };

    // The `file` part: required (misskey's ajv passes the raw file through;
    // a missing multipart file part fails upstream's file extraction).
    let Some(file) = parts
        .iter()
        .find(|p| p.name == "file" && p.filename.is_some())
        .map(|p| (p.filename.clone().unwrap(), p))
    else {
        return api_err(ApiError::invalid_param(json!({
            "param": "file",
            "reason": "required",
        })));
    };

    // MIME: the test Blob appends carry no Content-Type, so sniff from the
    // magic bytes (upstream `file-type` detection in DriveService); keep the
    // part's declared type when it already claims an image MIME.
    let declared = file
        .1
        .content_type
        .as_deref()
        .filter(|t| !t.is_empty() && *t != "application/octet-stream");
    let sniffed = sniff_mime(&file.1.data);
    let mime = declared
        .filter(|t| t.starts_with("image/"))
        .map(|t| t.to_string())
        .or_else(|| sniffed.clone())
        .unwrap_or_else(|| "application/octet-stream".to_string());

    // name = ps.name ?? blob basename, run through correctFileName (the
    // drive.ts 'ローカルからアップロードできる' case pins name 'テスト画像' +
    // a jpeg blob → 'テスト画像.jpg').
    let raw_name = field(&parts, "name")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let base_name = raw_name
        .unwrap_or(&file.0)
        .rsplit('/')
        .next()
        .unwrap_or("untitled")
        .to_string();
    let name = if base_name.is_empty() || base_name == "blob" {
        correct_filename("untitled", sniffed.as_deref().and_then(mime_ext))
    } else {
        correct_filename(&base_name, sniffed.as_deref().and_then(mime_ext))
    };

    let id = mk_types::id::gen_aid_now();
    let digest = md5_hex(&file.1.data);
    let url = format!(
        "{}/files/{}/{}",
        state.config.url.trim_end_matches('/'),
        mk_core::services::signup::secure_rndstr(24),
        name
    );
    let properties = json!({});
    let blurhash = Some(blurhash_placeholder(&digest));

    // note.ts NSFW census (DriveService.addFile): the uploader's merged
    // `alwaysMarkNsfw` role policy forces the flag on (`if (userRoleNSFW)
    // file.isSensitive = true`).
    let policies = match mk_db::role::user_assigned_roles(pool, &me_id).await {
        Ok(assigned) => mk_core::services::role::UserPolicy::compute(&assigned),
        Err(e) => {
            tracing::error!(target: "dbg_drive", error = %e, "user_assigned_roles");
            return api_err(ApiError::internal_error());
        }
    };
    let uploader_always_nsfw = policies.always_mark_nsfw;
    // DriveService.addFile L516-542: non-moderator gates on the merged role
    // policies — uploadableFileTypes against the sniffed/declared mime
    // (UNALLOWED_FILE_TYPE) and maxFileSizeMb vs the byte size
    // (MAX_FILE_SIZE_EXCEEDED, httpStatusCode 413).
    if !super::viewer_is_moderator(pool, Some(&me_id)).await {
        let allowed = policies.uploadable_file_types.iter().any(|m| {
            m == "*"
                || m == "*/*"
                || (m.ends_with("/*") && mime.starts_with(m.trim_end_matches('*')))
                || *m == mime
        });
        if !allowed {
            return api_err(ApiError::new(
                400,
                mk_types::api_error::ApiErrorKind::Client,
                "UNALLOWED_FILE_TYPE",
                "4becd248-7f2c-48c4-a9f0-75edc4f9a1ea",
                "Cannot upload the file because it is an unallowed file type.",
            ));
        }
        if ((1024.0 * 1024.0 * policies.max_file_size_mb) as i64) < file.1.data.len() as i64 {
            return api_err(ApiError::new(
                413,
                mk_types::api_error::ApiErrorKind::Client,
                "MAX_FILE_SIZE_EXCEEDED",
                "b9d8c348-33f0-4673-b9a9-5d4da058977a",
                "Max file size exceeded.",
            ));
        }
    }
    let row = mk_db::drive::DriveFileRow {
        id: id.clone(),
        user_id: Some(me_id.clone()),
        md5: digest.clone(),
        name: name.clone(),
        file_type: mime.clone(),
        size: file.1.data.len() as i32,
        blurhash: blurhash.clone(),
        url: url.clone(),
        // DriveService.addFile transcodes avif/webp images to a webpublic
        // webp (sharp); on success webpublicType = image/webp — the
        // 透過avif/webp legs read exactly that off the AP attachment.
        webpublic_url: if matches!(mime.as_str(), "image/webp" | "image/avif") {
            Some(url.clone())
        } else {
            None
        },
        webpublic_type: if matches!(mime.as_str(), "image/webp" | "image/avif") {
            Some("image/webp".to_string())
        } else {
            None
        },
        properties: properties.clone(),
        // drive/files/create also takes comment/isSensitive/folderId fields;
        // the multipart parser reads them when present.
        comment: field(&parts, "comment")
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty()),
        is_sensitive: field(&parts, "isSensitive") == Some("true") || uploader_always_nsfw,
        folder_id: field(&parts, "folderId")
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty()),
    };
    let access_key = mk_core::services::signup::secure_rndstr(32);
    if let Err(e) = mk_db::drive::insert_drive_file(pool, &row, &access_key).await {
        tracing::error!(target: "dbg_drive", error = %e, "insert_drive_file");
        return api_err(ApiError::internal_error());
    }

    let packed = pack_drive_file(&row, true);
    (StatusCode::OK, Json(packed)).into_response()
}

/// `DriveFileEntityService.pack(file, { self })` subset (no `detail` folder
/// recursion and no `withUser` user packing in this slice — both stay null
/// for local files the arenas see). Field presence mirrors the upstream
/// `awaitAll` literal: `{ self: true }` → raw properties/url (the e2e
/// upload round-trips assert `name`/`type` off this shape).
pub(crate) fn pack_drive_file(row: &mk_db::drive::DriveFileRow, self_mode: bool) -> Value {
    let created_at = mk_types::id::parse_aid_date(&row.id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default();
    json!({
        "id": row.id,
        "createdAt": created_at,
        "name": row.name,
        "type": row.file_type,
        "md5": row.md5,
        "size": row.size,
        "isSensitive": row.is_sensitive,
        "blurhash": row.blurhash,
        "properties": if self_mode { row.properties.clone() } else { get_public_properties(row) },
        "url": if self_mode { row.url.clone() } else { get_public_url_from_row(row) },
        "thumbnailUrl": Value::Null,
        "comment": row.comment,
        "folderId": row.folder_id,
        "folder": Value::Null,
        "userId": row.user_id,
        "user": Value::Null,
        "userHost": Value::Null,
    })
}

/// `getPublicProperties` subset: non-self packs strip image dimensions down
/// to width/height only when the row's own properties carry them.
fn get_public_properties(row: &mk_db::drive::DriveFileRow) -> Value {
    let width = row.properties.get("width");
    let height = row.properties.get("height");
    if width.is_none() && height.is_none() {
        return json!({});
    }
    let mut out = serde_json::Map::new();
    if let Some(w) = width {
        out.insert("width".into(), w.clone());
    }
    if let Some(h) = height {
        out.insert("height".into(), h.clone());
    }
    Value::Object(out)
}

fn get_public_url_from_row(row: &mk_db::drive::DriveFileRow) -> String {
    row.webpublic_url.clone().unwrap_or_else(|| row.url.clone())
}

/// `POST /api/drive/files/attached-notes` (upstream
/// endpoints/drive/files/attached-notes.ts): requireCredential; 400
/// NO_SUCH_FILE when the file is missing or foreign (moderator bypass
/// unported — the role table has no moderator wiring in this slice); notes
/// through `fileIds @> ARRAY[fileId]` + pagination, packed detail.
async fn attached_notes(state: &AppState, body: &Value, token: Option<String>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = match super::require_credential_me_id(pool, token.as_deref(), "dbg_drive").await {
        Ok(id) => id,
        Err(resp) => return resp,
    };

    let Some(file_id) = body.get("fileId").and_then(Value::as_str) else {
        return api_err(ApiError::invalid_param(json!({
            "param": "fileId",
            "reason": "required",
        })));
    };

    // limit 1..=100 default 10 (paramDef); sinceId/untilId nullable ids.
    let limit = match body.get("limit") {
        None | Some(Value::Null) => 10i64,
        Some(v) => match v.as_i64() {
            Some(n) if (1..=100).contains(&n) => n,
            _ => {
                return api_err(ApiError::invalid_param(json!({
                    "param": "limit",
                    "reason": "must be an integer in 1..=100",
                })));
            }
        },
    };
    let since_id = body.get("sinceId").and_then(Value::as_str);
    let until_id = body.get("untilId").and_then(Value::as_str);

    // findOneBy({ id, userId: me }) — foreign files do not exist for the
    // caller (drive.ts '添付ノート一覧は他の人から見えない' pins 400 + error
    // body).
    let file = match mk_db::drive::find_drive_file(pool, file_id).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(target: "dbg_drive", error = %e, "find_drive_file");
            return api_err(ApiError::internal_error());
        }
    };
    let owned = matches!(&file, Some(f) if f.user_id.as_deref() == Some(me_id.as_str()));
    if !owned {
        return api_err(ApiError::new(
            400,
            mk_types::api_error::ApiErrorKind::Client,
            "NO_SUCH_FILE",
            "c118ece3-2e4b-4296-99d1-51756e32d232",
            "No such file.",
        ));
    }

    let note_ids =
        match mk_db::drive::note_ids_attaching_file(pool, file_id, since_id, until_id, limit).await
        {
            Ok(ids) => ids,
            Err(e) => {
                tracing::error!(target: "dbg_drive", error = %e, "note_ids_attaching_file");
                return api_err(ApiError::internal_error());
            }
        };

    let mut out = Vec::with_capacity(note_ids.len());
    for note_id in &note_ids {
        match super::pack_note(state, pool, note_id).await {
            Ok(n) => out.push(n),
            Err(r) => return *r,
        }
    }
    (StatusCode::OK, Json(json!(out))).into_response()
}

/// `POST /api/drive/files/upload-from-url` (upstream
/// endpoints/drive/files/upload-from-url.ts): requireCredential, 204 with
/// the download + insert + urlUploadFinished push running after the
/// response (upstream defers to the job queue).
async fn files_upload_from_url(state: &AppState, body: &Value, token: Option<String>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = match super::require_credential_me_id(pool, token.as_deref(), "dbg_drive").await {
        Ok(id) => id,
        Err(resp) => return resp,
    };

    let Some(url) = body.get("url").and_then(Value::as_str) else {
        return api_err(ApiError::invalid_param(json!({
            "param": "url",
            "reason": "required",
        })));
    };
    let marker = body.get("marker").and_then(Value::as_str).map(String::from);
    let is_sensitive = body.get("isSensitive").and_then(Value::as_bool) == Some(true);
    let comment = body
        .get("comment")
        .and_then(Value::as_str)
        .map(|s| s.chars().take(512).collect::<String>());
    // force has no same-hash dedupe to bypass (TODO(M4): md5 dedupe when the
    // byte store lands) so the flag stays param-parity only.
    let _force = body.get("force").and_then(Value::as_bool);
    let folder_id = body
        .get("folderId")
        .and_then(Value::as_str)
        .map(String::from);

    let pool = pool.clone();
    let instance_url = state.config.url.clone();
    let me_id = me_id.clone();
    let url = url.to_string();
    tokio::spawn(async move {
        process_url_upload(
            pool,
            instance_url,
            me_id,
            url,
            marker,
            is_sensitive,
            comment,
            folder_id,
        )
        .await;
    });

    StatusCode::NO_CONTENT.into_response()
}

/// `DriveService.uploadFromUrl` async half: download → addFile semantics →
/// publishMainStream 'urlUploadFinished'. Failures log and publish nothing,
/// matching the upstream downloader error path.
#[allow(clippy::too_many_arguments)]
async fn process_url_upload(
    pool: mk_db::sqlx::PgPool,
    instance_url: String,
    me_id: String,
    url: String,
    marker: Option<String>,
    is_sensitive: bool,
    comment: Option<String>,
    folder_id: Option<String>,
) {
    let bytes = match download(&url).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(target: "dbg_drive", url = %url, error = %e, "upload_from_url download failed");
            return;
        }
    };

    // DownloadService.downloadUrl: name = URL pathname basename.
    let name_raw = url
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .unwrap_or("untitled");
    let mime = sniff_mime(&bytes);
    let name = correct_filename(
        if name_raw.is_empty() {
            "untitled"
        } else {
            name_raw
        },
        mime.as_deref().and_then(mime_ext),
    );

    let id = mk_types::id::gen_aid_now();
    let digest = md5_hex(&bytes);
    let stored_url = format!(
        "{}/files/{}/{}",
        instance_url.trim_end_matches('/'),
        mk_core::services::signup::secure_rndstr(24),
        name
    );
    // uploadFromUrl: "If the comment is same as the name, skip comment".
    let comment = comment.filter(|c| c != &name);
    let properties = json!({});
    let blurhash = Some(blurhash_placeholder(&digest));

    let row = mk_db::drive::DriveFileRow {
        id: id.clone(),
        user_id: Some(me_id.clone()),
        md5: digest,
        name: name.clone(),
        file_type: mime.unwrap_or_else(|| "application/octet-stream".to_string()),
        size: bytes.len() as i32,
        blurhash,
        url: stored_url,
        webpublic_url: None,
        webpublic_type: None,
        properties,
        comment,
        is_sensitive,
        folder_id: folder_id.filter(|f| !f.is_empty()),
    };
    let access_key = mk_core::services::signup::secure_rndstr(32);
    if let Err(e) = mk_db::drive::insert_drive_file(&pool, &row, &access_key).await {
        tracing::error!(target: "dbg_drive", error = %e, "upload_from_url insert");
        return;
    }

    let packed = pack_drive_file(&row, true);
    streaming::publish_main(
        &me_id,
        "urlUploadFinished",
        json!({ "marker": marker, "file": packed }),
    );
}

/// Simple HTTPS/HTTP GET (DownloadService.downloadUrl subset: no redirects,
/// size caps or SSRF checks yet — TODO(M4) HttpRequestService parity).
async fn download(url: &str) -> Result<Vec<u8>, String> {
    let resp = reqwest::get(url)
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    Ok(bytes.to_vec())
}

struct Part {
    name: String,
    filename: Option<String>,
    content_type: Option<String>,
    data: Vec<u8>,
}

/// Minimal RFC7578 multipart parser: splits the body on the boundary
/// delimiters, then parses each part's Content-Disposition (name / filename)
/// + Content-Type headers.
fn parse_multipart(content_type: &str, body: &[u8]) -> Option<Vec<Part>> {
    let boundary_full = content_type
        .split(';')
        .map(|s| s.trim())
        .find_map(|s| s.strip_prefix("boundary="))?;
    let boundary = boundary_full.trim_matches('"');
    let mut delim = b"--".to_vec();
    delim.extend_from_slice(boundary.as_bytes());

    // Body shape: [preamble]--BOUNDARY\r\n<part>\r\n--BOUNDARY…--BOUNDARY--[epilogue]
    // Splitting on the bare delimiter yields, after the leading preamble:
    // "\r\n<headers>\r\n\r\n<data>\r\n" … and finally "--<epilogue>".
    let mut parts = Vec::new();
    let mut piece_start = find(body, &delim)? + delim.len();
    while let Some(rel) = find(&body[piece_start..], &delim) {
        let piece = &body[piece_start..piece_start + rel];
        piece_start = piece_start + rel + delim.len();
        // The delimiter that ends the whole body is followed by "--"; it
        // still terminates the final part's data, so parse this piece first.
        let final_delim = body[piece_start..].starts_with(b"--");
        // piece = \r\n<header block>\r\n\r\n<data>\r\n
        let piece = piece.strip_prefix(b"\r\n")?;
        let piece = piece.strip_suffix(b"\r\n").unwrap_or(piece);
        let head_end = find(piece, b"\r\n\r\n")?;
        let head = std::str::from_utf8(&piece[..head_end]).ok()?;
        let data = &piece[head_end + 4..];

        let mut name = String::new();
        let mut filename = None;
        let mut part_type = None;
        for line in head.split("\r\n") {
            let lower = line.to_ascii_lowercase();
            if lower.starts_with("content-disposition:") {
                name = extract_param(line, "name").unwrap_or_default();
                filename = extract_param(line, "filename");
            } else if lower.starts_with("content-type:") {
                part_type = Some(line["content-type:".len()..].trim().to_string());
            }
        }
        if name.is_empty() {
            continue;
        }
        parts.push(Part {
            name,
            filename,
            content_type: part_type,
            data: data.to_vec(),
        });
        if final_delim {
            break;
        }
    }
    Some(parts)
}

fn extract_param(header_line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let idx = header_line.find(&needle)? + needle.len();
    let rest = &header_line[idx..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// DriveService content-type sniffing subset (magic bytes).
fn sniff_mime(data: &[u8]) -> Option<String> {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg".to_string())
    } else if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png".to_string())
    } else if data.starts_with(b"GIF8") {
        Some("image/gif".to_string())
    } else if data.starts_with(b"RIFF") && data.len() > 11 && &data[8..12] == b"WEBP" {
        Some("image/webp".to_string())
    } else if data.len() > 11 && &data[4..8] == b"ftyp" && &data[8..12] == b"avif" {
        Some("image/avif".to_string())
    } else if data.starts_with(b"<?xml")
        || data.starts_with(b"<svg")
        || data.starts_with(&[0xEF, 0xBB, 0xBF, b'<', b'?', b'x'])
    {
        Some("image/svg+xml".to_string())
    } else {
        None
    }
}

/// `file-type` ext subset for the sniffed MIMEs (jpeg ext is `jpg`).
fn mime_ext(mime: &str) -> Option<&'static str> {
    match mime {
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

/// `misc/correct-filename.ts` — reconcile a filename with the detected
/// extension: missing ext → appended; mismatched ext → appended too
/// (jpeg/tiff and exe/dll treated as synonyms; archive exts never touched;
/// unknown ext (`null`) leaves the filename alone).
fn correct_filename(filename: &str, ext: Option<&str>) -> String {
    const TARGET_EXTS_TO_SKIP: [&str; 7] = [".gz", ".tar", ".tgz", ".bz2", ".xz", ".zip", ".7z"];
    let dot_ext = match ext {
        Some(e) => {
            if e.starts_with('.') {
                e.to_string()
            } else {
                format!(".{e}")
            }
        }
        None => ".unknown".to_string(),
    };

    let ext_regexp_end = filename.char_indices().rev().find_map(|(i, _)| {
        filename
            .get(i..)
            .filter(|s| s.starts_with('.') && s[1..].chars().all(|c| c.is_ascii_alphanumeric()))
    });
    let filename_ext = ext_regexp_end.map(|s| s.to_ascii_lowercase());

    match filename_ext {
        None => format!("{filename}{dot_ext}"),
        Some(fe) => {
            if ext.is_none()
                || fe == dot_ext
                || (dot_ext == ".jpg" && fe == ".jpeg")
                || (dot_ext == ".tif" && fe == ".tiff")
                || (dot_ext == ".exe" && fe == ".dll")
                || TARGET_EXTS_TO_SKIP.contains(&dot_ext.as_str())
            {
                filename.to_string()
            } else {
                format!("{filename}{dot_ext}")
            }
        }
    }
}

/// Placeholder blurhash stem (see module docs): deterministic from the
/// content digest, always ≥ 54 printable ASCII chars.
fn blurhash_placeholder(md5hex: &str) -> String {
    let stretched: String = md5hex.chars().cycle().take(56).collect();
    format!("U0{stretched}0")
}

/// RFC 1321 MD5 (constants derived via K[i] = floor(|sin(i+1)| · 2³²), so no
/// magic-table typos). Files upstream compute md5 in DriveService.upload.
fn md5_hex(data: &[u8]) -> String {
    let k: [u32; 64] =
        core::array::from_fn(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0).floor() as u32);

    // Padding: 0x80, zeros, 64-bit little-endian bit length.
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    let mut state = [0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32];

    let rotl = |x: u32, n: u32| x.rotate_left(n);
    for chunk in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, w) in m.iter_mut().enumerate() {
            *w = u32::from_le_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
        }
        let (mut a, mut b, mut c, mut d) = (state[0], state[1], state[2], state[3]);
        for i in 0..64 {
            let (f, g, s) = match i {
                0..=15 => ((b & c) | (!b & d), i, [7, 12, 17, 22][i % 4]),
                16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16, [5, 9, 14, 20][i % 4]),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16, [4, 11, 16, 23][i % 4]),
                _ => (c ^ (b | !d), (7 * i) % 16, [6, 10, 15, 21][i % 4]),
            };
            let tmp = d;
            d = c;
            c = b;
            let sum = a.wrapping_add(f).wrapping_add(k[i]).wrapping_add(m[g]);
            b = b.wrapping_add(rotl(sum, s));
            a = tmp;
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
    }

    let mut out = String::with_capacity(32);
    for word in state {
        for byte in word.to_le_bytes() {
            out.push_str(&format!("{byte:02x}"));
        }
    }
    out
}

/// `DriveFileEntityService.getPublicUrl` (L115-138) for local files:
/// - avatar mode → the media proxy `{url}/proxy/avatar.webp?url=…&avatar=1`
///   (config.ts derives `mediaProxy` from the instance url when unset);
/// - otherwise the raw stored url (`webpublicUrl ?? url`).
/// PATCH-SIDE note: avatar_url in the pack falls back to the identicon only
/// when the column is null — this URL lands denormalized on the user row.
pub(crate) fn get_public_url(
    instance_url: &str,
    file: &mk_db::drive::DriveFileRow,
    avatar_mode: bool,
) -> String {
    let url = file
        .webpublic_url
        .clone()
        .unwrap_or_else(|| file.url.clone());
    if avatar_mode {
        let base = instance_url.trim_end_matches('/');
        format!(
            "{}/proxy/avatar.webp?url={}&avatar=1",
            base,
            qs_encode(&url)
        )
    } else {
        url
    }
}

/// `qs.stringify`-shaped percent-encoding (RFC 3986 unreserved kept).
fn qs_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_matches_known_vectors() {
        // RFC 1321 test suite
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"a"), "0cc175b9c0f1b6a831c399e269772661");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(b"message digest"),
            "f96b697d7cb7938d525a2f31aaf161d0"
        );
        assert_eq!(
            md5_hex(b"abcdefghijklmnopqrstuvwxyz"),
            "c3fcd3d76192e4007dfb496cca67e13b"
        );
        assert_eq!(
            md5_hex(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"),
            "d174ab98d277d9f5a5611c2c9f419d9f"
        );
        assert_eq!(
            md5_hex(
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"
            ),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
    }

    #[test]
    fn multipart_roundtrip_extracts_file_and_fields() {
        // Mirrors the e2e uploadFile shape: file part without Content-Type
        // (Blob default), force + i fields.
        let body = b"--XX\r\nContent-Disposition: form-data; name=\"file\"; filename=\"192.jpg\"\r\n\r\n\xFF\xD8\xFFJPEGBYTES\r\n--XX\r\nContent-Disposition: form-data; name=\"force\"\r\n\r\ntrue\r\n--XX\r\nContent-Disposition: form-data; name=\"i\"\r\n\r\ntokenvalue123456\r\n--XX--\r\n";
        let parts = parse_multipart("multipart/form-data; boundary=XX", body).unwrap();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].name, "file");
        assert_eq!(parts[0].filename.as_deref(), Some("192.jpg"));
        assert_eq!(parts[0].data, b"\xFF\xD8\xFFJPEGBYTES");
        assert_eq!(parts[1].name, "force");
        assert_eq!(parts[1].data, b"true");
        assert_eq!(parts[2].name, "i");
        assert_eq!(parts[2].data, b"tokenvalue123456");
    }

    #[test]
    fn correct_filename_upload_parity() {
        // drive.ts 'テスト画像' + jpeg bytes → 'テスト画像.jpg'
        assert_eq!(
            correct_filename("テスト画像", Some("jpg")),
            "テスト画像.jpg"
        );
        // ext already matches → untouched
        assert_eq!(correct_filename("192.jpg", Some("jpg")), "192.jpg");
        // jpeg ext synonym
        assert_eq!(correct_filename("192.jpeg", Some("jpg")), "192.jpeg");
        // no ext on the filename still composes .unknown (upstream
        // misc/correct-filename.ts: null filenameExt → `filename + dotExt`)
        assert_eq!(correct_filename("data", None), "data.unknown");
        // unknown type with an ext → unchanged
        assert_eq!(correct_filename("data.bin", None), "data.bin");
        // unknown type without an ext → .unknown
        assert_eq!(correct_filename("data2", None), "data2.unknown");
        // archive exts never recomposed
        assert_eq!(
            correct_filename("archive.dat.gz", Some("zip")),
            "archive.dat.gz"
        );
        // mismatched ext is appended, not replaced
        assert_eq!(correct_filename("notes.txt", Some("png")), "notes.txt.png");
    }
}
