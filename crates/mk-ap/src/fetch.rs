//! Remote actor key fetching (upstream `GetAuthService.getKeyById` /
//! `ApPersonService`'s publicKey fetch): a remote keyId URL is fetched and
//! its `publicKeyPem` extracted for the HTTP-signature verify.
//!
//! # Transports
//!
//! - **`https://`** keyIds — everything a real federation peer uses — ride the
//!   shared TLS client in [`mk_core::services::tls_http`] (reqwest + rustls).
//!   The old TLS dead-end is closed: a redirect is followed and the
//!   `#fragment` suffix (`.../users/<id>#main-key`) is dropped by the URL
//!   parser before the request.
//! - **`http://`** keyIds keep the original minimal plain-TCP client below.
//!   It remains for loopback test endpoints (and is the zero-TLS path).
//! - Any other scheme is rejected with an error.
//!
//! # Documented dead-ends (this lane is the best-effort shape)
//!
//! - **Remote key caching**: the 0001 `user` baseline has no `publicKey`
//!   column (`user_keypair` is the LOCAL user's pair), so upstream's
//!   person-row cache has nowhere to live; `KeyCache` is the in-process
//!   stand-in (15 min TTL, 1024-entry sweep) behind `fetch_actor_key_cached`.

use mk_core::services::tls_http;
use mk_db::sqlx::PgPool;
use once_cell::sync::Lazy;
use serde_json::Value;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a fetched remote `publicKeyPem` stays usable before a re-fetch.
const KEY_CACHE_TTL: Duration = Duration::from_secs(15 * 60);
/// Insert-time sweep threshold: past this many entries, expired ones go.
const KEY_CACHE_CAPACITY: usize = 1024;

/// Process-global TTL cache for remote actor keys (the 0001 baseline has no
/// `publicKey` column, so upstream's person-row cache cannot be mirrored).
pub struct KeyCache {
    entries: tokio::sync::Mutex<HashMap<String, (String, Instant)>>,
}

impl KeyCache {
    pub fn new() -> Self {
        Self {
            entries: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// A fresh entry's PEM; `None` on a miss or once the TTL has elapsed.
    pub async fn get(&self, key_id: &str) -> Option<String> {
        let entries = self.entries.lock().await;
        entries
            .get(key_id)
            .filter(|(_, at)| at.elapsed() < KEY_CACHE_TTL)
            .map(|(pem, _)| pem.clone())
    }

    /// Insert (or refresh) an entry; over capacity, first sweep expired rows.
    pub async fn put(&self, key_id: &str, pem: String) {
        let mut entries = self.entries.lock().await;
        if entries.len() > KEY_CACHE_CAPACITY {
            entries.retain(|_, (_, at)| at.elapsed() < KEY_CACHE_TTL);
        }
        entries.insert(key_id.to_string(), (pem, Instant::now()));
    }
}

impl Default for KeyCache {
    fn default() -> Self {
        Self::new()
    }
}

/// The one process-wide cache (all verifies share it).
pub static KEY_CACHE: Lazy<KeyCache> = Lazy::new(KeyCache::new);

/// Split an `http://` keyId URL into `(host, port, path)` for the plain-TCP
/// client below. `https://` returns None (those ride the TLS client); the
/// `#fragment` suffix (`.../users/<id>#main-key`) is stripped.
fn split_http_url(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("http://")?;
    let no_frag = rest.split('#').next()?;
    let (authority, path) = match no_frag.split_once('/') {
        Some((a, p)) => (a, format!("/{}", p)),
        None => (no_frag, "/".to_string()),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().ok()?),
        None => (authority.to_string(), 80),
    };
    Some((host, port, path))
}

/// `GET` the actor document behind `key_id` (fragment stripped by the
/// respective transport) and pull the `publicKeyPem` — a single `publicKey`
/// object is used directly, an array is matched by the full keyId. Errors
/// carry the transport/HTTP status or parse stage.
pub async fn fetch_actor_key(key_id: &str) -> Result<String, String> {
    let doc = if key_id.starts_with("https://") {
        tls_http::get_json(key_id).await?
    } else if key_id.starts_with("http://") {
        fetch_http_doc(key_id).await?
    } else {
        return Err(format!("actor fetch: unsupported keyId scheme ({key_id})"));
    };
    extract_public_key(&doc, key_id)
}

/// Plain-TCP `GET` for `http://` keyIds (loopback test endpoints).
async fn fetch_http_doc(key_id: &str) -> Result<Value, String> {
    let (host, port, path) =
        split_http_url(key_id).ok_or_else(|| "actor fetch: bad http keyId".to_string())?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nAccept: application/activity+json\r\nConnection: close\r\n\r\n"
    );
    let mut stream = tokio::net::TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|e| format!("actor fetch connect: {e}"))?;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| format!("actor fetch write: {e}"))?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .map_err(|e| format!("actor fetch read: {e}"))?;
    // split headers/body on the first CRLFCRLF
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("actor fetch: no header/body split")?;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let body = &raw[split + 4..];
    let status: u16 = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or("actor fetch: bad status line")?;
    if status >= 400 {
        return Err(format!("actor fetch: HTTP {status}"));
    }
    serde_json::from_slice(body).map_err(|e| format!("actor fetch parse: {e}"))
}

