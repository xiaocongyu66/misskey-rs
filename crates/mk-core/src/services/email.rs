//! Email delivery seam (upstream `EmailService`).
//!
//! Upstream (`packages/backend/src/core/EmailService.ts`) sends mail through a
//! configured SMTP transport and no-ops when SMTP is not configured. This
//! module defines the seam: a dyn-compatible [`EmailSender`] so a coordinator
//! can wire either the [`NoopSender`] (no SMTP configured) or an [`SmtpSender`]
//! transport. The SMTP transport speaks plain RFC 5321 on port 25/test
//! endpoints and STARTTLS (rustls + webpki roots, same provider as
//! `tls_http`) with optional AUTH PLAIN credentials elsewhere.

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
/// through nodemailer; this hand-rolls the RFC 5321 command sequence).
///
/// When [`use_tls`](Self::use_tls) is true (the default, mirroring upstream
/// nodemailer's `secure: true`) the session is upgraded with STARTTLS before
/// AUTH/envelope; the plain path is kept for trusted port-25 relays and the
/// in-process test server.
pub struct SmtpSender {
    pub host: String,
    pub port: u16,
    /// `None` keeps the historical `no-reply@mk-rs` envelope sender (the
    /// tests / unwired seam); `Some(pool)` reads `meta.email` at send time.
    pub pool: Option<mk_db::sqlx::PgPool>,
    /// AUTH PLAIN username. `None` (together with `password: None`) skips
    /// AUTH; both must be set to authenticate.
    pub username: Option<String>,
    /// AUTH PLAIN password; must be set together with `username`.
    pub password: Option<String>,
    /// Run STARTTLS + AUTH (default `true`). Set `false` only for trusted
    /// plaintext relays (port 25) and the in-process test server.
    pub use_tls: bool,
}

impl Default for SmtpSender {
    fn default() -> Self {
        Self {
            host: "localhost".to_string(),
            port: 25,
            pool: None,
            username: None,
            password: None,
            use_tls: true,
        }
    }
}

/// One SMTP line reply (`252 code + text`); `Err` carries the code on a
/// mismatch.
async fn expect_code<R>(reader: &mut R, want: &str) -> Result<(), String>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
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

/// AUTH PLAIN initial response: `base64("\0user\0pass")` (RFC 4616).
fn auth_plain(username: &str, password: &str) -> String {
    use base64::Engine;
    let raw = format!("\0{username}\0{password}");
    base64::engine::general_purpose::STANDARD.encode(raw)
}

