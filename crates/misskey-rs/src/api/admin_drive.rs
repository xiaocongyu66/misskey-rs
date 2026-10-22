//! `admin/drive/*` + `admin/delete-all-files-of-a-user` — ports of upstream
//! `endpoints/admin/drive/{files,show-file,cleanup,clean-remote-files}.ts`
//! and `endpoints/admin/delete-all-files-of-a-user.ts`. SQL lives in
//! `mk_db::drive` (the `drive_file` table ships in the 0001 baseline; no
//! migration).
//!
//! Gates: `admin/drive/{files,show-file,cleanup,clean-remote-files}` declare
//! `requireModerator`; `admin/delete-all-files-of-a-user` declares
//! `requireAdmin`. The shared `admin::gate` runs the credential → suspended →
//! role chain, so the non-admin shapes match the other admin arms.
//!
//! DOCUMENTED DEVIATIONS:
//! - `clean-remote-files` upstream queues `createCleanRemoteFilesJob`; this
//!   tree has no job lane for it, so the `userHost IS NOT NULL` delete runs
//!   inline.
//! - `cleanup` upstream deletes every `userId IS NULL` row; the slice plan
//!   asks for the safest query instead: no `note.fileIds` reference and no
//!   `user.avatarId`/`bannerId` reference (see `admin_cleanup_orphans`).
//! - `show-file` returns the flat file object plus a `user` member (mission
//!   shape); upstream returns the file object alone. The viewer is always a
//!   moderator here, so `requestIp` is exposed and `requestHeaders` is
//!   exposed without the owner-moderator suppression check.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

use super::admin::{Gate, gate};

/// Endpoint names owned by this module (all POST-only).
pub(super) fn handles(name: &str) -> bool {
    matches!(
        name,
        "admin/drive/files"
            | "admin/drive/show-file"
            | "admin/drive/cleanup"
            | "admin/drive/clean-remote-files"
            | "admin/delete-all-files-of-a-user"
    )
}

type Check<T = Response> = Result<T, Response>;

/// Entry from the admin-drive dispatch arm in mod.rs (body pre-parsed, token
/// pre-extracted).
pub(super) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<&str>,
) -> Response {
    let result: Check<Response> = match name {
        "admin/delete-all-files-of-a-user" => {
            // Upstream requireAdmin (not requireModerator).
            if let Err(resp) = gate(state, token, Gate::Admin).await {
                return resp;
            }
            delete_all_files_of_a_user(state, body).await
        }
        _ => {
            // requireModerator for every admin/drive/* endpoint.
            if let Err(resp) = gate(state, token, Gate::Moderator).await {
                return resp;
            }
            let Some(pool) = &state.pool else {
                return internal();
            };
            match name {
                "admin/drive/files" => files(pool, body).await,
                "admin/drive/show-file" => show_file(state, body).await,
                "admin/drive/cleanup" => match mk_db::drive::admin_cleanup_orphans(pool).await {
                    Ok(_) => Ok(StatusCode::NO_CONTENT.into_response()),
                    Err(e) => Err(db_err("admin drive cleanup", e)),
                },
                _ => {
                    // admin/drive/clean-remote-files
                    match mk_db::drive::admin_clean_remote_files(pool).await {
                        Ok(_) => Ok(StatusCode::NO_CONTENT.into_response()),
                        Err(e) => Err(db_err("admin clean remote files", e)),
                    }
                }
            }
        }
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_admin_drive", error = %e, "db error: {context}");
    internal()
}

/// show-file.ts meta.errors.noSuchFile (caf3ca38-c6e5-472e-a30c-b05377dcc240).
fn no_such_file() -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "NO_SUCH_FILE",
        "caf3ca38-c6e5-472e-a30c-b05377dcc240",
        "No such file.",
    ))
}

