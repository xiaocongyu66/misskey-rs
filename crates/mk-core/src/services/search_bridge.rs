//! Note search engine bridge (upstream `SearchService` / Meilisearch wiring).
//!
//! Upstream (`packages/backend/src/core/SearchService.ts`, `MeilisearchService`)
//! pushes created/updated/deleted notes into a Meilisearch index when it is
//! configured, and text search endpoints fall back to SQL `ILIKE` otherwise.
//! This module defines the seam those endpoints will call: a dynamic
//! [`NoteSearchEngine`] so the coordinator can wire either the [`NoopEngine`]
//! (no search backend; endpoints keep their SQL fallbacks) or the minimal
//! [`MeilisearchEngine`].
//!
//! # Engine config contract (`engine_from_config`)
//!
//! Accepts the **top-level config map as `serde_json::Value`** (or the
//! `meilisearch` subtree itself):
//!
//! - no `meilisearch.host` (or an empty one) -> [`NoopEngine`]
//! - `meilisearch.host` set -> [`MeilisearchEngine`]:
//!   - `host`: bare host or full URL; scheme-less hosts get `http://`,
//!     `meilisearch.ssl: true` upgrades to `https://`; `meilisearch.port`
//!     is appended unless the host already carries one (default 7700).
//!   - `apiKey` (or `api_key`): sent as `Authorization: Bearer` when present.
//!   - `index`: note index uid, default `notes` (upstream's default).
//!
//! # Minimal HTTP client (no new dependencies)
//!
//! The repo had no HTTP client dependency when this was written, so instead
//! of pulling reqwest the engine speaks HTTP/1.1 over plain TCP through
//! tokio (already a workspace dependency). It covers Meilisearch's default
//! `http://localhost:7700` shape: `PUT /indexes/:uid/documents?primaryKey=id`
//! (auto-creates the index), `DELETE /indexes/:uid/documents/:id` and
//! `POST /indexes/:uid/search`. `https://` hosts fail at request time — put
//! TLS behind a reverse proxy on localhost, or swap the transport for a real
//! client later (only `request_async` would change).
//!
//! # Coordinator wiring (NOT done here; do not edit main.rs from sessions)
//!
//! Next session, `crates/misskey-rs/src/main.rs` should carry the engine on
//! `AppState`. Exact one-line diff (field + init):
//!
//! ```text
//! // AppState (crates/misskey-rs/src/main.rs):
//! pub search_engine: std::sync::Arc<dyn mk_core::services::search_bridge::NoteSearchEngine>,
//! // init (same file, in the `let state = AppState { ... }` literal):
//! search_engine: mk_core::services::search_bridge::engine_from_config(&config.raw_json),
//! ```
//!
//! ...with `config.raw_json` exposed by `crates/misskey-rs/src/config.rs`
//! (`serde_yaml::from_str::<serde_json::Value>(&raw)?` alongside the typed
//! parse — see [`engine_from_config`] for the accepted shapes).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;

/// Dyn-compatible (boxed-future) async trait so `Arc<dyn NoteSearchEngine>`
/// can live in shared state without an `async-trait` dependency.
pub type EngineFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A note search backend: index maintenance add/remove plus query.
pub trait NoteSearchEngine: Send + Sync + 'static {
    /// Upserts one note document (`primaryKey=id`; the doc must carry `id`).
    fn add<'a>(&'a self, id: &'a str, doc: Value) -> EngineFuture<'a, Result<(), SearchError>>;
    /// Removes a note from the index.
    fn remove<'a>(&'a self, id: &'a str) -> EngineFuture<'a, Result<(), SearchError>>;
    /// Full-text query; returns raw hit documents (upstream packs them into
    /// `Note` responses at the caller).
    fn search<'a>(
        &'a self,
        query: &'a str,
        limit: u64,
    ) -> EngineFuture<'a, Result<Vec<Value>, SearchError>>;
    /// Whether this is the [`NoopEngine`] (no search backend configured):
    /// index writes are skipped and queries keep the endpoints' SQL
    /// fallbacks — upstream's `if (!this.meilisearch) return` arms. Defaults
    /// to `false`, so real engines (and test doubles) ride the engine lane.
    fn is_noop(&self) -> bool {
        false
    }
}

