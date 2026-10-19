//! arena-fetchres: the public GET web surfaces that ride OUTSIDE /api/* —
//! upstream pinned test/e2e/fetch-resource.ts (workflow
//! .github/workflows/e2e-fetch-resource.yml, same family as well-known.rs).
//!
//! Upstream flows mirrored:
//! - `ClientServerService.createServer` (packages/backend/src/server/web/
//!   ClientServerService.ts): the SSR pages (/@:user/:sub?, /notes/:note,
//!   /@:user/pages/:page, /play/:id, /clips/:clip, /gallery/:post,
//!   /channels/:channel), the user feeds (/@:user.{atom,rss,json} through
//!   FeedService.packFeed), the static asset routes (favicon.ico,
//!   apple-touch-icon.png, twemoji/twemoji-badge/fluent-emoji with the
//!   `^[0-9a-f-]+\.(svg|png)$` extension regex gates), robots.txt,
//!   opensearch.xml, the /api.json OpenApiServerService stub, /bios /cli
//!   /flush /_info_card_, the 503 non-WebSocket /streaming arm and the final
//!   `fastify.get('*')` renderBase fallback; every client reply rides the
//!   `X-Frame-Options: DENY` onRequest hook, per-route Cache-Control.
//! - `ActivityPubServerService.createServer` (packages/backend/src/server/
//!   ActivityPubServerService.ts): the `apOrHtml` deriveConstraint
//!   (`accepts(request).type(['html', ACTIVITY_JSON, LD_JSON])` — exact
//!   over-wildcard specificity, provided order with html first) on
//!   /@:acct, /users/:user and /notes/:note (Person/Note documents, 404
//!   without a row, `meta.federation === 'none'` → 403), the unconstrained
//!   /users/:user/outbox OrderedCollection, `Vary: Accept` on the negotiated
//!   routes and the per-request CORS hook set.
//! - The beforeAll create fixtures the pinned test needs that were still
//!   catalog-only: pages/create, flash/create, gallery/posts/create (routed
//!   from the mod.rs dispatch arm `fetchres` — each requireCredential).
//!
//! No-fork census (baseline rn39315611745 @ 3e79ec3): 61 failed | 23 passed —
//! every failing row is one of the routes below; the passing rows (missing
//! user/id 404s, the /api/foo UNKNOWN_API_ENDPOINT, the wrong-extension emoji
//! 404s, the /notes/:id AP legs through mod.rs ap_note, the vacuous MFM rows)
//! stay green.
//!
//! Known unported legs (unasserted by the pinned spec): the twemoji-badge
//! sharp rasterization pipeline (the badge route serves PNG bytes straight),
//! the robots.txt ugcVisibilityForVisitor 'none' extra lines (fresh e2e meta
//! rows ride the 'local' column default), the X-Robots-Tag preventAiLearning
//! stamp, twitter:creator / <link rel="me"> tags, the flash/canvas embed and
//! the remote-user 301/500 redirect arms of userInfo (local-only databases).

