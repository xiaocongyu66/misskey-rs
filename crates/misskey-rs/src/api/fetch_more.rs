//! Fetch/proxy endpoints — ports of upstream
//! `endpoints/fetch-rss.ts`, `endpoints/fetch-external-resources.ts` and
//! `endpoints/page-push.ts`.
//!
//! - `fetch-rss` (guest-ok, allowGet, cacheSec 180): normalizes the URL
//!   (`INVALID_URL` 400 for empty/oversize/non-http(s)/credentialed input),
//!   fetches it through the crate's reqwest client (5s, Accept
//!   `application/rss+xml, */*`, 1 MiB cap), then hand-parses RSS 2.0 /
//!   Atom and returns the rss-parser-shaped object (`items` plus the
//!   channel-level `title`/`link`/`description` when present). Fetch or
//!   parse failures collapse into `FETCH_RSS_FAILED` 422; the upstream
//!   in-flight dedupe map is not reproduced, but the 32-request
//!   concurrency cap is (`FETCH_RSS_UNAVAILABLE` 503).
//! - `fetch-external-resources` (requireCredential): GETs the URL as JSON
//!   (`{ type, data }`), rejects a missing/blank pair with
//!   `EXT_RESOURCE_RETURNED_INVALID_SCHEMA`, and compares the sha512 hex of
//!   `data` (CRLF normalized to LF) against the `hash` param
//!   (`EXT_RESOURCE_HASH_DIDNT_MATCH`).
//! - `page-push` (requireCredential): validates the page exists
//!   (`NO_SUCH_PAGE`) and answers 204. Upstream publishes `pageEvent` to
//!   the page owner's main stream (`GlobalEventService.publishMainStream`)
//!   with the caller's packed UserDetailed; this slice has no SSE/streaming
//!   fan-out wired here, so the owner id is resolved for the documented
//!   target but nothing is recorded or broadcast. Note upstream itself has
//!   no owner-authorization gate — any authenticated caller may push to an
//!   existing page — and this port mirrors that (existence only).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha512};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// Endpoint names routed into this module (single source of truth for the
/// `// fetch/proxy` dispatch arm in mod.rs).
pub(crate) const FETCH_ENDPOINTS: &[&str] = &["fetch-rss", "fetch-external-resources", "page-push"];

pub(crate) fn handles(name: &str) -> bool {
    FETCH_ENDPOINTS.contains(&name)
}

const MAX_URL_LENGTH: usize = 8192;
const MAX_RESPONSE_SIZE: usize = 1024 * 1024;
const MAX_CONCURRENT_REQUESTS: usize = 32;

/// Live fetch-rss requests (upstream `activeRequestCount`).
static ACTIVE_FETCH_RSS: AtomicUsize = AtomicUsize::new(0);

/// Decrements the live-request counter even when the fetch future unwinds.
struct FetchSlot;

impl Drop for FetchSlot {
    fn drop(&mut self) {
        ACTIVE_FETCH_RSS.fetch_sub(1, Ordering::SeqCst);
    }
}

fn api_error(
    status: u16,
    kind: mk_types::api_error::ApiErrorKind,
    code: &str,
    id: &str,
    msg: &str,
) -> Response {
    api_err(ApiError::new(status, kind, code, id, msg))
}

fn invalid_url() -> Response {
    api_error(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "INVALID_URL",
        "89b7ee05-ccfc-4bdd-9b13-61172fd1e06c",
        "Invalid URL.",
    )
}

fn fetch_rss_failed() -> Response {
    api_error(
        422,
        mk_types::api_error::ApiErrorKind::Server,
        "FETCH_RSS_FAILED",
        "8db5d3d8-31d7-452f-b0cc-ca3b8925de12",
        "Failed to fetch RSS.",
    )
}

fn fetch_rss_unavailable() -> Response {
    api_error(
        503,
        mk_types::api_error::ApiErrorKind::Server,
        "FETCH_RSS_UNAVAILABLE",
        "91e6ff44-c63f-4725-9ad0-b7a40d7f7655",
        "RSS fetching is temporarily unavailable.",
    )
}

fn ext_invalid_schema() -> Response {
    api_error(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "EXT_RESOURCE_RETURNED_INVALID_SCHEMA",
        "bb774091-7a15-4a70-9dc5-6ac8cf125856",
        "External resource returned invalid schema.",
    )
}