#[derive(Debug)]
pub enum SearchError {
    Io(std::io::Error),
    /// Notable case: the minimal client only speaks plain HTTP (see module
    /// docs), so `https://` endpoints produce this.
    Url(String),
    Http {
        status: u16,
        body: String,
    },
    Encoding(serde_json::Error),
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchError::Io(err) => write!(f, "search engine i/o error: {err}"),
            SearchError::Url(what) => write!(f, "search engine transport error: {what}"),
            SearchError::Http { status, body } => {
                write!(f, "search engine http error {status}: {body}")
            }
            SearchError::Encoding(err) => write!(f, "search engine payload error: {err}"),
        }
    }
}

impl std::error::Error for SearchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SearchError::Io(err) => Some(err),
            SearchError::Encoding(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for SearchError {
    fn from(err: std::io::Error) -> Self {
        SearchError::Io(err)
    }
}

impl From<serde_json::Error> for SearchError {
    fn from(err: serde_json::Error) -> Self {
        SearchError::Encoding(err)
    }
}

/// Engine used when no search backend is configured: index writes are
/// accepted and dropped, queries return nothing (callers keep their SQL
/// fallbacks, matching upstream behavior without Meilisearch).
pub struct NoopEngine;

impl NoteSearchEngine for NoopEngine {
    fn is_noop(&self) -> bool {
        true
    }

    fn add<'a>(&'a self, _id: &'a str, _doc: Value) -> EngineFuture<'a, Result<(), SearchError>> {
        Box::pin(std::future::ready(Ok(())))
    }

    fn remove<'a>(&'a self, _id: &'a str) -> EngineFuture<'a, Result<(), SearchError>> {
        Box::pin(std::future::ready(Ok(())))
    }

    fn search<'a>(
        &'a self,
        _query: &'a str,
        _limit: u64,
    ) -> EngineFuture<'a, Result<Vec<Value>, SearchError>> {
        Box::pin(std::future::ready(Ok(Vec::new())))
    }
}

/// Minimal Meilisearch client bound to one index (documented in module docs).
#[derive(Debug, Clone)]
pub struct MeilisearchEngine {
    /// Scheme://host:port, no trailing slash.
    base: String,
    api_key: Option<String>,
    index: String,
}

impl MeilisearchEngine {
    pub fn new(base: String, api_key: Option<String>, index: String) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            api_key,
            index,
        }
    }

    async fn request_async(
        &self,
        method: &'static str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, SearchError> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (host, port) = split_base(&self.base)?;
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nConnection: close\r\n"
        );
        if let Some(key) = &self.api_key {
            request.push_str(&format!("Authorization: Bearer {key}\r\n"));
        }
        let body_bytes = match body {
            Some(body) => serde_json::to_vec(body)?,
            None => Vec::new(),
        };
        request.push_str(&format!("Content-Length: {}\r\n\r\n", body_bytes.len()));

        let mut stream = tokio::net::TcpStream::connect((host.as_str(), port)).await?;
        stream.write_all(request.as_bytes()).await?;
        stream.write_all(&body_bytes).await?;
        stream.flush().await?;

        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await?;
        let (head, body) = split_head_and_body(&raw)?;
        let status = parse_status(head)?;
        if status >= 400 {
            return Err(SearchError::Http {
                status,
                body: String::from_utf8_lossy(body).into_owned(),
            });
        }
        Ok(if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(body)?
        })
    }
}

/// `scheme://host[:port]` -> host + port (default 80). Plain HTTP only.
fn split_base(base: &str) -> Result<(String, u16), SearchError> {
    let rest = base
        .strip_prefix("http://")
        .ok_or_else(|| {
            SearchError::Url(format!(
                "minimal Meilisearch client speaks plain http:// only, got {base:?}"
            ))
        })?
        .trim_end_matches('/');
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            (h.to_string(), p.parse::<u16>().unwrap_or(80))
        }
        _ => (rest.to_string(), 80),
    };
    Ok((host, port))
}

fn split_head_and_body(raw: &[u8]) -> Result<(&[u8], &[u8]), SearchError> {
    raw.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|idx| (&raw[..idx + 4], &raw[idx + 4..]))
        .ok_or_else(|| SearchError::Url("malformed http response (no head terminator)".into()))
}

fn parse_status(head: &[u8]) -> Result<u16, SearchError> {
    let line = String::from_utf8_lossy(head).into_owned();
    line.lines()
        .next()
        .and_then(|status_line| status_line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| SearchError::Url(format!("malformed http status line: {line:?}")))
}

