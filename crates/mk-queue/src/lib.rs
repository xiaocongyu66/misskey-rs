//! PG-backed durable job queue (`mk_queue_jobs`) + in-process worker.
//!
//! # Divergence from upstream (documented, intentional)
//!
//! Upstream Misskey brokers jobs through Redis + BullMQ
//! (`packages/backend/src/queue/*`: system, webhooks, db, deliver, inbox
//! processors, per-queue concurrency, delayed jobs via the scheduler). This
//! rewrite has **no Redis broker**: jobs live in Postgres and are polled
//! in-process by [`spawn_worker`] with a 500ms sleep between empty polls.
//! Claiming uses `FOR UPDATE SKIP LOCKED`, which gives the same
//! at-least-once, no-double-claim semantics as BullMQ's visibility timeout
//! without a second datastore. Backpressure, per-queue concurrency knobs and
//! the standalone `--worker-only` mode (M7) can layer on top of this later.
//!
//! Note on crate boundaries: `mk-db` documents itself as the only crate that
//! touches SQL — `mk-queue` is the exception, owning exactly the private
//! `mk_queue_jobs` table (defined in `0003_mk_queue.sql`; numbered after the
//! 0002 notifications slice) and nothing from the
//! Misskey schema. Timestamps stay SQL-side (`now()` / `run_after`) because
//! the workspace sqlx build has no `chrono` support; the `json` feature is
//! enabled here (additive feature unification) for jsonb payload round-trips.
//!
//! Nothing in this crate is wired into note creation: notes stay synchronous
//! through M2 (coordinator decision).

use std::time::Duration;

use serde_json::Value;
use sqlx::PgPool;

/// Poll cadence for the in-process worker (upstream BullMQ lets Redis block
/// until a job appears; polling keeps this broker-free).
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Per-attempt delay when a job fails: `min(1h, 2^(attempts-1))` seconds, so
/// 1s, 2s, 4s, ... capped at one hour. `attempts` is incremented on claim, so
/// the first failure (attempts = 1) waits 1s before becoming claimable again.
pub const BACKOFF_CAP_SECS: i64 = 3600;

/// A durable job row as handed to a worker handler.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: i64,
    pub kind: String,
    pub payload: Value,
    pub attempts: i32,
}

impl Job {
    /// Deserializes the payload into a handler-shaped type (the queue itself
    /// treats payloads as opaque jsonb).
    pub fn payload_as<T: serde::de::DeserializeOwned>(&self) -> Option<T> {
        serde_json::from_value(self.payload.clone()).ok()
    }
}

/// Handle onto the durable queue. Cheap to clone (shares the pool).
#[derive(Clone)]
pub struct QueueHandle {
    pool: PgPool,
}

