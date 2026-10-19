//! arena-oauth — `server/oauth/OAuth2ProviderService.ts` + `oauth/errors.ts`
//! port (upstream test/e2e/oauth.ts): PKCE-S256 authorization-code provider
//! under /oauth/*.
//!
//! Routes (all OUTSIDE /api, like the upstream fastify /oauth instances):
//! - `GET /oauth/authorize` — client-information discovery (IndieAuth:
//!   Link headers, `<link rel=redirect_uri>` HTML tags, JSON client
//!   metadata), strict redirect_uri allow-listing, scope filtering to the
//!   misskey-js `permissions` kinds, mandatory PKCE S256; success renders
//!   the transaction-id HTML page, invalid params render as direct JSON
//!   errors (400/501/500) or indirect redirects once the redirect_uri has
//!   been validated (`error`/`state`/`iss` query — RFC9207).
//! - `POST /oauth/decision` — consumes the transaction and either issues
//!   the authorization code (redirect) or reports direct JSON errors.
//! - `POST /oauth/token` — authorization_code grant only (PKCE verify,
//!   single-use codes with revoke-on-reuse, access_token rows in the
//!   `access_token` table); CORS-permissive like upstream @fastify/cors.
//! - everything else under /oauth/* → 404 UNKNOWN_OAUTH_ENDPOINT.
//!
//! The MemoryKVCache pair (authorization transactions / grant codes, 5m
//! TTL) rides process globals — the upstream service is a single-instance
//! Nest provider. `MISSKEY_TEST_CHECK_IP_RANGE` rides `/api/_test/env`
//! (harness /env forward), mirroring the upstream process.env read.

use std::collections::HashMap;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;

use super::well_known;
use crate::AppState;

/// CacheService TTL — upstream `new MemoryKVCache<X>(1000 * 60 * 5)`.
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);
/// token/transaction/code lengths (`secureRndstr(128)` calls).
const RND_LEN: usize = 128;

// ---------------------------------------------------------------------------
// process-globals
// ---------------------------------------------------------------------------

struct CacheEntry<T> {
    at: Instant,
    value: T,
}

struct OAuthState {
    transactions: Mutex<HashMap<String, CacheEntry<AuthorizationTransaction>>>,
    grants: Mutex<HashMap<String, CacheEntry<AuthorizationCodeGrant>>>,
}

impl OAuthState {
    fn new() -> Self {
        Self {
            transactions: Mutex::new(HashMap::new()),
            grants: Mutex::new(HashMap::new()),
        }
    }

    /// MemoryKVCache.get — expired entries read as absent (and are dropped
    /// like upstream's periodic sweep would).
    fn get<T>(&self, map: &Mutex<HashMap<String, CacheEntry<T>>>, key: &str) -> Option<T>
    where
        T: Clone,
    {
        let mut guard = lock(map);
        match guard.get(key) {
            Some(entry) if entry.at.elapsed() < CACHE_TTL => Some(entry.value.clone()),
            Some(_) => {
                guard.remove(key);
                None
            }
            None => None,
        }
    }

    fn set<T>(&self, map: &Mutex<HashMap<String, CacheEntry<T>>>, key: String, value: T) {
        lock(map).insert(
            key,
            CacheEntry {
                at: Instant::now(),
                value,
            },
        );
    }

    fn delete<T>(&self, map: &Mutex<HashMap<String, CacheEntry<T>>>, key: &str) -> Option<T> {
        lock(map).remove(key).map(|e| e.value)
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// the `_test/env` toggle — `process.env.MISSKEY_TEST_CHECK_IP_RANGE === '1'`.
static CHECK_IP_RANGE: AtomicBool = AtomicBool::new(false);

/// mod.rs `_test/env` arm setter (mirrors two_factor::set_check_duplicated_totp).
pub(crate) fn set_check_ip_range(v: bool) {
    CHECK_IP_RANGE.store(v, Ordering::Relaxed);
}

fn oauth_state() -> &'static OAuthState {
    static STATE: OnceLock<OAuthState> = OnceLock::new();
    STATE.get_or_init(OAuthState::new)
}

/// reqwest Client shared by discovery fetches.
fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest client")
    })
}

// ---------------------------------------------------------------------------
// domain shapes
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ClientInformation {
    id: String,
    redirect_uris: Vec<String>,
    name: String,
    logo: Option<String>,
}

#[derive(Clone)]
struct AuthorizationRequest {
    redirect_uri: String,
    state: Option<String>,
    scopes: Vec<String>,
    code_challenge: String,
}

#[derive(Clone)]
struct AuthorizationTransaction {
    client: ClientInformation,
    request: AuthorizationRequest,
}

#[derive(Clone)]
struct AuthorizationCodeGrant {
    client_id: String,
    user_id: String,
    redirect_uri: String,
    code_challenge: String,
    scopes: Vec<String>,
    granted_token: Option<String>,
    revoked: bool,
    used: bool,
}

// ---------------------------------------------------------------------------
// OAuthProviderError surface + response helpers
// ---------------------------------------------------------------------------

/// `oauth/errors.ts` — status resolved per the pinned usages (every provider
/// error exposes its description; only the discovery parser-failure wrap
/// flips to a 500 + `server_error`).
struct OAuthError {
    error: String,
    error_description: String,
    status: u16,
}