/// Pull `publicKeyPem` from an actor document. A single `publicKey` object is
/// used directly; an array is matched by the full keyId, falling back to its
/// first entry.
fn extract_public_key(doc: &Value, key_id: &str) -> Result<String, String> {
    let pk = doc.get("publicKey").ok_or("actor fetch: no publicKey")?;
    match pk {
        Value::Object(_) => pk
            .get("publicKeyPem")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "actor fetch: no publicKeyPem".to_string()),
        Value::Array(items) => {
            let frag_suffix = format!(" {key_id}");
            items
                .iter()
                .find(|k| {
                    k.get("id")
                        .and_then(|v| v.as_str())
                        .map(|id| key_id.ends_with(id) || frag_suffix.contains(id))
                        .unwrap_or(false)
                })
                .or_else(|| items.first())
                .and_then(|k| k.get("publicKeyPem"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .ok_or_else(|| "actor fetch: no publicKeyPem in array".to_string())
        }
        _ => Err("actor fetch: publicKey shape".to_string()),
    }
}

/// Cache-first variant of [`fetch_actor_key`]: a hit skips the network, a miss
/// fetches the raw PEM and stores it in the process-global [`KEY_CACHE`].
pub async fn fetch_actor_key_cached(key_id: &str) -> Result<String, String> {
    if let Some(pem) = KEY_CACHE.get(key_id).await {
        return Ok(pem);
    }
    let pem = fetch_actor_key(key_id).await?;
    KEY_CACHE.put(key_id, pem.clone()).await;
    Ok(pem)
}

/// The cache half upstream gets from the person row: a REMOTE user already
/// stored in `user` (host IS NOT NULL, `uri` = the actor id) carries its key
/// only in the upstream column the 0001 baseline lacks — today this probe
/// exists so the verify lane prefers a row hit and documents the miss. The
/// `user_keypair` table is the LOCAL pair and must never be consulted here.
pub async fn remote_key_cached(pool: &PgPool, actor_uri: &str) -> Option<()> {
    // SELECT 1 is INT4 — an i64 decode fails and .ok() would swallow the hit
    let exists: Option<i32> =
        sqlx::query_scalar(r#"SELECT 1 FROM "user" WHERE "uri" = $1 AND "host" IS NOT NULL"#)
            .bind(actor_uri)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    // no publicKey column on the 0001 baseline → a hit still needs the fetch,
    // so the probe only reports that the row exists
    exists.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// https keyIds now ride the TLS client, so the old fixed "TLS pending"
    /// string is gone. Loopback port 9 is closed, pinning a transport Err
    /// without any external network.
    #[tokio::test]
    async fn https_keyids_surface_transport_errors() {
        let out = fetch_actor_key("https://127.0.0.1:9/users/u#main-key").await;
        let err = out.expect_err("closed loopback port must fail");
        assert!(err.contains("tls http get"), "err: {err}");
    }

    /// A malformed https URL errors cleanly (no panic) before any network.
    #[tokio::test]
    async fn malformed_https_url_errors_cleanly() {
        let err = fetch_actor_key("https://").await.expect_err("no host");
        assert!(err.contains("tls http"), "err: {err}");
    }

    #[test]
    fn split_http_url_strips_the_fragment() {
        let (host, port, path) =
            split_http_url("http://remote.example:8128/users/u#main-key").expect("split");
        assert_eq!(
            (host.as_str(), port, path.as_str()),
            ("remote.example", 8128, "/users/u")
        );
        assert!(split_http_url("https://secure.example/users/u").is_none());
    }

    /// A fake plain-HTTP actor on an ephemeral port: counts the GETs it serves
    /// and answers every one with the same actor document.
    #[tokio::test]
    async fn cached_fetch_skips_the_second_network_round_trip() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::AsyncWriteExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake actor");
        let port = listener.local_addr().expect("local addr").port();
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                seen.fetch_add(1, Ordering::SeqCst);
                let body = r#"{"publicKey":{"publicKeyPem":"TEST-PEM"}}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/activity+json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });

        // the ephemeral port makes this keyId unique, so the process-global
        // cache cannot collide with another test's entries
        let key_id = format!("http://127.0.0.1:{port}/users/u#main-key");
        let first = fetch_actor_key_cached(&key_id).await.expect("first fetch");
        let second = fetch_actor_key_cached(&key_id).await.expect("cached fetch");
        assert_eq!(first, "TEST-PEM");
        assert_eq!(second, "TEST-PEM");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "the cached second lookup must not hit the network"
        );
    }

    #[tokio::test]
    async fn expired_entries_are_a_miss() {
        let cache = KeyCache::new();
        let key_id = "http://remote.example/users/u#main-key";
        cache.put(key_id, "OLD-PEM".to_string()).await;
        // age the entry past the TTL directly instead of sleeping in the test
        cache.entries.lock().await.insert(
            key_id.to_string(),
            (
                "OLD-PEM".to_string(),
                Instant::now() - KEY_CACHE_TTL - Duration::from_secs(1),
            ),
        );
        assert_eq!(cache.get(key_id).await, None);
    }

    #[tokio::test]
    async fn remote_key_cached_hits_only_for_remote_rows() {
        if std::env::var("DATABASE_URL").is_err() {
            return;
        }
        let base = std::env::var("DATABASE_URL").unwrap();
        let pool = mk_db::test_support::fresh_db("mktest_ap_keys").await;
        mk_db::run_migrations(&pool).await.unwrap();
        let _ = &base;
        let uid = mk_types::id::gen_aid_now();
        sqlx::query(r#"INSERT INTO "user" ("id", "username", "usernameLower", "host", "uri") VALUES ($1, $2, $2, $3, $4)"#)
            .bind(&uid)
            .bind("remuser")
            .bind("remote.example")
            .bind("http://remote.example/users/u")
            .execute(&pool)
            .await
            .unwrap();
        // a hit reports the row (the fetch still needs to run)
        assert_eq!(
            remote_key_cached(&pool, "http://remote.example/users/u").await,
            Some(())
        );
        // a local-only uri misses entirely
        assert_eq!(
            remote_key_cached(&pool, "http://remote.example/users/other").await,
            None
        );
    }
}
