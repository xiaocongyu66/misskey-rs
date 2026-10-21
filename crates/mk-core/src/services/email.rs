//! Email delivery seam (upstream `EmailService`).
//!
//! Upstream (`packages/backend/src/core/EmailService.ts`) sends mail through a
//! configured SMTP transport and no-ops when SMTP is not configured. This
//! module defines the seam: a dyn-compatible [`EmailSender`] so a coordinator
//! can wire either the [`NoopSender`] (no SMTP configured) or a future
//! [`SmtpSender`] transport. Nothing is wired yet — the seam + contract tests
//! only.

use std::future::Future;
use std::pin::Pin;

/// Dyn-compatible (boxed-future) async trait shape so `Arc<dyn EmailSender>`
/// can live in shared state without an `async-trait` dependency (mirrors
/// `EngineFuture` in `search_bridge.rs`).
pub type EmailFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

/// Sends an email. Mirrors upstream `EmailService.sendEmail(to, subject, html, text)`.
pub trait EmailSender: Send + Sync {
    fn send_email(&self, to: &str, subject: &str, html: &str, text: &str) -> EmailFuture<'_>;
}

/// No-op sender: used when SMTP is not configured, mirroring upstream's
/// early-return when the transport is missing. Always succeeds.
pub struct NoopSender;

impl EmailSender for NoopSender {
    fn send_email(&self, _to: &str, _subject: &str, _html: &str, _text: &str) -> EmailFuture<'_> {
        Box::pin(async { Ok(()) })
    }
}

/// Minimal SMTP transport (upstream `EmailService.ts` builds the envelope
/// through nodemailer; this hand-rolls the RFC 5321 command sequence over a
/// plain tokio TCP stream — no AUTH/STARTTLS/8BITMIME extensions, TLS is a
/// documented TODO).
pub struct SmtpSender {
    pub host: String,
    pub port: u16,
    /// `None` keeps the historical `no-reply@mk-rs` envelope sender (the
    /// tests / unwired seam); `Some(pool)` reads `meta.email` at send time.
    pub pool: Option<mk_db::sqlx::PgPool>,
}

/// One SMTP line reply (`252 code + text`); `Err` carries the code on a
/// mismatch.
async fn expect_code(
    reader: &mut tokio::io::BufReader<tokio::net::tcp::OwnedReadHalf>,
    want: &str,
) -> Result<(), String> {
    use tokio::io::AsyncBufReadExt;
    let mut line = String::new();
    // last line of a reply has ' ' after the code; intermediate '-' lines loop
    loop {
        line.clear();
        reader
            .read_line(&mut line)
            .await
            .map_err(|e| format!("smtp read: {e}"))?;
        if line.len() < 4 {
            return Err(format!("smtp short reply: {line:?}"));
        }
        let (code, sep) = line.split_at(3);
        if code != want {
            return Err(format!("smtp expected {want}, got {code}"));
        }
        if sep.starts_with(' ') {
            return Ok(());
        }
    }
}