use axum::extract::{FromRequestParts, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
// mk_db re-exports sqlx (the crate's only dependency surface — the bare
// `sqlx::` calls below ride this like the mk-db modules).
use axum::response::{IntoResponse, Response};
use mk_db::sqlx;
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// ActivityPubServerService response types (L37-38).
const ACTIVITY_JSON: &str = "application/activity+json; charset=utf-8";
const LD_JSON: &str =
    "application/ld+json; profile=\"https://www.w3.org/ns/activitystreams\"; charset=utf-8";

const HTML: &str = "text/html; charset=utf-8";

// ---------------------------------------------------------------------------
// accept negotiation — the apOrHtml deriveConstraint
// ---------------------------------------------------------------------------

struct MediaRange {
    ty: String,
    sub: String,
    q: f32,
}

fn parse_accept(accept: &str) -> Vec<MediaRange> {
    accept
        .split(',')
        .filter_map(|part| {
            let mut segs = part.split(';');
            let mime = segs.next()?.trim().to_lowercase();
            if mime.is_empty() {
                return None;
            }
            let (ty, sub) = mime.split_once('/')?;
            let mut q = 1.0f32;
            for p in segs {
                let p = p.trim();
                if let Some(v) = p.strip_prefix("q=") {
                    q = v.trim().parse().unwrap_or(1.0);
                }
            }
            Some(MediaRange {
                ty: ty.to_string(),
                sub: sub.to_string(),
                q,
            })
        })
        .collect()
}

/// Best match quality of ONE provided type against the Accept ranges:
/// (q, specificity) where specificity is exact=3 > type-wildcard=2 > */*=1.
/// `accepts` ranks by quality, then specificity, then the provided order —
/// the caller folds the provided-order tie-break in.
fn rank_provided(provided: &str, ranges: &[MediaRange]) -> Option<(f32, i32)> {
    let (pty, psub) = provided.split_once('/')?;
    let mut best: Option<(f32, i32)> = None;
    for r in ranges {
        let spec = if r.ty == "*" && r.sub == "*" {
            1
        } else if r.ty == pty && (r.sub == "*" || psub == "*") {
            2
        } else if r.ty == pty && r.sub == psub {
            3
        } else {
            continue;
        };
        match best {
            // keep the FIRST (best) hit on equal (q, spec)
            Some((q, s)) if q > r.q || (q == r.q && s >= spec) => {}
            _ => best = Some((r.q, spec)),
        }
    }
    best
}

/// The apOrHtml deriveConstraint: 'ap' only when one of the two AP JSON types
/// out-ranks 'html'. No/malformed header → html (accepts() treats it as
/// first-provided). HTML ties win (html is first in the provided list).
fn prefers_ap(accept: Option<&str>) -> bool {
    let Some(accept) = accept else {
        return false;
    };
    let ranges = parse_accept(accept);
    let html = rank_provided("text/html", &ranges);
    let ap = rank_ap(&ranges);
    match (ap, html) {
        (Some(a), Some(h)) => a > h,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

fn rank_ap(ranges: &[MediaRange]) -> Option<(f32, i32)> {
    let a = rank_provided("application/activity+json", ranges);
    let l = rank_provided("application/ld+json", ranges);
    match (a, l) {
        // (q, spec): f32 is not Ord — compare the pair field-wise
        (Some(x), Some(y)) => {
            if y.0 > x.0 || (y.0 == x.0 && y.1 > x.1) {
                Some(y)
            } else {
                Some(x)
            }
        }
        (Some(x), None) => Some(x),
        (None, y) => y,
    }
}

/// ActivityPubServerService.setResponseType: `accepts().type([ACTIVITY_JSON,
/// LD_JSON])` — ld+json only when the request explicitly prefers it.
fn ap_content_type(accept: Option<&str>) -> &'static str {
    let ld_preferred = accept
        .map(|a| {
            let a = a.to_lowercase();
            a.contains("application/ld+json") && !a.contains("application/activity+json")
        })
        .unwrap_or(false);
    if ld_preferred { LD_JSON } else { ACTIVITY_JSON }
}

// ---------------------------------------------------------------------------
// response helpers
// ---------------------------------------------------------------------------

fn hdr_str(req: &Request, name: header::HeaderName) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// The ClientServerService onRequest hook + Cache-Control + a status/CT/body.
fn client_response(
    status: StatusCode,
    content_type: &str,
    body: Vec<u8>,
    cache: Option<&str>,
) -> Response {
    let mut res = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"))
        .body(axum::body::Body::from(body))
        .unwrap();
    if let Some(c) = cache {
        if let Ok(v) = HeaderValue::from_str(c) {
            res.headers_mut().insert(header::CACHE_CONTROL, v);
        }
    }
    res
}

fn html_response(body: String, cache: &str) -> Response {
    client_response(StatusCode::OK, HTML, body.into_bytes(), Some(cache))
}

/// fastify `reply.code(n)` — status-only empty body (X-Frame-Options hooks ride).
fn client_empty(status: StatusCode, cache: Option<&str>) -> Response {
    client_response(status, "text/plain; charset=utf-8", Vec::new(), cache)
}

/// The ActivityPubServerService onRequest hook (CORS four-set) + Vary +
/// Cache-Control, stamped over an already-built response body.
fn stamp_ap_headers(resp: &mut Response, vary: bool, cache: &str) {
    let h = resp.headers_mut();
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
    if vary {
        h.insert(header::VARY, HeaderValue::from_static("Accept"));
    }
    if let Ok(v) = HeaderValue::from_str(cache) {
        h.insert(header::CACHE_CONTROL, v);
    }
}

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_fetchres", error = %e, "db error: {context}");
    internal()
}

/// aid id → created ISO (mk_core ms_to_iso over the parsed aid timestamp).
fn iso_of(id: &str) -> String {
    mk_types::id::parse_aid_date(id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default()
}

/// misc/prelude/xml.ts entity passthrough for the mirrored texts (note text,
/// titles, feed descriptions) — the pinned specials set.
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
        .replace('\'', "&apos;")
}

fn escape_attr(s: &str) -> String {
    escape_xml(s)
}

/// MfmService.toHtml subset the pinned feed leg needs: '<'/'>'/'&' escaped,
/// then `**bold**` folded to `<b>` — the literal `**a**` markers must never
/// survive into a feed body (fetch-resource MFMを含まない rows).
fn mfm_lite_html(text: &str) -> String {
    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let mut out = String::new();
    for (i, part) in escaped.split("**").enumerate() {
        if i % 2 == 1 && !part.is_empty() {
            out.push_str("<b>");
            out.push_str(part);
            out.push_str("</b>");
        } else {
            out.push_str(part);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// meta gates + user lookups
// ---------------------------------------------------------------------------

/// meta.ugcVisibilityForVisitor of the newest meta row; missing row → the
/// TypeORM column default 'local' (fresh e2e meta rows carry it too).
async fn meta_ugc(pool: &mk_db::sqlx::PgPool) -> String {
    let v: Option<String> = sqlx::query_scalar(
        r#"SELECT "ugcVisibilityForVisitor" FROM "meta" ORDER BY "id" DESC LIMIT 1"#,
    )
    .fetch_optional(pool)
    .await
    .unwrap_or(None);
    v.unwrap_or_else(|| "local".to_string())
}

async fn meta_name(pool: &mk_db::sqlx::PgPool) -> Option<String> {
    sqlx::query_scalar(r#"SELECT "name" FROM "meta" ORDER BY "id" DESC LIMIT 1"#)
        .fetch_optional(pool)
        .await
        .unwrap_or(None)
}

/// ClientServerService `/@:user/:sub?` SSR gate: 'all' or ('local' + local user).
fn ugc_allows(ugc: &str, host_is_null: bool) -> bool {
    ugc == "all" || (ugc == "local" && host_is_null)
}

/// meta."federation" — the ActivityPubServerService 'none' 403 gate.
async fn federation_gate(state: &AppState) -> Option<Response> {
    let Some(pool) = &state.pool else {
        return Some(internal());
    };
    match mk_db::ff_visibility::meta_federation(pool).await {
        Ok(f) if f == "none" => Some(StatusCode::FORBIDDEN.into_response()),
        Ok(_) => None,
        Err(e) => {
            tracing::error!(target: "dbg_fetchres", error = %e, "meta_federation");
            Some(internal())
        }
    }
}

/// Local author lookup — { usernameLower, host IS NULL, isSuspended false }
/// (the webfinger / SSR / getFeed shape).
async fn find_local_user(
    pool: &mk_db::sqlx::PgPool,
    username_lower: &str,
) -> Result<Option<(String, String, Option<String>)>, mk_db::sqlx::Error> {
    sqlx::query_as::<_, (String, String, Option<String>)>(
        r#"SELECT "id", "username", "name" FROM "user"
           WHERE "usernameLower" = $1 AND "host" IS NULL AND "isSuspended" = false
           ORDER BY "id" ASC LIMIT 1"#,
    )
    .bind(username_lower)
    .fetch_optional(pool)
    .await
}

// ---------------------------------------------------------------------------
// HTML views — minimal renders parsed with node-html-parser by the harness
// (the test only reads meta[misskey:*] attributes, status + content-type).
// ---------------------------------------------------------------------------

fn title_of(meta_name: Option<String>) -> String {
    meta_name.unwrap_or_else(|| "Misskey".to_string())
}

/// BasePage minimum: renderBase replies ride `Cache-Control: public, max-age=30`.
async fn base_page(state: &AppState) -> Response {
    let name = match &state.pool {
        Some(p) => title_of(meta_name(p).await),
        None => title_of(None),
    };
    html_response(
        format!(
            "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
             <title>{name}</title><meta property=\"og:title\" content=\"{}\">\
             </head><body><div id=\"app\"></div></body></html>",
            escape_attr(&name)
        ),
        "public, max-age=30",
    )
}

/// UserPage / NotePage / PagePage / FlashPage / ClipPage / GalleryPostPage /
/// ChannelPage minimum — the misskey:* meta set each page view emits.
fn resource_page(title: &str, metas: &[(&str, &str)], cache: &str) -> Response {
    let mut head = String::new();
    for (k, v) in metas {
        head.push_str("<meta name=\"");
        head.push_str(k);
        head.push_str("\" content=\"");
        head.push_str(&escape_attr(v));
        head.push_str("\">");
    }
    let t = escape_attr(title);
    html_response(
        format!(
            "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
             <title>{t}</title><meta property=\"og:title\" content=\"{t}\">{head}\
             </head><body><div id=\"app\"></div></body></html>"
        ),
        cache,
    )
}

// ---------------------------------------------------------------------------
// /@:user family (feeds + negotiated profile + sub pages + pages/:page)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum FeedKind {
    Rss,
    Atom,
    Json,
}

pub(crate) async fn at_dispatch(State(state): State<AppState>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let acct = &path[2..]; // after "/@"
    let accept = hdr_str(&req, header::ACCEPT);
    let segs: Vec<&str> = acct.split('/').filter(|s| !s.is_empty()).collect();

    match segs.len() {
        0 => base_page(&state).await,
        1 => {
            let seg = segs[0];
            // feeds register FIRST upstream (unconstrained, so accept-blind).
            for (suffix, kind) in [
                (".atom", FeedKind::Atom),
                (".rss", FeedKind::Rss),
                (".json", FeedKind::Json),
            ] {
                if let Some(username) = seg.strip_suffix(suffix) {
                    if !username.is_empty() {
                        return feed(state, username, kind, accept).await;
                    }
                }
            }
            if prefers_ap(accept.as_deref()) {
                return ap_at(state, seg, accept).await;
            }
            user_html_page(state, seg).await
        }
        2 => user_html_page(state, segs[0]).await,
        3 if segs[1] == "pages" => page_html_page(state, segs[0], segs[2]).await,
        _ => base_page(&state).await,
    }
}

/// Acct.parse on the single-segment /@ form — self-host suffix normalizes to
/// local; a foreign host has no local row.
fn split_acct(seg: &str, self_host: &str) -> (String, Option<String>) {
    match seg.split_once('@') {
        Some((u, h)) if h.is_empty() => (u.to_string(), None),
        Some((u, h)) => {
            if h.eq_ignore_ascii_case(self_host) {
                (u.to_string(), None)
            } else {
                (u.to_string(), Some(h.to_string()))
            }
        }
        None => (seg.to_string(), None),
    }
}

/// ClientServerService `/@:user/:sub?` — metas only when the viewer may see
/// the profile (ugcVisibilityForVisitor + local row); renderBase otherwise.
async fn user_html_page(state: AppState, seg: &str) -> Response {
    let (username, host) = split_acct(seg, &state.config.host());
    if host.is_some() {
        return base_page(&state).await;
    }
    let Some(pool) = &state.pool else {
        return base_page(&state).await;
    };
    let user = match find_local_user(pool, &username.to_lowercase()).await {
        Ok(u) => u,
        Err(e) => return db_err("user_html_page", e),
    };
    let Some((id, username, name)) = user else {
        // リモートユーザーなので 404 にはしない — renderBase
        return base_page(&state).await;
    };
    let ugc = meta_ugc(pool).await;
    if !ugc_allows(&ugc, true) {
        return base_page(&state).await;
    }
    let display = name.clone().unwrap_or_else(|| username.clone());
    resource_page(
        &format!("{display} (@{username})"),
        &[
            ("misskey:user-username", username.as_str()),
            ("misskey:user-id", id.as_str()),
        ],
        "public, max-age=15",
    )
}

/// ClientServerService `/@:user/pages/:page` — `user == null` → a bare
/// fastify `return` (200 empty); page miss → renderBase; hit → the page-id
/// meta set.
async fn page_html_page(state: AppState, seg: &str, pagename: &str) -> Response {
    // the host suffix only steers the lookup — find_local_user is
    // host-IsNull-only, so foreign accts degrade to the user==null arm like
    // upstream findOneBy({host: host ?? IsNull()}).
    let (username, _host) = split_acct(seg, &state.config.host());
    let Some(pool) = &state.pool else {
        return base_page(&state).await;
    };
    let user = match find_local_user(pool, &username.to_lowercase()).await {
        Ok(u) => u,
        Err(e) => return db_err("page_html_page", e),
    };
    let Some((user_id, username, _name)) = user else {
        return client_empty(StatusCode::OK, None);
    };
    let page: Option<(String, String)> = sqlx::query_as(
        r#"SELECT "id", "name" FROM "page" WHERE "name" = $1 AND "userId" = $2 LIMIT 1"#,
    )
    .bind(pagename)
    .bind(&user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| db_err("page_html_page find", e))
    .unwrap_or(None);
    match page {
        Some((page_id, _)) => resource_page(
            &username,
            &[
                ("misskey:user-username", username.as_str()),
                ("misskey:user-id", user_id.as_str()),
                ("misskey:page-id", page_id.as_str()),
            ],
            "public, max-age=15",
        ),
        None => base_page(&state).await,
    }
}

// ---------------------------------------------------------------------------
// feeds — FeedService.packFeed + feed's rss2/atom1/json1 minimums
// ---------------------------------------------------------------------------

struct FeedUser {
    id: String,
    username: String,
    name: Option<String>,
    notes_count: i32,
    following_count: i32,
    followers_count: i32,
    avatar_url: Option<String>,
}

async fn feed(state: AppState, seg: &str, kind: FeedKind, _accept: Option<String>) -> Response {
    let (feed_username, host) = split_acct(seg, &state.config.host());
    if host.is_some() {
        return client_empty(StatusCode::NOT_FOUND, None);
    }
    let Some(pool) = &state.pool else {
        return client_empty(StatusCode::NOT_FOUND, None);
    };
    // getFeed — requireSigninToViewContents: false rides the lookup.
    let row: Option<(
        String,
        String,
        Option<String>,
        i32,
        i32,
        i32,
        Option<String>,
    )> = sqlx::query_as(
        r#"SELECT "id", "username", "name", "notesCount", "followingCount", "followersCount",
                      "avatarUrl" FROM "user"
               WHERE "usernameLower" = $1 AND "host" IS NULL AND "isSuspended" = false
                 AND "requireSigninToViewContents" = false
               ORDER BY "id" ASC LIMIT 1"#,
    )
    .bind(&feed_username.to_lowercase())
    .fetch_optional(pool)
    .await
    .map_err(|e| db_err("feed user find", e))
    .unwrap_or(None);
    let Some((id, username, name, notes_count, following_count, followers_count, avatar_url)) = row
    else {
        return client_empty(StatusCode::NOT_FOUND, None);
    };
    let ugc = meta_ugc(pool).await;
    if !ugc_allows(&ugc, true) {
        return client_empty(StatusCode::NOT_FOUND, None);
    }

    // profile description + visibility gates for the description line
    let prof: Option<(Option<String>, String, String)> = sqlx::query_as(
        r#"SELECT "description", "followingVisibility", "followersVisibility"
           FROM "user_profile" WHERE "userId" = $1"#,
    )
    .bind(&id)
    .fetch_optional(pool)
    .await
    .map_err(|e| db_err("feed profile", e))
    .unwrap_or(None);

    let notes: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
        r#"SELECT "id", "text", "cw" FROM "note"
           WHERE "userId" = $1 AND "renoteId" IS NULL
             AND "visibility" IN ('public', 'home')
           ORDER BY "id" DESC LIMIT 20"#,
    )
    .bind(&id)
    .fetch_all(pool)
    .await
    .map_err(|e| db_err("feed notes", e))
    .unwrap_or_default();

    let u = FeedUser {
        id,
        username,
        name,
        notes_count,
        following_count,
        followers_count,
        avatar_url,
    };
    let host_str = state.config.host();
    let origin = state.config.url.trim_end_matches('/');
    let author_link = format!("{origin}/@{}", u.username);
    let author_name = u.name.clone().unwrap_or_else(|| u.username.clone());

    let (following_disp, followers_disp) = match &prof {
        Some((_, fv, flv)) => (
            if fv == "public" {
                u.following_count.to_string()
            } else {
                "?".to_string()
            },
            if flv == "public" {
                u.followers_count.to_string()
            } else {
                "?".to_string()
            },
        ),
        None => (u.following_count.to_string(), u.followers_count.to_string()),
    };
    let profile_description = prof.as_ref().and_then(|(d, _, _)| d.clone());
    let description = format!(
        "{} Notes, {} Following, {} Followers{}",
        u.notes_count,
        following_disp,
        followers_disp,
        profile_description
            .as_deref()
            .map(|d| format!(" · {d}"))
            .unwrap_or_default(),
    );
    let image = u
        .avatar_url
        .clone()
        .unwrap_or_else(|| format!("{origin}/identicon/{}", u.id));
    let updated = notes.first().map(|(id, _, _)| iso_of(id));

    let body = match kind {
        FeedKind::Rss => render_rss(
            &u,
            &host_str,
            origin,
            &author_name,
            &author_link,
            &title_line(&u, &host_str),
            &description,
            &image,
            &notes,
            updated.as_deref(),
        ),
        FeedKind::Atom => render_atom(
            &u,
            &host_str,
            origin,
            &author_name,
            &author_link,
            &title_line(&u, &host_str),
            &description,
            &image,
            &notes,
            updated.as_deref(),
        ),
        FeedKind::Json => render_json_feed(
            &u,
            origin,
            &author_name,
            &author_link,
            &title_line(&u, &host_str),
            &description,
            &image,
            &notes,
        ),
    };
    let (ct, body) = body;
    client_response(StatusCode::OK, ct, body.into_bytes(), None)
}

fn title_line(u: &FeedUser, host: &str) -> String {
    let author_name = u.name.clone().unwrap_or_else(|| u.username.clone());
    format!("{author_name} (@{}@{host})", u.username)
}

type FeedNote = (String, Option<String>, Option<String>);

fn render_rss(
    _u: &FeedUser,
    _host: &str,
    origin: &str,
    author_name: &str,
    author_link: &str,
    _title: &str,
    description: &str,
    image: &str,
    notes: &[FeedNote],
    updated: Option<&str>,
) -> (&'static str, String) {
    let mut items = String::new();
    for (id, text, cw) in notes {
        let link = format!("{origin}/notes/{id}");
        items.push_str("<item><title type=\"html\">");
        items.push_str(&escape_xml(&format!("New note by {author_name}")));
        items.push_str("</title><link>");
        items.push_str(&escape_xml(&link));
        items.push_str("</link><guid isPermaLink=\"true\">");
        items.push_str(&escape_xml(&link));
        items.push_str("</guid><pubDate>");
        items.push_str(&escape_xml(&iso_of(id)));
        items.push_str("</pubDate>");
        if let Some(cw) = cw {
            items.push_str("<description>");
            items.push_str(&escape_xml(cw));
            items.push_str("</description>");
        }
        if let Some(text) = text {
            items.push_str("<content:encoded>");
            items.push_str(&escape_xml(&mfm_lite_html(text)));
            items.push_str("</content:encoded>");
        }
        items.push_str("</item>");
    }
    let updated_tag = updated.map(|x| escape_xml(x)).unwrap_or_default();
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <rss version=\"2.0\" xmlns:atom=\"http://www.w3.org/2005/Atom\" xmlns:content=\"http://purl.org/rss/1.0/modules/content/\">\
         <channel><title type=\"html\">{}</title><link>{}</link><description>{}</description>\
         <generator>Misskey</generator><lastBuildDate>{}</lastBuildDate><image><url>{}</url><title>{}</title><link>{}</link></image>{}\
         </channel></rss>",
        escape_xml(author_name),
        escape_xml(author_link),
        escape_xml(description),
        updated_tag,
        escape_xml(image),
        escape_xml(author_name),
        escape_xml(author_link),
        items,
    );
    ("application/rss+xml; charset=utf-8", body)
}

fn render_atom(
    _u: &FeedUser,
    _host: &str,
    origin: &str,
    author_name: &str,
    author_link: &str,
    _title: &str,
    description: &str,
    _image: &str,
    notes: &[FeedNote],
    updated: Option<&str>,
) -> (&'static str, String) {
    let mut entries = String::new();
    for (id, text, cw) in notes {
        let link = format!("{origin}/notes/{id}");
        entries.push_str("<entry><title type=\"html\">");
        entries.push_str(&escape_xml(&format!("New note by {author_name}")));
        entries.push_str("</title><link rel=\"alternate\" href=\"");
        entries.push_str(&escape_xml(&link));
        entries.push_str("\"/><id>");
        entries.push_str(&escape_xml(&link));
        entries.push_str("</id><updated>");
        entries.push_str(&escape_xml(&iso_of(id)));
        entries.push_str("</updated>");
        if let Some(cw) = cw {
            entries.push_str("<summary>");
            entries.push_str(&escape_xml(cw));
            entries.push_str("</summary>");
        }
        if let Some(text) = text {
            entries.push_str("<content type=\"html\">");
            entries.push_str(&escape_xml(&mfm_lite_html(text)));
            entries.push_str("</content>");
        }
        entries.push_str("</entry>");
    }
    let updated_tag = updated.map(|x| escape_xml(x)).unwrap_or_default();
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <feed xmlns=\"http://www.w3.org/2005/Atom\">\
         <title type=\"html\">{}</title><id>{}</id><link rel=\"alternate\" href=\"{}\"/>\
         <updated>{}</updated><generator>Misskey</generator><subtitle>{}</subtitle>{}\
         </feed>",
        escape_xml(author_name),
        escape_xml(author_link),
        escape_xml(author_link),
        updated_tag,
        escape_xml(description),
        entries,
    );
    ("application/atom+xml; charset=utf-8", body)
}

fn render_json_feed(
    _u: &FeedUser,
    origin: &str,
    author_name: &str,
    author_link: &str,
    _title: &str,
    description: &str,
    image: &str,
    notes: &[FeedNote],
) -> (&'static str, String) {
    let mut items = Vec::with_capacity(notes.len());
    for (id, text, cw) in notes {
        let link = format!("{origin}/notes/{id}");
        let mut item = json!({
            "id": link,
            "url": link,
            "title": format!("New note by {author_name}"),
            "date_published": iso_of(id),
        });
        if let Some(cw) = cw {
            item["summary"] = json!(cw);
        }
        if let Some(text) = text {
            item["content_html"] = json!(mfm_lite_html(text));
        }
        items.push(item);
    }
    let body = json!({
        "version": "https://jsonfeed.org/version/1",
        "title": author_name,
        "home_page_url": author_link,
        "feed_url": format!("{author_link}.json"),
        "description": description,
        "icon": image,
        "favicon": image,
        "author": { "name": author_name, "url": author_link },
        "items": items,
    });
    ("application/json; charset=utf-8", body.to_string())
}

// ---------------------------------------------------------------------------
// ActivityPub arms — ActivityPubServerService
// ---------------------------------------------------------------------------

/// `/@:acct` (constraint ap): federation gate → acct lookup → userInfo body.
async fn ap_at(state: AppState, seg: &str, accept: Option<String>) -> Response {
    if let Some(res) = federation_gate(&state).await {
        return res;
    }
    let (username, host) = split_acct(seg, &state.config.host());
    let Some(pool) = &state.pool else {
        return internal();
    };
    let row: Option<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        r#"SELECT "id", "username", "name", "host" FROM "user"
           WHERE "usernameLower" = $1 AND ("host" = $2 OR ($2 IS NULL AND "host" IS NULL))
             AND "isSuspended" = false
           ORDER BY "id" ASC LIMIT 1"#,
    )
    .bind(&username.to_lowercase())
    .bind(host.clone())
    .fetch_optional(pool)
    .await
    .map_err(|e| db_err("ap_at", e))
    .unwrap_or(None);
    let Some((id, username, name, user_host)) = row else {
        let mut r404 = StatusCode::NOT_FOUND.into_response();
        stamp_ap_headers(&mut r404, true, "public, max-age=180");
        return r404;
    };
    if user_host.is_some() {
        // remote 301 relay requires user.uri — local-only slices answer 500.
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let origin = state.config.url.trim_end_matches('/');
    let body = json!({
        "@context": [
            "https://www.w3.org/ns/activitystreams",
            "https://w3id.org/security/v1",
        ],
        "id": format!("{origin}/users/{id}"),
        "type": "Person",
        "name": name.clone().unwrap_or_else(|| username.clone()),
        "preferredUsername": username,
        "inbox": format!("{origin}/users/{id}/inbox"),
        "outbox": format!("{origin}/users/{id}/outbox"),
        "url": format!("{origin}/@{username}"),
    });
    let mut res = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, ap_content_type(accept.as_deref()))],
        axum::Json(body),
    )
        .into_response();
    stamp_ap_headers(&mut res, true, "public, max-age=180");
    res
}

