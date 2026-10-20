//! ActivityPub: renderer, resolver, signed HTTP (draft-cavage-11), inbox
//! processing, audience resolution, JSON-LD-lite traversal.
//! Isolates the heavy crypto/HTTP dependency tree (from M8 on).

// fed-min: the draft-cavage-11 signature-verify skeleton (upstream
// ApRequestService.ts #genSigningString + InboxProcessorService verify).
pub mod signature;
// fed-min: the inbox POST dispatch skeleton (upstream ApInboxService
// .performOneActivity; HTTP side answers 202, worker side does the work).
pub mod inbox;
// fed-2: the worker-side processors with real effects (upstream
// InboxProcessorService verify + ApInboxService Create{Note}/Follow/Like/
// Undo arms over mk-db rows; remote fetches are the documented boundary).
pub mod processor;
