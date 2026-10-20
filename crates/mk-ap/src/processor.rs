//! Inbox worker processors with real effects (local-loop scope).
//!
//! Upstream anchors:
//! - `packages/backend/src/queue/processors/InboxProcessorService.ts` — the
//!   worker-side trust chain: parse the `Signature:` header, resolve
//!   `getAuthUserFromKeyId(signature.keyId)`, then
//!   `httpSignature.verifySignature(signature, authUser.key.keyPem)`, then
//!   the `authUser.user.uri !== getApId(activity.actor)` gate before
//!   `apInboxService.performActivity`.
//! - `packages/backend/src/core/activitypub/ApInboxService.ts` — the
//!   `performOneActivity` arms this slice pins: Create{Note} (create →
//!   createNote), Follow, Like (→ ReactionService.create), Undo (undoFollow /
//!   undoLike).
//!
//! Schema notes (0001_init.sql vs upstream): the `user` table carries
//! `host` (NULL = local) and `uri` (NULL for local users) but NOT a
//! `keypair` column — the PEMs live in the separate `user_keypair` table
//! (`userId`, `publicKey`, `privateKey`), the 0001-schema stand-in for
//! upstream's `user_publickey` (keyId-keyed) rows. Local keyIds therefore
//! route by the `ApDbResolverService.parseUri` rule
//! (`https://<config host>/users/<id>`, fragment allowed) instead of a keyId
//! lookup. Remote keyIds are the resolver boundary of this slice: they
//! return [`Err`] (the caller answers 202 anyway — the HTTP row already
//! accepted the job upstream-side; the failure is a worker job reason).
//! Remote-fetch stubs (resolving a string `activity.object`, extracting
//! emojis, following out to remote inboxes) do not exist here — every arm
//! degrades to a documented [`RouteEffect::Skipped`] or Err.

use mk_db::sqlx::PgPool;
use serde_json::Value;

use crate::inbox::{ActivityKind, classify};
use crate::signature::{parse_signature_header, verify_http_signature};

/// `activitystreams` Public collection aliases (upstream `ap.ts` AP_PUBLIC +
/// `isPublic`'s two short forms).
const AP_PUBLIC: [&str; 3] = [
    "https://www.w3.org/ns/activitystreams#Public",
    "as:Public",
    "Public",
];

/// What the routed arm did. `Skipped` carries the upstream worker's reason
/// string (a skip is a SUCCESSFUL route — upstream returns it from
/// performOneActivity as the job result, never a thrown error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteEffect {
    /// Create{Note}: the note row was inserted (`note.uri` = object.id).
    NoteCreated { note_id: String },
    /// Create{Note} for a uri we already hold (upstream 'skip: note exists';
    /// the unique index on note.uri is the backing constraint).
    NoteExists,
    /// Follow: the following row was inserted (+ counter bumps).
    Followed,
    /// Like: the note_reaction row was inserted (+ the reactions map bump).
    Liked { reaction: String },
    /// Undo{Follow}: the pending request or following row was removed
    /// (upstream 'ok: follow request canceled' / 'ok: unfollowed').
    Unfollowed,
    /// Undo{Like}: the reaction row was removed (upstream swallows the
    /// not-reacted IdentifiableError → still 'ok').
    Unliked,
    /// The arm ran and chose to do nothing (upstream's reason strings,
    /// quoted where they are the skip message).
    Skipped(&'static str),
}

/// The resolved `getAuthUserFromKeyId` row: the keyId owner (id/host/uri from
/// the `user` table) plus their `user_keypair.publicKey` (the verify PEM).
#[derive(sqlx::FromRow)]
struct AuthUserRow {
    id: String,
    host: Option<String>,
    uri: Option<String>,
    #[sqlx(rename = "isSuspended")]
    is_suspended: bool,
    #[sqlx(rename = "publicKey")]
    public_key: String,
}

