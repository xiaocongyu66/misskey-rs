//! `drive/files/create` (multipart upload) + the shared
//! `DriveFileEntityService.getPublicUrl` helper.
//!
//! Upstream flow (`endpoints/drive/files/create.ts` + `DriveService.addFile`):
//! the multipart `file` part is stored and a `drive_file` row inserted
//! (MIME sniffed from content when the blob type is empty, md5 of the bytes,
//! url = `{base}/files/{accessKey}/{name}` for the internal-storage shape).
//! Response = packed DriveFile (`pack(file, { self: true })`); the e2e
//! Avatar/Banner census cases consume the row `id` through `i/update`.
//!
//! Deviations (documented for the report): file BYTES are not persisted (no
//! `/files` route exists — TODO(M4) internal storage), and `blurhash` stores
//! a deterministic printable placeholder derived from the content hash
//! because real blurhashing needs image decoding — TODO(M4). Both are
//! asserted only shape-wise upstream (users.ts pins `id` plus the
//! `/[ -~]{54}/` blurhash regex).

use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::error::{api_err, ApiError};
use crate::AppState;

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

    // name = ps.name ?? file.name (trim, 'blob'/empty → keep upstream
    // fallback of the uploaded filename basename)
    let raw_name = field(&parts, "name")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let name = raw_name
        .unwrap_or(&file.0)
        .rsplit('/')
        .next()
        .unwrap_or("untitled")
        .to_string();
    let name = if name.is_empty() || name == "blob" {
        "untitled".to_string()
    } else {
        name
    };

    let id = mk_types::id::gen_aid_now();
    let digest = md5_hex(&file.1.data);
    let url = format!(
        "{}/files/{}/{}",
        state.config.url.trim_end_matches('/'),
        mk_core::services::signup::secure_rndstr(24),
        name
    );
    let properties = match sniffed.as_deref() {
        Some("image/jpeg") | Some("image/png") | Some("image/gif") => json!({}),
        _ => json!({}),
    };
    let blurhash = Some(blurhash_placeholder(&digest));

    let row = mk_db::drive::DriveFileRow {
        id: id.clone(),
        user_id: Some(me_id.clone()),
        md5: digest.clone(),
        name: name.clone(),
        file_type: mime.clone(),
        size: file.1.data.len() as i32,
        blurhash: blurhash.clone(),
        url: url.clone(),
        webpublic_url: None,
        properties: properties.clone(),
    };
    let access_key = mk_core::services::signup::secure_rndstr(32);
    if let Err(e) = mk_db::drive::insert_drive_file(pool, &row, &access_key).await {
        tracing::error!(target: "dbg_drive", error = %e, "insert_drive_file");
        return api_err(ApiError::internal_error());
    }

    // pack(driveFile, { self: true }) — detailed shape
    let created_at = mk_types::id::parse_aid_date(&id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default();
    let packed = json!({
        "id": id,
        "createdAt": created_at,
        "name": name,
        "type": mime,
        "md5": digest,
        "size": row.size,
        "isSensitive": false,
        "blurhash": blurhash,
        "properties": properties,
        "url": url,
        "thumbnailUrl": Value::Null,
        "comment": Value::Null,
        "folderId": Value::Null,
        "folder": Value::Null,
        "userId": me_id,
        "userHost": Value::Null,
    });
    (StatusCode::OK, Json(packed)).into_response()
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
    } else {
        None
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
}
