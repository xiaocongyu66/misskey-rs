//! queue-min: the two new durable-queue processors riding the PG broker
//! (`mk_queue_jobs`, FOR UPDATE SKIP LOCKED claim — see mk-queue): upstream
//! `queue/processors/EndedPollNotificationProcessorService.ts` +
//! `queue/processors/DeleteAccountProcessorService.ts`, dispatched by `kind`
//! out of ONE worker (the move_account::spawn_move_worker shape).
//!
//! Enqueue sides:
//! - `endedPollNotification`: notes/create's poll branch (NoteCreateService.ts
//!   L819-829 — one `{ delay: expiresAt - now }` delayed job per expiring
//!   poll, both the text and the renote arm of the ONE upstream create()).
//! - `deleteAccount`: i/delete-account + admin/delete-account
//!   (DeleteAccountService.ts createDeleteAccountJob —
//!   `{ user: { id }, soft: true }`).
//!
//! Upstream DeleteAccountProcessorService is the full purge (notes, files,
//! pages, search unindex, `soft: false` user-row removal); the port runs the
//! SOFT slice (notes purge + `isDeleted` re-assert) — the TODO list rides
//! run_delete_account and mk-db::delete_account::purge_notes.
//!
//! Divergences (documented, intentional): no BullMQ removeOnComplete age
//! caps — mk_queue deletes a completed job outright (no completed set); the
//! pollEnded insert carries a dedup probe because the at-least-once queue
//! (plus the scan arm racing the per-note job) can re-deliver the same
//! (notifiee, note) pair; deleteAccount's purge is the soft slice only.

use serde_json::Value;

/// Boot wiring (main.rs): the shared consumer for the new kinds — the
/// PG-broker replacement for upstream's BullMQ endedPollNotification queue
/// worker + the db-queue `deleteAccount` processor.
pub fn spawn_queue_workers(pool: mk_db::sqlx::PgPool) {
    mk_queue::spawn_worker(
        pool.clone(),
        vec![
            "endedPollNotification".to_string(),
            "deleteAccount".to_string(),
        ],
        std::sync::Arc::new(move |_queue, job| {
            let pool = pool.clone();
            Box::pin(async move {
                match job.kind.as_str() {
                    "endedPollNotification" => {
                        run_ended_poll_notification(
                            &pool,
                            job.payload.get("noteId").and_then(Value::as_str),
                        )
                        .await
                    }
                    "deleteAccount" => {
                        let uid = job
                            .payload
                            .get("user")
                            .and_then(|u| u.get("id"))
                            .and_then(Value::as_str)
                            .map(|s| s.to_string());
                        let Some(uid) = uid else {
                            return Err(format!(
                                "deleteAccount job {}: malformed payload",
                                job.id
                            ));
                        };
                        run_delete_account(&pool, &uid).await
                    }
                    other => Err(format!("unknown queue kind: {other}")),
                }
            })
        }),
    );
}

/// `EndedPollNotificationProcessorService.process` + the hourly window scan:
/// the per-note arm (`note_id = Some`) processes the delayed job the
/// note-create path enqueued for that poll; the payload-less arm sweeps the
/// last-hour window (polls expired within one hour with at least one vote —
/// mk-db::poll::ended_poll_note_ids). Recipients: author + distinct voters,
/// local only (`user.host IS NULL` — upstream's per-user
/// `profile.userHost === null` gate). The insert mirrors the social.rs
/// notify_* shape (NewNotification with `notifier_id: None` — upstream
/// `createNotification(userId, 'pollEnded', { noteId })` passes no notifier).
pub(crate) async fn run_ended_poll_notification(
    pool: &mk_db::sqlx::PgPool,
    note_id: Option<&str>,
) -> Result<(), String> {
    let note_ids = mk_db::poll::ended_poll_note_ids(pool, note_id)
        .await
        .map_err(|e| e.to_string())?;
    for nid in note_ids {
        let recipients = mk_db::poll::poll_ended_recipient_ids(pool, &nid)
            .await
            .map_err(|e| e.to_string())?;
        for uid in recipients {
            // at-least-once re-delivery (retried claim, scan re-fire) must
            // not duplicate the row — upstream: one delayed job per note +
            // removeOnComplete.
            if mk_db::notification::poll_ended_notified(pool, &uid, &nid)
                .await
                .map_err(|e| e.to_string())?
            {
                continue;
            }
            mk_db::notification::insert_notification(
                pool,
                &mk_db::notification::NewNotification {
                    id: &mk_types::id::gen_aid_now(),
                    r#type: "pollEnded",
                    user_id: &uid,
                    notifier_id: None,
                    note_id: Some(&nid),
                    reaction: None,
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// `DeleteAccountProcessorService.process` (soft slice): the notes purge +
/// the `isDeleted` re-assert. The synchronous endpoint half already flipped
/// the flag (DeleteAccountService.ts); the job owns the notes purge.
/// TODO(queue-min): drive files / pages purge, search unindex, the
/// `soft: false` user-row removal and the Delete Activity delivery stay
/// unported (upstream `{ // Delete files }` onward).
pub(crate) async fn run_delete_account(pool: &mk_db::sqlx::PgPool, uid: &str) -> Result<(), String> {
    // `usersRepository.findOneBy` — a gone user silently finishes (upstream
    // `if (user == null) return`).
    match mk_db::delete_account::fetch_is_deleted(pool, uid).await {
        Ok(Some(_)) => {}
        Ok(None) => return Ok(()),
        Err(e) => return Err(e.to_string()),
    }
    let purged = mk_db::delete_account::purge_notes(pool, uid)
        .await
        .map_err(|e| e.to_string())?;
    mk_db::delete_account::soft_delete(pool, uid)
        .await
        .map_err(|e| e.to_string())?;
    tracing::info!(target: "dbg_queue", user = %uid, purged, "deleteAccount: notes purged (soft)");
    Ok(())
}