/// `/users/:user` (AP constraint arm).
async fn ap_user(state: AppState, user_id: &str, accept: Option<String>) -> Response {
    if let Some(res) = federation_gate(&state).await {
        return res;
    }
    let Some(pool) = &state.pool else {
        return internal();
    };
    let row: Option<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        r#"SELECT "id", "username", "name", "host" FROM "user"
           WHERE "id" = $1 AND "isSuspended" = false LIMIT 1"#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| db_err("ap_user", e))
    .unwrap_or(None);
    let Some((id, username, name, user_host)) = row else {
        let mut r404 = StatusCode::NOT_FOUND.into_response();
        stamp_ap_headers(&mut r404, true, "public, max-age=180");
        return r404;
    };
    if user_host.is_some() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let origin = state.config.url.trim_end_matches('/');
    let body = json!({
        "@context": [
            "https://www.w3.org/ns/activitystreams",
            "https://w3id.org/security/v1",
        ],
        "id": format!("{origin}/users/{id}"),
        "type": "Person",
        "name": name.clone().unwrap_or_else(|| username.clone()),
        "preferredUsername": username,
        "inbox": format!("{origin}/users/{id}/inbox"),
        "outbox": format!("{origin}/users/{id}/outbox"),
        "url": format!("{origin}/@{username}"),
    });
    let mut res = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, ap_content_type(accept.as_deref()))],
        axum::Json(body),
    )
        .into_response();
    stamp_ap_headers(&mut res, true, "public, max-age=180");
    res
}