fn ext_hash_mismatch() -> Response {
    api_error(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "EXT_RESOURCE_HASH_DIDNT_MATCH",
        "693ba8ba-b486-40df-a174-72f8279b56a4",
        "Hash did not match.",
    )
}

fn no_such_page() -> Response {
    api_error(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "NO_SUCH_PAGE",
        "4a13ad31-6729-46b4-b9af-e86b265c2e74",
        "No such page.",
    )
}

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn invalid_param(param: &str, reason: &str) -> Response {
    api_err(ApiError::invalid_param(json!({
        "param": param,
        "reason": reason,
    })))
}

fn required_str(body: &Value, key: &str) -> Result<String, Response> {
    match body.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(invalid_param(key, "must be a string")),
        None => Err(invalid_param(key, "required")),
    }
}

fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest client for fetch endpoints")
    })
}

/// `POST /api/fetch-rss` entry (body pre-parsed, token + credential-declared
/// pre-extracted). Guest-ok, but a declared credential is still validated
/// (pages_more pattern).
pub(crate) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
    credential_declared: bool,
) -> Response {
    match name {
        "fetch-rss" => {
            if let Some(pool) = &state.pool {
                if token.is_some() || credential_declared {
                    if let Err(resp) = super::social::require_checked_user(
                        pool,
                        token.as_deref(),
                        credential_declared,
                    )
                    .await
                    {
                        return resp;
                    }
                }
            }
            fetch_rss(body).await
        }
        "fetch-external-resources" | "page-push" => {
            let Some(pool) = &state.pool else {
                return internal();
            };
            let me_id = match super::social::require_checked_user(
                pool,
                token.as_deref(),
                credential_declared,
            )
            .await
            {
                Ok(id) => id,
                Err(resp) => return resp,
            };
            match name {
                "fetch-external-resources" => fetch_external_resources(body).await,
                _ => page_push(pool, body, &me_id).await,
            }
        }
        _ => api_err(ApiError::unknown_endpoint()),
    }
}

// ---------------------------------------------------------------------------
// fetch-rss
// ---------------------------------------------------------------------------

/// Upstream `normalizeUrl`: length, URL parse, http(s) only, no credentials,
/// fragment stripped.
fn normalize_url(input: &str) -> Result<String, Response> {
    if input.is_empty() || input.len() > MAX_URL_LENGTH {
        return Err(invalid_url());
    }
    let mut url = reqwest::Url::parse(input).map_err(|_| invalid_url())?;
    if (url.scheme() != "http" && url.scheme() != "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid_url());
    }
    url.set_fragment(None);
    Ok(url.to_string())
}

async fn fetch_rss(body: &Value) -> Response {
    let raw_url = match required_str(body, "url") {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let url = match normalize_url(&raw_url) {
        Ok(u) => u,
        Err(resp) => return resp,
    };

    // upstream `activeRequestCount >= 32` → 503. (The in-flight map dedupe is
    // intentionally not reproduced; see the module docs.)
    if ACTIVE_FETCH_RSS.fetch_add(1, Ordering::SeqCst) >= MAX_CONCURRENT_REQUESTS {
        ACTIVE_FETCH_RSS.fetch_sub(1, Ordering::SeqCst);
        return fetch_rss_unavailable();
    }
    let _slot = FetchSlot;

    match fetch_and_parse_rss(&url).await {
        Ok(feed) => (StatusCode::OK, Json(feed)).into_response(),
        Err(resp) => resp,
    }
}

async fn fetch_and_parse_rss(url: &str) -> Result<Value, Response> {
    let res = http_client()
        .get(url)
        .header(reqwest::header::ACCEPT, "application/rss+xml, */*")
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|_| fetch_rss_failed())?;

    if !res.status().is_success() {
        return Err(fetch_rss_failed());
    }

    // Redirects may land on a non-HTTP scheme (upstream re-checks `res.url`).
    let final_url = res.url();
    if final_url.scheme() != "http" && final_url.scheme() != "https" {
        return Err(fetch_rss_failed());
    }

    let bytes = res.bytes().await.map_err(|_| fetch_rss_failed())?;
    if bytes.len() > MAX_RESPONSE_SIZE {
        return Err(fetch_rss_failed());
    }
    let text = String::from_utf8_lossy(&bytes);
    Ok(parse_feed(&text))
}