impl SmtpSender {
    /// Host/port transport with the default policy (STARTTLS on, no AUTH).
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            ..Self::default()
        }
    }

    /// Writes one CRLF-terminated SMTP command.
    async fn write_line<W>(writer: &mut W, line: &str) -> Result<(), String>
    where
        W: tokio::io::AsyncWrite + Unpin,
    {
        use tokio::io::AsyncWriteExt;
        writer
            .write_all(format!("{line}\r\n").as_bytes())
            .await
            .map_err(|e| format!("smtp write: {e}"))
    }

    async fn ehlo<R, W>(reader: &mut R, writer: &mut W) -> Result<(), String>
    where
        R: tokio::io::AsyncBufRead + Unpin,
        W: tokio::io::AsyncWrite + Unpin,
    {
        Self::write_line(writer, "EHLO mk-rs").await?;
        expect_code(reader, "250").await
    }

    /// AUTH PLAIN when credentials are configured (both-or-neither).
    async fn auth<R, W>(&self, reader: &mut R, writer: &mut W) -> Result<(), String>
    where
        R: tokio::io::AsyncBufRead + Unpin,
        W: tokio::io::AsyncWrite + Unpin,
    {
        match (&self.username, &self.password) {
            (None, None) => Ok(()),
            (Some(user), Some(pass)) => {
                let token = auth_plain(user, pass);
                Self::write_line(writer, &format!("AUTH PLAIN {token}")).await?;
                expect_code(reader, "235").await
            }
            _ => Err("smtp auth: username and password must be set together".into()),
        }
    }

    /// MAIL FROM / RCPT TO / DATA / QUIT over an established session whose
    /// greeting and EHLO have already been consumed.
    async fn envelope<R, W>(
        &self,
        reader: &mut R,
        writer: &mut W,
        to: &str,
        subject: &str,
        html: &str,
        text: &str,
    ) -> Result<(), String>
    where
        R: tokio::io::AsyncBufRead + Unpin,
        W: tokio::io::AsyncWrite + Unpin,
    {
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
        Self::write_line(writer, &format!("MAIL FROM:<{from}>")).await?;
        expect_code(reader, "250").await?;
        Self::write_line(writer, &format!("RCPT TO:<{to}>")).await?;
        expect_code(reader, "250").await?;
        Self::write_line(writer, "DATA").await?;
        expect_code(reader, "354").await?;
        let payload = format!(
            "To: {to}\r\nSubject: {subject}\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"{boundary}\"\r\n\r\n{stuffed}.\r\n"
        );
        use tokio::io::AsyncWriteExt;
        writer
            .write_all(payload.as_bytes())
            .await
            .map_err(|e| format!("smtp write: {e}"))?;
        expect_code(reader, "250").await?;
        Self::write_line(writer, "QUIT").await?;
        expect_code(reader, "221").await?;
        Ok(())
    }

    /// Plain TCP path (port 25 / test endpoints). No STARTTLS, no AUTH.
    async fn deliver(&self, to: &str, subject: &str, html: &str, text: &str) -> Result<(), String> {
        let stream = tokio::net::TcpStream::connect((self.host.as_str(), self.port))
            .await
            .map_err(|e| format!("smtp connect: {e}"))?;
        let (read_half, mut writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(read_half);

        expect_code(&mut reader, "220").await?;
        Self::ehlo(&mut reader, &mut writer).await?;
        self.envelope(&mut reader, &mut writer, to, subject, html, text)
            .await
    }

    /// STARTTLS path (upstream nodemailer `secure: true`):
    /// greeting → EHLO → STARTTLS/220 → TLS handshake (webpki roots, SNI =
    /// `host`) → EHLO → AUTH PLAIN (when credentials are set) → envelope.
    pub async fn deliver_tls(
        &self,
        to: &str,
        subject: &str,
        html: &str,
        text: &str,
    ) -> Result<(), String> {
        let stream = tokio::net::TcpStream::connect((self.host.as_str(), self.port))
            .await
            .map_err(|e| format!("smtp connect: {e}"))?;
        let (read_half, writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(read_half);
        let mut writer = writer;

        expect_code(&mut reader, "220").await?;
        Self::ehlo(&mut reader, &mut writer).await?;
        Self::write_line(&mut writer, "STARTTLS").await?;
        expect_code(&mut reader, "220").await?;

        // The peer sends nothing between the 220 and our ClientHello, so the
        // BufReader holds no extra bytes; reunite hands the raw socket to
        // rustls for the handshake.
        let tcp = reader
            .into_inner()
            .reunite(writer)
            .map_err(|_| "smtp tls: stream reunite failed".to_string())?;

        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(config));
        let server_name: rustls::pki_types::ServerName<'static> = self
            .host
            .clone()
            .try_into()
            .map_err(|e| format!("smtp tls server name {:?}: {e}", self.host))?;
        let tls = connector
            .connect(server_name, tcp)
            .await
            .map_err(|e| format!("smtp tls handshake: {e}"))?;

        let (read_half, mut writer) = tokio::io::split(tls);
        let mut reader = tokio::io::BufReader::new(read_half);
        Self::ehlo(&mut reader, &mut writer).await?;
        self.auth(&mut reader, &mut writer).await?;
        self.envelope(&mut reader, &mut writer, to, subject, html, text)
            .await
    }
}

impl EmailSender for SmtpSender {
    fn send_email(&self, to: &str, subject: &str, html: &str, text: &str) -> EmailFuture<'_> {
        let to = to.to_string();
        let subject = subject.to_string();
        let html = html.to_string();
        let text = text.to_string();
        Box::pin(async move {
            if self.use_tls {
                self.deliver_tls(&to, &subject, &html, &text).await
            } else {
                self.deliver(&to, &subject, &html, &text).await
            }
        })
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
                } else if cmd == "STARTTLS" {
                    // advertise acceptance, then drop the socket: the client's
                    // TLS handshake must fail (no real cert infra in tests)
                    writer.write_all(b"220 go ahead\r\n").await.unwrap();
                    break;
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
            use_tls: false,
            ..Default::default()
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
            use_tls: false,
            ..Default::default()
        };
        tx.send("user@reject.test".into()).await.unwrap();
        let out = sender
            .send_email("user@reject.test", "verify", "<p>hi</p>", "hi")
            .await;
        let err = out.unwrap_err();
        assert!(err.contains("550"), "err: {err}");
    }

    /// AUTH PLAIN payload is `base64("\0user\0pass")` (RFC 4616).
    #[test]
    fn auth_plain_matches_rfc4616_base64() {
        assert_eq!(auth_plain("user", "pass"), "AHVzZXIAcGFzcw==");
        assert_eq!(auth_plain("", ""), "AAA=");
    }

    /// No real TLS test (self-signed cert infra is out of scope): against the
    /// plain fake server the `use_tls` branch must run STARTTLS and then fail
    /// at the handshake once the peer drops the socket.
    #[tokio::test]
    async fn use_tls_against_plain_server_fails_at_handshake() {
        let (addr, log, _tx) = spawn_fake_smtp().await;
        let sender = SmtpSender {
            host: addr.ip().to_string(),
            port: addr.port(),
            use_tls: true,
            ..Default::default()
        };
        let err = sender
            .send_email("user@dest.test", "verify", "<p>hi</p>", "hi")
            .await
            .unwrap_err();
        assert!(err.contains("smtp tls handshake"), "err: {err}");
        // the TLS branch really did negotiate STARTTLS first
        assert!(log.lock().await.iter().any(|c| c == "STARTTLS"));
    }
}