/// The InboxProcessorService worker lane, one call: keyId → `user_keypair`
/// publicKey, `verify_http_signature`, the actor gate, then the per-type arm
/// (upstream `performOneActivity`). Every failure — unparseable signature,
/// remote keyId (resolver boundary), unknown owner, bad verify, actor
/// mismatch, DB error — collapses to [`Err`]; the HTTP surface answers 202
/// regardless (the enqueue already happened upstream-side).
///
/// - `headers`: raw (name, value) pairs, names matched case-insensitively
///   (the `signature`/`host`/`date`/`digest` values are what the verify and
///   the signing string read).
/// - `local_host`: `Config::host()` — the keyId/actor/notes-uri locality
///   comparisons all fold against it.
pub async fn verify_and_route(
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: &[u8],
    local_host: &str,
    pool: &PgPool,
) -> Result<RouteEffect, ()> {
    let header = |name: &str| -> Option<String> {
        headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };
    let sig_header = header("signature").ok_or(())?;

    // getAuthUserFromKeyId (the local half): the keyId must parse against our
    // config host — `https://<local_host>/users/<id>[#fragment]` — and the
    // owner row must carry a keypair PEM. Remote keyIds stay Err here.
    let parsed = parse_signature_header(&sig_header).ok_or(())?;
    let owner_id = parse_local_ap_id(&parsed.key_id, local_host, "users").ok_or(())?;
    let auth = get_auth_user_from_key_id(pool, &owner_id).await?;

    // the RSA verify itself (the keyId came back through parse already)
    verify_http_signature(
        &sig_header,
        method,
        path,
        &header("host").unwrap_or_default(),
        body,
        &header("date").unwrap_or_default(),
        &header("digest").unwrap_or_default(),
        &auth.public_key,
    )?;

    // the InboxProcessorService actor gate: the signer must BE activity.actor
    let activity: Value = serde_json::from_slice(body).map_err(|_| ())?;
    if !actor_matches_auth_user(activity.get("actor"), &auth, local_host) {
        return Err(());
    }
    // performOneActivity's suspended gate: `if (actor.isSuspended) return`
    if auth.is_suspended {
        return Ok(RouteEffect::Skipped("skip: actor is suspended"));
    }

    match classify(&activity) {
        ActivityKind::Create => create_note(pool, &auth, &activity, local_host).await,
        ActivityKind::Follow => follow(pool, &auth, &activity, local_host).await,
        ActivityKind::Like => like(pool, &auth, &activity, local_host).await,
        ActivityKind::Undo => undo(pool, &auth, &activity, local_host).await,
        // Delete and the remaining upstream arms keep their worker lanes
        // pending (Delete{Person} tombstones, Update, Accept, ...).
        ActivityKind::Delete | ActivityKind::Other => {
            Ok(RouteEffect::Skipped("unhandled arm (worker lane pending)"))
        }
    }
}

/// `userPublickeysRepository.findOneBy({ keyId })` + the user join, keyed by
/// the parsed local id: `user_keypair.publicKey` is the verify PEM
/// (upstream `authUser.key.keyPem`), `user.host` NULL = local. Deleted users
/// drop out (`if (user.isDeleted) return null`).
async fn get_auth_user_from_key_id(pool: &PgPool, user_id: &str) -> Result<AuthUserRow, ()> {
    let row = sqlx::query_as::<_, AuthUserRow>(
        r#"
        SELECT u."id", u."host", u."uri", u."isSuspended", kp."publicKey"
        FROM "user_keypair" kp JOIN "user" u ON u."id" = kp."userId"
        WHERE kp."userId" = $1 AND u."isDeleted" = FALSE
        "#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| ())?
    .ok_or(())?;
    Ok(row)
}

/// `ApDbResolverService.parseUri` (the local half): `<scheme>://<host>/
/// <ap_type>/<id>` with the fragment stripped (actors sign `#main-key`), the
/// host compared case-insensitively (`toPuny` folds), and the id taken from
/// the second path segment (trailing segments ride upstream's `rest`).
fn parse_local_ap_id(uri: &str, local_host: &str, ap_type: &str) -> Option<String> {
    let rest = uri.split('#').next().unwrap_or(uri);
    let (_scheme, authority_and_path) = rest.split_once("://")?;
    let (host, path) = authority_and_path.split_once('/')?;
    if !host.eq_ignore_ascii_case(local_host) {
        return None;
    }
    let mut segs = path.split('/');
    if segs.next()? != ap_type {
        return None;
    }
    let id = segs.next()?;
    if id.is_empty() {
        return None;
    }
    Some(id.to_string())
}

/// `getApId`: string passthrough, object → its `id`, anything else → None.
fn get_ap_id(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) => Some(s.clone()),
        v => v.get("id").and_then(|id| id.as_str()).map(str::to_string),
    }
}