// ---------------------------------------------------------------------------
// minimal RSS 2.0 / Atom parser (rss-parser output shape)
// ---------------------------------------------------------------------------

fn open_boundary(after: &str) -> bool {
    match after.chars().next() {
        Some('>') | Some('/') => true,
        Some(c) => c.is_whitespace(),
        None => false,
    }
}

/// Byte offset of the first `<tag` whose name ends at a valid boundary.
fn find_tag_start(hay: &str, tag: &str) -> Option<usize> {
    let open = format!("<{tag}");
    let mut from = 0;
    while let Some(rel) = hay[from..].find(&open) {
        let i = from + rel;
        if open_boundary(&hay[i + open.len()..]) {
            return Some(i);
        }
        from = i + open.len();
    }
    None
}

/// Inner text of every `<tag>...</tag>` container, in document order.
fn blocks<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = find_tag_start(rest, tag) {
        let after = &rest[i + 1 + tag.len()..];
        let Some(gt) = after.find('>') else { break };
        let start = i + 1 + tag.len() + gt + 1;
        let Some(end) = rest[start..].find(&close) else {
            break;
        };
        out.push(&rest[start..start + end]);
        rest = &rest[start + end + close.len()..];
    }
    out
}

/// Text of the first `<tag>...</tag>` (self-closing tags yield `None`).
fn tag_text(block: &str, tag: &str) -> Option<String> {
    let close = format!("</{tag}>");
    let i = find_tag_start(block, tag)?;
    let after = &block[i + 1 + tag.len()..];
    let gt = after.find('>')?;
    if after[..gt].trim_end().ends_with('/') {
        return None;
    }
    let start = i + 1 + tag.len() + gt + 1;
    let end = block[start..].find(&close)?;
    Some(decode_xml_text(&block[start..start + end]))
}

fn all_tag_texts(block: &str, tag: &str) -> Vec<String> {
    blocks(block, tag)
        .into_iter()
        .map(decode_xml_text)
        .filter(|s| !s.is_empty())
        .collect()
}

/// `href` of the first `<tag ... href="...">`.
fn attr_value(block: &str, tag: &str, attr: &str) -> Option<String> {
    let i = find_tag_start(block, tag)?;
    let after = &block[i + 1 + tag.len()..];
    let gt = after.find('>')?;
    let tag_src = &after[..gt];
    let needle = format!("{attr}=");
    let ai = tag_src.find(&needle)?;
    let rest = tag_src[ai + needle.len()..].trim_start();
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &rest[1..];
    let end = rest.find(quote)?;
    Some(decode_xml_text(&rest[..end]))
}

fn link_value(block: &str, atom: bool) -> Option<String> {
    if atom {
        attr_value(block, "link", "href").or_else(|| tag_text(block, "link"))
    } else {
        tag_text(block, "link").or_else(|| attr_value(block, "link", "href"))
    }
}

fn decode_xml_text(raw: &str) -> String {
    let s = raw.trim();
    let s = s
        .strip_prefix("<![CDATA[")
        .and_then(|r| r.strip_suffix("]]>"))
        .unwrap_or(s);
    unescape_xml(s).trim().to_string()
}

fn unescape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(semi) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let entity = &tail[1..semi];
        let decoded = match entity {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "amp" => Some('&'),
            _ => {
                if let Some(hex) = entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                {
                    u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
                } else {
                    entity
                        .strip_prefix('#')
                        .and_then(|d| d.parse::<u32>().ok())
                        .and_then(char::from_u32)
                }
            }
        };
        match decoded {
            Some(c) => out.push(c),
            None => out.push_str(&tail[..=semi]),
        }
        rest = &tail[semi + 1..];
    }
    out.push_str(rest);
    out
}