impl OAuthError {
    fn invalid_request(description: &str) -> Self {
        Self {
            error: "invalid_request".to_string(),
            error_description: description.to_string(),
            status: 400,
        }
    }

    fn invalid_grant() -> Self {
        Self {
            error: "invalid_grant".to_string(),
            error_description: "grant request is invalid".to_string(),
            status: 400,
        }
    }

    fn invalid_scope() -> Self {
        Self {
            error: "invalid_scope".to_string(),
            error_description: "`scope` parameter has no known scope".to_string(),
            status: 400,
        }
    }

    fn unsupported_response_type() -> Self {
        Self {
            error: "unsupported_response_type".to_string(),
            error_description: "unsupported response type requested".to_string(),
            status: 501,
        }
    }

    fn unsupported_grant_type() -> Self {
        Self {
            error: "unsupported_grant_type".to_string(),
            error_description: "unsupported grant type requested".to_string(),
            status: 400,
        }
    }

    /// decision: transaction get miss → 403 access_denied.
    fn forbidden_access_denied() -> Self {
        Self {
            error: "access_denied".to_string(),
            error_description: "Invalid or expired transaction ID".to_string(),
            status: 403,
        }
    }

    /// discovery fetch failed (StatusError upstream) → plain invalid_request.
    fn fetch_failed() -> Self {
        Self::invalid_request("Failed to fetch client information")
    }

    /// discovery parse/other failure → wrapped 500 server_error.
    fn parse_failed() -> Self {
        Self {
            error: "server_error".to_string(),
            error_description: "Failed to parse client information".to_string(),
            status: 500,
        }
    }
}

/// `sendOAuthProviderError` — {error, error_description} + no-store + hook.
fn oauth_error_response(err: &OAuthError) -> Response {
    let body = json!({
        "error": err.error,
        "error_description": err.error_description,
    });
    let mut res = (
        StatusCode::from_u16(err.status).unwrap_or(StatusCode::BAD_REQUEST),
        axum::Json(body),
    )
        .into_response();
    apply_no_store(&mut res);
    stamp_oauth_hook(&mut res);
    res
}

/// `applyNoStore` — Cache-Control: no-store + Pragma: no-cache.
fn apply_no_store(res: &mut Response) {
    let h = res.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
}

/// createServer onRequest hook — clickjacking defense (unasserted, kept for
/// parity with the upstream provider-wide hook).
fn stamp_oauth_hook(res: &mut Response) {
    let h = res.headers_mut();
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-ancestors 'none'"),
    );
}

/// `redirectWithQuery` + `appendIssuer` — 302 to the redirect_uri with the
/// payload then `iss` (RFC9207) as query pairs. `Url::query_pairs_mut`
/// percent-encodes like URLSearchParams.
fn redirect_with_query(redirect_uri: &str, payload: &[(&str, String)], issuer: &str) -> Response {
    let mut url = match Url::parse(redirect_uri) {
        Ok(u) => u,
        Err(_) => {
            let mut res = StatusCode::BAD_REQUEST.into_response();
            apply_no_store(&mut res);
            stamp_oauth_hook(&mut res);
            return res;
        }
    };
    {
        let mut pairs = url.query_pairs_mut();
        pairs.clear();
        for (k, v) in payload {
            pairs.append_pair(k, v);
        }
        pairs.append_pair("iss", issuer);
    }

    let mut res = Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, url.to_string())
        .body(axum::body::Body::empty())
        .unwrap();
    apply_no_store(&mut res);
    stamp_oauth_hook(&mut res);
    res
}

fn oauth_success_response(status: StatusCode, body: Value, cors: bool) -> Response {
    let mut res = (status, axum::Json(body)).into_response();
    if cors {
        res.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
    }
    apply_no_store(&mut res);
    stamp_oauth_hook(&mut res);
    res
}

// ---------------------------------------------------------------------------
// parameter shapes
// ---------------------------------------------------------------------------

/// fastify `request.query` / body params — repeated keys become arrays.
#[derive(Default)]
struct Params(HashMap<String, Vec<String>>);

impl Params {
    fn insert(&mut self, key: String, value: String) {
        self.0.entry(key).or_default().push(value);
    }

    /// `firstValue` — first element when string-typed.
    fn first(&self, key: &str) -> Option<String> {
        self.0.get(key)?.first().cloned()
    }

