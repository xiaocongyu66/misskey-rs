//! `/.well-known/*` + `/nodeinfo/*` — routes that ride OUTSIDE `/api/*`
//! (well-known arena, upstream test/e2e/well-known.ts).
//!
//! Upstream flows mirrored:
//! - `WellKnownServerService.createServer` (packages/backend/src/server/
//!   WellKnownServerService.ts): the `onRequest` hook stamps
//!   `Access-Control-Allow-*` + `Access-Control-Expose-Headers: Vary` on
//!   every response, `OPTIONS /.well-known/*` → 204, `host-meta` (XRD XML),
//!   `host-meta.json` (LRD/JRD link template), `/.well-known/nodeinfo` (the
//!   NodeinfoServerService.getLinks relay), `oauth-authorization-server`
//!   (OAuth2ProviderService.generateRFC8414), and webfinger — JRD by default
//!   (`{ subject, links: [self ActivityPub, profile-page html,
//!   ostatus subscribe template] }`), XRD only when the Accept header
//!   prefers `application/xrd+xml`; `Vary: Accept` + `Cache-Control:
//!   public, max-age=180`; federation 'none' → 403 on every federating
//!   route; resource forms: `{url}/users/:id`, `{url}/@user`, `acct:user@host`
//!   (422 for foreign hosts, 404 unknown, 400 missing `resource`).
//! - `NodeinfoServerService` (packages/backend/src/server/
//!   NodeinfoServerService.ts): `nodeinfo2` document — software.name
//!   'misskey' + config.version (the harness resolves it from the pinned
//!   built/meta.json), openRegistrations, usage counts from postgres,
//!   metadata block; 2.0 deletes `software.repository`. Each response is
//!   stamped with the profile'd `application/json` content type and the
//!   same CORS/Vary set, `Cache-Control: public, max-age=600`.
//!
//! Known unported legs (unasserted by the pinned spec): XML escaping is a
//! plain entity pass (the mirrored values never carry markup), the oauth
//! metadata `scopes_supported` list is the static misskey-js `permissions`
//! snapshot, and the activeHalfyear/activeMonth chart counters stay null
//! (upstream defers them likewise). The 10m MemorySingleCache is likewise
//! unported — each request recomputes from postgres, so the upstream
//! cache-taint quirk (a 2.0 re-fetch deleting software.repository off the
//! SHARED cached base, later 2.1 relays inheriting the deletion within the
//! 10m window) cannot reproduce and we ship the clean document instead.
//! document usage counters read postgres directly where upstream reads the
//! notes/users hour charts — fresh e2e dbs make both equal the real counts.

use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;

/// NodeinfoServerService consts.
const NODEINFO_HOMEPAGE: &str = "https://misskey-hub.net";
const MAX_NOTE_TEXT_LENGTH: i64 = 3000; // upstream const.js MAX_NOTE_TEXT_LENGTH

/// Pinned-commit upstream version (built/meta.json in the e2e harness —
/// `node -e "require('./package.json').version"`): used when config.yaml /
/// env carry no explicit version (upstream config.ts L267-275).
const PINNED_VERSION: &str = "2026.10.1-alpha.0";

/// WellKnownServerService content types.
const JRD: &str = "application/jrd+json";
const XRD: &str = "application/xrd+xml";

