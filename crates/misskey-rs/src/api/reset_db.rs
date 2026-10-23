//! `reset-db` (upstream `endpoints/reset-db.ts` @ ea2c3aed) — the test-only
//! database reset endpoint.
//!
//! Upstream gate: `requireCredential: false` and NO admin/role check — the
//! only guard is `if (process.env.NODE_ENV !== 'test') throw new
//! Error('NODE_ENV is not a test')`, an uncaught error ApiCallService turns
//! into the bare 500 `INTERNAL_ERROR` body. The same gate is preserved here:
//! `state.config.test` (the workspace's NODE_ENV=test mirror, same flag the
//! `_test/reset` harness route checks) — outside test mode the endpoint
//! answers 500 INTERNAL_ERROR and touches nothing. Keep it that way: this
//! truncates every table in the instance.
//!
//! Divergences from upstream, all documented:
//! - Redis `flushdb` is skipped: no redis client is wired into AppState
//!   (single-instance fanout is in-process here).
//! - The post-reset `metaService.fetch(true)` + `metaUpdated` internal event
//!   is skipped: there is no in-process meta cache or internal-event bus to
//!   refresh (meta reads hit the table directly).
//! - The truncate is `mk_db::user::truncate_all` (TRUNCATE ... RESTART
//!   IDENTITY CASCADE over every public table in one DO block) instead of
//!   upstream's 3-retried per-table `DELETE FROM ... CASCADE` — same
//!   end state for the test suites that drive this endpoint.
//! - The upstream trailing `await 1000ms` settle delay is kept.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// The endpoint names routed into this module (single source of truth for the
/// `reset-db` dispatch arm in mod.rs).
pub(crate) const RESET_DB_ENDPOINTS: &[&str] = &["reset-db"];

pub(crate) fn handles(name: &str) -> bool {
    RESET_DB_ENDPOINTS.contains(&name)
}

/// Entry from the `reset-db` dispatch arm in mod.rs.
pub(crate) async fn handle(state: &AppState) -> Response {
    // upstream: `if (process.env.NODE_ENV !== 'test') throw new
    // Error('NODE_ENV is not a test')` → uncaught → bare 500 INTERNAL_ERROR.
    if !state.config.test {
        tracing::warn!(target: "dbg_reset_db", "reset-db refused: NODE_ENV is not a test");
        return api_err(ApiError::internal_error());
    }
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    tracing::info!(target: "dbg_reset_db", "---- Resetting database...");
    if let Err(err) = mk_db::user::truncate_all(pool).await {
        tracing::error!(target: "dbg_reset_db", error = %err, "truncate_all");
        return api_err(ApiError::internal_error());
    }
    tracing::info!(target: "dbg_reset_db", "---- Database reset complete.");
    // upstream settle delay: `await new Promise(resolve => setTimeout(resolve, 1000))`.
    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
    // handler returned undefined → ApiCallService.send(undefined) → 204.
    StatusCode::NO_CONTENT.into_response()
}