    /// `normalizeScope` — flatMap split /\s+/ over every occurrence.
    fn scopes(&self, key: &str) -> Vec<String> {
        self.0
            .get(key)
            .map(|vals| {
                vals.iter()
                    .flat_map(|v| v.split_whitespace())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// `parseUrlEncodedParameters` — '+' decodes to space (URLSearchParams).
fn parse_url_encoded(raw: &str) -> Params {
    let mut params = Params::default();
    for pair in raw.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        params.insert(percent_decode_query(k), percent_decode_query(v));
    }
    params
}

fn percent_decode_query(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push(((h << 4) | l) as u8);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// fastify JSON body → `toRequestParameters` (string fields and arrays of
/// strings kept; non-object bodies → empty).
fn params_from_json(v: &Value) -> Params {
    let mut params = Params::default();
    if let Some(obj) = v.as_object() {
        for (k, val) in obj {
            match val {
                Value::String(s) => params.insert(k.clone(), s.clone()),
                Value::Array(items) if items.iter().all(|i| i.is_string()) => {
                    for i in items {
                        params.insert(k.clone(), i.as_str().unwrap_or("").to_string());
                    }
                }
                _ => {}
            }
        }
    }
    params
}

/// query-string body dispatch: urlencoded string parse, JSON object parse,
/// anything else → empty (toRequestParameters).
fn parse_request_params(content_type: Option<&str>, body: &[u8]) -> Params {
    let ct = content_type.unwrap_or("").to_ascii_lowercase();
    if ct.starts_with("application/x-www-form-urlencoded") {
        return parse_url_encoded(&String::from_utf8_lossy(body));
    }
    if ct.starts_with("application/json") {
        if let Ok(v) = serde_json::from_slice::<Value>(body) {
            return params_from_json(&v);
        }
        return Params::default();
    }
    // fastify: an unknown/absent content-type leaves request.body undefined
    // (the CORS probe POST lands here) → toRequestParameters(undefined) = {}
    if body.is_empty() {
        return Params::default();
    }
    // node-fetch JSON bodies ride application/json; string bodies would be
    // text/plain — upstream's parser hands those to the JSON parser and
    // nulls the body, so fall back to the urlencoded attempt then empty.
    let s = String::from_utf8_lossy(body);
    if s.trim_start().starts_with('{') {
        if let Ok(v) = serde_json::from_str::<Value>(&s) {
            return params_from_json(&v);
        }
    }
    Params::default()
}

// ---------------------------------------------------------------------------
// client-id validation + discovery
// ---------------------------------------------------------------------------

/// `validateClientId` — IndieAuth client identifier rules (the pinned test
/// client uses `http://127.0.0.1:<port>/`; https required outside test).
fn validate_client_id(raw: &str, test: bool) -> Result<Url, OAuthError> {
    let url = Url::parse(raw)
        .map_err(|_| OAuthError::invalid_request("client_id must be a valid URL"))?;

    // NODE_ENV !== 'test' → https only; the harness runs test: true.
    let scheme_ok = if test {
        matches!(url.scheme(), "http" | "https")
    } else {
        url.scheme() == "https"
    };
    if !scheme_ok {
        return Err(OAuthError::invalid_request(
            "client_id must be a valid HTTPS URL",
        ));
    }

    if url.path().split('/').any(|s| s == "." || s == "..") {
        return Err(OAuthError::invalid_request(
            "client_id must not contain dot path segments",
        ));
    }

    if !url.fragment().unwrap_or("").is_empty() {
        return Err(OAuthError::invalid_request(
            "client_id must not contain a fragment component",
        ));
    }

    if !url.username().is_empty() || url.password().is_some() {
        return Err(OAuthError::invalid_request(
            "client_id must not contain a username or a password",
        ));
    }

    // "host names MUST be domain names or a loopback interface and MUST NOT
    // be IPv4 or IPv6 addresses except for IPv4 127.0.0.1 or IPv6 [::1]" —
    // the upstream regex wants a dotted domain (`/\.\w+$/`) or the three
    // allowlisted loopback hosts.
    let host = url.host_str().unwrap_or("");
    let domain_like = host.contains('.')
        && host
            .rsplit('.')
            .next()
            .map(|tld| {
                !tld.is_empty() && tld.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
            .unwrap_or(false);
    if !domain_like && !["localhost", "127.0.0.1", "::1"].contains(&host) {
        return Err(OAuthError::invalid_request(
            "client_id must have a domain name as a host name",
        ));
    }

    Ok(url)
}

/// `#resolveAuthorizationRequest` IP guard — dns.lookup + ipaddr
/// `range() !== 'unicast'`. The pinned spec only drives 127.0.0.1 here
/// (loopback ≠ unicast → reject); non-global resolutions map to the reject.
fn ip_range_allowed(host_or_ip: &str) -> bool {
    match (host_or_ip, 0u16).to_socket_addrs() {
        Ok(mut addrs) => match addrs.next() {
            Some(addr) => ipaddr_is_global(addr.ip()),
            None => false,
        },
        Err(_) => false,
    }
}

/// Stable stand-in for the unstable `IpAddr::is_global` (issue #27709):
/// rejects the ipaddr.js `range() !== 'unicast'` set — private, loopback,
/// link-local (incl. fe80::/10 without the std `ip` feature), multicast,
/// broadcast, documentation and unspecified addresses. Not exhaustive over
/// RFC reserves the nightly gate covers; the pinned e2e spec only drives
/// 127.0.0.1 (loopback → reject) and public hostnames here.
fn ipaddr_is_global(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_multicast()
                || v4.is_unspecified())
        }
        IpAddr::V6(v6) => {
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (v6.segments()[0] & 0xffc0) == 0xfe80)
        }
    }
}

fn check_ip_range_needed(test: bool) -> bool {
    // upstream: NODE_ENV !== 'test' || MISSKEY_TEST_CHECK_IP_RANGE === '1'
    !test || CHECK_IP_RANGE.load(Ordering::Relaxed)
}

// --- tiny HTML scans (node-html-parser selectors over the pinned client
// documents: link[rel=redirect_uri][href], .h-app .p-name, .h-app .u-logo).
// All scans ride an ASCII-lowered copy (byte-length stable, so offsets map
// 1:1 onto the original text — case-sensitive values slice from `text`). ---

struct Span {
    start: usize,
    /// index just past the open tag's '>'
    end: usize,
}

fn find_open_tag(lower: &str, from: usize, tag: &str) -> Option<Span> {
    let mut at = from;
    let needle = format!("<{}", tag);
    while let Some(rel) = lower[at..].find(&needle) {
        let open = at + rel;
        // boundary check: next char must not extend the tag name
        let after = lower
            .as_bytes()
            .get(open + needle.len())
            .copied()
            .unwrap_or(b'>');
        if after.is_ascii_alphanumeric() || after == b':' || after == b'-' {
            at = open + needle.len();
            continue;
        }
        let close = lower[open..].find('>')? + open;
        return Some(Span {
            start: open,
            end: close + 1,
        });
    }
    None
}

/// next open tag (at/after `from`) whose class attribute contains `needle`.
fn find_class_from(lower: &str, from: usize, needle: &str) -> Option<Span> {
    let mut at = from;
    while let Some(idx) = lower[at..].find(needle) {
        let needle_at = at + idx;
        // enclosing open tag: the last '<' before the needle, its '>' after
        let tag_start = lower[..needle_at].rfind('<')?;
        let tag_close = lower[tag_start..].find('>')? + tag_start;
        let open_tag = &lower[tag_start..=tag_close];
        if let Some(cx) = open_tag.find("class=") {
            let value = open_tag[cx + 6..].trim_start();
            let (raw, _) = if let Some(stripped) = value.strip_prefix('"') {
                (stripped, true)
            } else if let Some(stripped) = value.strip_prefix('\'') {
                (stripped, true)
            } else {
                (value, false)
            };
            let end = raw.find(['"', '\'', '>']).unwrap_or(raw.len());
            let class = &raw[..end];
            if class.split_whitespace().any(|seg| seg == needle) {
                return Some(Span {
                    start: tag_start,
                    end: tag_close + 1,
                });
            }
        }
        at = tag_start + 1;
    }
    None
}

fn tag_attributes(tag: &str) -> Vec<(String, String)> {
    let mut attrs = Vec::new();
    let bytes = tag.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'<') {
            i += 1;
        }
        let name_start = i;
        while i < bytes.len()
            && bytes[i] != b'='
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'>'
        {
            i += 1;
        }
        if name_start == i {
            i += 1;
            continue;
        }
        let name = tag[name_start..i].to_ascii_lowercase();
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'=') {
            i += 1;
        }
        let value = if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
            let quote = bytes[i];
            i += 1;
            let vstart = i;
            while i < bytes.len() && bytes[i] != quote {
                i += 1;
            }
            let v = tag[vstart..i].to_string();
            i += 1;
            v
        } else {
            let vstart = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                i += 1;
            }
            tag[vstart..i].to_string()
        };
        attrs.push((name, value));
    }
    attrs
}