impl NoteSearchEngine for MeilisearchEngine {
    /// Index auto-creation comes free with the first document push (upstream
    /// Meilisearch behavior), so an explicit `POST /indexes` round-trip is
    /// not needed for the gateway case.
    fn add<'a>(&'a self, _id: &'a str, doc: Value) -> EngineFuture<'a, Result<(), SearchError>> {
        Box::pin(async move {
            self.request_async(
                "PUT",
                &format!("/indexes/{}/documents?primaryKey=id", self.index),
                Some(&Value::Array(vec![doc])),
            )
            .await
            .map(|_| ())
        })
    }

    fn remove<'a>(&'a self, id: &'a str) -> EngineFuture<'a, Result<(), SearchError>> {
        Box::pin(async move {
            self.request_async(
                "DELETE",
                &format!("/indexes/{}/documents/{id}", self.index),
                None,
            )
            .await
            .map(|_| ())
        })
    }

    fn search<'a>(
        &'a self,
        query: &'a str,
        limit: u64,
    ) -> EngineFuture<'a, Result<Vec<Value>, SearchError>> {
        Box::pin(async move {
            let body = serde_json::json!({ "q": query, "limit": limit });
            let response = self
                .request_async(
                    "POST",
                    &format!("/indexes/{}/search", self.index),
                    Some(&body),
                )
                .await?;
            Ok(response
                .get("hits")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default())
        })
    }
}

/// Builds the engine from config (`engine_from_config` contract is spelled
/// out in the module docs).
pub fn engine_from_config(cfg: &Value) -> Arc<dyn NoteSearchEngine> {
    // Accept either the full config map or the meilisearch subtree itself.
    let meili = cfg.get("meilisearch").unwrap_or(cfg);
    let host = meili
        .get("host")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|host| !host.is_empty());
    let Some(host) = host else {
        return Arc::new(NoopEngine);
    };

    let want_tls = meili.get("ssl").and_then(Value::as_bool).unwrap_or(false);
    let (scheme, rest) = if let Some(rest) = host.strip_prefix("http://") {
        ("http", rest)
    } else if let Some(rest) = host.strip_prefix("https://") {
        ("https", rest)
    } else if want_tls {
        ("https", host)
    } else {
        ("http", host)
    };
    let rest = rest.trim_end_matches('/');

    let (host_part, explicit_port) = match rest.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            (h.to_string(), Some(p.parse::<u16>().unwrap_or(7700)))
        }
        _ => (rest.to_string(), None),
    };
    let port =
        explicit_port.unwrap_or(meili.get("port").and_then(Value::as_u64).unwrap_or(7700) as u16);
    let api_key = meili
        .get("apiKey")
        .or_else(|| meili.get("api_key"))
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty())
        .map(str::to_string);
    let index = meili
        .get("index")
        .and_then(Value::as_str)
        .filter(|index| !index.is_empty())
        .unwrap_or("notes")
        .to_string();

    Arc::new(MeilisearchEngine::new(
        format!("{scheme}://{host_part}:{port}"),
        api_key,
        index,
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn no_config_resolves_to_noop() {
        let hits = engine_from_config(&json!({}))
            .search("aso", 10)
            .await
            .unwrap();
        assert!(hits.is_empty());
        let hits = engine_from_config(&json!({ "meilisearch": { "host": "" } }))
            .search("aso", 10)
            .await
            .unwrap();
        assert!(hits.is_empty());
        // Subtree form also resolves.
        let hits = engine_from_config(&json!({ "host": "" }))
            .search("aso", 10)
            .await
            .unwrap();
        assert!(hits.is_empty());
    }

    /// Host set -> the real engine is built; an unroutable port makes the
    /// transport failure observable without any network dependency.
    #[tokio::test]
    async fn host_config_resolves_to_meilisearch_engine() {
        let engine = engine_from_config(&json!({
            "meilisearch": { "host": "127.0.0.1", "port": 1 }
        }));
        let err = engine.search("aso", 10).await.expect_err("connect refused");
        assert!(matches!(err, SearchError::Io(_)));

        // Full-URL hosts are honored, scheme-less defaults to http:// with the
        // configured/standalone port; https is rejected by the minimal client.
        assert!(matches!(
            engine_from_config(&json!({ "meilisearch": { "host": "https://meili.internal" } }))
                .search("aso", 10)
                .await,
            Err(SearchError::Url(_))
        ));

        let err = engine_from_config(&json!({
            "meilisearch": { "host": "http://127.0.0.1:1", "apiKey": "secret" }
        }))
        .remove("note-id")
        .await
        .expect_err("connect refused");
        assert!(matches!(err, SearchError::Io(_)));
    }
}
