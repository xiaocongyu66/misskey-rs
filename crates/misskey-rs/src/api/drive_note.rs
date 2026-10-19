//! note-agent: `drive/files/update` minimal slice (the comment / name /
//! isSensitive triangle the note.ts NSFW census rides; db part in
//! `mk-db/src/drive_note.rs`). Upstream: `endpoints/drive/files/update.ts`
//! (NO_SUCH_FILE → ACCESS_DENIED → updateFile's INVALID_FILE_NAME /
//! NO_SUCH_FOLDER / RESTRICTED_BY_ROLE ladder) + `DriveService.updateFile`
//! (`alwaysMarkNsfw` is evaluated against the FILE OWNER's policies).

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::AppState;
use crate::error::{ApiError, api_err};

fn meta_err(code: &'static str, id: &'static str, msg: &'static str) -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        code,
        id,
        msg,
    ))
}

/// ajv `misskey:id` format.
fn is_alnum_id(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// `POST /api/drive/files/update` (requireCredential, write:drive).
pub(crate) async fn files_update(
    state: &AppState,
    body: &serde_json::Value,
    token: Option<&str>,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = match super::require_credential_me_id(pool, token, "dbg_drive_update").await {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let Some(file_id) = body
        .get("fileId")
        .and_then(|v| v.as_str())
        .filter(|s| is_alnum_id(s))
    else {
        return api_err(ApiError::invalid_param(json!({
            "param": "fileId",
            "reason": "must be an alphanumeric id",
        })));
    };
    let is_sensitive = match body.get("isSensitive") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Bool(b)) => Some(*b),
        Some(_) => {
            return api_err(ApiError::invalid_param(json!({
                "param": "isSensitive",
                "reason": "must be a boolean",
            })));
        }
    };
    let name = match body.get("name") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) => Some(s.as_str()),
        Some(_) => {
            return api_err(ApiError::invalid_param(json!({
                "param": "name",
                "reason": "must be a string",
            })));
        }
    };
    let (set_comment, comment) = match body.get("comment") {
        None | Some(serde_json::Value::Null) => (false, None),
        Some(serde_json::Value::String(s)) if s.chars().count() <= 512 => (true, Some(s.as_str())),
        Some(_) => {
            return api_err(ApiError::invalid_param(json!({
                "param": "comment",
                "reason": "must be a string of up to 512 chars",
            })));
        }
    };
    // folderId: the schema allows id | null. Undefined leaves the folder
    // untouched, null moves the file to root (親フォルダを無しにできる), an
    // id re-parents after the owned-folder check (他人のフォルダには
    // 入れられない / 存在しないフォルダで怒られる). Lookup order matches
    // upstream: file → access → updateFile → folder lookup.
    let folder_id = match body.get("folderId") {
        None => None,
        Some(serde_json::Value::Null) => Some(None),
        Some(serde_json::Value::String(s)) => {
            if !is_alnum_id(s) {
                return api_err(ApiError::invalid_param(json!({
                    "param": "folderId",
                    "reason": "must be an alphanumeric id",
                })));
            }
            Some(Some(s.clone()))
        }
        Some(_) => {
            return api_err(ApiError::invalid_param(json!({
                "param": "folderId",
                "reason": "must be an id or null",
            })));
        }
    };

    let Some(file) = (match mk_db::drive_note::find_file_by_id(pool, file_id).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(target: "dbg_drive", error = %e, "find_file_by_id");
            return api_err(ApiError::internal_error());
        }
    }) else {
        return meta_err(
            "NO_SUCH_FILE",
            "e7778c7e-3af9-49cd-9690-6dbc3e6c972d",
            "No such file.",
        );
    };

    // access: owner or moderator
    if file.user_id.as_deref() != Some(me_id.as_str()) {
        let moderator = match super::resolve_me(pool, token).await {
            Some(me) => super::viewer_is_moderator(pool, Some(me.as_str())).await,
            None => false,
        };
        if !moderator {
            return meta_err(
                "ACCESS_DENIED",
                "01a53b27-82fc-445b-a0c1-b558465a8ed2",
                "Access denied.",
            );
        }
    }

    // DriveService.updateFile: name validation + the owner-policy NSFW gate.
    if let Some(n) = name {
        let trimmed_invalid = n.trim().is_empty()
            || n.len() > 200
            || n.contains('\\')
            || n.contains('/')
            || n.contains("..");
        if trimmed_invalid {
            return meta_err(
                "INVALID_FILE_NAME",
                "395e7156-f9f0-475e-af89-53c3c23080c2",
                "Invalid file name.",
            );
        }
    }
    // resolve the target folder (files/update.ts ps.folderId → GetterService
    // getFolder OwnedDriveFolder → NO_SUCH_FOLDER)
    let set_folder = folder_id.is_some();
    let new_folder = match &folder_id {
        None => None,
        Some(None) => Some(None),
        Some(Some(fid)) => match mk_db::drive::find_folder_owned(pool, fid, &me_id).await {
            Ok(Some(_)) => Some(Some(fid.clone())),
            Ok(None) => {
                return meta_err(
                    "NO_SUCH_FOLDER",
                    "ea8fb7a5-af77-4a08-b608-c0218176cd73",
                    "No such folder.",
                );
            }
            Err(e) => {
                tracing::error!(target: "dbg_drive", error = %e, "find_folder_owned");
                return api_err(ApiError::internal_error());
            }
        },
    };
    if let (Some(false), true) = (is_sensitive, file.is_sensitive) {
        // alwaysMarkNsfw is the FILE OWNER's merged policy.
        let owner_always_nsfw = match &file.user_id {
            Some(owner) => match mk_db::role::user_assigned_roles(pool, owner).await {
                Ok(assigned) => {
                    mk_core::services::role::UserPolicy::compute(&assigned).always_mark_nsfw
                }
                Err(e) => {
                    tracing::error!(target: "dbg_drive", error = %e, "user_assigned_roles");
                    return api_err(ApiError::internal_error());
                }
            },
            None => false,
        };
        if owner_always_nsfw {
            return meta_err(
                "RESTRICTED_BY_ROLE",
                "7f59dccb-f465-75ab-5cf4-3ce44e3282f7",
                "This feature is restricted by your role.",
            );
        }
    }

    if let Err(e) = mk_db::drive_note::update_file(
        pool,
        file_id,
        name,
        is_sensitive,
        set_comment,
        comment,
        set_folder,
        new_folder.flatten().as_deref(),
    )
    .await
    {
        tracing::error!(target: "dbg_drive", error = %e, "update_file");
        return api_err(ApiError::internal_error());
    }

    // response: pack(file, { self: true }) — raw url/properties; all fields
    // materialize from the updated row (aid createdAt).
    let Some(updated) = (match mk_db::drive_note::find_file_by_id(pool, file_id).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(target: "dbg_drive", error = %e, "find_file_by_id[after]");
            return api_err(ApiError::internal_error());
        }
    }) else {
        return api_err(ApiError::internal_error());
    };
    let created_at = mk_types::id::parse_aid_date(&updated.id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default();
    (
        StatusCode::OK,
        axum::Json(mk_core::packers::note::drive_file_pack(
            &updated, created_at,
        )),
    )
        .into_response()
}