fn attr_value<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

fn resolve_against(base: &Url, href: &str) -> Option<String> {
    Some(
        Url::parse(href)
            .or_else(|_| base.join(href))
            .ok()?
            .to_string(),
    )
}

/// `parseMicroformats` + `<link rel=redirect_uri>` scan → (uris, name, logo).
fn parse_client_html(
    text: &str,
    base: &Url,
    client_id_url: &Url,
) -> (Vec<String>, Option<String>, Option<String>) {
    let mut redirect_uris: Vec<String> = Vec::new();
    let mut name: Option<String> = None;
    let mut logo: Option<String> = None;
    // ascii-lowered copy: byte-length stable, so every span computed here
    // indexes `text` 1:1 (attribute/tag casing can never shift offsets)
    let lower = text.to_ascii_lowercase();
    let id_norm = client_id_url.to_string();

    let mut at = 0usize;
    while let Some(tag) = find_open_tag(&lower, at, "link") {
        let attrs = tag_attributes(&text[tag.start..tag.end]);
        if attr_value(&attrs, "rel") == Some("redirect_uri") {
            if let Some(href) = attr_value(&attrs, "href") {
                redirect_uris.push(href.to_string());
            }
        }
        at = tag.end;
    }

    if let Some(h_app) = find_class_from(&lower, 0, "h-app") {
        if let Some(p_name) = find_class_from(&lower, h_app.start, "p-name") {
            let attrs = tag_attributes(&text[p_name.start..p_name.end]);
            let href = attr_value(&attrs, "href")
                .or_else(|| attr_value(&attrs, "src"))
                .map(|s| s.to_string());
            if let Some(resolved) = href.as_deref().and_then(|h| resolve_against(base, h)) {
                if resolved == id_norm {
                    // textContent: the chunk after the open tag up to the
                    // next '<', trimmed (the pinned anchors are plain text)
                    let tail = &text[p_name.end..];
                    let end = tail.find('<').unwrap_or(tail.len());
                    name = Some(tail[..end].trim().to_string());
                }
            }
        }
        if let Some(u_logo) = find_class_from(&lower, h_app.start, "u-logo") {
            let attrs = tag_attributes(&text[u_logo.start..u_logo.end]);
            let href = attr_value(&attrs, "href")
                .or_else(|| attr_value(&attrs, "src"))
                .map(|s| s.to_string());
            if let Some(resolved) = href.as_deref().and_then(|h| resolve_against(base, h)) {
                logo = Some(resolved);
            }
        }
    }

    (redirect_uris, name, logo)
}