/// InboxProcessorService L139's `authUser.user.uri !== getApId(activity.actor)`
/// gate. Upstream's local-user `uri` is null exactly like ours, so the
/// identity check that survives both schemas is uri-parse: the claimed actor
/// must resolve to the keyId owner's id against the config host — or equal a
/// non-null stored `uri` (remote-style rows).
fn actor_matches_auth_user(actor: Option<&Value>, auth: &AuthUserRow, local_host: &str) -> bool {
    let Some(claimed) = get_ap_id(actor) else {
        return false;
    };
    if parse_local_ap_id(&claimed, local_host, "users").as_deref() == Some(auth.id.as_str()) {
        return true;
    }
    auth.uri.as_deref() == Some(claimed.as_str())
}

// ---------------------------------------------------------------------------
// Create{Note}
// ---------------------------------------------------------------------------

/// ApInboxService.create → createNote, the local-loop body: inline objects
/// only (a string `activity.object` needs the resolver — upstream resolves
/// it over the network; here it is a documented skip). The row carries
/// `uri` = object.id, `text` = content folded (`<br/>` → \n, tags stripped),
/// `userId` = the verified keyId owner and `userHost` = their host column.
async fn create_note(
    pool: &PgPool,
    auth: &AuthUserRow,
    activity: &Value,
    local_host: &str,
) -> Result<RouteEffect, ()> {
    let Some(object) = activity.get("object") else {
        return Ok(RouteEffect::Skipped(
            "skip: activity has no object property",
        ));
    };
    let Some(note) = object.as_object() else {
        return Ok(RouteEffect::Skipped(
            "resolver boundary: activity.object is not an inline object",
        ));
    };
    let note = Value::Object(note.clone());
    let Some(uri) = note.get("id").and_then(|v| v.as_str()) else {
        return Ok(RouteEffect::Skipped("skip: note.id is not a string"));
    };

    // upstream: absent attributedTo defaults to the actor (create() copies
    // activity.actor over), and a mismatching one is the createNote skip.
    let actor_uri = auth
        .uri
        .clone()
        .unwrap_or_else(|| format!("https://{local_host}/users/{}", auth.id));
    if let Some(attributed) = get_ap_id(note.get("attributedTo")) {
        if attributed != actor_uri {
            return Ok(RouteEffect::Skipped(
                "skip: actor.uri !== note.attributedTo",
            ));
        }
    }
    // upstream: host in actor.uri !== note.id → skip
    let actor_host = auth.host.clone().unwrap_or_else(|| local_host.to_string());
    if uri_host(uri)
        .map(|h| !h.eq_ignore_ascii_case(&actor_host))
        .unwrap_or(true)
    {
        return Ok(RouteEffect::Skipped("skip: host in actor.uri !== note.id"));
    }

    // upsert half: the unique index on note.uri backs 'skip: note exists'
    let exists: bool =
        sqlx::query_scalar(r#"SELECT EXISTS(SELECT 1 FROM "note" WHERE "uri" = $1)"#)
            .bind(uri)
            .fetch_one(pool)
            .await
            .map_err(|_| ())?;
    if exists {
        return Ok(RouteEffect::NoteExists);
    }

    let text = note
        .get("content")
        .and_then(|c| c.as_str())
        .map(html_text_fold);
    let visibility = get_visibility(activity, &note);
    let note_id = mk_types::id::gen_aid_now();
    sqlx::query(
        r#"
        INSERT INTO "note" ("id", "text", "userId", "visibility", "uri", "userHost")
        VALUES ($1, $2, $3, $4::note_visibility_enum, $5, $6)
        "#,
    )
    .bind(&note_id)
    .bind(&text)
    .bind(&auth.id)
    .bind(visibility)
    .bind(uri)
    .bind(&auth.host)
    .execute(pool)
    .await
    .map_err(|_| ())?;
    Ok(RouteEffect::NoteCreated { note_id })
}

/// `createNote`'s host gate helper: the host of an https uri (None when the
/// uri has no authority to extract).
fn uri_host(uri: &str) -> Option<String> {
    let (_scheme, rest) = uri.split_once("://")?;
    let (host, _) = rest.split_once('/')?;
    Some(host.to_string())
}

/// Create{Note} content → text, the local-loop subset of
/// `ApMfmService.htmlToMfm` (the full upstream pass is an HTML→MFM tree
/// walk): `<br>` variants fold to `\n`, every other tag is stripped
/// (unterminated tail dropped like a truncated HTML parse), and the five XML
/// entities plus the `&#39;` form decode.
fn html_text_fold(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    loop {
        match rest.find('<') {
            None => {
                decode_entities_into(rest, &mut out);
                break;
            }
            Some(open) => {
                decode_entities_into(&rest[..open], &mut out);
                let after_open = &rest[open + 1..];
                match after_open.find('>') {
                    None => break, // unterminated tag: drop the tail
                    Some(close) => {
                        let tag = after_open[..close].to_ascii_lowercase();
                        let tag = tag.trim().trim_end_matches('/').trim();
                        if tag == "br" || tag.starts_with("br ") {
                            out.push('\n');
                        }
                        rest = &after_open[close + 1..];
                    }
                }
            }
        }
    }
    out
}

fn decode_entities_into(s: &str, out: &mut String) {
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        let (entity, len) = if tail.starts_with("&amp;") {
            ("&", 5)
        } else if tail.starts_with("&lt;") {
            ("<", 4)
        } else if tail.starts_with("&gt;") {
            (">", 4)
        } else if tail.starts_with("&quot;") {
            ("\"", 6)
        } else if tail.starts_with("&apos;") {
            ("'", 6)
        } else if tail.starts_with("&#39;") {
            ("'", 5)
        } else {
            ("&", 1)
        };
        out.push_str(entity);
        rest = &tail[len..];
    }
    out.push_str(rest);
}

