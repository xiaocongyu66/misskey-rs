//! Inbox POST dispatch skeleton.
//!
//! Upstream: `packages/backend/src/core/activitypub/ApInboxService.ts`
//! `performOneActivity` is the type dispatch — Create/Delete/Update/Follow/
//! Accept/Reject/Add/Remove/Announce/Like/Undo/Block/Flag/Move arms — and an
//! unrecognized type is NOT an error: it returns
//! `` `unrecognized activity type: ${activity.type}` `` as the *job reason*.
//! The HTTP route (`ActivityPubServerService.ts` fastify.post `/inbox` and
//! `/users/:user/inbox`) parses the body, enqueues, and answers 202 — the
//! per-type work and the signature verify run worker-side
//! (InboxProcessorService). This skeleton mirrors that split: parse the JSON,
//! extract `type`, dispatch; unknown types and unparseable bodies are
//! accepted silently (202 parity).

/// The `performOneActivity` arms this slice pins. The rest of the upstream
/// table (Update/Accept/Reject/Add/Remove/Announce/Block/Flag/Move) classifies
/// as [`ActivityKind::Other`] until their worker lanes land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    Follow,
    Create,
    Like,
    Undo,
    Delete,
    Other,
}

impl ActivityKind {
    /// The upstream arm name, for logs and the dispatch-table pins.
    pub fn as_str(self) -> &'static str {
        match self {
            ActivityKind::Follow => "Follow",
            ActivityKind::Create => "Create",
            ActivityKind::Like => "Like",
            ActivityKind::Undo => "Undo",
            ActivityKind::Delete => "Delete",
            ActivityKind::Other => "Other",
        }
    }
}

/// What the HTTP handler needs: the classified arm (everything upstream
/// accepts answers 202, so no status field — the handler is constant-202).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InboxOutcome {
    pub kind: ActivityKind,
}

/// `performOneActivity`'s type dispatch, parse-free.
pub fn classify(activity: &serde_json::Value) -> ActivityKind {
    match activity.get("type").and_then(|t| t.as_str()) {
        Some("Follow") => ActivityKind::Follow,
        Some("Create") => ActivityKind::Create,
        Some("Like") => ActivityKind::Like,
        Some("Undo") => ActivityKind::Undo,
        Some("Delete") => ActivityKind::Delete,
        _ => ActivityKind::Other,
    }
}

/// Parse the posted body, extract `type`, dispatch. An unparseable body still
/// yields an accepted outcome: upstream the fastify route enqueued the job
/// before any parse could fail in the worker, so the HTTP answer never turns
/// into a 4xx (InboxProcessorService's parse failure is a Bull job error, not
/// a response status).
pub fn handle(body: &[u8]) -> InboxOutcome {
    let activity = serde_json::from_slice(body).unwrap_or(serde_json::Value::Null);
    InboxOutcome {
        kind: classify(&activity),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_table_matches_perform_one_activity_arms() {
        for (ty, kind) in [
            ("Follow", ActivityKind::Follow),
            ("Create", ActivityKind::Create),
            ("Like", ActivityKind::Like),
            ("Undo", ActivityKind::Undo),
            ("Delete", ActivityKind::Delete),
        ] {
            assert_eq!(classify(&serde_json::json!({ "type": ty })), kind);
            assert_eq!(kind.as_str(), ty);
        }
        // the unlisted upstream arms ride Other (never an HTTP error)
        for ty in ["Update", "Accept", "Reject", "Add", "Remove", "Announce"] {
            assert_eq!(
                classify(&serde_json::json!({ "type": ty })),
                ActivityKind::Other
            );
        }
        // unrecognized type → still accepted (upstream: the job reason string)
        let unknown = handle(br#"{"type":"EvilActivity"}"#);
        assert_eq!(unknown.kind, ActivityKind::Other);
        // unparseable body → 202 parity, no 4xx
        let garbage = handle(b"not json");
        assert_eq!(garbage.kind, ActivityKind::Other);
    }
}
