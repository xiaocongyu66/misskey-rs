//! Web-push delivery leg (upstream `core/PushNotificationService.ts`
//! `pushNotification` + `sendNotification`): list a user's sw subscriptions,
//! apply the subscription/type filter, then per row sign a VAPID JWT and POST
//! to the push endpoint over the minimal search_bridge-style TCP client.
//!
//! # Documented dead-ends (deliberate, this lane is the smoke shape)
//!
//! - **aes128gcm payload encryption (RFC 8291)**: NOT implemented. We send
//!   the VAPID `Authorization` header + `Content-Encoding: aes128gcm` with an
//!   EMPTY body — real push services reject such a request, but the wire
//!   shape this lane exercises (header set, 404/410 subscription pruning,
//!   error swallowing) is complete. Upstream payload envelope would be
//!   `{type, body, userId, dateTime: Date.now()}` JSON.
//! - **VAPID keypair**: `meta."swPrivateKey"` (base64url, 32-byte scalar)
//!   through [`crate::services::config::push_vapid_key`] gives a stable
//!   identity; when it is unset we log and keep the historical per-call
//!   random key. `sub` rides `meta.email` (`mailto:`), else the placeholder.
//! - **TLS**: the minimal client (mirroring
//!   [`crate::services::search_bridge`]) speaks plain HTTP only, so
//!   `https://` endpoints (everything upstream would actually deliver to,
//!   per `isValidEndpoint`) are skipped with a log; only `http://` endpoints
//!   get a real delivery attempt. A real client swap only changes `post_push`.
//!
//! Errors are logged and never propagated (upstream `.catch`), so callers
//! fire-and-forget this behind `tokio::spawn`.

use mk_db::sqlx::PgPool;

/// `pushNotification(userId, type, body)` — fetch subscriptions, filter, and
/// attempt one POST per surviving row. Returns the number of rows that got a
/// delivery attempt (post-filter, post-endpoint-parse); every transport /
/// status error is logged and swallowed. 404/410 answers delete the
/// subscription row (upstream `err.statusCode === 410` arm; 404 rides the
/// same "endpoint gone" prune).
pub async fn send_to_user(
    pool: &PgPool,
    user_id: &str,
    notif_type: &str,
    body: &serde_json::Value,
) -> usize {
    // Upstream bails before any fetch when worker config is absent; the
    // random fallback key below always "exists", so no early return here.
    let rows = match mk_db::sw::list_by_user(pool, user_id).await {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("push: list_by_user({user_id}): {e}");
            return 0;
        }
    };

    // The payload upstream would encrypt (RFC 8291 TODO) — built for shape
    // parity, then intentionally left unsent behind the empty body.
    let _payload = serde_json::json!({
        "type": notif_type,
        "body": body,
        "userId": user_id,
        "dateTime": chrono_ms(),
    });

    // Stable VAPID identity from meta when configured; the random key is the
    // documented fallback (new identity per send, but the leg keeps running).
    let key = match crate::services::config::push_vapid_key(pool).await {
        Some(key) => key,
        None => {
            eprintln!("push: meta.swPrivateKey unset/invalid — random per-send key");
            random_key32()
        }
    };
    let sub = crate::services::config::push_mailto(pool)
        .await
        .unwrap_or_else(|| "mailto:push@example.invalid".to_string());
    let mut attempted = 0usize;
    for row in &rows {
        // upstream L105-107: read-bookkeeping pushes need the user's opt-in
        if notif_type == "readAllNotifications" && !row.send_read_message {
            continue;
        }
        let Some((host, port, path, origin, tls)) = endpoint_parts(&row.endpoint) else {
            eprintln!("push: skip {} (unparseable endpoint)", row.endpoint);
            continue;
        };
        let Some(jwt) = crate::services::web_push::vapid_jwt(&origin, &sub, &key, chrono_ms())
        else {
            eprintln!("push: vapid_jwt failed for {}", row.endpoint);
            continue;
        };
        let Some(k) = crate::services::web_push::vapid_public_key(&key) else {
            eprintln!("push: vapid public key failed for {}", row.endpoint);
            continue;
        };
        attempted += 1;
        // upstream pushNotification body {type, body, userId, dateTime} sealed
        // per RFC 8291 against the subscription's p256dh/auth; an encryption
        // failure falls back to the empty body (logged once per send)
        let payload = serde_json::json!({
            "type": notif_type,
            "body": body,
            "userId": user_id,
            "dateTime": chrono_ms(),
        });
        let sealed = crate::services::web_push::encrypt_payload(
            serde_json::to_vec(&payload).unwrap_or_default().as_slice(),
            &random_key32(),
            &row.publickey,
            &row.auth,
        );
        let empty: Vec<u8> = Vec::new();
        let push_body = match &sealed {
            Some(bytes) => bytes.as_slice(),
            None => {
                eprintln!("push: payload encryption failed for {}", row.endpoint);
                empty.as_slice()
            }
        };
        let push_result = if tls {
            // https endpoints ride the shared TLS client
            let auth = format!("vapid t={jwt}, k={k}");
            crate::services::tls_http::post(
                &row.endpoint,
                &[
                    ("Authorization", auth.as_str()),
                    ("Content-Encoding", "aes128gcm"),
                    ("TTL", "2419200"),
                    ("Content-Type", "application/octet-stream"),
                ],
                push_body,
            )
            .await
        } else {
            post_push(&host, port, &path, &jwt, &k, push_body).await
        };
        match push_result {
            Ok(status) => {
                if status == 404 || status == 410 {
                    // upstream L131-139: endpoint gone → drop the subscription
                    match mk_db::sw::delete_subscription(
                        pool,
                        user_id,
                        &row.endpoint,
                        &row.auth,
                        &row.publickey,
                    )
                    .await
                    {
                        Ok(n) if n > 0 => eprintln!("push: pruned dead endpoint {}", row.endpoint),
                        Ok(_) => {}
                        Err(e) => eprintln!("push: prune {}: {e}", row.endpoint),
                    }
                }
            }
            Err(e) => eprintln!("push: deliver {}: {e}", row.endpoint),
        }
    }
    attempted
}