/// rss-parser `contentSnippet`: tags stripped, whitespace collapsed.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_feed(xml: &str) -> Value {
    let rss_items = blocks(xml, "item");
    let atom = rss_items.is_empty();
    let items = if atom {
        blocks(xml, "entry")
    } else {
        rss_items
    };

    // Channel/feed-level fields live before the first item/entry.
    let head_end = find_tag_start(xml, "item")
        .or_else(|| find_tag_start(xml, "entry"))
        .unwrap_or(xml.len());
    let head = &xml[..head_end];

    let mut out = serde_json::Map::new();
    if let Some(v) = tag_text(head, "title") {
        out.insert("title".to_string(), json!(v));
    }
    if let Some(v) = link_value(head, atom) {
        out.insert("link".to_string(), json!(v));
    }
    if let Some(v) = tag_text(head, "description").or_else(|| tag_text(head, "subtitle")) {
        out.insert("description".to_string(), json!(v));
    }

    let mut packed = Vec::with_capacity(items.len());
    for item in items {
        let mut obj = serde_json::Map::new();
        if let Some(v) = tag_text(item, "title") {
            obj.insert("title".to_string(), json!(v));
        }
        if let Some(v) = link_value(item, atom) {
            obj.insert("link".to_string(), json!(v));
        }
        if let Some(v) = tag_text(item, "guid").or_else(|| tag_text(item, "id")) {
            obj.insert("guid".to_string(), json!(v));
        }
        if let Some(v) = tag_text(item, "pubDate")
            .or_else(|| tag_text(item, "published"))
            .or_else(|| tag_text(item, "updated"))
        {
            obj.insert("pubDate".to_string(), json!(v));
        }
        if let Some(v) = tag_text(item, "dc:creator") {
            obj.insert("creator".to_string(), json!(v));
        }
        let summary = tag_text(item, "summary");
        let content = tag_text(item, "description").or_else(|| tag_text(item, "content"));
        if let Some(v) = &summary {
            obj.insert("summary".to_string(), json!(v));
        }
        if let Some(v) = &content {
            obj.insert("content".to_string(), json!(v));
            obj.insert("contentSnippet".to_string(), json!(strip_tags(v)));
        }
        let categories = all_tag_texts(item, "category");
        if !categories.is_empty() {
            obj.insert("categories".to_string(), json!(categories));
        }
        packed.push(Value::Object(obj));
    }
    out.insert("items".to_string(), Value::Array(packed));
    Value::Object(out)
}

// ---------------------------------------------------------------------------
// fetch-external-resources
// ---------------------------------------------------------------------------

async fn fetch_external_resources(body: &Value) -> Response {
    let url = match required_str(body, "url") {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let hash = match required_str(body, "hash") {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    // upstream HttpRequestService.getJson: GET, 5s, 256 KiB, JSON accept.
    let res = match http_client()
        .get(url.as_str())
        .header(reqwest::header::ACCEPT, "application/json, */*")
        .timeout(Duration::from_secs(5))
        .send()
        .await
    {
        Ok(res) if res.status().is_success() => res,
        // A non-2xx (or transport failure) is an unhandled upstream error.
        _ => return internal(),
    };

    let text = match res.text().await {
        Ok(t) => t,
        Err(_) => return internal(),
    };
    if text.len() > 1024 * 256 {
        return internal();
    }
    let parsed: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return ext_invalid_schema(),
    };
    let (Some(kind), Some(data)) = (
        parsed.get("type").and_then(Value::as_str),
        parsed.get("data").and_then(Value::as_str),
    ) else {
        return ext_invalid_schema();
    };
    if kind.is_empty() || data.is_empty() {
        return ext_invalid_schema();
    }

    let normalized = data.replace("\r\n", "\n");
    let digest = Sha512::digest(normalized.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        hex.push_str(&format!("{byte:02x}"));
    }
    if hex != hash {
        return ext_hash_mismatch();
    }

    (StatusCode::OK, Json(json!({ "type": kind, "data": data }))).into_response()
}

// ---------------------------------------------------------------------------
// page-push
// ---------------------------------------------------------------------------

async fn page_push(pool: &mk_db::sqlx::PgPool, body: &Value, _me_id: &str) -> Response {
    let page_id = match required_str(body, "pageId") {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if let Err(resp) = required_str(body, "event") {
        return resp;
    }

    match mk_db::page::find_page(pool, &page_id).await {
        // Upstream resolves the owner here and publishes `pageEvent` to
        // `page.userId`'s main stream; no SSE fan-out is wired in this slice,
        // so the call is a validated no-op.
        Ok(Some(_page)) => StatusCode::NO_CONTENT.into_response(),
        Ok(None) => no_such_page(),
        Err(e) => {
            tracing::error!(target: "dbg_fetch", error = %e, "page_push find_page");
            internal()
        }
    }
}