impl SmtpSender {
    async fn deliver(&self, to: &str, subject: &str, html: &str, text: &str) -> Result<(), String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let stream = tokio::net::TcpStream::connect((self.host.as_str(), self.port))
            .await
            .map_err(|e| format!("smtp connect: {e}"))?;
        let (read_half, mut writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(read_half);

        expect_code(&mut reader, "220").await?;
        let cmd = |s: String| format!("{s}\r\n");
        writer
            .write_all(cmd("EHLO mk-rs".into()).as_bytes())
            .await
            .map_err(|e| format!("smtp write: {e}"))?;
        expect_code(&mut reader, "250").await?;

        // minimal MIME multipart/alternative (text + html parts)
        let boundary = "mk-rs-alt-boundary";
        let body = format!(
            "--{boundary}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{text}\r\n--{boundary}\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{html}\r\n--{boundary}--\r\n"
        );
        // dot-stuffing: RFC 5321 §4.5.2 — a body line starting with '.' gets
        // one extra '.' prepended
        let stuffed: String = body
            .lines()
            .map(|l| {
                if l.starts_with('.') {
                    format!(".{l}\r\n")
                } else {
                    format!("{l}\r\n")
                }
            })
            .collect();

        // envelope sender from meta.email when a pool is wired, else the
        // historical literal (0001 has no meta.url to derive a host from)
        let from = match &self.pool {
            Some(pool) => crate::services::config::email_from(pool).await,
            None => crate::services::config::DEFAULT_EMAIL_FROM.to_string(),
        };
        writer
            .write_all(cmd(format!("MAIL FROM:<{from}>")).as_bytes())
            .await
            .map_err(|e| format!("smtp write: {e}"))?;
        expect_code(&mut reader, "250").await?;
        writer
            .write_all(cmd(format!("RCPT TO:<{to}>")).as_bytes())
            .await
            .map_err(|e| format!("smtp write: {e}"))?;
        expect_code(&mut reader, "250").await?;
        writer
            .write_all(cmd("DATA".into()).as_bytes())
            .await
            .map_err(|e| format!("smtp write: {e}"))?;
        expect_code(&mut reader, "354").await?;
        let payload = format!(
            "To: {to}\r\nSubject: {subject}\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"{boundary}\"\r\n\r\n{stuffed}.\r\n"
        );
        writer
            .write_all(payload.as_bytes())
            .await
            .map_err(|e| format!("smtp write: {e}"))?;
        expect_code(&mut reader, "250").await?;
        writer
            .write_all(cmd("QUIT".into()).as_bytes())
            .await
            .map_err(|e| format!("smtp write: {e}"))?;
        expect_code(&mut reader, "221").await?;
        let _ = &mut writer;
        Ok(())
    }
}

impl EmailSender for SmtpSender {
    fn send_email(&self, to: &str, subject: &str, html: &str, text: &str) -> EmailFuture<'_> {
        let to = to.to_string();
        let subject = subject.to_string();
        let html = html.to_string();
        let text = text.to_string();
        Box::pin(async move { self.deliver(&to, &subject, &html, &text).await })
    }
}

/// Alphabet/length of the signup verification code. The upstream email
/// section of `SignupService.ts` was unreadable within the skim budget, so
/// this lands the documented fallback shape: 8 lowercase alphanumerics.
const VERIFY_CODE_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
const VERIFY_CODE_LEN: usize = 8;

/// Generates the signup email verification code (8 lowercase alphanumerics).
pub fn generate_code() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..VERIFY_CODE_LEN)
        .map(|_| VERIFY_CODE_ALPHABET[rng.gen_range(0..VERIFY_CODE_ALPHABET.len())] as char)
        .collect()
}

/// email-leg verify: consumes the stored code for `user_id` when `code`
/// matches, flipping user_profile."emailVerified" (mk-db
/// `take_email_verify_code`). False when nothing matches (stale/unknown code).
pub async fn verify_email(pool: &mk_db::sqlx::PgPool, user_id: &str, code: &str) -> bool {
    matches!(
        mk_db::user::take_email_verify_code(pool, user_id, code).await,
        Ok(true)
    )
}

/// Password-reset request (upstream `request-reset-password.ts`): generates
/// a code, stores it in `password_reset_request` (30-minute TTL, see mk-db
/// `set_password_reset_code`) and emails it. Upstream sends a
/// `/reset-password/<64-char token>` link; this slice reuses the 8-char
/// signup code alphabet (`generate_code`) and mails the code itself — the
/// SMTP sender is still the NoopSender stand-in, so delivery is
/// fire-and-forget until a transport is wired.
pub async fn request_password_reset(
    pool: &mk_db::sqlx::PgPool,
    user_id: &str,
    email: &str,
) -> Result<(), String> {
    let code = generate_code();
    mk_db::user::set_password_reset_code(pool, user_id, &code)
        .await
        .map_err(|e| format!("password reset code write: {e}"))?;
    let sender = NoopSender;
    sender
        .send_email(
            email,
            "Password reset requested",
            &format!("<p>To reset your password, use this code: <b>{code}</b></p>"),
            &format!("To reset your password, use this code: {code}"),
        )
        .await
}

#[cfg(test)]
mod email4_tests {
    use super::*;

