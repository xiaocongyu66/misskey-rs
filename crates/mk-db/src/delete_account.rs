//! delete-account: db paths behind i/delete-account + admin/delete-account
//! (upstream core/DeleteAccountService.ts + the DeleteAccountProcessor job).
//!
//! Upstream deletion is two-stage: the endpoint's synchronous half is a
//! single write — `usersRepository.update(user.id, { isDeleted: true })` —
//! plus an enqueued job that purges notes/files and (soft:false jobs) later
//! removes the user row. The purge never lands inside the e2e window
//! (test/e2e/users.ts 削除済 cases): the deleted user keeps appearing in the
//! users list, users/search and users/get-frequently-replied-users, so the
//! only observable DB state change is the flag this module flips.

use crate::sqlx::PgPool;

/// The synchronous DeleteAccountService write:
/// `await this.usersRepository.update(user.id, { isDeleted: true })`.
/// Idempotent (the queue re-runs on retry); lists/reads elsewhere in mk-db
/// carry no isDeleted gate, matching upstream where only the pack (me view)
/// and the two endpoints' early-returns read the flag.
pub async fn soft_delete(pool: &PgPool, user_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "user" SET "isDeleted" = true WHERE "id" = $1"#)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// The `user.isDeleted` short-circuit both endpoints read
/// (i/delete-account.ts / admin/delete-account.ts: an already-deleted target
/// returns immediately). `None` = the user row is gone — the endpoints reach
/// the row via `usersRepository.findOneByOrFail`, so absence is a thrown
/// Error → 500 (the api layer maps it).
pub async fn fetch_is_deleted(pool: &PgPool, user_id: &str) -> Result<Option<bool>, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(r#"SELECT "isDeleted" FROM "user" WHERE "id" = $1"#)
        .bind(user_id)
        .fetch_optional(pool)
        .await
}

/// queue-min (DeleteAccountProcessorService `{ // Delete notes }`): the notes
/// purge leg of the soft `deleteAccount` queue job. Upstream pages 100 ids
/// per batch and unindexes each note; the port deletes in one statement (no
/// search index to unindex yet). The user row itself SURVIVES — the 削除済
/// census keeps reading it from the users lists.
/// TODO(queue-min): drive files / pages / avatar-banner purge, the
/// `soft: false` user-row removal and the Delete Activity delivery stay
/// unported (upstream `{ // Delete files }` onward).
pub async fn purge_notes(pool: &PgPool, user_id: &str) -> Result<u64, sqlx::Error> {
    let res = sqlx::query(r#"DELETE FROM "note" WHERE "userId" = $1"#)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}