/// httpLinkHeader.parse(link).get('rel', 'redirect_uri') — the pinned
/// headers are comma-joined `<uri>; rel="redirect_uri"` items.
fn parse_link_header(header_value: &str) -> Vec<String> {
    let mut uris = Vec::new();
    for item in header_value.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let mut parts = item.split(';');
        let target = parts.next().unwrap_or("").trim();
        if !(target.starts_with('<') && target.ends_with('>')) {
            continue;
        }
        for param in parts {
            let param = param.trim();
            if let Some(rel) = param.strip_prefix("rel=") {
                if rel.trim_matches('"') == "redirect_uri" {
                    uris.push(target[1..target.len() - 1].to_string());
                }
            }
        }
    }
    uris
}

/// `discoverClientInformation` — GET the client_id URL and shape the
/// ClientInformation (Link header + JSON metadata or HTML microformats).
async fn discover_client_information(id: &Url) -> Result<ClientInformation, OAuthError> {
    let res = http_client()
        .get(id.to_string())
        .send()
        .await
        .map_err(|_| OAuthError::parse_failed())?;

    // StatusError → 'Failed to fetch client information'
    if !res.status().is_success() {
        return Err(OAuthError::fetch_failed());
    }

    let final_url: Url = Url::parse(res.url().as_str()).unwrap_or_else(|_| id.clone());

    let mut redirect_uris_raw: Vec<String> = Vec::new();
    if let Some(link) = res
        .headers()
        .get(header::LINK)
        .and_then(|v| v.to_str().ok())
    {
        redirect_uris_raw.extend(parse_link_header(link));
    }

    let id_str = id.to_string();
    let media_type = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.split(';').next().unwrap_or("").trim().to_lowercase());

    let mut name: Option<String> = None;
    let mut logo: Option<String> = None;

    if media_type.as_deref() == Some("application/json") {
        // JSON client metadata (11 July 2024 spec). The json()/prefix
        // mismatch failures throw OAuthProviderError inside upstream's
        // discovery and are rethrown (NOT wrapped) — direct 400s below.
        // reqwest is built without the `json` feature — read the body and
        // parse explicitly (mirrors the other raw-response readers here).
        let body_text = res.text().await.map_err(|_| OAuthError::parse_failed())?;
        let json: Value =
            serde_json::from_str(&body_text).map_err(|_| OAuthError::parse_failed())?;
        let s = |k: &str| -> Option<String> {
            json.get(k).and_then(|v| v.as_str()).map(|v| v.to_owned())
        };

        if s("client_id").as_deref() != Some(id_str.as_str()) {
            return Err(OAuthError::invalid_request(
                "client_id in the document does not match the client_id URL",
            ));
        }

        match s("client_uri") {
            Some(client_uri) if id_str.starts_with(&client_uri) => {}
            _ => {
                return Err(OAuthError::invalid_request(
                    "client_uri is not a prefix of client_id",
                ));
            }
        }

        if let Some(client_name) = s("client_name") {
            name = Some(client_name);
        }
        if let Some(logo_uri) = s("logo_uri") {
            logo = resolve_against(&final_url, &logo_uri);
        }
        if let Some(uris) = json.get("redirect_uris").and_then(|v| v.as_array()) {
            for uri in uris.iter().filter_map(|u| u.as_str()) {
                redirect_uris_raw.push(uri.to_string());
            }
        }
    } else {
        // HTML microformats (12 February 2022 spec)
        let text = res.text().await.map_err(|_| OAuthError::parse_failed())?;
        let (link_uris, html_name, html_logo) = parse_client_html(&text, &final_url, &id);
        redirect_uris_raw.extend(link_uris);
        if !text.is_empty() {
            if let Some(n) = html_name {
                name = Some(n);
            }
            if let Some(l) = html_logo {
                logo = Some(l);
            }
        }
    }

    let redirect_uris = redirect_uris_raw
        .iter()
        .filter_map(|uri| resolve_against(&final_url, uri))
        .collect();

    Ok(ClientInformation {
        id: id_str,
        redirect_uris,
        name: name.unwrap_or_else(|| id.clone().to_string()),
        logo,
    })
}

// ---------------------------------------------------------------------------
// PKCE
// ---------------------------------------------------------------------------

/// PKCE S256 — BASE64URL(SHA256(verifier)) vs challenge (pkce-challenge
/// verifyChallenge byte contract).
pub(crate) fn s256_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64url_nopad(&digest)
}