/// OAuth2ProviderService scopes_supported = misskey-js `permissions`
/// (packages/misskey-js/src/consts.ts at the pinned commit).
const OAUTH_KINDS: &[&str] = &[
    "read:account",
    "write:account",
    "read:blocks",
    "write:blocks",
    "read:drive",
    "write:drive",
    "read:favorites",
    "write:favorites",
    "read:following",
    "write:following",
    "read:messaging",
    "write:messaging",
    "read:mutes",
    "write:mutes",
    "write:notes",
    "read:notifications",
    "write:notifications",
    "read:reactions",
    "write:reactions",
    "write:votes",
    "read:pages",
    "write:pages",
    "write:page-likes",
    "read:page-likes",
    "read:user-groups",
    "write:user-groups",
    "read:channels",
    "write:channels",
    "read:gallery",
    "write:gallery",
    "read:gallery-likes",
    "write:gallery-likes",
    "read:flash",
    "write:flash",
    "read:flash-likes",
    "write:flash-likes",
    "read:admin:abuse-user-reports",
    "write:admin:delete-account",
    "write:admin:delete-all-files-of-a-user",
    "read:admin:index-stats",
    "read:admin:table-stats",
    "read:admin:user-ips",
    "read:admin:meta",
    "write:admin:reset-password",
    "write:admin:resolve-abuse-user-report",
    "write:admin:send-email",
    "read:admin:server-info",
    "read:admin:show-moderation-log",
    "read:admin:show-user",
    "write:admin:suspend-user",
    "write:admin:unset-mfa",
    "write:admin:unset-user-avatar",
    "write:admin:unset-user-banner",
    "write:admin:unsuspend-user",
    "write:admin:meta",
    "write:admin:user-note",
    "write:admin:roles",
    "read:admin:roles",
    "write:admin:relays",
    "read:admin:relays",
    "write:admin:invite-codes",
    "read:admin:invite-codes",
    "write:admin:announcements",
    "read:admin:announcements",
    "write:admin:avatar-decorations",
    "read:admin:avatar-decorations",
    "write:admin:federation",
    "write:admin:account",
    "read:admin:account",
    "write:admin:emoji",
    "read:admin:emoji",
    "write:admin:queue",
    "read:admin:queue",
    "write:admin:promo",
    "write:admin:drive",
    "read:admin:drive",
    "write:admin:ad",
    "read:admin:ad",
    "write:invite-codes",
    "read:invite-codes",
    "write:clip-favorite",
    "read:clip-favorite",
    "read:federation",
    "write:report-abuse",
    "write:chat",
    "read:chat",
    "updateServerSettings",
    "suspend",
    "unsuspend",
    "updateUserNote",
    "addCustomEmoji",
    "updateCustomEmoji",
    "deleteCustomEmoji",
    "assignRole",
    "unassignRole",
    "createRole",
    "updateRole",
    "deleteRole",
    "clearQueue",
    "promoteQueue",
    "deleteDriveFile",
    "deleteNote",
    "createGlobalAnnouncement",
    "createUserAnnouncement",
    "updateGlobalAnnouncement",
    "updateUserAnnouncement",
    "deleteGlobalAnnouncement",
    "deleteUserAnnouncement",
    "resetPassword",
    "suspendRemoteInstance",
    "unsuspendRemoteInstance",
    "updateRemoteInstanceNote",
    "markSensitiveDriveFile",
    "unmarkSensitiveDriveFile",
    "resolveAbuseReport",
    "forwardAbuseReport",
    "updateAbuseReportNote",
    "createInvitation",
    "createAd",
    "updateAd",
    "deleteAd",
    "createAvatarDecoration",
    "updateAvatarDecoration",
    "deleteAvatarDecoration",
    "unsetMfa",
    "unsetUserAvatar",
    "unsetUserBanner",
    "createSystemWebhook",
    "updateSystemWebhook",
    "deleteSystemWebhook",
    "createAbuseReportNotificationRecipient",
    "updateAbuseReportNotificationRecipient",
    "deleteAbuseReportNotificationRecipient",
    "deleteAccount",
    "deletePage",
    "deleteFlash",
    "deleteGalleryPost",
    "deleteChatRoom",
    "updateProxyAccountDescription",
    "gtlAvailable",
    "ltlAvailable",
    "canPublicNote",
    "mentionLimit",
    "canInvite",
    "inviteLimit",
    "inviteLimitCycle",
    "inviteExpirationTime",
    "canManageCustomEmojis",
    "canManageAvatarDecorations",
    "canSearchNotes",
    "canSearchUsers",
    "canUseTranslator",
    "canHideAds",
    "canCreateChannel",
    "driveCapacityMb",
    "maxFileSizeMb",
    "alwaysMarkNsfw",
    "canUpdateBioMedia",
    "pinLimit",
    "antennaLimit",
    "wordMuteLimit",
    "webhookLimit",
    "clipLimit",
    "noteEachClipsLimit",
    "userListLimit",
    "userEachUserListsLimit",
    "rateLimitFactor",
    "avatarDecorationLimit",
    "canImportAntennas",
    "canImportBlocking",
    "canImportFollowing",
    "canImportMuting",
    "canImportUserLists",
    "chatAvailability",
    "uploadableFileTypes",
    "noteDraftLimit",
    "scheduledNoteLimit",
    "watermarkAvailable",
    "system",
    "endedPollNotification",
    "postScheduledNote",
    "deliver",
    "inbox",
    "db",
    "relationship",
    "objectStorage",
    "userWebhookDeliver",
    "systemWebhookDeliver",
    "map",
    "bw",
    "isLlotheo",
    "canPutEverywhere",
    "loopedBoard",
    "timeLimitForEachTurn",
];