impl QueueHandle {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Inserts a job of `kind` with a jsonb `payload` and returns its id.
    ///
    /// arena-exports: the table is (re)ensured first — the upstream e2e
    /// harness runs TypeORM `initTestDb` with `dropSchema: true` AFTER this
    /// server's own migrations (setup.e2e.ts beforeAll), and the rebuild
    /// wipes non-entity tables like `mk_queue_jobs`. `IF NOT EXISTS` is an
    /// idempotent catalog probe, and the durable-queue invariant (an
    /// accepted enqueue must survive) holds regardless of who wiped the
    /// table underneath.
    pub async fn enqueue(&self, kind: &str, payload: &Value) -> Result<i64, sqlx::Error> {
        sqlx::query(
            r#"CREATE TABLE IF NOT EXISTS "mk_queue_jobs" (
                   "id" bigserial NOT NULL,
                   "kind" text NOT NULL,
                   "payload" jsonb DEFAULT '{}'::jsonb NOT NULL,
                   "attempts" integer DEFAULT 0 NOT NULL,
                   "run_after" timestamp with time zone DEFAULT now() NOT NULL,
                   "created_at" timestamp with time zone DEFAULT now() NOT NULL
               )"#,
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            r#"CREATE INDEX IF NOT EXISTS mk_queue_jobs_kind_run_after_idx
               ON "mk_queue_jobs" USING btree (kind, run_after)"#,
        )
        .execute(&self.pool)
        .await?;
        sqlx::query_scalar(
            r#"INSERT INTO "mk_queue_jobs" ("kind", "payload")
               VALUES ($1, $2)
               RETURNING "id""#,
        )
        .bind(kind)
        .bind(payload)
        .fetch_one(&self.pool)
        .await
    }

    /// arena-move: delayed-enqueue variant — BullMQ's `{ delay }` option
    /// (QueueService.createDelayedUnfollowJob passes the 24h / test-10s
    /// delay through). Same idempotent table re-ensure as [`enqueue`]; the
    /// job simply lands with `run_after = now() + delay` so [`claim_next`]
    /// skips it until due (the "explicitly delayed" arm of its WHERE).
    pub async fn enqueue_delayed(
        &self,
        kind: &str,
        payload: &Value,
        delay: std::time::Duration,
    ) -> Result<i64, sqlx::Error> {
        sqlx::query(
            r#"CREATE TABLE IF NOT EXISTS "mk_queue_jobs" (
                   "id" bigserial NOT NULL,
                   "kind" text NOT NULL,
                   "payload" jsonb DEFAULT '{}'::jsonb NOT NULL,
                   "attempts" integer DEFAULT 0 NOT NULL,
                   "run_after" timestamp with time zone DEFAULT now() NOT NULL,
                   "created_at" timestamp with time zone DEFAULT now() NOT NULL
               )"#,
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            r#"CREATE INDEX IF NOT EXISTS mk_queue_jobs_kind_run_after_idx
               ON "mk_queue_jobs" USING btree (kind, run_after)"#,
        )
        .execute(&self.pool)
        .await?;
        sqlx::query_scalar(
            r#"INSERT INTO "mk_queue_jobs" ("kind", "payload", "run_after")
               VALUES ($1, $2, now() + make_interval(secs => $3))
               RETURNING "id""#,
        )
        .bind(kind)
        .bind(payload)
        .bind(delay.as_secs_f64())
        .fetch_one(&self.pool)
        .await
    }

    /// Atomically claims the oldest due job of one of `kinds`, incrementing
    /// its attempt counter. `FOR UPDATE SKIP LOCKED` guarantees concurrent
    /// workers never receive the same row; jobs whose `run_after` is still in
    /// the future (failed and backing off, or explicitly delayed) are skipped.
    pub async fn claim_next(&self, kinds: &[&str]) -> Result<Option<Job>, sqlx::Error> {
        let kinds: Vec<String> = kinds.iter().map(|k| k.to_string()).collect();
        let row = sqlx::query_as::<_, (i64, String, Value, i32)>(
            r#"
            WITH next AS (
                SELECT "id" FROM "mk_queue_jobs"
                WHERE "kind" = ANY($1) AND "run_after" <= now()
                ORDER BY "id"
                FOR UPDATE SKIP LOCKED
                LIMIT 1
            )
            UPDATE "mk_queue_jobs" AS j
            SET "attempts" = j."attempts" + 1
            FROM next
            WHERE j."id" = next."id"
            RETURNING j."id", j."kind", j."payload", j."attempts"
            "#,
        )
        .bind(kinds)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(id, kind, payload, attempts)| Job {
            id,
            kind,
            payload,
            attempts,
        }))
    }

    /// Removes a successfully finished job.
    pub async fn complete(&self, id: i64) -> Result<(), sqlx::Error> {
        sqlx::query(r#"DELETE FROM "mk_queue_jobs" WHERE "id" = $1"#)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Marks a failed job as due again after an exponentially growing delay
    /// (see [`BACKOFF_CAP_SECS`]). The job's `attempts` already count the just
    /// failed attempt (incremented on claim), so repeated failures slow down
    /// on their own.
    pub async fn retry_with_backoff(&self, id: i64) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"UPDATE "mk_queue_jobs"
               SET "run_after" = now()
                 + make_interval(secs => LEAST(3600, POWER(2.0, GREATEST("attempts" - 1, 0)))::int)
               WHERE "id" = $1"#,
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

/// A handler receives the shared queue handle (for enqueueing follow-up
/// jobs) and the claimed job. Returning `Err` schedules the job for a retry
/// with backoff; `Ok` completes (deletes) it.
pub type BoxedJobFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;

pub type JobHandler = std::sync::Arc<dyn Fn(QueueHandle, Job) -> BoxedJobFuture + Send + Sync>;

/// Spawns an in-process worker polling the given job kinds every
/// [`POLL_INTERVAL`]. No external broker is involved (see module docs); stop
/// the worker by aborting the returned [`tokio::task::JoinHandle`].
pub fn spawn_worker(
    pool: PgPool,
    kinds: Vec<String>,
    handler: JobHandler,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let handle = QueueHandle::new(pool);
        let kind_refs: Vec<&str> = kinds.iter().map(String::as_str).collect();
        loop {
            match handle.claim_next(&kind_refs).await {
                Ok(Some(job)) => {
                    let outcome = handler(handle.clone(), job.clone()).await;
                    let res = match outcome {
                        Ok(()) => handle.complete(job.id).await,
                        Err(err) => {
                            tracing::warn!(
                                job_id = job.id,
                                kind = %job.kind,
                                attempts = job.attempts,
                                %err,
                                "job failed; scheduling retry with backoff"
                            );
                            handle.retry_with_backoff(job.id).await
                        }
                    };
                    if let Err(err) = res {
                        tracing::error!(job_id = job.id, %err, "queue bookkeeping failed");
                    }
                }
                Ok(None) => tokio::time::sleep(POLL_INTERVAL).await,
                Err(err) => {
                    tracing::error!(%err, "queue poll failed");
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Per-test isolated db with the full migration chain (which installs
    /// `mk_queue_jobs`). No DATABASE_URL (local) -> skip.
    async fn test_pool(suffix: &str) -> Option<PgPool> {
        if std::env::var("DATABASE_URL").is_err() {
            return None;
        }
        let pool = mk_db::test_support::fresh_db(suffix).await;
        mk_db::run_migrations(&pool).await.expect("migrations");
        Some(pool)
    }

    #[tokio::test]
    async fn enqueue_claim_complete_roundtrip_and_retry() {
        let Some(pool) = test_pool("mkqueue_rt").await else {
            return;
        };
        let handle = QueueHandle::new(pool);

        let id = handle
            .enqueue("test.hello", &json!({ "n": 1 }))
            .await
            .unwrap();
        let job = handle
            .claim_next(&["test.hello"])
            .await
            .unwrap()
            .expect("one due job");
        assert_eq!(job.id, id);
        assert_eq!(job.kind, "test.hello");
        assert_eq!(job.payload, json!({ "n": 1 }));
        assert_eq!(job.attempts, 1);
        handle.complete(job.id).await.unwrap();
        assert!(handle.claim_next(&["test.hello"]).await.unwrap().is_none());

        // Retry path: failing keeps the job and pushes run_after into the
        // future; it becomes claimable again afterwards with the attempt
        // counter increased.
        let id2 = handle
            .enqueue("test.retry", &json!({ "n": 2 }))
            .await
            .unwrap();
        let first = handle.claim_next(&["test.retry"]).await.unwrap().unwrap();
        assert_eq!(first.id, id2);
        assert_eq!(first.attempts, 1);
        handle.retry_with_backoff(first.id).await.unwrap();
        // 2^(attempts-1) = 1s backoff after the first attempt: not due yet.
        assert!(handle.claim_next(&["test.retry"]).await.unwrap().is_none());
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let second = handle.claim_next(&["test.retry"]).await.unwrap().unwrap();
        assert_eq!(second.id, id2);
        assert_eq!(second.attempts, 2);
        handle.complete(second.id).await.unwrap();

        // Kinds filter: an unrelated job never spills into other kinds.
        handle.enqueue("test.other", &json!({})).await.unwrap();
        assert!(
            handle
                .claim_next(&["test.unrelated"])
                .await
                .unwrap()
                .is_none()
        );
        let other = handle.claim_next(&["test.other"]).await.unwrap().unwrap();
        handle.complete(other.id).await.unwrap();
        assert!(handle.claim_next(&["test.other"]).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn worker_completes_after_transient_handler_failure() {
        let Some(pool) = test_pool("mkqueue_worker").await else {
            return;
        };
        let enqueue = QueueHandle::new(pool.clone());
        enqueue
            .enqueue("test.worker", &json!({ "ok": false }))
            .await
            .unwrap();

        let handler_calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let calls_in_handler = handler_calls.clone();
        let worker = spawn_worker(
            pool,
            vec!["test.worker".to_string()],
            std::sync::Arc::new(move |_queue, _job| {
                let calls = calls_in_handler.clone();
                Box::pin(async move {
                    // First run fails -> retry with backoff; second run wins.
                    if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                        Err("transient failure".to_string())
                    } else {
                        Ok(())
                    }
                })
            }),
        );

        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while handler_calls.load(std::sync::atomic::Ordering::SeqCst) < 2
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        worker.abort();
        assert_eq!(
            handler_calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "worker should have run the handler twice (fail once, then succeed)"
        );
    }
}