/// ajv `misskey:id` format.
fn is_aid(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn invalid_param(param: &str, reason: &str) -> Response {
    super::invalid_param(param, reason)
}

/// limit `{ minimum: 1, maximum: 100, default: 10 }`.
fn parse_limit(body: &Value) -> Check<i64> {
    match body.get("limit") {
        None | Some(Value::Null) => Ok(10),
        Some(v) => match v.as_i64() {
            Some(n) if (1..=100).contains(&n) => Ok(n),
            _ => Err(invalid_param("limit", "must be an integer in 1..=100")),
        },
    }
}

/// `sinceId`/`untilId` (exclusive) with the `sinceDate`/`untilDate` aid
/// conversion QueryService performs (`idService.gen(ms)`). Id params win over
/// their date twins, matching upstream's if/else chain.
fn parse_bounds(body: &Value) -> Check<(Option<String>, Option<String>)> {
    let mut since_id: Option<String> = None;
    let mut until_id: Option<String> = None;
    match body.get("sinceId") {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) if is_aid(s) => since_id = Some(s.clone()),
        Some(_) => return Err(invalid_param("sinceId", "must be an alphanumeric id")),
    }
    match body.get("untilId") {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) if is_aid(s) => until_id = Some(s.clone()),
        Some(_) => return Err(invalid_param("untilId", "must be an alphanumeric id")),
    }
    if since_id.is_none() {
        if let Some(v) = body.get("sinceDate") {
            let Some(ms) = v.as_i64() else {
                return Err(invalid_param("sinceDate", "must be an integer"));
            };
            since_id = Some(mk_types::id::gen_aid(ms));
        }
    }
    if until_id.is_none() {
        if let Some(v) = body.get("untilDate") {
            let Some(ms) = v.as_i64() else {
                return Err(invalid_param("untilDate", "must be an integer"));
            };
            until_id = Some(mk_types::id::gen_aid(ms));
        }
    }
    Ok((since_id, until_id))
}

/// type `{ pattern: /^[a-zA-Z0-9\/\-*]+$/ }`: exact, or `ends with /*` → the
/// LIKE prefix arm (upstream `type.replace('/*', '/') + '%'`).
fn parse_type(body: &Value) -> Check<(Option<String>, Option<String>)> {
    match body.get("type") {
        None | Some(Value::Null) => Ok((None, None)),
        Some(Value::String(s)) => {
            let valid = !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '/' || c == '*' || c == '-');
            if !valid {
                return Err(invalid_param(
                    "type",
                    "must match ^[a-zA-Z0-9/\\-*]+$ or null",
                ));
            }
            if s.ends_with("/*") {
                Ok((None, Some(s.replace("/*", "/"))))
            } else {
                Ok((Some(s.clone()), None))
            }
        }
        Some(_) => Err(invalid_param("type", "must be a string or null")),
    }
}

/// Flat `DriveFile` entity shape (the admin endpoints return the raw row, not
/// the public pack). `user` is populated by `show-file` only.
fn pack_file(row: &mk_db::drive::AdminDriveFileRow, user: Option<Value>) -> Value {
    let created_at = mk_types::id::parse_aid_date(&row.id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default();
    json!({
        "id": row.id,
        "createdAt": created_at,
        "userId": row.user_id,
        "userHost": row.user_host,
        "md5": row.md5,
        "name": row.name,
        "type": row.file_type,
        "size": row.size,
        "comment": row.comment,
        "blurhash": row.blurhash,
        "properties": row.properties,
        "storedInternal": row.stored_internal,
        "url": row.url,
        "thumbnailUrl": row.thumbnail_url,
        "webpublicUrl": row.webpublic_url,
        "webpublicType": row.webpublic_type,
        "accessKey": row.access_key,
        "uri": row.uri,
        "src": row.src,
        "folderId": row.folder_id,
        "isSensitive": row.is_sensitive,
        "isLink": row.is_link,
        "maybeSensitive": row.maybe_sensitive,
        "maybePorn": row.maybe_porn,
        "requestIp": row.request_ip,
        "requestHeaders": row.request_headers,
        "user": user.unwrap_or(Value::Null),
    })
}

/// `admin/drive/files`: global paged list. A `userId` wins over the
/// origin/hostname filters (upstream if/else).
async fn files(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let limit = parse_limit(body)?;
    let (since_id, until_id) = parse_bounds(body)?;
    let (type_exact, type_prefix) = parse_type(body)?;

    let user_id = match body.get("userId") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if is_aid(s) => Some(s.clone()),
        Some(_) => {
            return Err(invalid_param(
                "userId",
                "must be an alphanumeric id or null",
            ));
        }
    };

    let origin = match body.get("origin") {
        None | Some(Value::Null) => "local".to_string(),
        Some(Value::String(s)) if matches!(s.as_str(), "combined" | "local" | "remote") => {
            s.clone()
        }
        Some(_) => {
            return Err(invalid_param(
                "origin",
                "must be one of combined, local, remote",
            ));
        }
    };

    let hostname = match body.get("hostname") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return Err(invalid_param("hostname", "must be a string or null")),
    };

    let rows = mk_db::drive::admin_files_list(
        pool,
        since_id.as_deref(),
        until_id.as_deref(),
        user_id.as_deref(),
        &origin,
        hostname.as_deref(),
        type_exact.as_deref(),
        type_prefix.as_deref(),
        limit,
    )
    .await
    .map_err(|e| db_err("admin files list", e))?;

    let out: Vec<Value> = rows.iter().map(|r| pack_file(r, None)).collect();
    Ok(Json(json!(out)).into_response())
}

