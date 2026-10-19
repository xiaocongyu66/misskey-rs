//! arena-move: the pure account-migration helpers (upstream
//! `misc/acct.ts` Acct.parse, UserEntityService.genLocalUserUri /
//! getUserUri, ApPersonService.fetchPerson's local-URI shortcut and the
//! TypeORM `simple-array` codec backing `user.alsoKnownAs`).
//!
//! Everything here is a deterministic function so the `// arena-move`
//! tail in api/tests.rs (and the pinned e2e) can pin parity without a
//! database. The remote-federation arms of fetchPerson / resolveUser have no
//! lane in misskey-rs (documented in the module header of api::move_account)
//! — a URI that does not point back at this server resolves to "no person".

/// `Acct.parse(line)` — `@` prefixed input tolerated, `{ username, host }`
/// (endpoints/i/update.ts L463 + endpoints/i/move.ts L105 feed this).
/// Missing/empty input still parses (empty username) so the caller can map
/// it to the endpoint's own NO_SUCH_USER error.
pub fn parse_acct(line: &str) -> Option<(String, Option<String>)> {
    let acct = line.strip_prefix('@').unwrap_or(line);
    let mut parts = acct.splitn(2, '@');
    let username = parts.next()?;
    let host = parts.next().filter(|h| !h.is_empty());
    Some((username.to_string(), host.map(|h| h.to_string())))
}

/// TypeORM `simple-array` write shape (column type `text`, joined with ',').
pub fn marshal_aliases(uris: &[String]) -> String {
    uris.join(",")
}

/// TypeORM `simple-array` read shape. `NULL` means no aliases; a stored list
/// is always non-empty because the write side persists `null` when the
/// resolved set comes out empty.
pub fn unmarshal_aliases(raw: Option<&str>) -> Vec<String> {
    match raw {
        Some(csv) => csv.split(',').map(|s| s.to_string()).collect(),
        None => Vec::new(),
    }
}

/// `UserEntityService.genLocalUserUri`: `${config.url}/users/${id}` — the
/// base is trimmed so trailing-slash and bare bases produce the same
/// canonical URI.
pub fn local_user_uri(base_url: &str, user_id: &str) -> String {
    format!("{}/users/{}", base_url.trim_end_matches('/'), user_id)
}

/// `ApPersonService.fetchPerson` local shortcut: a URI pointing at this
/// server carries the user id in its last path segment. Any other URI is a
/// remote person, resolved by the federation fetch upstream — `None` here.
pub fn local_id_from_uri(base_url: &str, uri: &str) -> Option<String> {
    let prefix = format!("{}/users/", base_url.trim_end_matches('/'));
    let tail = match uri.strip_prefix(&prefix) {
        Some(t) => t,
        None => return None,
    };
    if tail.is_empty() {
        None
    } else {
        Some(tail.to_string())
    }
}

/// i/move alias parity core (endpoints/i/move.ts L122-129): the
/// destination's alsoKnownAs list is scanned with substring `includes`, NOT
/// equality — `knownAs.includes(fromUrl)`.
pub fn aliases_admit_local_user(alias_uris: &[String], local_user_uri: &str) -> bool {
    alias_uris.iter().any(|uri| uri.contains(local_user_uri))
}