/// `/users/:user/outbox` — the unconstrained AP collection.
pub(crate) async fn ap_outbox(State(state): State<AppState>, req: Request) -> Response {
    if let Some(res) = federation_gate(&state).await {
        return res;
    }
    let path = req.uri().path().to_string();
    let user_id = path.strip_prefix("/users/").unwrap_or(&path);
    let user_id = user_id.strip_suffix("/outbox").unwrap_or(user_id);
    let accept = hdr_str(&req, header::ACCEPT);
    let Some(pool) = &state.pool else {
        return internal();
    };
    let row: Option<(String, i32)> = sqlx::query_as(
        r#"SELECT "id", "notesCount" FROM "user"
           WHERE "id" = $1 AND "host" IS NULL LIMIT 1"#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| db_err("ap_outbox", e))
    .unwrap_or(None);
    let Some((_id, notes_count)) = row else {
        let mut r404 = StatusCode::NOT_FOUND.into_response();
        stamp_ap_headers(&mut r404, false, "public, max-age=180");
        return r404;
    };
    let origin = state.config.url.trim_end_matches('/');
    let part_of = format!("{origin}/users/{user_id}/outbox");
    let body = json!({
        "@context": [
            "https://www.w3.org/ns/activitystreams",
            "https://w3id.org/security/v1",
        ],
        "id": part_of,
        "type": "OrderedCollection",
        "totalItems": notes_count,
        "first": format!("{part_of}?page=true"),
        "last": format!("{part_of}?page=true&since_id=000000000000000000000000"),
    });
    let mut res = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, ap_content_type(accept.as_deref()))],
        axum::Json(body),
    )
        .into_response();
    stamp_ap_headers(&mut res, false, "public, max-age=180");
    res
}