fn base64url_nopad(bytes: &[u8]) -> String {
    const TBL: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TBL[(n >> 18) as usize & 63] as char);
        out.push(TBL[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(TBL[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(TBL[n as usize & 63] as char);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// bearer resolution (AuthenticateService non-native branch, ApiCallService
// kind gate) — consumed by mod.rs notes/create
// ---------------------------------------------------------------------------

/// access_token row resolution: `hash = lower(token)` OR `token = token`
/// (AuthenticateService L60-66 findOne OR array).
pub(crate) struct AccessTokenRow {
    pub user_id: String,
    pub permission: Vec<String>,
}

pub(crate) async fn resolve_access_token(
    pool: &mk_db::sqlx::PgPool,
    token: &str,
) -> Result<Option<AccessTokenRow>, mk_db::sqlx::Error> {
    let row: Option<(String, Vec<String>)> = mk_db::sqlx::query_as(
        r#"SELECT "userId", "permission" FROM "access_token"
           WHERE "hash" = $1 OR "token" = $2 LIMIT 1"#,
    )
    .bind(token.to_lowercase())
    .bind(token)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(user_id, permission)| AccessTokenRow {
        user_id,
        permission,
    }))
}

/// `#revokeGrantCode` DB leg — drop the token row minted for the grant.
async fn delete_access_token(
    pool: &mk_db::sqlx::PgPool,
    token: &str,
) -> Result<(), mk_db::sqlx::Error> {
    mk_db::sqlx::query(r#"DELETE FROM "access_token" WHERE "token" = $1"#)
        .bind(token)
        .execute(pool)
        .await
        .map(|_| ())
}

// ---------------------------------------------------------------------------
// handlers
// ---------------------------------------------------------------------------

/// WellKnownServerService-style host() origin: config.url minus trailing '/'.
fn issuer_origin(state: &AppState) -> String {
    state.config.url.trim_end_matches('/').to_string()
}

/// OAuthPage meta block — the e2e getMeta() parser reads
/// `[misskey:oauth:transaction-id|client-name|client-logo]`; scope rides the
/// upstream page too. Attribute values escaped like the tsx serializer.
fn authorize_html(
    transaction_id: &str,
    client_name: &str,
    client_logo: Option<&str>,
    scope: &[String],
) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    let mut head = format!(
        "<meta name=\"misskey:oauth:transaction-id\" content=\"{}\"/>",
        esc(transaction_id)
    );
    head.push_str(&format!(
        "<meta name=\"misskey:oauth:client-name\" content=\"{}\"/>",
        esc(client_name)
    ));
    if let Some(logo) = client_logo {
        head.push_str(&format!(
            "<meta name=\"misskey:oauth:client-logo\" content=\"{}\"/>",
            esc(logo)
        ));
    }
    head.push_str(&format!(
        "<meta name=\"misskey:oauth:scope\" content=\"{}\"/>",
        esc(&scope.join(" "))
    ));
    format!(
        "<!DOCTYPE html><html><head><title>Authorization</title>{}</head><body></body></html>",
        head
    )
}

/// `#resolveAuthorizationRequest` — validation through client discovery and
/// the redirect_uri allow-list (everything that fails BEFORE the validated
/// redirect uri is capturable → direct error responses).
async fn resolve_authorization_request(
    state: &AppState,
    params: &Params,
) -> Result<
    (
        ClientInformation,
        String,
        Option<String>,
        Vec<String>,
        Option<String>,
        Option<String>,
    ),
    OAuthError,
> {
    let response_type = params.first("response_type");
    if response_type.as_deref() != Some("code") {
        return Err(OAuthError::unsupported_response_type());
    }

    let client_id = params
        .first("client_id")
        .ok_or_else(|| OAuthError::invalid_request("client_id must be provided"))?;

    let client_url = validate_client_id(&client_id, state.config.test)?;

    if check_ip_range_needed(state.config.test)
        && !ip_range_allowed(client_url.host_str().unwrap_or(""))
    {
        return Err(OAuthError::invalid_request(
            "client_id resolves to disallowed IP range.",
        ));
    }

    let client_info = discover_client_information(&client_url).await?;

    // explicit-list check (draft-ietf-oauth-security-topics §4.1.3)
    let redirect_uri = params.first("redirect_uri");
    let Some(redirect_uri) = redirect_uri else {
        return Err(OAuthError::invalid_request("Invalid redirect_uri"));
    };
    if !client_info.redirect_uris.iter().any(|u| *u == redirect_uri) {
        return Err(OAuthError::invalid_request("Invalid redirect_uri"));
    }

    Ok((
        client_info,
        redirect_uri,
        params.first("state"),
        params.scopes("scope"),
        params.first("code_challenge"),
        params.first("code_challenge_method"),
    ))
}

/// `#finalizeAuthorizationRequest` — scope filter + mandatory PKCE.
fn finalize_authorization_request(
    requested_scope: &[String],
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
) -> Result<(Vec<String>, String), OAuthError> {
    // upstream: [...new Set(scopes)].filter(in kinds) — Set keeps insertion
    // order, so first occurrences survive dedupe
    let mut seen = std::collections::HashSet::new();
    let scopes: Vec<String> = requested_scope
        .iter()
        .filter(|s| well_known::OAUTH_KINDS.contains(&s.as_str()))
        .filter(|s| seen.insert((*s).clone()))
        .cloned()
        .collect();
    if requested_scope.is_empty() || scopes.is_empty() {
        return Err(OAuthError::invalid_scope());
    }

    let Some(code_challenge) = code_challenge else {
        return Err(OAuthError::invalid_request(
            "`code_challenge` parameter is required",
        ));
    };
    if code_challenge_method.as_deref() != Some("S256") {
        return Err(OAuthError::invalid_request(
            "`code_challenge_method` parameter must be set as S256",
        ));
    }

    Ok((scopes, code_challenge))
}

/// GET /oauth/authorize.
async fn authorize(state: &AppState, req: Request) -> Response {
    let issuer = issuer_origin(state);
    let query = req.uri().query().unwrap_or("").to_string();
    let params = parse_url_encoded(&query);

    // upstream captures these INSIDE the try so only failures after a
    // successful resolve can redirect (indirect error)
    let mut validated_redirect_uri: Option<String> = None;
    let mut state_param: Option<String> = None;

    let flow =
        async {
            let (
                client_info,
                redirect_uri,
                st,
                requested_scope,
                code_challenge,
                code_challenge_method,
            ) = resolve_authorization_request(state, &params).await?;
            validated_redirect_uri = Some(redirect_uri.clone());
            state_param = st.clone();
            let (scopes, code_challenge) = finalize_authorization_request(
                &requested_scope,
                code_challenge,
                code_challenge_method,
            )?;

            let transaction_id = mk_core::services::signup::secure_rndstr(RND_LEN);
            oauth_state().set(
                &oauth_state().transactions,
                transaction_id.clone(),
                AuthorizationTransaction {
                    client: client_info.clone(),
                    request: AuthorizationRequest {
                        redirect_uri: redirect_uri.clone(),
                        state: st.clone(),
                        scopes: scopes.clone(),
                        code_challenge: code_challenge.clone(),
                    },
                },
            );

            Ok::<String, OAuthError>(authorize_html(
                &transaction_id,
                &client_info.name,
                client_info.logo.as_deref(),
                &scopes,
            ))
        }
        .await;

    match flow {
        Ok(html) => {
            let mut res = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                .body(axum::body::Body::from(html))
                .unwrap();
            apply_no_store(&mut res);
            stamp_oauth_hook(&mut res);
            res
        }
        Err(err) => {
            // indirect error only post-validation and never for
            // unsupported_response_type
            if let Some(redirect) = &validated_redirect_uri {
                if err.error != "unsupported_response_type" {
                    let mut payload: Vec<(&str, String)> = vec![("error", err.error.clone())];
                    if let Some(st) = &state_param {
                        payload.push(("state", st.clone()));
                    }
                    return redirect_with_query(redirect, &payload, &issuer);
                }
            }
            oauth_error_response(&err)
        }
    }
}

/// POST /oauth/decision.
async fn decision(state: &AppState, req: Request) -> Response {
    let issuer = issuer_origin(state);
    let content_type = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let bytes = match axum::body::to_bytes(req.into_body(), 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => {
            return oauth_error_response(&OAuthError::invalid_request("request is invalid"));
        }
    };
    let params = parse_request_params(content_type.as_deref(), &bytes);

    let flow = async {
        let transaction_id = params
            .first("transaction_id")
            .ok_or_else(|| OAuthError::invalid_request("Missing transaction ID"))?;

        let transaction = oauth_state()
            .get(&oauth_state().transactions, &transaction_id)
            .ok_or_else(OAuthError::forbidden_access_denied)?;
        oauth_state().delete(&oauth_state().transactions, &transaction_id);

        // cancel check rides BEFORE the login-token verification
        let cancel = params
            .first("cancel")
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        if cancel {
            let mut payload: Vec<(&str, String)> = vec![("error", "access_denied".to_string())];
            if let Some(st) = &transaction.request.state {
                payload.push(("state", st.clone()));
            }
            return Ok::<Response, OAuthError>(redirect_with_query(
                &transaction.request.redirect_uri,
                &payload,
                &issuer,
            ));
        }

        let login_token = params
            .first("login_token")
            .ok_or_else(|| OAuthError::invalid_request("No user"))?;

        let Some(pool) = &state.pool else {
            return Err(OAuthError::invalid_request("No such user"));
        };
        let user_id = mk_db::user::find_local_user_id_by_token(pool, &login_token)
            .await
            .ok()
            .flatten()
            .ok_or_else(|| OAuthError::invalid_request("No such user"))?;

        let code = mk_core::services::signup::secure_rndstr(RND_LEN);
        oauth_state().set(
            &oauth_state().grants,
            code.clone(),
            AuthorizationCodeGrant {
                client_id: transaction.client.id.clone(),
                user_id: user_id.clone(),
                redirect_uri: transaction.request.redirect_uri.clone(),
                code_challenge: transaction.request.code_challenge.clone(),
                scopes: transaction.request.scopes.clone(),
                granted_token: None,
                revoked: false,
                used: false,
            },
        );

        let mut payload: Vec<(&str, String)> = vec![("code", code)];
        if let Some(st) = &transaction.request.state {
            payload.push(("state", st.clone()));
        }
        Ok(redirect_with_query(
            &transaction.request.redirect_uri,
            &payload,
            &issuer,
        ))
    }
    .await;

    // any error on decision is direct — never a redirect
    match flow {
        Ok(res) => res,
        Err(err) => oauth_error_response(&err),
    }
}

/// `#revokeGrantCode` — cache delete + revoke flag + token row delete.
async fn revoke_grant_code(
    pool: Option<&mk_db::sqlx::PgPool>,
    grant: &mut AuthorizationCodeGrant,
    code: &str,
) {
    oauth_state().delete(&oauth_state().grants, code);
    grant.revoked = true;
    if let Some(token) = grant.granted_token.take() {
        if let Some(pool) = pool {
            if let Err(e) = delete_access_token(pool, &token).await {
                tracing::error!(target: "dbg_oauth", error = %e, "revoke grant token delete");
            }
        }
    }
}

/// POST /oauth/token (CORS-permissive; @fastify/cors default origin `*`).
async fn token_server(state: &AppState, req: Request) -> Response {
    let content_type = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let bytes = match axum::body::to_bytes(req.into_body(), 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => {
            return oauth_error_response(&OAuthError::invalid_request("request is invalid"));
        }
    };
    let params = parse_request_params(content_type.as_deref(), &bytes);

    let flow = async {
        let grant_type = params
            .first("grant_type")
            .ok_or_else(|| OAuthError::invalid_request("grant_type is required"))?;
        if grant_type != "authorization_code" {
            return Err(OAuthError::unsupported_grant_type());
        }

        let code = params.first("code").ok_or_else(OAuthError::invalid_grant)?;
        let client_id = params.first("client_id");
        let redirect_uri = params.first("redirect_uri");
        let code_verifier = params.first("code_verifier");

        let Some(grant) = oauth_state().get(&oauth_state().grants, &code) else {
            return Err(OAuthError::invalid_grant());
        };
        let mut grant = grant;

        // RFC6749 §4.1.2 — single-use codes: used flips on the CACHED row
        // (the next reuse attempt finds it and revokes)
        if grant.used {
            revoke_grant_code(state.pool.as_ref(), &mut grant, &code).await;
            return Err(OAuthError::invalid_grant());
        }
        grant.used = true;
        oauth_state().set(&oauth_state().grants, code.clone(), grant.clone());

        // RFC6749 §4.1.3
        if client_id.as_deref() != Some(grant.client_id.as_str())
            || redirect_uri.as_deref() != Some(grant.redirect_uri.as_str())
        {
            return Err(OAuthError::invalid_grant());
        }

        let Some(code_verifier) = code_verifier else {
            return Err(OAuthError::invalid_grant());
        };
        if s256_challenge(&code_verifier) != grant.code_challenge {
            return Err(OAuthError::invalid_grant());
        }

        let access_token = mk_core::services::signup::secure_rndstr(RND_LEN);

        let token_json;
        {
            let Some(pool) = &state.pool else {
                return Err(OAuthError::invalid_grant());
            };
            let id = mk_types::id::gen_aid_now();
            let inserted = mk_db::sqlx::query(
                r#"INSERT INTO "access_token"
                   ("id", "token", "hash", "userId", "lastUsedAt", "name", "permission")
                   VALUES ($1, $2, $3, $4, now(), $5, $6)"#,
            )
            .bind(&id)
            .bind(&access_token)
            .bind(&access_token)
            .bind(&grant.user_id)
            .bind(&grant.client_id)
            .bind(&grant.scopes)
            .execute(pool)
            .await;

            if let Err(e) = inserted {
                tracing::error!(target: "dbg_oauth", error = %e, "access_token insert");
                return Err(OAuthError::invalid_grant());
            }

            // parallel-revoke window: the code was revoked mid-exchange
            if grant.revoked {
                let _ = delete_access_token(pool, &access_token).await;
                return Err(OAuthError::invalid_grant());
            }

            token_json = json!({
                "access_token": access_token.clone(),
                "token_type": "Bearer",
                "scope": grant.scopes.join(" "),
            });
        }

        grant.granted_token = Some(access_token);
        oauth_state().set(&oauth_state().grants, code, grant);
        Ok(oauth_success_response(StatusCode::OK, token_json, true))
    }
    .await;

    match flow {
        Ok(res) => res,
        Err(err) => {
            let mut res = oauth_error_response(&err);
            // the CORS probe rides every token-server response
            res.headers_mut().insert(
                header::ACCESS_CONTROL_ALLOW_ORIGIN,
                HeaderValue::from_static("*"),
            );
            res
        }
    }
}

/// /oauth/* catchall — createServer `fastify.all('/*')` 404.
fn unknown_oauth_endpoint() -> Response {
    let body = json!({
        "error": {
            "message": "Unknown OAuth endpoint.",
            "code": "UNKNOWN_OAUTH_ENDPOINT",
            "id": "aa49e620-26cb-4e28-aad6-8cbcb58db147",
            "kind": "client",
        },
    });
    let mut res = (StatusCode::NOT_FOUND, axum::Json(body)).into_response();
    stamp_oauth_hook(&mut res);
    res
}

/// Router entry — `/oauth/{*rest}` any() (main.rs mounts this outside /api).
pub(crate) async fn dispatch(State(state): State<AppState>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    match (method.as_str(), path.as_str()) {
        ("GET", "/oauth/authorize") => authorize(&state, req).await,
        ("POST", "/oauth/decision") => decision(&state, req).await,
        ("POST", "/oauth/token") => token_server(&state, req).await,
        _ => unknown_oauth_endpoint(),
    }
}
