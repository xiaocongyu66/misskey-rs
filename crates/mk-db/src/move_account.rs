//! arena-move: account-migration SQL (upstream AccountMoveService.ts).
//!
//! Owns the `movedToUri` / `movedAt` / `alsoKnownAs` plane on "user" plus the
//! `postMoveProcess` scan legs (copy blockings/mutings/role-assignments and
//! list memberships, the follow-copy source scan, the delayed-unfollow
//! source scan and `adjustFollowingCounts`). The job *kinds* and their
//! scheduling ride mk-queue; endpoint bodies live in the api crate
//! (api::move_account). Timestamps bridge timestamptz → epoch-ms like the
//! rest of mk-db (no chrono feature in the workspace sqlx).

use sqlx::PgPool;

// ---------------------------------------------------------------------------
// move-out plane (AccountMoveService.moveFromLocal L82-87)
// ---------------------------------------------------------------------------

/// `update.movedToUri = dstUri; update.movedAt = new Date()` (+ the
/// alsoKnownAs merge computed by the caller —
/// `src.alsoKnownAs?.includes(dstUri) ? src.alsoKnownAs : concat ?? [dstUri]`).
/// The alias list persists as TypeORM `simple-array` (comma-joined text).
pub async fn set_move_out(
    pool: &PgPool,
    src_id: &str,
    moved_to_uri: &str,
    also_known_as_csv: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"UPDATE "user"
           SET "movedToUri" = $2, "movedAt" = now(), "alsoKnownAs" = $3
           WHERE "id" = $1"#,
    )
    .bind(src_id)
    .bind(moved_to_uri)
    .bind(also_known_as_csv)
    .execute(pool)
    .await?;
    Ok(())
}

/// endpoints/i/update.ts alsoKnownAs branch — `updates.alsoKnownAs` write
/// (CSV or NULL when the resolved set came out empty).
pub async fn set_aliases(
    pool: &PgPool,
    user_id: &str,
    also_known_as_csv: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "user" SET "alsoKnownAs" = $2 WHERE "id" = $1"#)
        .bind(user_id)
        .bind(also_known_as_csv)
        .execute(pool)
        .await?;
    Ok(())
}

/// `(user.movedToUri, user.alsoKnownAs)` raw read for the gate / pack.
pub async fn read_move_plane(
    pool: &PgPool,
    user_id: &str,
) -> Result<Option<(Option<String>, Option<String>)>, sqlx::Error> {
    let row = sqlx::query_as::<_, (Option<String>, Option<String>)>(
        r#"SELECT "movedToUri", "alsoKnownAs" FROM "user" WHERE "id" = $1"#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// `user?.movedToUri` probe for the central prohibitMoved gate.
pub async fn get_moved_to_uri(pool: &PgPool, user_id: &str) -> Result<Option<String>, sqlx::Error> {
    let row: Option<Option<String>> =
        sqlx::query_scalar(r#"SELECT "movedToUri" FROM "user" WHERE "id" = $1"#)
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.flatten())
}

// ---------------------------------------------------------------------------
// src-account scans (postMoveProcess legs)
// ---------------------------------------------------------------------------

/// followingsRepository.findBy({ followerId: src.id }) — the delayed-unfollow
/// source list (createDelayedUnfollowJob data `to: { followeeId }`).
pub async fn own_followings(pool: &PgPool, follower_id: &str) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        r#"SELECT "followeeId" FROM "following" WHERE "followerId" = $1 ORDER BY "id""#,
    )
    .bind(follower_id)
    .fetch_all(pool)
    .await
}

/// postMoveProcess follow-copy scan: `followingsRepository.findBy({
/// followeeId: src.id, followerHost: IsNull(), followerId: Not(proxy.id) })`.
/// The rust server has no federation proxy/system accounts on "user", so the
/// Not(proxy) arm is a no-op here (documented in the module header).
pub async fn local_followers_of(
    pool: &PgPool,
    followee_id: &str,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        r#"SELECT "followerId" FROM "following"
           WHERE "followeeId" = $1 AND "followerHost" IS NULL ORDER BY "id""#,
    )
    .bind(followee_id)
    .fetch_all(pool)
    .await
}

/// copyBlocking: `blockingsRepository.findBy({ blockeeId: src.id })` → blocker ids.
pub async fn blockers_of(pool: &PgPool, blockee_id: &str) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(r#"SELECT "blockerId" FROM "blocking" WHERE "blockeeId" = $1 ORDER BY "id""#)
        .bind(blockee_id)
        .fetch_all(pool)
        .await
}

/// copyMutings source: mutings of src where `expiresAt IS NULL OR > now()`
/// (the `{ muteeId: src.id, expiresAt: IsNull() }` | MoreThan(findBy) union).
pub async fn mutings_of(
    pool: &PgPool,
    mutee_id: &str,
) -> Result<Vec<(String, Option<i64>)>, sqlx::Error> {
    sqlx::query_as::<_, (String, Option<i64>)>(
        r#"SELECT "muterId",
                  (extract(epoch from "expiresAt") * 1000)::bigint AS "expiresAtMs"
           FROM "muting"
           WHERE "muteeId" = $1
             AND ("expiresAt" IS NULL OR "expiresAt" > now())
           ORDER BY "id""#,
    )
    .bind(mutee_id)
    .fetch_all(pool)
    .await
}

