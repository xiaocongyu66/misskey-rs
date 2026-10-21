//! Remote person ingest — the first half of upstream `ApPersonService`
//! (`fetchPerson` + `createPerson`): resolve an actor URI to a local `user`
//! row, creating a remote row on first sight.
//!
//! Upstream anchors:
//! - `packages/backend/src/core/activitypub/models/ApPersonService.ts`
//!   `fetchPerson(uri)` → `usersRepository.findOneBy({ uri })`; a miss falls
//!   through to `createPerson`, whose INSERT carries `id`,
//!   `username`/`usernameLower` (from `preferredUsername`, lowercased),
//!   `host` (`punyHost(uri)`), `inbox`, `uri: person.id` and the profile
//!   counters/flags.
//! - `packages/backend/src/core/activitypub/ApInboxService.ts` resolves
//!   `activity.actor` through `resolvePerson` before `performActivity`; this
//!   module is the worker-side row half of that resolution.
//!
//! Schema note (0001_init.sql): the `user` table carries `host` (NULL =
//! local), `uri`, `inbox`, `username`, `usernameLower`, but NO `publicKey`
//! column — remote keys live only in the in-process cache
//! (`fetch::KEY_CACHE`), so the row lands without key material. The unique
//! index is `("usernameLower", host)`; two remote actors may share a
//! `preferredUsername`, so a collision suffixes a short hash of the actor URI
//! (upstream relies on the duplicate-key catch + `findOneBy({ uri })`).

use mk_db::sqlx::PgPool;
use serde_json::Value;

/// `user.username` / `user."usernameLower"` are `varchar(128)`.
const USERNAME_MAX: usize = 128;

/// `ApPersonService.fetchPerson` + the `createPerson` INSERT: return the
/// local user id for `actor_uri`, inserting the remote row on first sight.
///
/// Idempotent: an existing `uri` row wins; otherwise the INSERT uses
/// `ON CONFLICT DO NOTHING` and the row is re-selected by `uri` (upstream's
/// duplicate-key fallback). `actor_doc` supplies `preferredUsername` and
/// `inbox`; `actor_uri` supplies the `uri`/`host` pair (the caller fetched
/// the document from that URL).
pub async fn ensure_remote_person(
    pool: &PgPool,
    actor_uri: &str,
    actor_doc: &Value,
) -> Result<String, String> {
    if let Some(id) = find_id_by_uri(pool, actor_uri).await? {
        return Ok(id);
    }

    let host = uri_authority(actor_uri)
        .ok_or_else(|| format!("actor uri has no authority: {actor_uri}"))?;
    let preferred = actor_doc
        .get("preferredUsername")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "actor doc has no preferredUsername".to_string())?;
    let inbox = actor_doc
        .get("inbox")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "actor doc has no inbox".to_string())?;

    let (username, username_lower) = username_for(pool, preferred, &host, actor_uri).await?;
    let id = mk_types::id::gen_aid_now();

    sqlx::query(
        r#"
        INSERT INTO "user" ("id", "username", "usernameLower", "host", "uri", "inbox")
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(&id)
    .bind(&username)
    .bind(&username_lower)
    .bind(&host)
    .bind(actor_uri)
    .bind(inbox)
    .execute(pool)
    .await
    .map_err(|e| format!("remote person insert: {e}"))?;

    // the idempotency tail: a re-delivery (or a concurrent creator that won
    // the race) is found by uri, exactly like upstream's duplicate-key catch.
    find_id_by_uri(pool, actor_uri)
        .await?
        .ok_or_else(|| format!("remote person insert conflicted without a uri row: {actor_uri}"))
}

async fn find_id_by_uri(pool: &PgPool, uri: &str) -> Result<Option<String>, String> {
    sqlx::query_scalar(r#"SELECT "id" FROM "user" WHERE "uri" = $1"#)
        .bind(uri)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("user uri lookup: {e}"))
}

/// The `("usernameLower", host)` unique-index probe plus the collision suffix:
/// upstream would hit the duplicate-key catch and reuse the existing row, but
/// that row belongs to a DIFFERENT actor — so the second actor gets
/// `<preferred>_<short uri hash>` (case preserved on `username`, folded on
/// `usernameLower`). The probe repeats on the suffixed candidate so a hash
/// collision degrades to a counter rather than a failed insert.
async fn username_for(
    pool: &PgPool,
    preferred: &str,
    host: &str,
    actor_uri: &str,
) -> Result<(String, String), String> {
    let base: String = preferred.chars().take(USERNAME_MAX).collect();
    let lower = base.to_lowercase();
    if !username_taken(pool, &lower, host).await? {
        return Ok((base, lower));
    }

    let hash = short_hash(actor_uri);
    let mut n = 0u32;
    loop {
        let tag = if n == 0 {
            format!("_{hash}")
        } else {
            format!("_{hash}-{n}")
        };
        let keep = USERNAME_MAX.saturating_sub(tag.chars().count());
        let stem: String = preferred.chars().take(keep).collect();
        let candidate = format!("{stem}{tag}");
        let candidate_lower = candidate.to_lowercase();
        if !username_taken(pool, &candidate_lower, host).await? {
            return Ok((candidate, candidate_lower));
        }
        n += 1;
    }
}

