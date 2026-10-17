//! Ajv-parity validation of Misskey `paramDef` JSON Schemas, including the
//! Misskey-specific `nullable`/`optional` keywords, `useDefaults` behaviour,
//! the `ref` cross-reference, and the `misskey:id` format
//! (`/^[a-zA-Z0-9]+$/` — deliberately NOT length-limited; length checks live
//! at entity-lookup time).
//!
//! Populated from M2 on, together with the CI codegen registry.
