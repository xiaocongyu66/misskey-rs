//! `admin/relays/*` SQL support (upstream `RelayService.listRelay` /
//! `addRelay` / `removeRelay`, `src/core/RelayService.ts`).
//!
//! The `relay` table ships in the 0001 baseline with exactly three columns:
//! `id` (varchar 32, PK), `inbox` (varchar 512, UNIQUE btree) and `status`
//! (`relay_status_enum`: requesting | accepted | rejected). There is no
//! timestamp column — upstream's entity is likewise `{ id, inbox, status }`.
//!
//! Note the unique btree on `inbox`: a duplicate `addRelay` hits it exactly
//! like upstream's uncaught `insertOne` QueryFailedError (the endpoint has no
//! duplicate pre-check), so the handler answers 500 INTERNAL_ERROR.

use sqlx::PgPool;

/// One `relay` row (`RelayEntityService.pack` shape: id/inbox/status).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RelayRow {
    pub id: String,
    pub inbox: String,
    pub status: String,
}

/// `RelayService.listRelay` — every row, no ordering upstream.
pub async fn list(pool: &PgPool) -> Result<Vec<RelayRow>, sqlx::Error> {
    sqlx::query_as::<_, RelayRow>(
        r#"
        SELECT "id", "inbox", "status"::text AS "status"
        FROM "relay"
        "#,
    )
    .fetch_all(pool)
    .await
}

/// `relayService.removeRelay` lookup (`findOneBy({ inbox })`).
pub async fn find_by_inbox(pool: &PgPool, inbox: &str) -> Result<Option<RelayRow>, sqlx::Error> {
    sqlx::query_as::<_, RelayRow>(
        r#"
        SELECT "id", "inbox", "status"::text AS "status"
        FROM "relay" WHERE "inbox" = $1
        "#,
    )
    .bind(inbox)
    .fetch_optional(pool)
    .await
}

/// `RelayService.addRelay` insert (`status` is always 'requesting' upstream).
pub async fn insert(pool: &PgPool, id: &str, inbox: &str, status: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO "relay" ("id", "inbox", "status")
        VALUES ($1, $2, $3::text::public.relay_status_enum)
        "#,
    )
    .bind(id)
    .bind(inbox)
    .bind(status)
    .execute(pool)
    .await?;
    Ok(())
}

/// `RelayService.removeRelay` delete leg (`relaysRepository.delete(id)`).
pub async fn delete_by_id(pool: &PgPool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"DELETE FROM "relay" WHERE "id" = $1"#)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