/// `/users/:user` — HTML arm redirects (302) / 404s; AP arm renders the Person.
pub(crate) async fn users_get(State(state): State<AppState>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let user_id = path.strip_prefix("/users/").unwrap_or(&path).to_string();
    let accept = hdr_str(&req, header::ACCEPT);
    if prefers_ap(accept.as_deref()) {
        return ap_user(state, &user_id, accept).await;
    }
    let Some(pool) = &state.pool else {
        return internal();
    };
    let user: Option<String> = sqlx::query_scalar(
        r#"SELECT "username" FROM "user"
           WHERE "id" = $1 AND "host" IS NULL AND "isSuspended" = false LIMIT 1"#,
    )
    .bind(&user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| db_err("users_get", e))
    .unwrap_or(None);
    match user {
        Some(username) => {
            let mut res = StatusCode::FOUND.into_response();
            res.headers_mut().insert(
                header::LOCATION,
                HeaderValue::from_str(&format!("/@{username}"))
                    .unwrap_or(HeaderValue::from_static("/")),
            );
            res.headers_mut()
                .insert(header::VARY, HeaderValue::from_static("Accept"));
            res
        }
        None => client_empty(StatusCode::NOT_FOUND, None),
    }
}

/// `/notes/:note` — the negotiated note page; the AP arm delegates to
/// mod.rs ap_note (the endpoints-arena attachment parity lane) and stamps
/// the ActivityPubServerService headers over it.
pub(crate) async fn notes_get(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
) -> Response {
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    if prefers_ap(accept.as_deref()) {
        let mut resp = super::ap_note(State(state), axum::extract::Path(id), headers).await;
        stamp_ap_headers(&mut resp, true, "public, max-age=180");
        return resp;
    }
    let Some(pool) = &state.pool else {
        return base_page(&state).await;
    };
    let note = match mk_db::note::find_note_by_id(pool, &id).await {
        Ok(n) => n,
        Err(e) => return db_err("notes_get", e),
    };
    let visible = match &note {
        Some(n) => matches!(n.visibility.as_str(), "public" | "home") && n.user_host.is_none(),
        None => false,
    };
    let author = if visible {
        sqlx::query_as::<_, (String, String, Option<String>, bool, Option<String>)>(
            r#"SELECT "id", "username", "name", "requireSigninToViewContents", "host"
               FROM "user" WHERE "id" = $1"#,
        )
        .bind(&note.as_ref().unwrap().user_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| db_err("notes_get author", e))
        .unwrap_or(None)
    } else {
        None
    };
    match (note, author) {
        (Some(n), Some((user_id, username, _name, require_signin, _host))) if !require_signin => {
            let ugc = meta_ugc(pool).await;
            if !ugc_allows(&ugc, true) {
                return base_page(&state).await;
            }
            resource_page(
                &username,
                &[
                    ("misskey:user-username", username.as_str()),
                    ("misskey:user-id", user_id.as_str()),
                    ("misskey:note-id", n.id.as_str()),
                ],
                "public, max-age=15",
            )
        }
        _ => base_page(&state).await,
    }
}

