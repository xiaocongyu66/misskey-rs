//! `retention` + `export-custom-emojis` — ports of
//! `endpoints/retention.ts` + `endpoints/export-custom-emojis.ts` at the
//! pinned upstream (misskey-dev/misskey ea2c3aed).
//!
//! `retention.ts` (read at the pin): `requireCredential: false`, `allowGet:
//! true`, `cacheSec: 60 * 60`. The handler reads the newest 30
//! `retention_aggregation` rows (`order: { id: 'DESC' }, take: 30`) and
//! answers `[{ createdAt, users, data }]` — NOT the older
//! `{ isRetentionEnabled, ... }` policy shape the slice brief mentioned.
//! Missing rows → `[]`. Db path: `mk_db::meta::retention_aggregations`.
//!
//! `export-custom-emojis.ts` (read at the pin) is a queue enqueue:
//! `requireCredential`, void handler → 204 after
//! `QueueService.createExportCustomEmojisJob(me)`, and
//! `ExportCustomEmojisProcessorService` later downloads the local emoji
//! originals, writes `meta.json` + the files into a stored-zip and adds it
//! to the requester's drive (`custom-emojis-yyyy-MM-dd-HH-mm-ss.zip`).
//! This slice cannot wire a new queue kind (the boot worker + DriveService
//! download lane live in other agents' files), so it implements the
//! synchronous variant the slice brief asks for instead:
//!
//! - POST `export-custom-emojis` (requireModerator — the brief's gate; the
//!   pinned meta is requireCredential, documented divergence) answers 200
//!   with the export body directly.
//! - optional `format` (an extension; the pinned paramDef is `{}`):
//!   `zip` (default) → `application/zip` holding `meta.json`, `meta` → the
//!   same `meta.json` document as JSON.
//! - `meta.json` mirrors the processor document
//!   (`{metaVersion:2, host, exportedAt, emojis:[{fileName, downloaded,
//!   emoji}]}`) with `downloaded: false` for every entry: fetching the
//!   original bytes is the DownloadService lane this server does not carry.
//!   Invalid emoji names (`^[a-zA-Z0-9_]+$`) are skipped like upstream.
//! - The zip is the STORED (method 0) variant — a hand-rolled ~60-line
//!   writer, no new crate (Cargo.toml has no zip dependency).

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

use super::admin::{Gate, gate};

/// Endpoint name owned by this module (POST-only).
pub(crate) fn handles(name: &str) -> bool {
    name == "export-custom-emojis"
}

// ---------------------------------------------------------------------------
// retention — endpoints/retention.ts
// ---------------------------------------------------------------------------

pub(crate) async fn retention(state: &AppState) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let rows = match mk_db::meta::retention_aggregations(pool).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!(target: "dbg_retention", error = %e, "retention_aggregations");
            return api_err(ApiError::internal_error());
        }
    };
    let body: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "createdAt": mk_core::services::signup::ms_to_iso(r.created_at_ms),
                "users": r.users_count,
                "data": r.data,
            })
        })
        .collect();
    // meta.cacheSec = 60 * 60 → `Cache-Control: private, max-age=3600`
    // (the same header the M0 GET_ALLOWED table emits).
    let mut resp = (StatusCode::OK, Json(Value::Array(body))).into_response();
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=3600"),
    );
    resp
}

// ---------------------------------------------------------------------------
// export-custom-emojis — endpoints/export-custom-emojis.ts
// ---------------------------------------------------------------------------