async fn username_taken(pool: &PgPool, username_lower: &str, host: &str) -> Result<bool, String> {
    sqlx::query_scalar(
        r#"SELECT EXISTS(SELECT 1 FROM "user" WHERE "usernameLower" = $1 AND "host" = $2)"#,
    )
    .bind(username_lower)
    .bind(host)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("username probe: {e}"))
}

/// `UtilityService.punyHost(uri)`'s shape: the authority segment, lowercased
/// (the `user.host` column). No port punycode folding — the 0001 tests use
/// plain authorities.
fn uri_authority(uri: &str) -> Option<String> {
    let (_scheme, rest) = uri.split_once("://")?;
    let authority = rest
        .split(|c: char| c == '/' || c == '?' || c == '#')
        .next()?;
    if authority.is_empty() {
        return None;
    }
    Some(authority.to_ascii_lowercase())
}

/// FNV-1a (64-bit) folded to 8 hex chars: the collision suffix source. Stable
/// across processes (unlike `DefaultHasher`) and dependency-free.
fn short_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:08x}", (h ^ (h >> 32)) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn fresh_pool(db: &str) -> PgPool {
        let pool = mk_db::test_support::fresh_db(db).await;
        mk_db::run_migrations(&pool).await.unwrap();
        pool
    }

    fn actor(uri: &str, preferred: &str) -> Value {
        json!({
            "id": uri,
            "type": "Person",
            "preferredUsername": preferred,
            "inbox": format!("{uri}/inbox"),
        })
    }

    #[tokio::test]
    async fn ensure_remote_person_is_idempotent() {
        if std::env::var("DATABASE_URL").is_err() {
            return;
        }
        let pool = fresh_pool("mktest_ap_person_idem").await;
        let uri = "http://remote.example/users/alice";
        let doc = actor(uri, "Alice");

        let first = ensure_remote_person(&pool, uri, &doc).await.unwrap();
        let second = ensure_remote_person(&pool, uri, &doc).await.unwrap();
        assert_eq!(first, second, "re-delivery must reuse the row");

        let rows: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "user" WHERE "uri" = $1"#)
            .bind(uri)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 1, "exactly one row per actor uri");

        // the createPerson column mapping: username/usernameLower/host/inbox
        let (username, lower, host, inbox): (String, String, String, String) = sqlx::query_as(
            r#"SELECT "username", "usernameLower", "host", "inbox" FROM "user" WHERE "id" = $1"#,
        )
        .bind(&first)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(username, "Alice");
        assert_eq!(lower, "alice");
        assert_eq!(host, "remote.example");
        assert_eq!(inbox, "http://remote.example/users/alice/inbox");
    }

    #[tokio::test]
    async fn username_collision_gets_a_distinct_suffix() {
        if std::env::var("DATABASE_URL").is_err() {
            return;
        }
        let pool = fresh_pool("mktest_ap_person_collision").await;
        let a_uri = "http://remote.example/users/a";
        let b_uri = "http://remote.example/users/b";

        let a = ensure_remote_person(&pool, a_uri, &actor(a_uri, "alice"))
            .await
            .unwrap();
        let b = ensure_remote_person(&pool, b_uri, &actor(b_uri, "alice"))
            .await
            .unwrap();
        assert_ne!(a, b);

        let names: Vec<(String, String)> = sqlx::query_as(
            r#"SELECT "username", "usernameLower" FROM "user" WHERE "host" = 'remote.example'"#,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(names.len(), 2, "both actors get a row");
        assert_ne!(names[0].0, names[1].0, "usernames stay distinct");
        assert_ne!(names[0].1, names[1].1, "the unique index is on the fold");
        // the loser is `<preferred>_<8 hex chars>` (row order is AID-random
        // within one millisecond, so compare as a set)
        let mut lowers: Vec<&str> = names.iter().map(|(_, l)| l.as_str()).collect();
        lowers.sort_unstable();
        let hashed = format!("alice_{}", short_hash(b_uri));
        let mut expected = vec!["alice", hashed.as_str()];
        expected.sort_unstable();
        assert_eq!(lowers, expected);
    }
}