/// `ApNoteService.getVisibility`'s audience half: Public in `to` → public,
/// in `cc` → home, otherwise followers. (The `specified` tail needs the
/// tagged-user resolution — out of the local-loop slice; `create()` copies
/// audiences between activity and object, so both sides are merged.)
fn get_visibility(activity: &Value, note: &Value) -> &'static str {
    let mut to = audience_ids(activity.get("to"));
    let mut cc = audience_ids(activity.get("cc"));
    to.extend(audience_ids(note.get("to")));
    cc.extend(audience_ids(note.get("cc")));
    if to.iter().any(|id| AP_PUBLIC.contains(&id.as_str())) {
        return "public";
    }
    if cc.iter().any(|id| AP_PUBLIC.contains(&id.as_str())) {
        return "home";
    }
    "followers"
}

/// `getApIds(getApId)` over a `to`/`cc` value: string, array, object (an
/// audience collection rides its `id`), or absent.
fn audience_ids(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| get_ap_id(Some(item)))
            .collect(),
        Some(_) => get_ap_id(value).into_iter().collect(),
        None => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Follow
// ---------------------------------------------------------------------------

/// ApInboxService.follow: the object must resolve to a LOCAL user (host IS
/// NULL) — then `UserFollowingService.follow` lands as mk-db's
/// insert_following (+ the denormalized counter bumps, insertFollowingDoc's
/// pending-request cleanup inside). `withReplies` has no AP source here →
/// false, matching the local M2 rows.
async fn follow(
    pool: &PgPool,
    auth: &AuthUserRow,
    activity: &Value,
    local_host: &str,
) -> Result<RouteEffect, ()> {
    let Some(followee_id) =
        local_user_id_from_object(activity.get("object"), pool, local_host).await?
    else {
        return Ok(RouteEffect::Skipped("skip: followee not found"));
    };
    // unique (followerId, followeeId) — a re-delivered Follow is the
    // upstream 'already following' IdentifiableError, i.e. a skip
    if mk_db::social::following_exists(pool, &auth.id, &followee_id)
        .await
        .map_err(|_| ())?
    {
        return Ok(RouteEffect::Skipped("skip: already following"));
    }
    mk_db::social::insert_following(
        pool,
        &mk_types::id::gen_aid_now(),
        &auth.id,
        &followee_id,
        false,
    )
    .await
    .map_err(|_| ())?;
    mk_db::social::bump_follow_counts(pool, &auth.id, &followee_id, 1)
        .await
        .map_err(|_| ())?;
    Ok(RouteEffect::Followed)
}

