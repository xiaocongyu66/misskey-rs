//! Job queue on Redis Streams (`mk:queue:{name}` + delayed ZSET + DLQ)
//! and the processors ported from `packages/backend/src/queue/processors/`.
//! Runs in-process or standalone via `misskey-rs --worker-only` (from M7 on).