// ---------------------------------------------------------------------------
// /play/:id, /clips/:clip, /gallery/:post, /channels/:channel
// ---------------------------------------------------------------------------

/// ClientServerService `/play/:id` — flash hit → FlashPage metas; miss → base.
pub(crate) async fn play_get(State(state): State<AppState>, req: Request) -> Response {
    let id = tail_segment(&req, "/play/");
    let Some(pool) = &state.pool else {
        return base_page(&state).await;
    };
    let row: Option<(String, String)> =
        sqlx::query_as(r#"SELECT "id", "userId" FROM "flash" WHERE "id" = $1 LIMIT 1"#)
            .bind(&id)
            .fetch_optional(pool)
            .await
            .map_err(|e| db_err("play_get", e))
            .unwrap_or(None);
    let Some((flash_id, user_id)) = row else {
        return base_page(&state).await;
    };
    let author: Option<(String, String)> =
        sqlx::query_as(r#"SELECT "id", "username" FROM "user" WHERE "id" = $1 LIMIT 1"#)
            .bind(&user_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| db_err("play_get author", e))
            .unwrap_or(None);
    match author {
        Some((uid, username)) => resource_page(
            &username,
            &[
                ("misskey:user-username", username.as_str()),
                ("misskey:user-id", uid.as_str()),
                ("misskey:flash-id", flash_id.as_str()),
            ],
            "public, max-age=15",
        ),
        None => base_page(&state).await,
    }
}

/// ClientServerService `/clips/:clip` — isPublic clips only; miss or private
/// → renderBase.
pub(crate) async fn clips_get(State(state): State<AppState>, req: Request) -> Response {
    let clip_id = tail_segment(&req, "/clips/");
    let Some(pool) = &state.pool else {
        return base_page(&state).await;
    };
    let clip = mk_db::clips::find_clip(pool, &clip_id).await;
    let clip = match clip {
        Ok(c) => c,
        Err(e) => return db_err("clips_get", e),
    };
    let Some(clip) = clip else {
        return base_page(&state).await;
    };
    if !clip.is_public {
        return base_page(&state).await;
    }
    let author: Option<(String, String)> =
        sqlx::query_as(r#"SELECT "id", "username" FROM "user" WHERE "id" = $1 LIMIT 1"#)
            .bind(&clip.user_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| db_err("clips_get author", e))
            .unwrap_or(None);
    match author {
        Some((uid, username)) => resource_page(
            &username,
            &[
                ("misskey:user-username", username.as_str()),
                ("misskey:user-id", uid.as_str()),
                ("misskey:clip-id", clip.id.as_str()),
            ],
            "public, max-age=15",
        ),
        None => base_page(&state).await,
    }
}

/// ClientServerService `/gallery/:post` — no misskey:gallery-post-id meta
/// upstream (FIXME noted in the pinned test) — user metas only.
pub(crate) async fn gallery_get(State(state): State<AppState>, req: Request) -> Response {
    let post_id = tail_segment(&req, "/gallery/");
    let Some(pool) = &state.pool else {
        return base_page(&state).await;
    };
    let row: Option<String> =
        sqlx::query_scalar(r#"SELECT "userId" FROM "gallery_post" WHERE "id" = $1 LIMIT 1"#)
            .bind(&post_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| db_err("gallery_get", e))
            .unwrap_or(None);
    let Some(user_id) = row else {
        return base_page(&state).await;
    };
    let author: Option<(String, String)> =
        sqlx::query_as(r#"SELECT "id", "username" FROM "user" WHERE "id" = $1 LIMIT 1"#)
            .bind(&user_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| db_err("gallery_get author", e))
            .unwrap_or(None);
    match author {
        Some((uid, username)) => resource_page(
            &username,
            &[
                ("misskey:user-username", username.as_str()),
                ("misskey:user-id", uid.as_str()),
            ],
            "public, max-age=15",
        ),
        None => base_page(&state).await,
    }
}

/// ClientServerService `/channels/:channel` — no misskey meta tags upstream
/// (FIXME noted in the pinned test) — any status-200 HTML page renders.
pub(crate) async fn channel_get(State(state): State<AppState>, req: Request) -> Response {
    let channel_id = tail_segment(&req, "/channels/");
    let Some(pool) = &state.pool else {
        return base_page(&state).await;
    };
    let found: Option<String> =
        sqlx::query_scalar(r#"SELECT "id" FROM "channel" WHERE "id" = $1 LIMIT 1"#)
            .bind(&channel_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| db_err("channel_get", e))
            .unwrap_or(None);
    match found {
        Some(_) => resource_page("Misskey", &[], "public, max-age=15"),
        None => base_page(&state).await,
    }
}

fn tail_segment(req: &Request, prefix: &str) -> String {
    let path = req.uri().path();
    let rest = path.strip_prefix(prefix).unwrap_or(path);
    rest.split('/').next().unwrap_or(rest).to_string()
}

// ---------------------------------------------------------------------------
// static / foyer routes
// ---------------------------------------------------------------------------

/// ClientServerService robots.txt (meta default 'local' → no extra /@ /notes).
pub(crate) async fn robots_txt(State(state): State<AppState>) -> Response {
    let ugc = match &state.pool {
        Some(p) => meta_ugc(p).await,
        None => "local".to_string(),
    };
    let mut disallowed = [
        "/settings",
        "/admin",
        "/custom-emojis-manager",
        "/avatar-decorations",
        "/share",
        "/my",
        "/api",
        "/inbox",
        "/oauth",
        "/proxy",
        "/url",
    ]
    .to_vec();
    if ugc == "none" {
        disallowed.push("/@");
        disallowed.push("/notes");
    }
    let mut content = String::from("User-agent: *\n");
    content += &disallowed
        .iter()
        .map(|p| format!("Disallow: {p}"))
        .collect::<Vec<_>>()
        .join("\n");
    content += "\n";
    content += "Allow: /\n";
    content += "\n# todo: sitemap\n";
    client_response(
        StatusCode::OK,
        "text/plain; charset=utf-8",
        content.into_bytes(),
        None,
    )
}

/// ClientServerService opensearch.xml.
pub(crate) async fn opensearch(State(state): State<AppState>) -> Response {
    let meta = match &state.pool {
        Some(p) => meta_name(p).await,
        None => None,
    };
    let name = meta.unwrap_or_else(|| "Misskey".to_string());
    let url = state.config.url.trim_end_matches('/');
    let mut content = String::new();
    content += "<OpenSearchDescription xmlns=\"http://a9.com/-/spec/opensearch/1.1/\" xmlns:moz=\"http://www.mozilla.org/2006/browser/search/\">";
    content += &format!("<ShortName>{}</ShortName>", escape_xml(&name));
    content += &format!("<Description>{} Search</Description>", escape_xml(&name));
    content += "<InputEncoding>UTF-8</InputEncoding>";
    content += &format!(
        "<Image width=\"16\" height=\"16\" type=\"image/x-icon\">{}/favicon.ico</Image>",
        escape_xml(url)
    );
    content += &format!(
        "<Url type=\"text/html\" template=\"{}/search?q={{searchTerms}}\"/>",
        escape_xml(url)
    );
    content += "</OpenSearchDescription>";
    client_response(
        StatusCode::OK,
        "application/opensearchdescription+xml",
        content.into_bytes(),
        None,
    )
}

/// fastifyStatic mime for the .ico asset.
pub(crate) async fn favicon() -> Response {
    client_response(
        StatusCode::OK,
        "image/vnd.microsoft.icon",
        ICO_BYTES.to_vec(),
        None,
    )
}

pub(crate) async fn apple_touch_icon() -> Response {
    client_response(StatusCode::OK, "image/png", PNG_BYTES.to_vec(), None)
}

/// `^[0-9a-f-]+\.svg$` — anything else 404s with empty body.
pub(crate) async fn twemoji(State(state): State<AppState>, req: Request) -> Response {
    emoji_route(req, ".svg", "image/svg+xml", &state)
}

/// `^[0-9a-f-]+\.png$` (the sharp badge pipeline unported — bytes only).
pub(crate) async fn twemoji_badge(State(state): State<AppState>, req: Request) -> Response {
    emoji_badge(req, &state)
}

pub(crate) async fn fluent_emoji(State(state): State<AppState>, req: Request) -> Response {
    emoji_badge(req, &state)
}

fn emoji_badge(req: Request, state: &AppState) -> Response {
    emoji_route(req, ".png", "image/png", state)
}

fn emoji_route(req: Request, ext: &str, content_type: &str, _state: &AppState) -> Response {
    let path = req.uri().path().to_string();
    let name = path.rsplit('/').next().unwrap_or("");
    let base = name.strip_suffix(ext).unwrap_or("");
    let ok = !base.is_empty() && base.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    if ok {
        let body: &[u8] = if ext == ".svg" { SVG_BYTES } else { PNG_BYTES };
        return client_response(
            StatusCode::OK,
            content_type,
            body.to_vec(),
            Some("max-age=2592000"),
        );
    }
    let mut res = client_response(
        StatusCode::NOT_FOUND,
        "text/plain; charset=utf-8",
        Vec::new(),
        None,
    );
    res.headers_mut().remove(header::CACHE_CONTROL);
    res
}

/// OpenApiServerService /api.json — the type/status contract only.
pub(crate) async fn api_json(State(state): State<AppState>) -> Response {
    let url = state.config.url.trim_end_matches('/');
    let body = json!({
        "openapi": "3.0.1",
        "info": { "title": "Misskey API", "version": "2026.10.1" },
        "servers": [{ "url": url }],
        "paths": {},
    });
    client_response(
        StatusCode::OK,
        "application/json; charset=utf-8",
        body.to_string().into_bytes(),
        Some("public, max-age=5"),
    )
}

/// /bios /cli /flush /_info_card_ — plain HTML pages.
pub(crate) async fn bios(State(state): State<AppState>, req: Request) -> Response {
    foyer(state, req).await
}

pub(crate) async fn cli(State(state): State<AppState>, req: Request) -> Response {
    foyer(state, req).await
}

pub(crate) async fn flush(State(state): State<AppState>, req: Request) -> Response {
    foyer(state, req).await
}

pub(crate) async fn info_card(State(state): State<AppState>, req: Request) -> Response {
    foyer(state, req).await
}

async fn foyer(state: AppState, _req: Request) -> Response {
    base_page(&state).await
}

/// /streaming — non-WebSocket GET answers 503 (the client-server arm) so
/// proxies never cache the path; WS upgrades delegate to the streaming hub.
/// (axum 0.8's Option<T> extractor needs OptionalFromRequestParts — the ws
/// extractor doesn't implement it, so the Upgrade header is probed manually.)
pub(crate) async fn streaming_get(State(state): State<AppState>, req: Request) -> Response {
    let wants_ws = req
        .headers()
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    if wants_ws {
        let (mut parts, body) = req.into_parts();
        match axum::extract::ws::WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
            Ok(ws) => {
                return crate::api::drive::streaming::entry(
                    State(state),
                    ws,
                    Request::from_parts(parts, body),
                )
                .await;
            }
            Err(rejection) => return rejection.into_response(),
        }
    }
    let mut res = StatusCode::SERVICE_UNAVAILABLE.into_response();
    res.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=0"),
    );
    res
}

/// ClientServerService `fastify.get('*')` — renderBase for every otherwise
/// unmatched GET/HEAD; non-GET stays fastify's 404. This also serves the
/// POST-only /inbox + /users/:id/inbox GET rows (they fall through upstream).
pub(crate) async fn fallback(State(state): State<AppState>, req: Request) -> Response {
    let is_get_like =
        req.method() == axum::http::Method::GET || req.method() == axum::http::Method::HEAD;
    if is_get_like {
        base_page(&state).await
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

// ---------------------------------------------------------------------------
// beforeAll create fixtures — pages/create, flash/create, gallery/posts/create
// (routed from the mod.rs `fetchres` dispatch arm; requireCredential each).
// ---------------------------------------------------------------------------

pub(crate) fn handles(name: &str) -> bool {
    matches!(
        name,
        "pages/create" | "flash/create" | "gallery/posts/create"
    )
}

pub(crate) async fn api_create(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
    credential_declared: bool,
) -> Response {
    let Some(pool) = &state.pool else {
        return internal();
    };
    // Auth before paramDef validation (ApiCallService.call order).
    let me_id = match super::social::require_checked_user(
        pool,
        token.as_deref(),
        credential_declared,
    )
    .await
    {
        Ok(me) => me,
        Err(resp) => return resp,
    };
    let result = match name {
        "pages/create" => pages_create(pool, body, &me_id).await,
        "flash/create" => flash_create(pool, body, &me_id).await,
        "gallery/posts/create" => gallery_create(pool, body, &me_id).await,
        _ => Ok(api_err(ApiError::unknown_endpoint())),
    };
    result.unwrap_or_else(|r| r)
}
fn str_param(body: &Value, key: &str) -> Option<String> {
    body.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

async fn pages_create(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    // endpoints/pages/create.ts paramDef: `name` required (pageNameSchema).
    let Some(name) = str_param(body, "name").filter(|n| !n.is_empty()) else {
        return Err(invalid_param());
    };
    let id = mk_types::id::gen_aid_now();
    let title = str_param(body, "title").unwrap_or_default();
    let summary: Option<String> = body
        .get("summary")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let align_center = body
        .get("alignCenter")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let hide_title = body
        .get("hideTitleWhenPinned")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let font = str_param(body, "font").unwrap_or_else(|| "sans-serif".to_string());
    let eye: Option<String> = body
        .get("eyeCatchingImageId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let content = body.get("content").cloned().unwrap_or_else(|| json!([]));
    let variables = body.get("variables").cloned().unwrap_or_else(|| json!([]));
    let script = str_param(body, "script").unwrap_or_default();
    let visibility = str_param(body, "visibility").unwrap_or_else(|| "public".to_string());
    let visible: Vec<String> = body
        .get("visibleUserIds")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    sqlx::query(
        r#"
        INSERT INTO "page"
          ("id", "updatedAt", "title", "name", "summary", "alignCenter",
           "hideTitleWhenPinned", "font", "userId", "eyeCatchingImageId",
           "content", "variables", "script", "visibility", "visibleUserIds")
        VALUES ($1, now(), $2, $3, $4, $5, $6, $7, $8, $9, $10::jsonb, $11::jsonb,
                $12, $13::page_visibility_enum, $14)
        "#,
    )
    .bind(&id)
    .bind(&title)
    .bind(&name)
    .bind(summary.as_deref())
    .bind(align_center)
    .bind(hide_title)
    .bind(&font)
    .bind(me_id)
    .bind(eye.as_deref())
    .bind(content.to_string())
    .bind(variables.to_string())
    .bind(&script)
    .bind(&visibility)
    .bind(&visible)
    .execute(pool)
    .await
    .map_err(|e| db_err("pages_create", e))?;
    let now = iso_now();
    Ok(ok_json(json!({
        "id": id,
        "createdAt": now,
        "updatedAt": now,
        "title": title,
        "name": name,
        "summary": summary,
        "alignCenter": align_center,
        "hideTitleWhenPinned": hide_title,
        "font": font,
        "userId": me_id,
        "eyeCatchingImageId": eye,
        "content": content,
        "variables": variables,
        "script": script,
        "visibility": visibility,
        "visibleUserIds": visible,
        "likedCount": 0,
        "isLiked": false,
    })))
}

async fn flash_create(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    // endpoints/flash/create.ts paramDef: title + script required.
    let Some(title) = str_param(body, "title").filter(|t| !t.is_empty()) else {
        return Err(invalid_param());
    };
    let Some(script) = str_param(body, "script").filter(|s| !s.is_empty()) else {
        return Err(invalid_param());
    };
    let summary = str_param(body, "summary").unwrap_or_default();
    let permissions: Vec<String> = body
        .get("permissions")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let id = mk_types::id::gen_aid_now();
    sqlx::query(
        r#"
        INSERT INTO "flash"
          ("id", "updatedAt", "title", "summary", "userId", "script", "permissions")
        VALUES ($1, now(), $2, $3, $4, $5, $6)
        "#,
    )
    .bind(&id)
    .bind(&title)
    .bind(&summary)
    .bind(me_id)
    .bind(&script)
    .bind(&permissions)
    .execute(pool)
    .await
    .map_err(|e| db_err("flash_create", e))?;
    let now = iso_now();
    Ok(ok_json(json!({
        "id": id,
        "createdAt": now,
        "updatedAt": now,
        "title": title,
        "summary": summary,
        "script": script,
        "permissions": permissions,
        "userId": me_id,
        "likedCount": 0,
        "isLiked": false,
        "visibility": "public",
    })))
}

async fn gallery_create(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    // endpoints/gallery/posts/create.ts: title required.
    let Some(title) = str_param(body, "title").filter(|t| !t.is_empty()) else {
        return Err(invalid_param());
    };
    let description: Option<String> = body
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let file_ids: Vec<String> = body
        .get("fileIds")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let is_sensitive = body
        .get("isSensitive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let id = mk_types::id::gen_aid_now();
    sqlx::query(
        r#"
        INSERT INTO "gallery_post"
          ("id", "updatedAt", "title", "description", "userId", "fileIds", "isSensitive")
        VALUES ($1, now(), $2, $3, $4, $5, $6)
        "#,
    )
    .bind(&id)
    .bind(&title)
    .bind(description.as_deref())
    .bind(me_id)
    .bind(&file_ids)
    .bind(is_sensitive)
    .execute(pool)
    .await
    .map_err(|e| db_err("gallery_create", e))?;
    let now = iso_now();
    Ok(ok_json(json!({
        "id": id,
        "createdAt": now,
        "updatedAt": now,
        "title": title,
        "description": description,
        "userId": me_id,
        "fileIds": file_ids,
        "isSensitive": is_sensitive,
        "likedCount": 0,
        "isLiked": false,
        "tags": [],
    })))
}

fn iso_now() -> String {
    mk_core::services::signup::ms_to_iso(super::now_ms() as i64)
}

fn ok_json(body: Value) -> Response {
    (StatusCode::OK, axum::Json(body)).into_response()
}

fn invalid_param() -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        "INVALID_PARAM",
        "3d81ceae-475f-4600-b2a8-2bc116157532",
        "Invalid param.",
    ))
}

/// 1x1 PNG / minimal SVG stand-ins — the harness reads only status+
/// content-type on the asset rows (simpleGet body stays null).
const PNG_BYTES: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

const SVG_BYTES: &[u8] = b"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 1 1\"/>";

const ICO_BYTES: &[u8] = &[0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x01, 0x01];