    #[tokio::test]
    async fn noop_sender_succeeds() {
        let sender = NoopSender;
        let out = sender
            .send_email("a@b.c", "subject", "<p>hi</p>", "hi")
            .await;
        assert_eq!(out, Ok(()));
    }

    /// Fake SMTP server: canned replies, recorded commands. Returns the
    /// bound address + a shared log of received lines.
    async fn spawn_fake_smtp() -> (
        std::net::SocketAddr,
        std::sync::Arc<tokio::sync::Mutex<Vec<String>>>,
        tokio::sync::mpsc::Sender<String>,
    ) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let log = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        // rcpt_tx flips the RCPT reply to 550 (the rejection leg)
        let (rcpt_tx, mut rcpt_rx) = tokio::sync::mpsc::channel::<String>(1);
        let log2 = log.clone();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = sock.into_split();
            let mut reader = tokio::io::BufReader::new(reader);
            let mut line = String::new();
            let mut reject = false;
            writer.write_all(b"220 fake ready\r\n").await.unwrap();
            loop {
                line.clear();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    break;
                }
                let cmd = line.trim_end().to_string();
                log2.lock().await.push(cmd.clone());
                if cmd.starts_with("EHLO") {
                    writer.write_all(b"250 fake\r\n").await.unwrap();
                } else if cmd.starts_with("MAIL FROM") {
                    writer.write_all(b"250 ok\r\n").await.unwrap();
                } else if cmd.starts_with("RCPT TO") {
                    if let Ok(target) = rcpt_rx.try_recv() {
                        reject = target.ends_with("@reject.test");
                    }
                    if reject {
                        writer
                            .write_all(b"550 mailbox unavailable\r\n")
                            .await
                            .unwrap();
                    } else {
                        writer.write_all(b"250 ok\r\n").await.unwrap();
                    }
                } else if cmd == "DATA" {
                    writer.write_all(b"354 go\r\n").await.unwrap();
                } else if cmd == "." {
                    if reject {
                        // after the 550 the client stops; end the session
                        break;
                    }
                    writer.write_all(b"250 queued\r\n").await.unwrap();
                } else if cmd == "QUIT" {
                    writer.write_all(b"221 bye\r\n").await.unwrap();
                    break;
                }
            }
        });
        (addr, log, rcpt_tx)
    }

    #[tokio::test]
    async fn smtp_delivers_command_sequence_with_dot_stuffing() {
        let (addr, log, _tx) = spawn_fake_smtp().await;
        let sender = SmtpSender {
            host: addr.ip().to_string(),
            port: addr.port(),
            pool: None,
        };
        sender
            .send_email("user@dest.test", "verify", "<p>hi</p>", "..leading dot")
            .await
            .expect("deliver");
        let log = log.lock().await;
        let seq: Vec<&str> = log.iter().map(|s| s.as_str()).collect();
        assert_eq!(
            seq.first().copied().unwrap_or(""),
            "EHLO mk-rs",
            "commands: {seq:?}"
        );
        assert_eq!(seq.first(), Some(&"EHLO mk-rs"));
        assert!(
            seq.iter()
                .any(|c| c.starts_with("MAIL FROM:<no-reply@mk-rs>"))
        );
        assert!(seq.iter().any(|c| *c == "RCPT TO:<user@dest.test>"));
        assert!(seq.iter().any(|c| *c == "DATA"));
        assert_eq!(seq.last().copied(), Some("QUIT"));
        // the body's leading-dot line arrives dot-stuffed ("...leading dot")
        assert!(
            log.iter().any(|c| *c == "...leading dot"),
            "dot-stuffing: {log:?}"
        );
        // and the terminating "." was NOT stuffed
        assert!(log.iter().any(|c| *c == "."));
    }

    #[tokio::test]
    async fn smtp_rcpt_rejection_carries_the_code() {
        let (addr, _log, tx) = spawn_fake_smtp().await;
        let sender = SmtpSender {
            host: addr.ip().to_string(),
            port: addr.port(),
            pool: None,
        };
        tx.send("user@reject.test".into()).await.unwrap();
        let out = sender
            .send_email("user@reject.test", "verify", "<p>hi</p>", "hi")
            .await;
        let err = out.unwrap_err();
        assert!(err.contains("550"), "err: {err}");
    }
}