// ---------------------------------------------------------------------------
// response helpers
// ---------------------------------------------------------------------------

/// The plugin-wide `onRequest` hook — stamped on every /.well-known/* reply.
fn stamp_wellknown_headers(res: &mut Response) {
    let h = res.headers_mut();
    h.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    h.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("Accept"),
    );
    h.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, OPTIONS"),
    );
    h.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("Vary"),
    );
}

fn wk_response(status: StatusCode, content_type: &str, body: Vec<u8>) -> Response {
    let mut res = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .body(axum::body::Body::from(body))
        .unwrap();
    stamp_wellknown_headers(&mut res);
    res
}

/// fastify `reply.code(n); return;` — status-only empty body.
fn wk_status(status: StatusCode) -> Response {
    let mut res = status.into_response();
    stamp_wellknown_headers(&mut res);
    res
}

/// misc/prelude/xml.ts escapers — the pinned specials set; the mirrored
/// attribute/value inputs (urls, rels, template uris) never carry markup.
fn escape_attribute(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn escape_value(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// WellKnownServerService XRD builder: attribute-carrying self-closing
/// elements or text-valued `<el>value</el>` pairs.
struct XrdEl {
    element: &'static str,
    value: Option<String>,
    attributes: Vec<(&'static str, String)>,
}

fn xrd(parts: &[XrdEl]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><XRD xmlns=\"http://docs.oasis-open.org/ns/xri/xrd-1.0\">",
    );
    for p in parts {
        out.push('<');
        out.push_str(p.element);
        for (k, v) in &p.attributes {
            out.push(' ');
            out.push_str(k);
            out.push_str("=\"");
            out.push_str(&escape_attribute(v));
            out.push('"');
        }
        match &p.value {
            Some(v) => {
                out.push('>');
                out.push_str(&escape_value(v));
                out.push_str("</");
                out.push_str(p.element);
                out.push('>');
            }
            None => out.push_str("/>"),
        }
    }
    out.push_str("</XRD>");
    out
}

/// NodeinfoServerService.getLinks — the relay body of /.well-known/nodeinfo.
fn nodeinfo_links(origin: &str) -> Value {
    json!([{
        "rel": "http://nodeinfo.diaspora.software/ns/schema/2.1",
        "href": format!("{origin}/nodeinfo/2.1"),
    }, {
        "rel": "http://nodeinfo.diaspora.software/ns/schema/2.0",
        "href": format!("{origin}/nodeinfo/2.0"),
    }])
}

// ---------------------------------------------------------------------------
// dispatch
// ---------------------------------------------------------------------------

pub(crate) async fn well_known_dispatch(State(state): State<AppState>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().clone();
    // owned extracts BEFORE any await: `&Request` across an await makes the
    // future non-Send (axum::body::Body is not Sync) and the Handler bound —
    // E0277 on CI 39208798436 / 39210555086 — never resolves.
    let accept = req
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let resource = extract_resource(&req);

    // fastify.options(allPath) → 204 (hook headers ride along).
    if method == Method::OPTIONS {
        return wk_status(StatusCode::NO_CONTENT);
    }

    if method != Method::GET {
        return wk_status(StatusCode::NOT_FOUND);
    }

    match path.as_str() {
        "/.well-known/host-meta" => host_meta(&state).await,
        "/.well-known/host-meta.json" => host_meta_json(&state).await,
        "/.well-known/nodeinfo" => nodeinfo_index(&state).await,
        "/.well-known/oauth-authorization-server" => oauth_authorization_server(&state).await,
        "/.well-known/webfinger" => webfinger(&state, resource, accept).await,
        // WellKnownServerService change-password redirect (302 → /settings/security)
        "/.well-known/change-password" => {
            let mut res = Response::builder()
                .status(StatusCode::FOUND)
                .header(
                    header::LOCATION,
                    format!(
                        "{}/settings/security",
                        state.config.url.trim_end_matches('/')
                    ),
                )
                .body(axum::body::Body::empty())
                .unwrap();
            stamp_wellknown_headers(&mut res);
            res
        }
        _ => wk_status(StatusCode::NOT_FOUND),
    }
}

/// meta.federation === 'none' → 403 on every federating route (missing row
/// resolves to the MetaService synthesized default 'none'). Reader shared
/// with the AP collections gate (mk-db/src/ff_visibility.rs — the
/// ff-visibility lane lands the same column read; the update-meta persist
/// arm lives there too).
async fn federation_gate(state: &AppState) -> Option<Response> {
    let Some(pool) = &state.pool else {
        return Some(wk_status(StatusCode::FORBIDDEN));
    };
    match mk_db::ff_visibility::meta_federation(pool).await {
        Ok(f) if f == "none" => Some(wk_status(StatusCode::FORBIDDEN)),
        Ok(_) => None,
        Err(e) => {
            tracing::error!(error = %e, "meta_federation");
            Some(wk_status(StatusCode::INTERNAL_SERVER_ERROR))
        }
    }
}

async fn host_meta(state: &AppState) -> Response {
    if let Some(res) = federation_gate(state).await {
        return res;
    }
    let origin = state.config.url.trim_end_matches('/');
    let body = xrd(&[XrdEl {
        element: "Link",
        value: None,
        attributes: vec![
            ("rel", "lrdd".to_string()),
            ("type", XRD.to_string()),
            (
                "template",
                format!("{origin}/.well-known/webfinger?resource={{uri}}"),
            ),
        ],
    }]);
    wk_response(StatusCode::OK, XRD, body.into_bytes())
}

async fn host_meta_json(state: &AppState) -> Response {
    if let Some(res) = federation_gate(state).await {
        return res;
    }
    let origin = state.config.url.trim_end_matches('/');
    let body = json!({
        "links": [{
            "rel": "lrdd",
            "type": JRD,
            "template": format!("{origin}/.well-known/webfinger?resource={{uri}}"),
        }],
    });
    // bare application/json → the main.rs charset layer normalizes to
    // `application/json; charset=utf-8` like fastify does.
    wk_response(
        StatusCode::OK,
        "application/json",
        body.to_string().into_bytes(),
    )
}

async fn nodeinfo_index(state: &AppState) -> Response {
    if let Some(res) = federation_gate(state).await {
        return res;
    }
    let origin = state.config.url.trim_end_matches('/');
    let body = json!({ "links": nodeinfo_links(origin) });
    wk_response(
        StatusCode::OK,
        "application/json",
        body.to_string().into_bytes(),
    )
}

/// OAuth2ProviderService.generateRFC8414 (no federation gate upstream).
async fn oauth_authorization_server(state: &AppState) -> Response {
    let origin = state.config.url.trim_end_matches('/');
    let body = json!({
        "issuer": origin,
        "authorization_endpoint": format!("{origin}/oauth/authorize"),
        "token_endpoint": format!("{origin}/oauth/token"),
        "scopes_supported": OAUTH_KINDS,
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code"],
        "service_documentation": "https://misskey-hub.net",
        "code_challenge_methods_supported": ["S256"],
        "authorization_response_iss_parameter_supported": true,
    });
    wk_response(
        StatusCode::OK,
        "application/json",
        body.to_string().into_bytes(),
    )
}

// ---------------------------------------------------------------------------
// webfinger
// ---------------------------------------------------------------------------

struct WebfingerUser {
    id: String,
    username: String,
}

/// upstream `request.query.resource` — first `resource=` pair of the raw
/// query, percent-decoded (node-fetch rides unencoded acct: URIs but real
/// world resources arrive escaped).
fn extract_resource(req: &Request) -> Option<String> {
    let query = req.uri().query()?;
    for pair in query.split('&') {
        if let Some(rest) = pair.strip_prefix("resource=") {
            return Some(percent_decode(rest));
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push(((h << 4) | l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Acct.parse: strip a leading '@', split('@', 2) — host absent → None.
fn acct_parse(acct: &str) -> (String, Option<String>) {
    let acct = acct.strip_prefix('@').unwrap_or(acct);
    let mut it = acct.splitn(2, '@');
    let username = it.next().unwrap_or("").to_string();
    let host = it.next().map(|h| h.to_string());
    (username, host)
}

async fn webfinger(state: &AppState, resource: Option<String>, accept: Option<String>) -> Response {
    if let Some(res) = federation_gate(state).await {
        return res;
    }
    let Some(resource) = resource else {
        // `typeof request.query.resource !== 'string'` → 400
        return wk_status(StatusCode::BAD_REQUEST);
    };

    let origin = state.config.url.trim_end_matches('/');
    let host = state.config.host();
    let lower = resource.to_lowercase();
    let url_lower = origin.to_lowercase();

    let lookup: Result<Option<WebfingerUser>, String> =
        if lower.starts_with(&format!("{url_lower}/users/")) {
            // fromId — resource.split('/').pop()
            let id = lower.rsplit('/').next().unwrap_or("").to_string();
            query_user_by_id(state, &id).await
        } else {
            let acct_str: String = if lower.starts_with(&format!("{url_lower}/@")) {
                lower.rsplit('/').next().unwrap_or("").to_string()
            } else if let Some(stripped) = lower.strip_prefix("acct:") {
                stripped.to_string()
            } else {
                lower.clone()
            };
            let (username, acct_host) = acct_parse(&acct_str);
            // fromAcct — local (or hostless) accounts only; foreign hosts 422
            if acct_host.is_some() && acct_host.as_deref() != Some(host.to_lowercase().as_str()) {
                return wk_status(
                    StatusCode::from_u16(422).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY),
                );
            }
            query_user_by_username(state, &username).await
        };

    let user = match lookup {
        Ok(Some(u)) => u,
        Ok(None) => return wk_status(StatusCode::NOT_FOUND),
        Err(e) => {
            tracing::error!(error = %e, "webfinger lookup");
            return wk_status(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    let subject = format!("acct:{}@{}", user.username, host);
    let self_href = format!("{origin}/users/{}", user.id);
    let profile_page = format!("{origin}/@{}", user.username);
    let subscribe = format!("{origin}/authorize-follow?acct={{uri}}");

    // vary(reply.raw, 'Accept') + Cache-Control (hook ACEH header exposes it)
    let wants_xrd = prefers_xrd(accept.as_deref().unwrap_or(""));

    if wants_xrd {
        let body = xrd(&[
            XrdEl {
                element: "Subject",
                value: Some(subject),
                attributes: vec![],
            },
            XrdEl {
                element: "Link",
                value: None,
                attributes: vec![
                    ("rel", "self".to_string()),
                    ("type", "application/activity+json".to_string()),
                    ("href", self_href),
                ],
            },
            XrdEl {
                element: "Link",
                value: None,
                attributes: vec![
                    ("rel", "http://webfinger.net/rel/profile-page".to_string()),
                    ("type", "text/html".to_string()),
                    ("href", profile_page),
                ],
            },
            XrdEl {
                element: "Link",
                value: None,
                attributes: vec![
                    ("rel", "http://ostatus.org/schema/1.0/subscribe".to_string()),
                    ("template", subscribe),
                ],
            },
        ]);
        let mut res = wk_response(StatusCode::OK, XRD, body.into_bytes());
        vary_accept(&mut res);
        res
    } else {
        let body = json!({
            "subject": subject,
            "links": [{
                "rel": "self",
                "type": "application/activity+json",
                "href": self_href,
            }, {
                "rel": "http://webfinger.net/rel/profile-page",
                "type": "text/html",
                "href": profile_page,
            }, {
                "rel": "http://ostatus.org/schema/1.0/subscribe",
                "template": subscribe,
            }],
        });
        let mut res = wk_response(StatusCode::OK, JRD, body.to_string().into_bytes());
        vary_accept(&mut res);
        res
    }
}

fn vary_accept(res: &mut Response) {
    res.headers_mut()
        .insert(header::VARY, HeaderValue::from_static("Accept"));
    res.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=180"),
    );
}

/// `request.accepts().type([jrd, xrd]) === xrd` — the pinned spec only ever
/// exercises the default (`*/*` or unset → jrd); a q-negotiated xrd win is
/// mirrored on q-value comparison of the two listed types.
fn prefers_xrd(accept: &str) -> bool {
    let mut jrd_q = 0.0f32;
    let mut xrd_q = 0.0f32;
    for part in accept.split(',') {
        let mut seg = part.splitn(2, ';');
        let mime = seg.next().unwrap_or("").trim().to_lowercase();
        let mut q = 1.0f32;
        for param in seg {
            let param = param.trim();
            if let Some(v) = param.strip_prefix("q=") {
                q = v.trim().parse().unwrap_or(1.0);
            }
        }
        match mime.as_str() {
            "application/xrd+xml" => xrd_q = xrd_q.max(q),
            "application/jrd+json" => jrd_q = jrd_q.max(q),
            "*/*" | "application/*" => {
                // wildcard fits both; the first listed type (jrd) wins ties
                jrd_q = jrd_q.max(q);
            }
            _ => {}
        }
    }
    xrd_q > jrd_q
}

async fn query_user_by_username(
    state: &AppState,
    username: &str,
) -> Result<Option<WebfingerUser>, String> {
    let Some(pool) = &state.pool else {
        return Err("no pool".to_string());
    };
    // fromAcct: { usernameLower, host: IsNull, isSuspended: false }
    mk_db::sqlx::query_as::<_, (String, String)>(
        r#"SELECT "id", "username" FROM "user"
           WHERE "usernameLower" = $1 AND "host" IS NULL AND "isSuspended" = false
           ORDER BY "id" ASC LIMIT 1"#,
    )
    .bind(username)
    .fetch_optional(pool)
    .await
    .map(|row| row.map(|(id, username)| WebfingerUser { id, username }))
    .map_err(|e| e.to_string())
}

async fn query_user_by_id(state: &AppState, id: &str) -> Result<Option<WebfingerUser>, String> {
    let Some(pool) = &state.pool else {
        return Err("no pool".to_string());
    };
    // fromId: { id, host: IsNull, isSuspended: false }
    mk_db::sqlx::query_as::<_, (String, String)>(
        r#"SELECT "id", "username" FROM "user"
           WHERE "id" = $1 AND "host" IS NULL AND "isSuspended" = false
           LIMIT 1"#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map(|row| row.map(|(id, username)| WebfingerUser { id, username }))
    .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// nodeinfo 2.0 / 2.1
// ---------------------------------------------------------------------------

pub(crate) async fn nodeinfo_20(State(state): State<AppState>) -> Response {
    let mut body = nodeinfo_document(&state, "2.0").await;
    // NodeinfoServerService 2.0 arm: `delete base.software.repository`
    if let Some(sw) = body.get_mut("software") {
        if let Some(o) = sw.as_object_mut() {
            o.remove("repository");
        }
    }
    nodeinfo_response("2.0", body)
}

pub(crate) async fn nodeinfo_21(State(state): State<AppState>) -> Response {
    let body = nodeinfo_document(&state, "2.1").await;
    nodeinfo_response("2.1", body)
}

fn nodeinfo_response(doc_version: &str, body: Value) -> Response {
    let mut res = wk_response(
        StatusCode::OK,
        &format!(
            "application/json; profile=\"http://nodeinfo.diaspora.software/ns/schema/{doc_version}#\""
        ),
        body.to_string().into_bytes(),
    );
    // NodeinfoServerService response stamps: the CORS/ACEH set rides the
    // shared helper; the cache-control line rides the fastify arm.
    res.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=600"),
    );
    res
}

/// NodeinfoServerService.nodeinfo2 — real postgres counts (users/note), meta
/// row through MetaService.fetch(true) semantics.
async fn nodeinfo_document(state: &AppState, doc_version: &str) -> Value {
    let version = state
        .config
        .version
        .clone()
        .or_else(|| std::env::var("MISSKEY_RS_VERSION").ok())
        .unwrap_or_else(|| PINNED_VERSION.to_string());

    let (meta, users_total, local_posts, proxy_name) = match &state.pool {
        Some(pool) => {
            let meta = mk_db::meta::fetch_nodeinfo_meta(pool).await.ok();
            let users_total: i64 =
                mk_db::sqlx::query_scalar(r#"SELECT count(*) FROM "user" WHERE "host" IS NULL"#)
                    .fetch_one(pool)
                    .await
                    .unwrap_or(0);
            // usage.localPosts — the local-notes count (deleted excluded)
            let local_posts: i64 = mk_db::sqlx::query_scalar(
                r#"SELECT count(*) FROM "note" WHERE "userHost" IS NULL AND "deletedAt" IS NULL"#,
            )
            .fetch_one(pool)
            .await
            .unwrap_or(0);
            // SystemAccountService.fetch('proxy').username — the system user
            // ships with the fixed name 'proxy' once created.
            let proxy_name: Option<String> = mk_db::sqlx::query_scalar(
                r#"SELECT "username" FROM "user" WHERE "usernameLower" = 'proxy' AND "host" IS NULL LIMIT 1"#,
            )
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
            (meta, users_total, local_posts, proxy_name)
        }
        None => (None, 0, 0, None),
    };

    let meta = meta.unwrap_or_else(|| mk_db::meta::MetaNodeinfoRow {
        name: None,
        description: None,
        maintainer_name: None,
        maintainer_email: None,
        // MiMeta / Init.js meta default — a fresh instance REGISTRATIONS OPEN
        // (openRegistrations true); the meta table by extension.
        disable_registration: false,
        langs: vec![],
        terms_of_service_url: None,
        privacy_policy_url: None,
        inquiry_url: None,
        impressum_url: None,
        repository_url: "https://github.com/misskey-dev/misskey".to_string(),
        feedback_url: "https://github.com/misskey-dev/misskey/issues/new".to_string(),
        email_required_for_signup: false,
        enable_hcaptcha: false,
        enable_recaptcha: false,
        enable_mcaptcha: false,
        enable_turnstile: false,
        enable_email: false,
        enable_service_worker: false,
        theme_color: None,
        policies: json!({}),
    });

    // basePolicies = { ...DEFAULT_POLICIES, ...meta.policies } — the pinned
    // DEFAULT_POLICIES carry ltlAvailable/gtlAvailable true.
    let policy_flag = |key: &str| -> bool {
        meta.policies
            .get(key)
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    };

    // 2.0 keeps homepage = nodeinfo_homepage; 2.1 overrides both repository
    // and homepage to meta.repositoryUrl (NodeinfoServerService L128-131).
    let repository_url = meta.repository_url.clone();
    let mut software = json!({
        "name": "misskey",
        "version": version,
        "homepage": NODEINFO_HOMEPAGE,
        "repository": repository_url.clone(),
    });
    if doc_version == "2.1" {
        software["homepage"] = json!(repository_url.clone());
    }

    let maintainer_name = meta.maintainer_name.clone();
    let maintainer_email = meta.maintainer_email.clone();

    json!({
        "version": doc_version,
        "software": software,
        "protocols": ["activitypub"],
        "services": {
            "inbound": [],
            "outbound": ["atom1.0", "rss2.0"],
        },
        "openRegistrations": !meta.disable_registration,
        "usage": {
            "users": {
                "total": users_total,
                "activeHalfyear": null,
                "activeMonth": null,
            },
            "localPosts": local_posts,
            "localComments": 0,
        },
        "metadata": {
            "nodeName": meta.name,
            "nodeDescription": meta.description,
            "nodeAdmins": [{
                "name": maintainer_name.clone(),
                "email": maintainer_email.clone(),
            }],
            // deprecated
            "maintainer": {
                "name": maintainer_name,
                "email": maintainer_email,
            },
            "langs": meta.langs,
            "tosUrl": meta.terms_of_service_url,
            "privacyPolicyUrl": meta.privacy_policy_url,
            "inquiryUrl": meta.inquiry_url,
            "impressumUrl": meta.impressum_url,
            "repositoryUrl": repository_url.clone(),
            "feedbackUrl": meta.feedback_url,
            "disableRegistration": meta.disable_registration,
            "disableLocalTimeline": !policy_flag("ltlAvailable"),
            "disableGlobalTimeline": !policy_flag("gtlAvailable"),
            "emailRequiredForSignup": meta.email_required_for_signup,
            "enableHcaptcha": meta.enable_hcaptcha,
            "enableRecaptcha": meta.enable_recaptcha,
            "enableMcaptcha": meta.enable_mcaptcha,
            "enableTurnstile": meta.enable_turnstile,
            "maxNoteTextLength": MAX_NOTE_TEXT_LENGTH,
            "enableEmail": meta.enable_email,
            "enableServiceWorker": meta.enable_service_worker,
            "proxyAccountName": proxy_name.unwrap_or_else(|| "proxy".to_string()),
            "themeColor": meta.theme_color.clone().unwrap_or_else(|| "#86b300".to_string()),
        },
    })
}
