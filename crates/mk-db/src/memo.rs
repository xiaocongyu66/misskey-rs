//! Personal user memos (`user_memo`; upstream `UserMemoRepository`).
//! Memos are keyed by (viewer, target) — UNIQUE("userId", "targetUserId")
//! per the baseline schema's `IDX_faef300913c738265638ba3ebc`.
//! `endpoints/users/update-memo.ts`: null/empty memo deletes the row,
//! otherwise upsert (update when present, insert otherwise).

use sqlx::PgPool;

/// The viewer's memo text for a target user (None when unset).
pub async fn find_user_memo(
    pool: &PgPool,
    user_id: &str,
    target_user_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    let memo: Option<String> = sqlx::query_scalar(
        r#"SELECT "memo" FROM "user_memo" WHERE "userId" = $1 AND "targetUserId" = $2"#,
    )
    .bind(user_id)
    .bind(target_user_id)
    .fetch_optional(pool)
    .await?
    .flatten();
    Ok(memo)
}

/// `ps.memo == '' || ps.memo == null` → delete.
pub async fn delete_user_memo(
    pool: &PgPool,
    user_id: &str,
    target_user_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "user_memo" WHERE "userId" = $1 AND "targetUserId" = $2"#)
        .bind(user_id)
        .bind(target_user_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Set the memo (column is varchar(2048), validated upstream of this call).
/// Mirrors the findOneBy → update/insert branch as one atomic upsert on the
/// (viewer, target) unique index. `id` is a fresh aid from the caller
/// (mk-db's only-SQL rule: id generation lives above this crate).
pub async fn set_user_memo(
    pool: &PgPool,
    id: &str,
    user_id: &str,
    target_user_id: &str,
    memo: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO "user_memo" ("id", "userId", "targetUserId", "memo")
        VALUES ($4, $1, $2, $3)
        ON CONFLICT ("userId", "targetUserId") DO UPDATE SET "memo" = $3
        "#,
    )
    .bind(user_id)
    .bind(target_user_id)
    .bind(memo)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}
