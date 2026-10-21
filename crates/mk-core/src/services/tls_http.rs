//! TLS-capable HTTP `GET` helpers shared by the federation lanes.
//!
//! The repo's other minimal clients (`mk_core::services::search_bridge` and
//! the plain-TCP path in `mk_ap::fetch`) speak HTTP/1.1 over bare TCP and
//! dead-end on `https://`. This module closes that gap with the workspace
//! `reqwest` client (rustls + webpki roots — no OpenSSL, no system cert
//! store), so `https://` actor keyIds and any future TLS fetch share one
//! connection pool and TLS session cache.
//!
//! Only `GET` is exposed on purpose: federation reads (actor documents,
//! WebFinger, nodeinfo) are the current need, and a GET-only surface keeps
//! the shared client free of upload/redirect-policy surprises.

use std::sync::LazyLock;
use std::time::Duration;

use serde_json::Value;

/// One process-wide client: rustls TLS, redirects followed (reqwest default,
/// up to 10), 15s total request timeout. Building performs no network I/O.
static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent(concat!("misskey-rs/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(15))
        .build()
        .expect("reqwest client (rustls backend) must build")
});

/// `GET url` with `Accept: application/activity+json`, parsed as JSON
/// (ActivityPub actor documents / WebFinger responses).
pub async fn get_json(url: &str) -> Result<Value, String> {
    let body = get(url, "application/activity+json").await?;
    serde_json::from_str(&body).map_err(|e| format!("tls http json {url}: {e}"))
}

/// `GET url` with a permissive `Accept` header, returning the body as text.
pub async fn get_text(url: &str) -> Result<String, String> {
    get(url, "text/plain, */*").await
}

/// `POST url` with the given headers and body; returns the status code.
/// Used by the push lane for the https WebPush endpoints.
pub async fn post(url: &str, headers: &[(&str, &str)], body: &[u8]) -> Result<u16, String> {
    let mut req = CLIENT.post(url).body(body.to_vec());
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    let response = req
        .send()
        .await
        .map_err(|e| format!("tls http post {url}: {e}"))?;
    Ok(response.status().as_u16())
}

async fn get(url: &str, accept: &str) -> Result<String, String> {
    let response = CLIENT
        .get(url)
        .header(reqwest::header::ACCEPT, accept)
        .send()
        .await
        .map_err(|e| format!("tls http get {url}: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("tls http get {url}: HTTP {}", status.as_u16()));
    }
    response
        .text()
        .await
        .map_err(|e| format!("tls http body {url}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A URL the parser rejects must produce an Err (never panic) before any
    /// network use — CI runs without network.
    #[tokio::test]
    async fn malformed_urls_error_cleanly() {
        for url in ["not a url", "https://", "://missing-scheme"] {
            let err = get_json(url).await.expect_err("malformed url must fail");
            assert!(err.starts_with("tls http"), "{url:?}: {err}");
        }
    }

    /// Loopback port 9 is closed, so the TLS client fails at connect without
    /// needing any external network.
    #[tokio::test]
    async fn unroutable_host_surfaces_a_transport_error() {
        let err = get_json("https://127.0.0.1:9/users/u")
            .await
            .expect_err("closed port");
        assert!(err.contains("tls http get"), "err: {err}");
    }
}