/// Pack the file owner with the shared user pack (`show-file`'s `user`).
async fn pack_owner(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    user_id: Option<&str>,
) -> Check<Value> {
    let Some(user_id) = user_id else {
        return Ok(Value::Null);
    };
    let user = mk_db::user::find_user_by_id(pool, user_id)
        .await
        .map_err(|e| db_err("find_user_by_id[owner]", e))?;
    let Some(user) = user else {
        // upstream findOneByOrFail → 500.
        return Err(internal());
    };
    let profile = match mk_db::user::find_profile(pool, &user.id).await {
        Ok(p) => p,
        Err(e) => return Err(db_err("find_profile[owner]", e)),
    };
    let host = state.config.host();
    let packed = mk_core::services::user_pack::pack_inline(
        &user,
        &profile,
        false,
        None,
        mk_core::services::user_pack::InstanceCtx {
            url: &state.config.url,
            host: &host,
        },
    );
    Ok(packed)
}

/// `admin/drive/show-file`: anyOf fileId|url; NO_SUCH_FILE 400 on a miss;
/// flat file object + `user`.
async fn show_file(state: &AppState, body: &Value) -> Check<Response> {
    let Some(pool) = &state.pool else {
        return Err(internal());
    };

    let row = match body.get("fileId") {
        Some(Value::String(id)) if is_aid(id) => mk_db::drive::admin_find_file_by_id(pool, id)
            .await
            .map_err(|e| db_err("admin find file by id", e))?,
        None | Some(Value::Null) => match body.get("url") {
            Some(Value::String(url)) => mk_db::drive::admin_find_file_by_url(pool, url)
                .await
                .map_err(|e| db_err("admin find file by url", e))?,
            _ => {
                return Err(invalid_param(
                    "fileId",
                    "must have required property 'fileId'",
                ));
            }
        },
        Some(_) => match body.get("url") {
            Some(Value::String(url)) => mk_db::drive::admin_find_file_by_url(pool, url)
                .await
                .map_err(|e| db_err("admin find file by url", e))?,
            _ => return Err(invalid_param("fileId", "must be an alphanumeric id")),
        },
    };

    let Some(row) = row else {
        return Err(no_such_file());
    };
    let user = pack_owner(state, pool, row.user_id.as_deref()).await?;
    Ok(Json(pack_file(&row, Some(user))).into_response())
}

/// `admin/delete-all-files-of-a-user` (requireAdmin): delete the target's
/// files, 204.
async fn delete_all_files_of_a_user(state: &AppState, body: &Value) -> Check<Response> {
    let Some(pool) = &state.pool else {
        return Err(internal());
    };
    let user_id = match body.get("userId") {
        None | Some(Value::Null) => {
            return Err(invalid_param(
                "userId",
                "must have required property 'userId'",
            ));
        }
        Some(Value::String(s)) if is_aid(s) => s.clone(),
        Some(_) => return Err(invalid_param("userId", "must be an alphanumeric id")),
    };
    mk_db::drive::admin_delete_files_of_user(pool, &user_id)
        .await
        .map_err(|e| db_err("admin delete files of user", e))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