pub(crate) async fn export_custom_emojis(
    state: &AppState,
    body: &Value,
    token: Option<&str>,
) -> Response {
    if let Err(resp) = gate(state, token, Gate::Moderator).await {
        return resp;
    }
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let format = match body.get("format") {
        None | Some(Value::Null) => "zip",
        Some(Value::String(s)) => s.as_str(),
        Some(_) => return super::invalid_param("format", "must be string"),
    };
    if format != "zip" && format != "meta" {
        return super::invalid_param("format", "must be one of zip, meta");
    }

    // `emojisRepository.find({ where: { host: IsNull() }, order: { id:
    // 'ASC' } })` — list_local with no pagination/query, ascending.
    let rows = match mk_db::emoji::list_local(pool, None, i64::MAX, None, None, true).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!(target: "dbg_export_emoji", error = %e, "list_local");
            return api_err(ApiError::internal_error());
        }
    };

    let emojis: Vec<Value> = rows
        .iter()
        .filter(|e| valid_emoji_name(&e.name))
        .map(|e| {
            json!({
                "fileName": export_file_name(&e.name, e.file_type.as_deref()),
                // DownloadService (originalUrl → temp file) is not ported.
                "downloaded": false,
                "emoji": emoji_json(e),
            })
        })
        .collect();
    let meta = json!({
        "metaVersion": 2,
        "host": state.config.host(),
        "exportedAt": mk_core::services::signup::ms_to_iso(super::now_ms() as i64),
        "emojis": emojis,
    });

    if format == "meta" {
        return (StatusCode::OK, Json(meta)).into_response();
    }

    let files = [ZipFile {
        name: "meta.json".to_string(),
        data: serde_json::to_vec(&meta).unwrap_or_default(),
    }];
    let zip = build_stored_zip(&files);
    let mut resp = (StatusCode::OK, zip).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    resp.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"custom-emojis.zip\""),
    );
    resp
}

/// `^[a-zA-Z0-9_]+$` — the processor skips any other local emoji name.
fn valid_emoji_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// `mime.extension(emoji.type ?? 'image/png')`: a known image mime gets an
/// extension, an unknown one leaves the bare name (upstream `ext` undefined).
fn export_file_name(name: &str, file_type: Option<&str>) -> String {
    let ext = match file_type.unwrap_or("image/png") {
        "image/png" => Some("png"),
        "image/apng" => Some("apng"),
        "image/gif" => Some("gif"),
        "image/jpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/svg+xml" => Some("svg"),
        _ => None,
    };
    match ext {
        Some(ext) => format!("{name}.{ext}"),
        None => name.to_string(),
    }
}

/// The `emoji` field the processor's meta.json carries (the entity row).
fn emoji_json(e: &mk_db::emoji::EmojiRow) -> Value {
    json!({
        "id": e.id,
        "name": e.name,
        "host": e.host,
        "originalUrl": e.original_url,
        "uri": e.uri,
        "type": e.file_type,
        "aliases": e.aliases,
        "category": e.category,
        "publicUrl": e.public_url,
        "license": e.license,
        "localOnly": e.local_only,
        "isSensitive": e.is_sensitive,
        "roleIdsThatCanBeUsedThisEmojiAsReaction": e.role_ids,
    })
}

// ---------------------------------------------------------------------------
// stored-zip writer (no compression, no external crate)
// ---------------------------------------------------------------------------

struct ZipFile {
    name: String,
    data: Vec<u8>,
}

/// CRC-32 (IEEE 802.3, the zip polynomial) — table-less bit loop.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

/// Minimal zip archive with the STORED method (0): local headers + data,
/// central directory, EOCD. Names are ASCII (emoji-name regex + meta.json).
fn build_stored_zip(files: &[ZipFile]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    // Fixed DOS timestamp 1980-01-01 00:00 (date field 0x0021).
    const DOS_DATE: u16 = 0x0021;
    for f in files {
        let name = f.name.as_bytes();
        let crc = crc32(&f.data);
        let size = f.data.len() as u32;
        let offset = out.len() as u32;

        // Local file header.
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // method: stored
        out.extend_from_slice(&0u16.to_le_bytes()); // time
        out.extend_from_slice(&DOS_DATE.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes()); // compressed size
        out.extend_from_slice(&size.to_le_bytes()); // uncompressed size
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(name);
        out.extend_from_slice(&f.data);

        // Central directory entry.
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes()); // version made by
        central.extend_from_slice(&20u16.to_le_bytes()); // version needed
        central.extend_from_slice(&0u16.to_le_bytes()); // flags
        central.extend_from_slice(&0u16.to_le_bytes()); // method
        central.extend_from_slice(&0u16.to_le_bytes()); // time
        central.extend_from_slice(&DOS_DATE.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra len
        central.extend_from_slice(&0u16.to_le_bytes()); // comment len
        central.extend_from_slice(&0u16.to_le_bytes()); // disk number
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name);
    }

    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    out.extend_from_slice(&central);
    // End of central directory.
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // disk number
    out.extend_from_slice(&0u16.to_le_bytes()); // central dir disk
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment len
    out
}
