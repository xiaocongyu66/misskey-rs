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

/// SMTP transport sender — not implemented yet.
///
/// TODO: port upstream's SMTP transport (nodemailer) wiring behind this seam.
pub struct SmtpSender;

impl EmailSender for SmtpSender {
    fn send_email(&self, _to: &str, _subject: &str, _html: &str, _text: &str) -> EmailFuture<'_> {
        Box::pin(async { Err("smtp transport not implemented".to_string()) })
    }
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

    #[tokio::test]
    async fn smtp_sender_reports_unimplemented() {
        let sender = SmtpSender;
        let out = sender
            .send_email("a@b.c", "subject", "<p>hi</p>", "hi")
            .await;
        assert_eq!(out.unwrap_err(), "smtp transport not implemented");
    }
}
