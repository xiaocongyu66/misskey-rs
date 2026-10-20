//! ActivityPub: renderer, resolver, signed HTTP (draft-cavage-11), inbox
//! processing, audience resolution, JSON-LD-lite traversal.
//! Isolates the heavy crypto/HTTP dependency tree (from M8 on).

// fed-min: the draft-cavage-11 signature-verify skeleton (upstream
// ApRequestService.ts #genSigningString + InboxProcessorService verify).
pub mod signature;