/// `scheme://host[:port]/path` -> (host, port, path, origin). Plain-http
/// delivery only (see module docs); `https://` yields None here.
fn endpoint_parts(endpoint: &str) -> Option<(String, u16, String, String, bool)> {
    let (rest, tls, default_port) = if let Some(r) = endpoint.strip_prefix("https://") {
        (r, true, 443)
    } else {
        (endpoint.strip_prefix("http://")?, false, 80)
    };
    let (authority, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            (h, p.parse::<u16>().ok()?)
        }
        _ => (authority, default_port),
    };
    if host.is_empty() {
        return None;
    }
    let scheme = if tls { "https" } else { "http" };
    let origin = format!("{scheme}://{authority}");
    Some((host.to_string(), port, path.to_string(), origin, tls))
}

/// One wire attempt: VAPID auth header + aes128gcm marker + empty body.
/// Returns the response status; transport errors surface as Err.
async fn post_push(
    host: &str,
    port: u16,
    path: &str,
    jwt: &str,
    k: &str,
    body: &[u8],
) -> Result<u16, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {host}:{port}\r\n\
         Authorization: vapid t={jwt}, k={k}\r\n\
         Content-Encoding: aes128gcm\r\n\
         TTL: 2419200\r\n\
         Content-Type: application/octet-stream\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        body.len()
    );
    let mut stream = tokio::net::TcpStream::connect((host, port))
        .await
        .map_err(|e| format!("connect: {e}"))?;
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| format!("write: {e}"))?;
    stream
        .write_all(body)
        .await
        .map_err(|e| format!("write body: {e}"))?;
    stream.flush().await.map_err(|e| format!("flush: {e}"))?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .map_err(|e| format!("read: {e}"))?;
    let head = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|idx| &raw[..idx])
        .ok_or_else(|| "malformed response (no head terminator)".to_string())?;
    let status_line = String::from_utf8_lossy(head);
    status_line
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| format!("malformed status line: {status_line:?}"))
}

/// Per-call VAPID secret — OS randomness (TODO(config): meta.swPrivateKey).
fn random_key32() -> [u8; 32] {
    use rand::RngCore;
    let mut key = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut key);
    key
}

/// Wall clock in ms (upstream `Date.now()`); no clock injection needed since
/// nothing here is asserted on time.
fn chrono_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_parts_splits_http_and_https() {
        let (host, port, path, origin, tls) =
            endpoint_parts("http://127.0.0.1:9/dead/endpoint").expect("parts");
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 9);
        assert_eq!(path, "/dead/endpoint");
        assert_eq!(origin, "http://127.0.0.1:9");
        assert!(!tls);
        // https rides the TLS client now: default port 443, tls flag set
        let (host, port, _, origin, tls) =
            endpoint_parts("https://push.example.test/send").expect("https parts");
        assert_eq!(
            (host.as_str(), port, origin.as_str(), tls),
            ("push.example.test", 443, "https://push.example.test", true)
        );
        // scheme-less / garbage still refuse
        assert!(endpoint_parts("push.example.test/send").is_none());
        assert!(endpoint_parts("http:///nohost").is_none());
    }
}