/// copyMutings dedup: `expiresAt: IsNull()` muters who already mute dst.
/// Census rn39323486791: this raw string carried a stray trailing quote
/// (`IS NULL""#`) — the SQL reached Postgres as `... IS NULL"` and the
/// unterminated quoted identifier 500'd i/move mid-postMoveProcess (it used
/// to be masked because the delayed-unfollow enqueue crashed earlier).
pub async fn indefinite_muters_of(
    pool: &PgPool,
    mutee_id: &str,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        r#"SELECT "muterId" FROM "muting"
           WHERE "muteeId" = $1 AND "expiresAt" IS NULL"#,
    )
    .bind(mutee_id)
    .fetch_all(pool)
    .await
}

/// updateLists source: `userListMembershipsRepository.find({ userId: src.id })`
/// → (listId, listUserId = the list owner). withReplies is NOT carried into
/// the copied row (upstream repository.insert omits it → column default
/// false), so only this pair travels.
pub async fn list_memberships_of(
    pool: &PgPool,
    user_id: &str,
) -> Result<Vec<(String, String)>, sqlx::Error> {
    sqlx::query_as::<_, (String, String)>(
        r#"SELECT "userListId", "userListUserId" FROM "user_list_membership"
           WHERE "userId" = $1 ORDER BY "id""#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// updateLists dedup: list ids that already contain dst.
pub async fn list_ids_containing(pool: &PgPool, user_id: &str) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        r#"SELECT DISTINCT "userListId" FROM "user_list_membership" WHERE "userId" = $1"#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// copyRoles source: role assignments of src joined to roles that opt in via
/// `preserveAssignmentOnMoveAccount` → (roleId, expiresAt epoch-ms).
pub async fn portable_role_assignments(
    pool: &PgPool,
    user_id: &str,
) -> Result<Vec<(String, Option<i64>)>, sqlx::Error> {
    sqlx::query_as::<_, (String, Option<i64>)>(
        r#"SELECT a."roleId",
                  (extract(epoch from a."expiresAt") * 1000)::bigint AS "expiresAtMs"
           FROM "role_assignment" a
           JOIN "role" r ON r."id" = a."roleId"
           WHERE a."userId" = $1 AND r."preserveAssignmentOnMoveAccount" = TRUE
           ORDER BY a."id""#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// copyRoles dedup (RoleService.AlreadyAssignedError → continue): does dst
/// already carry the live assignment?
pub async fn role_assignment_exists(
    pool: &PgPool,
    role_id: &str,
    user_id: &str,
) -> Result<bool, sqlx::Error> {
    let n: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "role_assignment" WHERE "roleId" = $1 AND "userId" = $2"#,
    )
    .bind(role_id)
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

// ---------------------------------------------------------------------------
// adjustFollowingCounts (L291-322)
// ---------------------------------------------------------------------------

/// `usersRepository.update({ id: oldAccount.id }, { followersCount: 0,
/// followingCount: 0 })`.
pub async fn zero_move_counts(pool: &PgPool, old_account_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "user" SET "followersCount" = 0, "followingCount" = 0 WHERE "id" = $1"#)
        .bind(old_account_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// `usersRepository.decrement({ id: In(localFollowerIds) }, 'followingCount', 1)`.
pub async fn decrement_following_counts(
    pool: &PgPool,
    follower_ids: &[String],
) -> Result<(), sqlx::Error> {
    if follower_ids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        r#"UPDATE "user" SET "followingCount" = GREATEST("followingCount" - 1, 0)
           WHERE "id" = ANY($1)"#,
    )
    .bind(follower_ids)
    .execute(pool)
    .await?;
    Ok(())
}

/// `usersRepository.decrement({ id: In(oldFollowings) }, 'followersCount', 1)`.
pub async fn decrement_followers_counts(
    pool: &PgPool,
    followee_ids: &[String],
) -> Result<(), sqlx::Error> {
    if followee_ids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        r#"UPDATE "user" SET "followersCount" = GREATEST("followersCount" - 1, 0)
           WHERE "id" = ANY($1)"#,
    )
    .bind(followee_ids)
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// queued relationship legs (RelationshipProcessorService: follow/unfollow)
// ---------------------------------------------------------------------------

/// relationship `follow` job: insertFollowingDoc + the `!movedToUri`-gated
/// count increments (UserFollowingService.incrementFollowing L305-316 —
/// "Neither followee nor follower has moved"). `id` is caller-minted
/// (mk-types aid gen — mk-db stays free of an id dependency).
pub async fn follow_leg(
    pool: &PgPool,
    id: &str,
    follower_id: &str,
    followee_id: &str,
    with_replies: bool,
) -> Result<(), sqlx::Error> {
    if crate::social::following_exists(pool, follower_id, followee_id).await? {
        return Ok(()); // already following (IdentifiableError → processor skips)
    }
    crate::social::insert_following(pool, id, follower_id, followee_id, with_replies).await?;
    if !(get_moved_to_uri(pool, follower_id).await?.is_some()
        || get_moved_to_uri(pool, followee_id).await?.is_some())
    {
        crate::social::bump_follow_counts(pool, follower_id, followee_id, 1).await?;
    }
    Ok(())
}

/// relationship `unfollow` job: delete the edge; counts decrement ONLY when
/// neither endpoint has moved (UserFollowingService.decrementFollowing
/// L424-437 — the moved-actor rows age out silently).
pub async fn unfollow_leg(
    pool: &PgPool,
    follower_id: &str,
    followee_id: &str,
) -> Result<(), sqlx::Error> {
    if crate::social::delete_following(pool, follower_id, followee_id).await?
        && !(get_moved_to_uri(pool, follower_id).await?.is_some()
            || get_moved_to_uri(pool, followee_id).await?.is_some())
    {
        crate::social::bump_follow_counts(pool, follower_id, followee_id, -1).await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// mute/list + blocking/list scans (endpoints/mute/list.ts + blocking/list.ts
// over QueryService.makePaginationQuery)
// ---------------------------------------------------------------------------

/// One list row: partner id + the packed expiresAt (epoch-ms) for muting;
/// blocking rows carry no expiresAt column so the caller packs null.
pub async fn list_by_owner(
    pool: &PgPool,
    table: &str,
    owner_col: &str,
    partner_col: &str,
    me_id: &str,
    with_expiry: bool,
    since_id: Option<&str>,
    until_id: Option<&str>,
    since_date_ms: Option<i64>,
    until_date_ms: Option<i64>,
    limit: u32,
) -> Result<Vec<(String, String, Option<i64>)>, sqlx::Error> {
    // makePaginationQuery parity: ids are aid (chronological) — a sinceId/
    // sinceDate start flips the scan to ASC; the default is DESC.
    let expires_sel = if with_expiry {
        r#"(extract(epoch from r."expiresAt") * 1000)::bigint"#.to_string()
    } else {
        "NULL::bigint".to_string()
    };
    let asc = since_id.is_some() || since_date_ms.is_some();
    let mut sql = format!(
        r#"SELECT r."id", r."{partner_col}", {expires_sel}
           FROM "{table}" r
           WHERE r."{owner_col}" = $1
             AND ($2 = '' OR r."id" > $2)
             AND ($3 = '' OR r."id" < $3)"#
    );
    // sinceDate/untilDate are NOT backable here: MiMuting/MiBlocking carry no
    // createdAt column (0001 DDL mirrors the pinned entities), so upstream's
    // makePaginationQuery date arm would reference a nonexistent column and
    // 500. The api layer answers those params with the upstream internal
    // error before reaching this scan; the params stay in the signature for
    // the parity note.
    let _ = (since_date_ms, until_date_ms);
    sql.push_str(if asc {
        " ORDER BY r.\"id\" ASC"
    } else {
        " ORDER BY r.\"id\" DESC"
    });
    sql.push_str(&format!(" LIMIT {limit}"));
    // fixed 3-slot bind ($2/$3 empty-string sentinels keep the positional
    // indices aligned no matter which bounds are set)
    let mut q = sqlx::query_as::<_, (String, String, Option<i64>)>(&sql)
        .bind(me_id)
        .bind(since_id.unwrap_or(""))
        .bind(until_id.unwrap_or(""));
    q.fetch_all(pool).await
}

/// copyRoles assign leg (RoleService.assign) — a fresh role_assignment row
/// carrying the original's expiry (epoch-ms → timestamptz, the inverse of
/// the read side).
pub async fn insert_role_assignment(
    pool: &PgPool,
    id: &str,
    role_id: &str,
    user_id: &str,
    expires_ms: Option<i64>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO "role_assignment" ("id", "userId", "roleId", "expiresAt")
           VALUES ($1, $2, $3, CASE WHEN $4 IS NULL THEN NULL
                                    ELSE to_timestamp($4::double precision / 1000.0) END)"#,
    )
    .bind(id)
    .bind(user_id)
    .bind(role_id)
    .bind(expires_ms)
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// users/lists/list support (endpoints/users/lists/list.ts)
// ---------------------------------------------------------------------------

/// `userListsRepository.find({ userId: me.id })` — the caller's lists in
/// table order (no ORDER BY upstream either); only the pinned asserts read
/// the packed userIds membership, not an ordering.
pub async fn lists_of_owner(
    pool: &PgPool,
    owner_id: &str,
) -> Result<Vec<(String, String, bool)>, sqlx::Error> {
    sqlx::query_as::<_, (String, String, bool)>(
        r#"SELECT "id", "name", "isPublic" FROM "user_list" WHERE "userId" = $1"#,
    )
    .bind(owner_id)
    .fetch_all(pool)
    .await
}