/// `apDbResolverService.getUserFromApId(object)` for the follow/undo arms:
/// resolve the object to a LOCAL user id and enforce the local-user gate
/// (`skip: フォローしようとしているユーザーはローカルユーザーではありません`
/// — host column NULL). None covers both "did not parse as a local uri"
/// (the resolver boundary for remote uris) and "no such user".
async fn local_user_id_from_object(
    object: Option<&Value>,
    pool: &PgPool,
    local_host: &str,
) -> Result<Option<String>, ()> {
    let Some(uri) = get_ap_id(object) else {
        return Ok(None);
    };
    let Some(id) = parse_local_ap_id(&uri, local_host, "users") else {
        return Ok(None);
    };
    // outer None = no such user; inner None = host NULL (local)
    let host: Option<Option<String>> =
        sqlx::query_scalar(r#"SELECT "host" FROM "user" WHERE "id" = $1"#)
            .bind(&id)
            .fetch_optional(pool)
            .await
            .map_err(|_| ())?;
    match host {
        Some(None) => Ok(Some(id)), // local user (host NULL)
        Some(Some(_)) => Ok(None),  // remote followee → the local-user gate
        None => Ok(None),           // no such user
    }
}

// ---------------------------------------------------------------------------
// Like
// ---------------------------------------------------------------------------

/// ApInboxService.like → ReactionService.create, reaction_acceptance rules
/// ignored (plain emoji append): reaction value = `_misskey_reaction ??
/// content ?? name`, the note resolved through fetchNote's parseUri split
/// (local `https://<host>/notes/<id>` → id lookup; anything else → uri
/// lookup), a second reaction = the '51c42bb4…' already-reacted skip.
async fn like(
    pool: &PgPool,
    auth: &AuthUserRow,
    activity: &Value,
    local_host: &str,
) -> Result<RouteEffect, ()> {
    let Some(target_uri) = get_ap_id(activity.get("object")) else {
        return Ok(RouteEffect::Skipped("skip: target note not found"));
    };
    let Some(reaction) = activity
        .get("_misskey_reaction")
        .and_then(|v| v.as_str())
        .or_else(|| activity.get("content").and_then(|v| v.as_str()))
        .or_else(|| activity.get("name").and_then(|v| v.as_str()))
    else {
        return Ok(RouteEffect::Skipped("skip: no reaction value"));
    };
    let Some(note_id) = find_note_by_ap_id(pool, &target_uri, local_host).await? else {
        return Ok(RouteEffect::Skipped("skip: target note not found"));
    };
    if mk_db::note::find_note_reaction(pool, &note_id, &auth.id)
        .await
        .map_err(|_| ())?
        .is_some()
    {
        return Ok(RouteEffect::Skipped("skip: already reacted"));
    }
    mk_db::note::insert_note_reaction(
        pool,
        &mk_types::id::gen_aid_now(),
        &auth.id,
        &note_id,
        reaction,
    )
    .await
    .map_err(|_| ())?;
    mk_db::note::bump_reactions_map(pool, &note_id, reaction, 1)
        .await
        .map_err(|_| ())?;
    Ok(RouteEffect::Liked {
        reaction: reaction.to_string(),
    })
}

/// `apNoteService.fetchNote(targetUri)`'s parseUri split: local note uris
/// resolve by id (our local rows carry `uri` NULL), everything else by the
/// `note.uri` column (what the Create arm stores).
async fn find_note_by_ap_id(
    pool: &PgPool,
    uri: &str,
    local_host: &str,
) -> Result<Option<String>, ()> {
    if let Some(id) = parse_local_ap_id(uri, local_host, "notes") {
        let row: Option<String> = sqlx::query_scalar(r#"SELECT "id" FROM "note" WHERE "id" = $1"#)
            .bind(&id)
            .fetch_optional(pool)
            .await
            .map_err(|_| ())?;
        if row.is_some() {
            return Ok(row);
        }
    }
    let row: Option<String> = sqlx::query_scalar(r#"SELECT "id" FROM "note" WHERE "uri" = $1"#)
        .bind(uri)
        .fetch_optional(pool)
        .await
        .map_err(|_| ())?;
    Ok(row)
}

// ---------------------------------------------------------------------------
// Undo
// ---------------------------------------------------------------------------

/// ApInboxService.undo: the inner object must be INLINE (a string uri needs
/// resolver.resolve — the remote-fetch boundary), then the Follow/Like arms
/// (upstream checks isFollow before isLike, and the remaining object kinds
/// — Block/Announce/Accept — keep their lanes pending).
async fn undo(
    pool: &PgPool,
    auth: &AuthUserRow,
    activity: &Value,
    local_host: &str,
) -> Result<RouteEffect, ()> {
    let Some(inner) = activity.get("object").and_then(|v| v.as_object()) else {
        return Ok(RouteEffect::Skipped(
            "resolver boundary: undo object is not an inline object",
        ));
    };
    let inner = Value::Object(inner.clone());
    let inner_type = inner.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match inner_type {
        "Follow" => undo_follow(pool, auth, &inner, local_host).await,
        "Like" => undo_like(pool, auth, &inner, local_host).await,
        _ => Ok(RouteEffect::Skipped("skip: unknown object type")),
    }
}

/// `undoFollow`: request-then-following (upstream cancels a pending
/// follow_request first, else unfollows, else the
/// リクエストもフォローもされていない skip). The followee gate is the same
/// local-user check as Follow.
async fn undo_follow(
    pool: &PgPool,
    auth: &AuthUserRow,
    inner: &Value,
    local_host: &str,
) -> Result<RouteEffect, ()> {
    let Some(followee_id) =
        local_user_id_from_object(inner.get("object"), pool, local_host).await?
    else {
        return Ok(RouteEffect::Skipped("skip: followee not found"));
    };
    if mk_db::social::follow_request_exists(pool, &auth.id, &followee_id)
        .await
        .map_err(|_| ())?
    {
        mk_db::social::delete_follow_request(pool, &auth.id, &followee_id)
            .await
            .map_err(|_| ())?;
        return Ok(RouteEffect::Unfollowed);
    }
    if mk_db::social::following_exists(pool, &auth.id, &followee_id)
        .await
        .map_err(|_| ())?
    {
        mk_db::social::delete_following(pool, &auth.id, &followee_id)
            .await
            .map_err(|_| ())?;
        mk_db::social::bump_follow_counts(pool, &auth.id, &followee_id, -1)
            .await
            .map_err(|_| ())?;
        return Ok(RouteEffect::Unfollowed);
    }
    Ok(RouteEffect::Skipped(
        "skip: リクエストもフォローもされていない",
    ))
}

/// `undoLike`: delete the reaction row (+ the map decrement). The
/// not-reacted IdentifiableError ('60527ec9…') is swallowed upstream →
/// still 'ok', so an absent row stays a successful Unliked.
async fn undo_like(
    pool: &PgPool,
    auth: &AuthUserRow,
    inner: &Value,
    local_host: &str,
) -> Result<RouteEffect, ()> {
    let Some(target_uri) = get_ap_id(inner.get("object")) else {
        return Ok(RouteEffect::Skipped("skip: target note not found"));
    };
    let Some(note_id) = find_note_by_ap_id(pool, &target_uri, local_host).await? else {
        return Ok(RouteEffect::Skipped("skip: target note not found"));
    };
    if let Some(row) = mk_db::note::find_note_reaction(pool, &note_id, &auth.id)
        .await
        .map_err(|_| ())?
    {
        mk_db::note::delete_note_reaction(pool, &note_id, &auth.id)
            .await
            .map_err(|_| ())?;
        mk_db::note::bump_reactions_map(pool, &note_id, &row.reaction, -1)
            .await
            .map_err(|_| ())?;
    }
    Ok(RouteEffect::Unliked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const HOST: &str = "127.0.0.1:3000";

    #[test]
    fn parse_local_ap_id_pins_parse_uri_rules() {
        // actors sign `#main-key`; the fragment never reaches the id
        assert_eq!(
            parse_local_ap_id(
                &format!("https://{HOST}/users/abc123#main-key"),
                HOST,
                "users"
            ),
            Some("abc123".to_string())
        );
        // host folds case-insensitively (toPuny), scheme is irrelevant
        assert_eq!(
            parse_local_ap_id("HTTPS://127.0.0.1:3000/users/abc123", HOST, "users"),
            Some("abc123".to_string())
        );
        // remote host → not local (the resolver boundary)
        assert_eq!(
            parse_local_ap_id("https://remote.example/users/abc123", HOST, "users"),
            None
        );
        // wrong type segment + trailing segments ignored beyond the id
        assert_eq!(
            parse_local_ap_id(&format!("https://{HOST}/notes/abc123"), HOST, "users"),
            None
        );
        assert_eq!(
            parse_local_ap_id(&format!("https://{HOST}/notes/abc123/x"), HOST, "notes"),
            Some("abc123".to_string())
        );
        assert_eq!(parse_local_ap_id("not-a-uri", HOST, "users"), None);
    }

    #[test]
    fn html_text_fold_pins_the_content_to_text_subset() {
        // <br> variants fold to \n, other tags strip, entities decode
        assert_eq!(html_text_fold("a<br>b<br />c"), "a\nb\nc");
        assert_eq!(html_text_fold("<p>hi</p>"), "hi");
        assert_eq!(html_text_fold("<b>BOLD</b>"), "BOLD");
        assert_eq!(
            html_text_fold("x &amp; y &lt;z&gt; &#39;q&#39;"),
            "x & y <z> 'q'"
        );
        // a stray & stays literal (no entity match)
        assert_eq!(html_text_fold("100% &fun"), "100% &fun");
        // unterminated tag: the tail drops like a truncated parse
        assert_eq!(html_text_fold("keep<br"), "keep");
        assert_eq!(html_text_fold("keep<br tail"), "keep");
    }

    #[test]
    fn get_visibility_pins_the_audience_half() {
        let public = "https://www.w3.org/ns/activitystreams#Public";
        let activity = json!({ "type": "Create", "to": [public] });
        // the note fixture stays audience-free: create()'s audience copy has
        // already merged activity.to into object.to upstream, so a note-side
        // Public here would leak into EVERY leg's merged `to`
        let note = json!({ "id": "n" });
        assert_eq!(get_visibility(&activity, &note), "public");
        // Public in cc → home (the 'quiet post' leg)
        let home = json!({ "type": "Create", "to": ["https://a/users/1"], "cc": [public] });
        assert_eq!(get_visibility(&home, &note), "home");
        // as:Public short form + no audience → followers
        let short = json!({ "type": "Create", "to": ["as:Public"] });
        assert_eq!(get_visibility(&short, &note), "public");
        assert_eq!(
            get_visibility(&json!({ "type": "Create" }), &note),
            "followers"
        );
        // the merge itself: the NOTE's own to supplies Public after the copy
        let from_note = json!({ "id": "n", "to": [public] });
        assert_eq!(
            get_visibility(&json!({ "type": "Create" }), &from_note),
            "public"
        );
    }
}
