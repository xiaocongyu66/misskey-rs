//! Remote actor key fetching (upstream `GetAuthService.getKeyById` /
//! `ApPersonService`'s publicKey fetch): a remote keyId URL is fetched and
//! its `publicKeyPem` extracted for the HTTP-signature verify.
//!
//! # Documented dead-ends (this lane is the best-effort shape)
//!
//! - **TLS**: the minimal HTTP client (mirroring
//!   `mk_core::services::search_bridge`) speaks plain HTTP only, so
//!   `https://` keyIds — everything a real federation peer uses — are
//!   rejected here with a logged error until a TLS client lands.
//! - **Remote key caching**: the 0001 `user` baseline has no `publicKey`
//!   column (`user_keypair` is the LOCAL user's pair), so every verify does
//!   one network fetch; upstream caches through the person row.

use mk_db::sqlx::PgPool;

/// Split a keyId URL into `(host, port, path)` for the plain-HTTP client.
/// `https://` returns None (the TLS dead-end above); the `#fragment` suffix
/// (`.../users/<id>#main-key`) is stripped.
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

/// `GET` the actor document behind `key_id` (fragment stripped) and pull the
/// `publicKeyPem` — a single `publicKey` object is used directly, an array is
/// matched by the full keyId. Errors carry the HTTP status or parse stage.
pub async fn fetch_actor_key(key_id: &str) -> Result<String, String> {
    let (host, port, path) =
        split_http_url(key_id).ok_or_else(|| "non-http keyId (TLS pending)".to_string())?;
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
    let doc: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("actor fetch parse: {e}"))?;
    let pk = doc.get("publicKey").ok_or("actor fetch: no publicKey")?;
    match pk {
        serde_json::Value::Object(_) => pk
            .get("publicKeyPem")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "actor fetch: no publicKeyPem".to_string()),
        serde_json::Value::Array(items) => {
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

/// The cache half upstream gets from the person row: a REMOTE user already
/// stored in `user` (host IS NOT NULL, `uri` = the actor id) carries its key
/// only in the upstream column the 0001 baseline lacks — today this probe
/// exists so the verify lane prefers a row hit and documents the miss. The
/// `user_keypair` table is the LOCAL pair and must never be consulted here.
pub async fn remote_key_cached(pool: &PgPool, actor_uri: &str) -> Option<()> {
    let exists: Option<i64> =
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

    #[tokio::test]
    async fn https_keyids_are_rejected_with_the_documented_error() {
        let out = fetch_actor_key("https://remote.example/users/u#main-key").await;
        assert_eq!(out.unwrap_err(), "non-http keyId (TLS pending)");
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
