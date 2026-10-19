//! `/api/{name}` dispatch and the M0 endpoint set.
//!
//! The full 438-endpoint registry (CI codegen from upstream metas) replaces
//! the hand-written table here from M2 on; the dispatch chain already mirrors
//! `ApiCallService.handleRequest` ordering so endpoints slot in.

use axum::Json;
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};
use crate::ratelimit::Limit;

// social-agent: follow/mute/block/renote social actions (M2 truthful slice)
mod social;
// freq-agent: users/get-frequently-replied-users + pinned-users (users.ts
// census slice) — db paths in mk-db/src/freq.rs; the users/notes guest path
// and the admin/update-meta pinnedUsers slice ride along as requisites.
mod freq;
// i/update long-tail wave: drive/files/create (upload) + i/pin + i/unpin +
// users/update-memo endpoints closing the users.ts を書き換えることができる census
// pub(crate): main.rs routes `/streaming` into drive::streaming (drive-agent2).
pub(crate) mod drive;
// gate-roles: minimal admin/roles/create + admin/roles/assign seeding the
// users.ts role fixtures (Publicなロール/バッヂロール census)
mod roles_lite;
// delete-account: i/delete-account + admin/delete-account (削除済 census —
// soft `isDeleted` flag + the me-view pack field); db path in
// mk-db/src/delete_account.rs
mod delete_account;
// channels-agent: channels/* domain (channels/create ../favorite slice of
// upstream endpoints/channels/*) — db paths in mk-db/src/channel.rs
mod channel;
// clips-agent: the clips family (clips/{create,list,show,update,delete,
// favorite,unfavorite,my-favorites,notes,add-note,remove-note} +
// users/clips) — db paths in mk-db/src/clips.rs (clip/clip_note/
// clip_favorite tables ship in the 0001 baseline).
mod clips;
// announcements-agent: the announcements family (announcements +
// announcements/show + i/read-announcement + admin/announcements/*) — db
// paths in mk-db/src/announcement.rs (tables ship in the 0001 baseline).
// No `i/read-all-unread-announcements` exists at the pinned upstream.
mod announcement;
// note-agent: notes.ts acceptance slice — notes (global list),
// notes/polls/vote, notes/translate + the admin/update-meta
// sensitiveWords/prohibitedWords patch (db paths in mk-db meta.rs).
mod note_extras;
// note-agent: drive/files/update (drive/files/update.ts semantics; db part
// in mk-db/src/drive_note.rs)
mod drive_note;
// ul-tl-agent: users/lists/* + notes/user-list-timeline (upstream
// users/lists/{create,push,update-membership}.ts + notes/user-list-timeline.ts
// getFromDb; the channels/mute/create list-TL requisite rides along) — db
// paths in mk-db/src/lists.rs (user_list / user_list_membership ship in the
// 0001 baseline).
mod lists;
// ftt2: the FTT Redis push pipeline (NoteCreateService.pushToTl +
// FanoutTimelineService.push; the timelines.ts `FTT:` probes read
// `list:homeTimeline:*` Redis lists literally).
mod ftt;
// endpoints-arena: the /api/endpoints name catalog — mirrors upstream
// server/api/endpoint-list.ts source order exactly (438 names, admin/*
// included; upstream Endpoints endpoint returns `endpoints.map(x => x.name)`).
mod endpoints_catalog;
// stream-rtm: the /streaming timeline-channel half — `homeTimeline` +
// `localTimeline` fanout of the notesStream event (upstream
// GlobalEventService.publishNotesStream + stream/channels/{home,local}-timeline.ts).
mod streaming;
// well-known arena: /.well-known/* + /nodeinfo/* routes that ride OUTSIDE
// /api/* (upstream WellKnownServerService.ts + NodeinfoServerService.ts +
// OAuth2ProviderService.generateRFC8414). pub(crate): main.rs mounts the
// dispatcher directly on the top-level router.
pub(crate) mod well_known;
// ff-visibility arena: users/followers + users/following (upstream
// endpoints/users/{followers,following}.ts per-visibility gates) + the AP
// /users/:id/{followers,following} collections +
// admin/update-meta federation persistence (api/ff_vis.rs; db paths in
// mk-db/src/ff_visibility.rs).
pub(crate) mod ff_vis;

/// Endpoints that accept GET (upstream `meta.allowGet`) with `cacheSec`.
const GET_ALLOWED: &[(&str, u64)] = &[
    ("server-info", 60),
    ("emojis", 0),
    ("get-online-users-count", 60),
];

const M0_ENDPOINTS: &[&str] = &[
    "ping",
    "meta",
    "endpoints",
    "stats",
    "server-info",
    "get-online-users-count",
    "emojis",
    "drive",
];

pub async fn dispatch(State(state): State<AppState>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let Some(name) = path.strip_prefix("/api/") else {
        return api_err(ApiError::unknown_endpoint());
    };
    let name = name.to_string();
    let method = req.method().clone();

    // Route-like endpoints outside the endpoint registry (mirrors upstream
    // fastify.post handlers inside the /api plugin).
    if name == "signup" {
        if method != Method::POST {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let body = match parse_json_body(req).await {
            Ok(b) => b,
            Err(resp) => return resp,
        };
        return signup_route(&state, &body).await;
    }
    if name == "_test/reset" {
        if method != Method::POST || !state.config.test {
            return api_err(ApiError::unknown_endpoint());
        }
        return reset_route(&state).await;
    }

    // Registry endpoints that need a JSON body parsed before dispatch.
    if name == "users/show"
        || name == "signin"
        // endpoints-arena: SigninApiService.signin also serves the
        // /signin-flow fastify route (ApiServerService.ts L135 — same body).
        || name == "signin-flow"
        || name == "i"
        || name == "i/update"
        || name == "notes/create"
        || name == "notes/show"
        || name == "users/search"
        || name == "users/search-by-username-and-host"
        || name == "hashtags/users"
        || name == "users"
        // -- notifications slice (M2): param line + arm kept at the END of
        // -- the dispatch group so co-agent rebases stay trivial
        // -- (mute lane: upstream keeps the `i/` namespacing on both the
        // -- plain and the grouped list, endpoints/i/notifications[-grouped].ts;
        // -- the grouped pack shapes are TODO(M2+) — the arena legs gate on
        // -- `userId` presence, which both share)
        || name == "notifications"
        || name == "i/notifications"
        || name == "i/notifications-grouped"
        // mute lane: notes/mentions (endpoints/notes/mentions.ts) +
        // notes/thread-muting/{create,delete} (endpoints/notes/thread-muting/)
        || name == "notes/mentions"
        || name == "notes/thread-muting/create"
        || name == "notes/thread-muting/delete"
        // mute lane: the reset the Mute e2e stream legs run before each
        // waitFire (endpoints/notifications/mark-all-as-read.ts, 204)
        || name == "notifications/mark-all-as-read"
        // note-agent: note domain (local-timeline guest-ok)
        || name == "notes/timeline"
        || name == "notes/local-timeline"
        || name == "notes/hybrid-timeline"
        // timelines matrix: following/update flips the following.withReplies
        // push flag the Home/Hybrid reply legs read (social lane may absorb)
        || name == "following/update"
        || name == "notes/reactions"
        || name == "notes/reactions/create"
        || name == "notes/delete"
        // note-agent notes.ts slice (global list + polls/vote + translate)
        || name == "notes"
        || name == "notes/polls/vote"
        || name == "notes/translate"
        // freq-agent: users/get-frequently-replied-users slice + requisites
        // (pinned-users; users/notes and admin/update-meta pinnedUsers note)
        || name == "users/get-frequently-replied-users"
        || name == "pinned-users"
        || name == "users/notes"
        || name == "admin/update-meta"
        // ff-arena: the following/followers list arena (upstream
        // endpoints/users/{followers,following}.ts per-visibility gates)
        || name == "users/followers"
        || name == "users/following"
    {
        if method != Method::POST {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let headers = req.headers().clone();
        let body = match parse_json_body(req).await {
            Ok(b) => b,
            Err(resp) => return resp,
        };
        let token = extract_token(&body, &headers);
        if name == "signin" || name == "signin-flow" {
            let _ = token;
            return signin_route(&state, &body).await;
        }
        if name == "i" {
            return i_route(&state, &body, &headers, token).await;
        }
        if name == "i/update" {
            return i_update(&state, &body, token).await;
        }
        if name == "notes/create" {
            return notes_create(&state, &body, token).await;
        }
        if name == "notes/show" {
            return notes_show(&state, &body, token.as_deref()).await;
        }
        if name == "users/search" {
            return users_search(&state, &body, token).await;
        }
        if name == "users/search-by-username-and-host" {
            return users_search_by_username_and_host(&state, &body, token).await;
        }
        if name == "hashtags/users" {
            return hashtags_users(&state, &body, token).await;
        }
        if name == "users" {
            return users_list(&state, &body, token).await;
        }
        // -- notifications slice (M2) — added lines; keep at END of group --
        // mute lane: the `i/`-prefixed aliases ride the same handler
        // (upstream endpoint files sit at i/notifications[-grouped].ts).
        if name == "notifications" || name == "i/notifications" || name == "i/notifications-grouped"
        {
            return notifications_route(&state, &body, token).await;
        }
        // mute lane: notes/mentions + the thread-muting pair
        if name == "notes/mentions" {
            return notes_mentions(&state, &body, token).await;
        }
        if name == "notes/thread-muting/create" || name == "notes/thread-muting/delete" {
            return notes_thread_muting(&state, name.as_str(), &body, token).await;
        }
        if name == "notifications/mark-all-as-read" {
            return notifications_mark_all_as_read(&state, token).await;
        }
        // -- end notifications slice --

        // note-agent arms
        if name == "notes/local-timeline" {
            return social::notes_local_timeline(&state, &body, token.as_deref()).await;
        }
        if name == "notes/timeline" || name == "notes/hybrid-timeline" {
            return social::timeline_or_hybrid(&state, name.as_str(), &body, token.as_deref())
                .await;
        }
        // timelines matrix arm: withReplies/notify flag flip on a follow edge
        if name == "following/update" {
            return social::following_update(&state, &body, token.as_deref()).await;
        }
        if name == "notes/reactions" {
            return social::notes_reactions(&state, &body).await;
        }
        if name == "notes/reactions/create" || name == "notes/delete" {
            return social::note_credential_dispatch(
                &state,
                name.as_str(),
                &body,
                token.as_deref(),
            )
            .await;
        }
        // freq-agent arms: users/get-frequently-replied-users slice; the
        // admin/update-meta arm shadows the admin/* arm below for that name.
        if name == "users/get-frequently-replied-users" {
            return freq::frequently_replied(&state, &body, token.as_deref()).await;
        }
        if name == "pinned-users" {
            return freq::pinned_users(&state, token.as_deref()).await;
        }
        if name == "users/notes" {
            return freq::user_notes(&state, &body, token.as_deref()).await;
        }
        if name == "notes" {
            return note_extras::notes_list(&state, &body, token.as_deref()).await;
        }
        if name == "notes/polls/vote" || name == "notes/translate" {
            return note_extras::handle(&state, name.as_str(), &body, token).await;
        }
        // ff-arena: persist meta."federation" on admin/update-meta BEFORE the
        // note/freq lane arms — those `return` on the name, so an arm placed
        // after them never ran (census 39212045330: the ff-visibility.ts
        // beforeAll flip to 'all' was dropped and the AP public legs 403'd).
        // Errors return; success falls through so the other lanes' keys keep
        // landing under their own gates.
        if name == "admin/update-meta" && body.get("federation").is_some() {
            if let Err(resp) = ff_vis::persist_federation(&state, &body, token.as_deref()).await {
                return resp;
            }
        }
        // note-agent: the sensitiveWords/prohibitedWords patch of update-meta
        // (note.ts census keeps those lists in the meta row via this arm;
        // pinnedUsers and the rest still flow to the freq arm below).
        if name == "admin/update-meta" && note_extras::takes_update_meta(&body) {
            return note_extras::update_meta(&state, &body, token.as_deref()).await;
        }
        if name == "admin/update-meta" {
            return freq::update_meta(&state, &body, token.as_deref()).await;
        }
        // ff-arena arms: users/followers + users/following lists
        if ff_vis::FF_ENDPOINTS.contains(&name.as_str()) {
            return ff_vis::route(&state, name.as_str(), &body, token.as_deref()).await;
        }
        return users_show(&state, &body, token).await;
    }

    // ==== gate-roles DISPATCH ARM (admin/roles/create + assign) ====
    // Noses in ahead of the admin/* arm (roles_lite owns only these two
    // names; everything else keeps falling through to admin::handle).
    if roles_lite::handles(name.as_str()) {
        if method != Method::POST {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let headers = req.headers().clone();
        let body = match parse_json_body(req).await {
            Ok(b) => b,
            Err(resp) => return resp,
        };
        let token = extract_token(&body, &headers);
        return roles_lite::handle(&state, name.as_str(), &body, token).await;
    }
    // ==== END gate-roles DISPATCH ARM ====

    // ==== delete-account DISPATCH ARM (i/delete-account + admin/delete-account) ====
    // Self-contained block on the roles_lite-arm pattern: admin/delete-account
    // must nose in ahead of the admin/* catch-all (delete_account owns only
    // these two names). Method check, body parsing and token extraction live
    // in the arm; the credential/admin gates + isDeleted short-circuits in
    // api::delete_account.
    if delete_account::handles(name.as_str()) {
        if method != Method::POST {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let headers = req.headers().clone();
        let body = match parse_json_body(req).await {
            Ok(b) => b,
            Err(resp) => return resp,
        };
        let token = extract_token(&body, &headers);
        return delete_account::handle(&state, name.as_str(), &body, token).await;
    }
    // ==== END delete-account DISPATCH ARM ====

    // ==== announcements DISPATCH ARM (announcements slice) ====
    // announcements-agent: the announcements family names route into
    // api::announcement on the channels-arm pattern — method check, body
    // parsing, token extraction and the credential-declared probe live in
    // this arm; the requireCredential/requireModerator gates + endpoint
    // bodies in api::announcement (db paths in mk-db/src/announcement.rs).
    // The admin/announcements/* names shadow the admin/* catch-all below,
    // so this arm sits ahead of it (roles_lite/delete_account convention).
    if announcement::handles(name.as_str()) {
        if method != Method::POST {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let headers = req.headers().clone();
        let body = match parse_json_body(req).await {
            Ok(b) => b,
            Err(resp) => return resp,
        };
        // Upstream handleRequest: a non-string `i` → `reply.code(400)` empty.
        let credential_declared = body.get("i").is_some()
            || headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|h| h.to_str().ok())
                .map(|h| h.starts_with("Bearer "))
                .unwrap_or(false);
        let token = extract_token(&body, &headers);
        if token.is_none() {
            if let Some(v) = body.get("i") {
                if v.as_str().is_none() {
                    return StatusCode::BAD_REQUEST.into_response();
                }
            }
        }
        return announcement::handle(&state, name.as_str(), &body, token, credential_declared)
            .await;
    }
    // ==== END announcements DISPATCH ARM ====

    // ==== ADMIN DISPATCH ARM (M2 admin slice) ====
    // Single arm routing every `admin/*` name into api::admin so the mod.rs
    // surface stays one hunk (rebase-trivial per the slice plan). Method
    // checks, body parsing, token resolution and the credential/role gates
    // all live inside the admin module.
    if name.starts_with("admin/") {
        return admin::handle(&state, &name, req).await;
    }
    // ==== END ADMIN DISPATCH ARM ====

    // ==== i/update TAIL DISPATCH ARM (drive upload + pin + memo) ====
    // Self-contained block so co-agent rebases stay trivial (mirrors the
    // admin-arm pattern). i/pin and i/unpin take a JSON { noteId } body;
    // drive/files/create is the multipart upload route (upstream fastify
    // multipart plugin), users/update-memo a JSON { userId, memo } body.
    if name == "drive/files/create" {
        return drive::files_create(state, req).await;
    }
    // note-agent: drive/files/update minimal slice (comment/name/isSensitive)
    if name == "drive/files/update" {
        if method != Method::POST {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let headers = req.headers().clone();
        let body = match parse_json_body(req).await {
            Ok(b) => b,
            Err(resp) => return resp,
        };
        let token = extract_token(&body, &headers);
        return drive_note::files_update(&state, &body, token.as_deref()).await;
    }
    if name == "i/pin" || name == "i/unpin" || name == "users/update-memo" {
        if method != Method::POST {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let headers = req.headers().clone();
        let body = match parse_json_body(req).await {
            Ok(b) => b,
            Err(resp) => return resp,
        };
        // These routes carry no shared token-extraction quirks: `i` body
        // field only (the e2e client sends `i`; Bearer handled for parity).
        let token = extract_token(&body, &headers);
        return match name.as_str() {
            "i/pin" => i_pin(&state, &body, token).await,
            "i/unpin" => i_unpin(&state, &body, token).await,
            _ => users_update_memo(&state, &body, token).await,
        };
    }
    // ==== END i/update TAIL DISPATCH ARM ====

    // ==== drive-agent2 DISPATCH ARM (drive/files/attached-notes +
    // drive/files/upload-from-url — drive.ts arena) — on the roles-lite arm
    // pattern: method check, body parsing and token extraction live here;
    // the endpoint bodies in api::drive (db paths in mk-db/src/drive.rs,
    // the /streaming hub routed in main.rs).
    if name == "drive/files/attached-notes"
        || name == "drive/files/upload-from-url"
        // endpoints-arena: the drive/folders family (upstream
        // endpoints/drive/folders/{create,update}.ts)
        || name == "drive/folders/create"
        || name == "drive/folders/update"
    {
        if method != Method::POST {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let headers = req.headers().clone();
        let body = match parse_json_body(req).await {
            Ok(b) => b,
            Err(resp) => return resp,
        };
        let token = extract_token(&body, &headers);
        return drive::handle_json(&state, name.as_str(), &body, token).await;
    }
    // ==== END drive-agent2 DISPATCH ARM ====

    // social-agent dispatch block: follow/mute/block social actions
    // (following/*, mute/*, blocking/*) — requireCredential, JSON body,
    // same token extraction as the group above (upstream
    // endpoints/following + mute + blocking semantics).
    if social::SOCIAL_ENDPOINTS.contains(&name.as_str()) {
        if method != Method::POST {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let headers = req.headers().clone();
        let body = match parse_json_body(req).await {
            Ok(b) => b,
            Err(resp) => return resp,
        };
        // Upstream handleRequest: a non-string `i` → `reply.code(400)` empty.
        let credential_declared = body.get("i").is_some()
            || headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|h| h.to_str().ok())
                .map(|h| h.starts_with("Bearer "))
                .unwrap_or(false);
        let token = extract_token(&body, &headers);
        if token.is_none() {
            if let Some(v) = body.get("i") {
                if v.as_str().is_none() {
                    return StatusCode::BAD_REQUEST.into_response();
                }
            }
        }
        return social::route(&state, name.as_str(), &body, token, credential_declared).await;
    }

    // ==== channels DISPATCH ARM (channels/* slice) ====
    // channels-agent: all 13 `channels/*` names route into api::channel on
    // the roles_lite/delete-account arm pattern — method check, body
    // parsing, token extraction and the credential-declared probe live in
    // this arm; the requireCredential gates + endpoint bodies in
    // api::channel (db paths in mk-db/src/channel.rs).
    if channel::handles(name.as_str()) {
        if method != Method::POST {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let headers = req.headers().clone();
        let body = match parse_json_body(req).await {
            Ok(b) => b,
            Err(resp) => return resp,
        };
        // Upstream handleRequest: a non-string `i` → `reply.code(400)` empty.
        let credential_declared = body.get("i").is_some()
            || headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|h| h.to_str().ok())
                .map(|h| h.starts_with("Bearer "))
                .unwrap_or(false);
        let token = extract_token(&body, &headers);
        if token.is_none() {
            if let Some(v) = body.get("i") {
                if v.as_str().is_none() {
                    return StatusCode::BAD_REQUEST.into_response();
                }
            }
        }
        return channel::handle(&state, name.as_str(), &body, token, credential_declared).await;
    }
    // ==== END channels DISPATCH ARM ====

    // ==== ul-tl-agent DISPATCH ARM (users/lists/* + notes/user-list-timeline
    // + channels/mute/create) ====
    // Self-contained block on the channels-arm pattern: method check, body
    // parsing, token extraction and the credential-declared probe live in this
    // arm; all five names are requireCredential upstream, the requireCredential
    // gates + endpoint bodies in api::lists (db paths in mk-db/src/lists.rs).
    // channels/mute/create is not in channel::handles, so this arm shadows
    // nothing existing.
    if lists::handles(name.as_str()) {
        if method != Method::POST {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let headers = req.headers().clone();
        let body = match parse_json_body(req).await {
            Ok(b) => b,
            Err(resp) => return resp,
        };
        // Upstream handleRequest: a non-string `i` → `reply.code(400)` empty.
        let credential_declared = body.get("i").is_some()
            || headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|h| h.to_str().ok())
                .map(|h| h.starts_with("Bearer "))
                .unwrap_or(false);
        let token = extract_token(&body, &headers);
        if token.is_none() {
            if let Some(v) = body.get("i") {
                if v.as_str().is_none() {
                    return StatusCode::BAD_REQUEST.into_response();
                }
            }
        }
        return lists::handle(&state, name.as_str(), &body, token, credential_declared).await;
    }
    // ==== END ul-tl-agent DISPATCH ARM ====

    // ==== clips-agent DISPATCH ARM (clips/* + users/clips) ====
    // Self-contained block on the channels/ul-tl arm pattern: method check,
    // body parsing, token extraction and the credential-declared probe live
    // in this arm; the requireCredential gates (all but clips/show,
    // clips/notes, users/clips) + endpoint bodies in api::clips (db paths in
    // mk-db/src/clips.rs). None of the 12 names collide with an earlier arm
    // ("users/clips" is not in the users/show group's name list), so this
    // arm shadows nothing.
    if clips::handles(name.as_str()) {
        if method != Method::POST {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let headers = req.headers().clone();
        let body = match parse_json_body(req).await {
            Ok(b) => b,
            Err(resp) => return resp,
        };
        // Upstream handleRequest: a non-string `i` → `reply.code(400)` empty.
        let credential_declared = body.get("i").is_some()
            || headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|h| h.to_str().ok())
                .map(|h| h.starts_with("Bearer "))
                .unwrap_or(false);
        let token = extract_token(&body, &headers);
        if token.is_none() {
            if let Some(v) = body.get("i") {
                if v.as_str().is_none() {
                    return StatusCode::BAD_REQUEST.into_response();
                }
            }
        }
        return clips::handle(&state, name.as_str(), &body, token, credential_declared).await;
    }
    // ==== END clips-agent DISPATCH ARM ====

    if !M0_ENDPOINTS.contains(&name.as_str()) {
        return api_err(ApiError::unknown_endpoint());
    }

    let get_cache = GET_ALLOWED
        .iter()
        .find(|(n, _)| *n == name.as_str())
        .map(|(_, c)| *c);
    let get_allowed = get_cache.is_some();
    if method == Method::GET && !get_allowed {
        // Upstream: `reply.code(405); reply.send();` — empty body.
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }

    // Read endpoints in M0 are unauthenticated; the auth chain drops in at M1
    // here, in ApiCallService order.

    let result: Value = match name.as_str() {
        "ping" => json!({ "pong": now_ms() }),
        "meta" => meta(&state),
        // endpoints-arena: upstream Endpoints endpoint (endpoints/endpoints.ts)
        // returns `endpoints.map(x => x.name)` — the FULL registry in
        // endpoint-list.ts order, requireCredential: false, admin/* included.
        "endpoints" => json!(endpoints_catalog::ENDPOINT_CATALOG),
        "stats" => stats(&state).await,
        "server-info" => server_info(),
        "get-online-users-count" => json!({ "count": 0 }),
        "emojis" => json!({ "emojis": [] }),
        // endpoints.ts ドライブ情報 leg: usage 0 (DriveService no files tracked)
        "drive" => json!({
            "capacity": 1024 * 1024 * 1024,
            "usage": 0,
        }),
        _ => unreachable!("checked above"),
    };

    if let Some(cache_secs) = get_cache {
        // cacheSec: `Cache-Control: private, max-age=`
        let v = format!("private, max-age={cache_secs}");
        let mut resp = (StatusCode::OK, Json(result)).into_response();
        if let Ok(hv) = header::HeaderValue::from_str(&v) {
            resp.headers_mut().insert(header::CACHE_CONTROL, hv);
        }
        return resp;
    }

    // `null` results become 204 in upstream's ApiCallService.send.
    if result.is_null() {
        return StatusCode::NO_CONTENT.into_response();
    }

    (StatusCode::OK, Json(result)).into_response()
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// M0 placeholder driven by static config; the real shape comes from the
/// `meta` table via MetaService in M2 (MiMeta row + Redis invalidation).
fn meta(state: &AppState) -> Value {
    json!({
        "maintainerName": null,
        "maintainerEmail": null,
        "version": env!("CARGO_PKG_VERSION"),
        "provides": {},
        "name": null,
        "shortName": null,
        "uri": state.config.url.clone(),
        "description": null,
        "langs": [],
        "tosUrl": null,
        "repositoryUrl": null,
        "feedbackUrl": null,
        "impressumUrl": null,
        "privacyPolicyUrl": null,
        "disableRegistration": true,
        "emailRequiredForSignup": false,
        "approvalRequiredForSignup": false,
        "enableHcaptcha": false,
        "enableRecaptcha": false,
        "enableTurnstile": false,
        "enableMcaptcha": false,
        "swPublickey": null,
        "themeColor": null,
        "mascotImageUrl": "/assets/ai.png",
        "bannerUrl": null,
        "serverErrorImageUrl": null,
        "infoImageUrl": null,
        "notFoundImageUrl": null,
        "iconUrl": null,
        "backgroundImageUrl": null,
        "logoImageUrl": null,
        "maxNoteTextLength": 3000,
        "defaultLightTheme": null,
        "defaultDarkTheme": null,
        "ads": [],
        "notesPerOneAd": 20,
        "enableEmail": false,
        "enableServiceWorker": false,
        "translatorAvailable": false,
        "serverRules": [],
        "policies": {},
        "mediaProxy": format!("{}/proxy", state.config.url.trim_end_matches('/')),
        "features": {
            "registration": false,
            "emailRequiredForSignup": false,
            "localTimeline": true,
            "globalTimeline": true,
            "hcaptcha": false,
            "recaptcha": false,
            "turnstile": false,
            "objectStorage": false,
            "serviceWorker": false,
            "miauth": false,
        },
        "cacheRemoteFiles": false,
        "cacheRemoteSensitiveFiles": false,
    })
}

async fn stats(state: &AppState) -> Value {
    // Table counts; zeros while the baseline schema is placeholder-only.
    let _ = state;
    json!({
        "notesCount": 0,
        "originalNotesCount": 0,
        "usersCount": 0,
        "originalUsersCount": 0,
        "reactionsCount": 0,
        "instances": 0,
        "driveUsageLocal": 0,
        "driveUsageRemote": 0,
    })
}

/// M0 stand-in for the `systeminformation`-derived payload.
fn server_info() -> Value {
    json!({
        "machine": "misskey-rs",
        "os": std::env::consts::OS,
        "node": "misskey-rs",
        "psql": null,
        "cpu": {
            "model": null,
            "cores": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        },
        "mem": { "total": 0, "used": 0 },
    })
}

/// Rate limit shell wiring for future endpoints (limits arrive with real
/// endpoint metas in M2) — keeps the actor-key logic exercised.
#[allow(dead_code)]
fn limit_actor(state: &AppState, endpoint: &str) {
    let _ = Limit {
        max: 300,
        duration_ms: 60_000,
    };
    let _ = &state.rate_limiter;
    let _ = endpoint;
}

fn parse_json_body(
    req: Request,
) -> impl std::future::Future<Output = Result<Value, Response>> + Send {
    async move {
        let bytes = match axum::body::to_bytes(req.into_body(), 1024 * 1024).await {
            Ok(b) => b,
            Err(_) => {
                return Err(api_err(ApiError::invalid_param(json!({
                    "param": "",
                    "reason": "body could not be read",
                }))));
            }
        };
        if bytes.is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_slice(&bytes).map_err(|err| {
            api_err(ApiError::invalid_param(json!({
                "param": "",
                "reason": err.to_string(),
            })))
        })
    }
}

fn require_str<'a>(body: &'a Value, key: &str) -> Result<&'a str, Response> {
    body.get(key).and_then(|v| v.as_str()).ok_or_else(|| {
        api_err(ApiError::invalid_param(json!({
            "param": key,
            "reason": "expected a string",
        })))
    })
}

/// `POST /api/signup` — port of SignupApiService.signup, test-mode path
/// (captcha/invitation/email checks are skipped when config.test, matching
/// upstream `NODE_ENV === 'test'` shortcuts).
async fn signup_route(state: &AppState, body: &Value) -> Response {
    let username = match require_str(body, "username") {
        Ok(v) => v,
        Err(r) => return r,
    };
    let password = match require_str(body, "password") {
        Ok(v) => v,
        Err(r) => return r,
    };
    if password.is_empty() {
        // paramDef password minLength: 1
        return api_err(ApiError::invalid_param(json!({
            "param": "password",
            "reason": "expected a string of length 1 or more",
        })));
    }

    if let Some(pool) = &state.pool {
        let username_lower = username.to_lowercase();
        if let Err(e) = mk_db::user::username_exists_local(pool, &username_lower).await {
            tracing::error!(target: "dbg_signup", error = %e, "username_exists_local");
        }
        if let Ok(true) = mk_db::user::username_exists_local(pool, &username_lower).await {
            return reply_plain(400, "DUPLICATED_USERNAME");
        }
        if let Ok(true) = mk_db::user::used_username_exists(pool, &username_lower).await {
            return reply_plain(400, "USED_USERNAME");
        }
    } else {
        return api_err(ApiError::internal_error());
    }

    // SignupApiService.ts L116: `host` is read from the body only when
    // NODE_ENV=test — the timelines suite's remote-user legs
    // (signup({ host: genHost() })) ride this; the folded host then lands in
    // user."host" (SignupService.ts: toPunyNullable) and the note insert's
    // userHost denormalization.
    let host_param: Option<&str> = if state.config.test {
        body.get("host").and_then(|v| v.as_str())
    } else {
        None
    };

    let outcome = match mk_core::services::build_signup(username, password, None, host_param) {
        Ok(o) => o,
        Err(code) => return reply_plain(400, code),
    };

    if let Some(pool) = &state.pool {
        let new_user = mk_db::user::NewUser {
            id: &outcome.user_id,
            username: &outcome.username,
            username_lower: &outcome.username_lower,
            name: outcome.name.as_deref(),
            host: outcome.host.as_deref(),
            token: &outcome.token,
            password_hash: &outcome.password_hash,
            public_key: &outcome.public_key,
            private_key: &outcome.private_key,
        };
        if let Err(err) = mk_db::user::insert_new_user(pool, &new_user).await {
            tracing::error!(target: "dbg_signup", %err, "signup insert failed");
            return api_err(ApiError::internal_error());
        }
        // SignupService L160-162: the first signup of an unset meta becomes
        // root (admin gates resolve meta."rootUserId").
        if let Err(err) = mk_db::admin::stamp_root_user_id_if_unset(pool, &outcome.user_id).await {
            tracing::error!(target: "dbg_signup", %err, "rootUserId stamp failed");
            return api_err(ApiError::internal_error());
        }
    }

    let mut packed = serde_json::to_value(mk_core::services::me_detailed_fresh(
        &outcome,
        &state.config.url,
        &state.config.host(),
        true,
    ))
    .unwrap_or(json!({}));
    // SignupApiService returns the MeDetailed pack: complete the role-policy
    // keys the local pack struct does not carry yet (see apply_me_role_compat
    // — the 作りたて census case deep-equals DEFAULT_POLICIES and the compat
    // mutingNotificationTypes key, pack L618/L623 upstream).
    apply_me_role_compat(&mut packed);
    if let Some(obj) = packed.as_object_mut() {
        obj.insert("token".to_string(), json!(outcome.token.clone()));
    }

    (StatusCode::OK, Json(packed)).into_response()
}

/// Upstream `FastifyReplyError(400, '...')`: plain-text body, no envelope.
fn reply_plain(status: u16, text: &str) -> Response {
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST),
        text.to_string(),
    )
        .into_response()
}

async fn reset_route(state: &AppState) -> Response {
    match &state.pool {
        None => api_err(ApiError::internal_error()),
        Some(pool) => match mk_db::user::truncate_all(pool).await {
            Ok(_) => StatusCode::NO_CONTENT.into_response(),
            Err(err) => {
                tracing::error!(%err, "reset failed");
                api_err(ApiError::internal_error())
            }
        },
    }
}

/// Token extraction: `Authorization: Bearer` wins over body `i` (mirrors
/// upstream AuthenticateService consumers). Component of the M1 auth chain.
#[allow(dead_code)]
fn extract_token(body: &Value, headers: &axum::http::HeaderMap) -> Option<String> {
    if let Some(v) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
    {
        return Some(v.to_string());
    }
    body.get("i")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// `POST /api/signin` — SigninApiService.signin test-mode path (no captcha /
/// IP rate limit in test; 2FA unchanged → fresh users only take the plain
/// password branch).
async fn signin_route(state: &AppState, body: &Value) -> Response {
    let reply_error = |status: u16, id: &str| -> Response {
        (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({ "error": { "id": id } })),
        )
            .into_response()
    };

    let username = match body.get("username").and_then(|v| v.as_str()) {
        Some(u) => u,
        None => return StatusCode::BAD_REQUEST.into_response(),
    };
    if let Some(t) = body.get("token") {
        if t.as_str().is_none() {
            return StatusCode::BAD_REQUEST.into_response();
        }
    }

    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };

    let row = mk_db::user::find_local_user_by_username(pool, &username.to_lowercase())
        .await
        .ok()
        .flatten();
    let Some(user) = row else {
        return reply_error(404, "6cc579cc-885d-43d8-95c2-b8c7fc963280");
    };
    if user.is_suspended {
        return reply_error(403, "e03a5f46-d309-4865-9b69-56282d94e1eb");
    }

    let profile = match mk_db::user::find_profile(pool, &user.id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(target: "dbg_signin", error = %e, "find_profile");
            return api_err(ApiError::internal_error());
        }
    };

    // password flow: SigninFlowResponse { finished } variants. A present
    // non-string password (endpoints.ts クエリをインジェクションできない —
    // `password: { $gt: '' }`) is a 400 with an EMPTY body
    // (SigninApiService.ts L162-165 `typeof password !== 'string'`), not the
    // next-step 200 — the e2e signin-flow injection assert depends on it.
    let password = match body.get("password") {
        None | Some(Value::Null) => {
            return (
                StatusCode::OK,
                Json(json!({ "finished": false, "next": "captcha" })),
            )
                .into_response();
        }
        Some(Value::String(s)) => s.clone(),
        Some(_) => return StatusCode::BAD_REQUEST.into_response(),
    };

    let same = profile
        .password
        .as_deref()
        .map(|h| mk_core::services::verify_password(&password, h).unwrap_or(false))
        .unwrap_or(false);

    if !same {
        let _ = mk_db::user::insert_signin(pool, &mk_types::id::gen_aid_now(), &user.id, "", false)
            .await;
        return reply_error(403, "932c904e-9460-45b7-9ce6-7ed33be7eb2c");
    }

    // success (no-2FA branch); upstream fires the signin record async
    let _ =
        mk_db::user::insert_signin(pool, &mk_types::id::gen_aid_now(), &user.id, "", true).await;
    (
        StatusCode::OK,
        Json(json!({
            "finished": true,
            "id": user.id,
            "i": user.token,
        })),
    )
        .into_response()
}

/// `POST /api/users/show` — userId | userIds | username, me from `i`/Bearer.
async fn users_show(state: &AppState, body: &Value, token: Option<String>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };

    let me_id: Option<String> = resolve_me(pool, token.as_deref()).await;

    let ctx = mk_core::services::user_pack::InstanceCtx {
        url: &state.config.url,
        host: &state.config.host(),
    };

    let no_such_user = || {
        api_err(ApiError::new(
            404,
            mk_types::api_error::ApiErrorKind::Client,
            "NO_SUCH_USER",
            "4362f8dc-731f-4ad8-a694-be5a88922a24",
            "No such user.",
        ))
    };

    // userIds array form: missing users are suppressed upstream
    if let Some(ids) = body.get("userIds").and_then(|v| v.as_array()) {
        // gate-roles: viewer flag once per request, per-target role rows below
        let viewer_mod = viewer_is_moderator(pool, me_id.as_deref()).await;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids.iter().filter_map(|v| v.as_str()) {
            let row = match mk_db::user::find_user_by_id(pool, id).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(target: "dbg_users_show", error = %e, "find_user_by_id[userIds]");
                    continue;
                }
            };
            let Some(row) = row else { continue };
            let profile = match mk_db::user::find_profile(pool, &row.id).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(target: "dbg_users_show", error = %e, "find_profile[userIds]");
                    continue;
                }
            };
            let is_me = me_id.as_deref() == Some(row.id.as_str());
            let relation = match build_relation(pool, me_id.as_deref(), &row.id, is_me).await {
                Ok(r) => r,
                Err(resp) => return *resp,
            };
            let packed = {
                let assigned = match mk_db::role::user_assigned_roles(pool, &row.id).await {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::error!(target: "dbg_users_show", error = %e, "user_assigned_roles[userIds]");
                        continue;
                    }
                };
                let role_pack = mk_core::services::user_pack::RolePack {
                    assigned: &assigned,
                    target_is_root: row.username_lower == "root",
                    viewer_is_moderator: viewer_mod,
                };
                let mut packed = mk_core::services::user_pack::pack_inline_with_roles(
                    &row,
                    &profile,
                    is_me,
                    relation.as_ref(),
                    ctx,
                    false, // users/show: secrets stay hidden even for self
                    Some(&role_pack),
                );
                set_viewers_memo(pool, me_id.as_deref(), &row.id, &mut packed).await;
                // upstream users/show self-view packs the MeDetailed schema
                // (see the single-target arm note)
                if is_me {
                    apply_me_role_compat(&mut packed);
                }
                packed
            };
            out.push(packed);
        }
        return (StatusCode::OK, Json(json!(out))).into_response();
    }

    let row = if let Some(id) = body.get("userId").and_then(|v| v.as_str()) {
        match mk_db::user::find_user_by_id(pool, id).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(target: "dbg_users_show", error = %e, "find_user_by_id");
                None
            }
        }
    } else if let Some(username) = body.get("username").and_then(|v| v.as_str()) {
        match mk_db::user::find_local_user_by_username(pool, &username.to_lowercase()).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(target: "dbg_users_show", error = %e, "find_local_user_by_username");
                None
            }
        }
    } else {
        return api_err(ApiError::invalid_param(json!({
            "param": "userId",
            "reason": "either userId or username is required",
        })));
    };

    let Some(row) = row else {
        return no_such_user();
    };
    let profile = match mk_db::user::find_profile(pool, &row.id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(target: "dbg_users_show", error = %e, "find_profile");
            return no_such_user();
        }
    };
    let is_me = me_id.as_deref() == Some(row.id.as_str());
    let relation = match build_relation(pool, me_id.as_deref(), &row.id, is_me).await {
        Ok(r) => r,
        Err(resp) => return *resp,
    };
    // gate-roles: badge + public role lists and the merged-policy flags
    // (UserEntityService.pack reads roleService per user).
    let viewer_mod = viewer_is_moderator(pool, me_id.as_deref()).await;
    let packed = {
        let assigned = match mk_db::role::user_assigned_roles(pool, &row.id).await {
            Ok(a) => a,
            Err(e) => {
                tracing::error!(target: "dbg_users_show", error = %e, "user_assigned_roles");
                return api_err(ApiError::internal_error());
            }
        };
        let role_pack = mk_core::services::user_pack::RolePack {
            assigned: &assigned,
            target_is_root: row.username_lower == "root",
            viewer_is_moderator: viewer_mod,
        };
        let mut packed = mk_core::services::user_pack::pack_inline_with_roles(
            &row,
            &profile,
            is_me,
            relation.as_ref(),
            ctx,
            false, // users/show: secrets stay hidden even for self
            Some(&role_pack),
        );
        set_viewers_memo(pool, me_id.as_deref(), &row.id, &mut packed).await;
        // upstream users/show self-view packs the MeDetailed schema (i parity;
        // secrets stay hidden per includeSecrets: false)
        if is_me {
            apply_me_role_compat(&mut packed);
        }
        packed
    };

    (StatusCode::OK, Json(packed)).into_response()
}

/// Relation lookup for the pack, mirroring upstream `getRelation` scope:
/// only when a me is present AND the target is not me.
async fn build_relation(
    pool: &mk_db::sqlx::PgPool,
    me_id: Option<&str>,
    target_id: &str,
    is_me: bool,
) -> Result<Option<mk_db::relation::UserRelation>, Box<Response>> {
    if is_me || me_id.is_none() {
        return Ok(None);
    }
    match mk_db::relation::get_relation(pool, me_id.unwrap(), target_id).await {
        Ok(r) => Ok(Some(r)),
        Err(e) => {
            tracing::error!(target: "dbg_users_show", error = %e, "get_relation");
            Err(Box::new(api_err(ApiError::internal_error())))
        }
    }
}

/// gate-roles helper: `RoleService.isModerator(me)` for the pack (admin.rs
/// root shortcut: `meta.rootUserId` || `usernameLower == 'root'`; falls
/// through to assigned moderator/administrator roles).
async fn viewer_is_moderator(pool: &mk_db::sqlx::PgPool, me_id: Option<&str>) -> bool {
    let Some(me_id) = me_id else {
        return false;
    };
    match mk_db::admin::root_user_id(pool).await {
        Ok(Some(root)) if root == me_id => return true,
        Ok(_) => {}
        Err(e) => {
            tracing::error!(target: "dbg_users_show", error = %e, "root_user_id");
        }
    }
    match mk_db::user::find_user_by_id(pool, me_id).await {
        Ok(Some(row)) if row.username_lower == "root" => return true,
        Ok(_) => {}
        Err(e) => {
            tracing::error!(target: "dbg_users_show", error = %e, "find_user_by_id[viewer]");
            return false;
        }
    }
    match mk_db::role::user_assigned_roles(pool, me_id).await {
        Ok(assigned) => mk_core::services::role::has_moderator_role(&assigned),
        Err(e) => {
            tracing::error!(target: "dbg_users_show", error = %e, "user_assigned_roles[viewer]");
            false
        }
    }
}

/// Shared me-id resolution from a token: only native user tokens (length 16,
/// `misc/token.ts isNativeUserToken`) are matched against `user.token` for
/// local users; anything else resolves to "not me" (DB errors are logged).
async fn resolve_me(pool: &mk_db::sqlx::PgPool, token: Option<&str>) -> Option<String> {
    let Some(t) = token else {
        return None;
    };
    if t.len() != 16 {
        return None;
    }
    match mk_db::user::find_local_user_id_by_token(pool, t).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(target: "dbg_resolve_me", error = %e, "find_local_user_id_by_token");
            None
        }
    }
}

/// `POST /api/i` — requireCredential endpoint returning the caller's
/// MeDetailed. Auth gating mirrors `ApiCallService.call()`: no token →
/// CREDENTIAL_REQUIRED (401); unresolvable token → AUTHENTICATION_FAILED
/// (401, from AuthenticateService's AuthenticationError); suspended →
/// YOUR_ACCOUNT_SUSPENDED (403). The endpoint body is `endpoints/i.ts`.
async fn i_route(
    state: &AppState,
    body: &Value,
    headers: &axum::http::HeaderMap,
    token: Option<String>,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };

    // Upstream handleRequest: a non-string `i` (with no Bearer taking
    // precedence) → `reply.code(400)` with an empty body.
    let has_bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .map(|h| h.starts_with("Bearer "))
        .unwrap_or(false);
    if !has_bearer {
        if let Some(v) = body.get("i") {
            if v.as_str().is_none() {
                return StatusCode::BAD_REQUEST.into_response();
            }
        }
    }

    let credential_required = || {
        api_err(ApiError::new(
            401,
            mk_types::api_error::ApiErrorKind::Client,
            "CREDENTIAL_REQUIRED",
            "1384574d-a912-4b81-8601-c7b1c4085df1",
            "Credential required.",
        ))
    };
    let auth_failed = || {
        api_err(ApiError::new(
            401,
            mk_types::api_error::ApiErrorKind::Client,
            "AUTHENTICATION_FAILED",
            "b0a7f5f8-dc2f-4171-b91f-de88ad238e14",
            "Authentication failed. Please ensure your token is correct.",
        ))
    };

    // AuthenticateService: token absent → [null, null] user; present token
    // that matches no user (native shape) → AuthenticationError.
    let Some(token) = token else {
        return credential_required();
    };
    if token.len() != 16 {
        // Non-native tokens go through the access-token/app lookup, which M1
        // does not implement — same 401 as upstream's invalid signature path.
        return auth_failed();
    }
    let Some(me_id) = resolve_me(pool, Some(token.as_str())).await else {
        return auth_failed();
    };

    let row = match mk_db::user::find_user_by_id(pool, &me_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_i", error = %e, "find_user_by_id");
            return api_err(ApiError::internal_error());
        }
    };
    let Some(row) = row else {
        return auth_failed();
    };

    // call(): requireCredential suspended gate.
    if row.is_suspended {
        return api_err(ApiError::new(
            403,
            mk_types::api_error::ApiErrorKind::Permission,
            "YOUR_ACCOUNT_SUSPENDED",
            "a8c724b3-6e9c-4b46-b1a8-bc3ed6258370",
            "Your account has been suspended.",
        ));
    }

    let mut profile = match mk_db::user::find_profile(pool, &row.id).await {
        Ok(p) => p,
        Err(mk_db::sqlx::Error::RowNotFound) => {
            // endpoint body: userProfile == null → USER_IS_DELETED
            return api_err(ApiError::new(
                403,
                mk_types::api_error::ApiErrorKind::Permission,
                "USER_IS_DELETED",
                "e5b3b9f0-2b8f-4b9f-9c1f-8c5c1b2e1b1a",
                "User is deleted.",
            ));
        }
        Err(e) => {
            tracing::error!(target: "dbg_i", error = %e, "find_profile");
            return api_err(ApiError::internal_error());
        }
    };

    // i.ts: append today to loggedInDates (once per day) before packing.
    let today = js_today();
    if !profile.logged_in_dates.contains(&today) {
        profile.logged_in_dates.push(today.clone());
        // Upstream awaits this update inside the handler; a failure is logged
        // and the response still reflects the in-memory append.
        if let Err(err) = mk_db::user::append_logged_in_date(pool, &row.id, &today).await {
            tracing::error!(target: "dbg_i", error = %err, "append_logged_in_date");
        }
    }

    let ctx = mk_core::services::user_pack::InstanceCtx {
        url: &state.config.url,
        host: &state.config.host(),
    };

    (
        StatusCode::OK,
        Json({
            let mut v = mk_core::services::user_pack::pack_inline(&row, &profile, true, None, ctx);
            apply_me_unread(pool, &mut v).await;
            apply_me_role_compat(&mut v);
            v
        }),
    )
        .into_response()
}

/// i.ts `today`: `${getFullYear()}/${getMonth() + 1}/${getDate()}` (unpadded).
/// JS `Date` uses server-local time; we use UTC (civil_from_days, the same
/// Howard Hinnant algorithm as `ms_to_iso`).
fn js_today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y}/{m}/{d}")
}

/// Upstream `ApiCallService.#sendAuthenticationError`: 401 with the stable
/// AUTHENTICATION_FAILED error (plus a WWW-Authenticate header, which the
/// `api_err` envelope cannot carry yet).
fn authentication_failed() -> Response {
    api_err(ApiError::new(
        401,
        mk_types::api_error::ApiErrorKind::Client,
        "AUTHENTICATION_FAILED",
        "b0a7f5f8-dc2f-4171-b91f-de88ad238e14",
        "Authentication failed. Please ensure your token is correct.",
    ))
}

fn invalid_param(param: &str, reason: &str) -> Response {
    api_err(ApiError::invalid_param(json!({
        "param": param,
        "reason": reason,
    })))
}

/// Optional nullable string param (`{ type: 'string', nullable: true }`):
/// absent → skip, null → write NULL, other types → INVALID_PARAM.
fn opt_nullable_str(body: &Value, key: &str) -> Result<Option<Option<String>>, Response> {
    match body.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(s)) => Ok(Some(Some(s.clone()))),
        Some(_) => Err(invalid_param(key, "expected a string or null")),
    }
}

fn opt_bool(body: &Value, key: &str) -> Result<Option<bool>, Response> {
    match body.get(key) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(invalid_param(key, "expected a boolean")),
    }
}

fn opt_enum(body: &Value, key: &str, allowed: &[&str]) -> Result<Option<String>, Response> {
    match body.get(key) {
        None => Ok(None),
        Some(Value::String(s)) if allowed.contains(&s.as_str()) => Ok(Some(s.clone())),
        Some(_) => Err(invalid_param(
            key,
            &format!("expected one of: {}", allowed.join(", ")),
        )),
    }
}

/// Enforces the schema min/maxLength of `nameSchema`/`descriptionSchema`/
/// `locationSchema` (raw value, before post-validation trim like upstream).
fn with_str_limits(
    value: Result<Option<Option<String>>, Response>,
    key: &str,
    min: usize,
    max: usize,
) -> Result<Option<Option<String>>, Response> {
    match value? {
        Some(Some(s)) => {
            let len = s.chars().count();
            if len < min {
                return Err(invalid_param(key, "must not be empty"));
            }
            if len > max {
                return Err(invalid_param(key, "must be shorter than the limit"));
            }
            Ok(Some(Some(s)))
        }
        other => Ok(other),
    }
}

/// `birthdaySchema` pattern: `^([0-9]{4})-([0-9]{2})-([0-9]{2})$`.
fn valid_birthday(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            _ => c.is_ascii_digit(),
        })
}

/// `endpoints/i/update.ts` paramDef M1 subset → [`mk_db::user::ProfileUpdate`].
fn parse_profile_update(body: &Value) -> Result<mk_db::user::ProfileUpdate, Response> {
    let name = match with_str_limits(opt_nullable_str(body, "name"), "name", 1, 50)? {
        None => None,
        Some(v) => Some(match v {
            None => None,
            Some(s) => {
                // JS String.prototype.trim: White_Space + <BOM> U+FEFF,
                // which char::is_whitespace does NOT cover
                let t = s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
                if t.is_empty() {
                    None
                } else {
                    Some(t.to_string())
                }
            }
        }),
    };
    let description = with_str_limits(
        opt_nullable_str(body, "description"),
        "description",
        1,
        1500,
    )?;
    let location = with_str_limits(opt_nullable_str(body, "location"), "location", 1, 50)?;
    // TODO(M2): validate `lang` against langmap keys + null (upstream paramDef
    // enum); no langmap port exists yet, so M1 stores the raw string.
    let lang = opt_nullable_str(body, "lang")?;
    let url = opt_nullable_str(body, "url")?;
    // birthday column is character(10): "YYYY-MM-DD" strings persist as-is,
    // explicit null → NULL, ABSENT → untouched (upstream i/update.ts L296
    // `if (ps.birthday !== undefined)` — collapsing undefined into the null
    // branch re-nulled unchanged birthdays on every partial update)
    let birthday = match body.get("birthday") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(s)) => {
            if !valid_birthday(s) {
                return Err(invalid_param("birthday", "expected YYYY-MM-DD"));
            }
            Some(Some(s.clone()))
        }
        Some(_) => return Err(invalid_param("birthday", "expected a string or null")),
    };

    // fields: array of { name, value }, 0..16 items; entries with an empty
    // trimmed name/value are dropped and survivors are trimmed (upstream's
    // post-ajv filter/map).
    let fields = match body.get("fields") {
        None => None,
        Some(Value::Array(items)) => {
            if items.len() > 16 {
                return Err(invalid_param(
                    "fields",
                    "array must not have more than 16 items",
                ));
            }
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let fname = item.get("name").and_then(|x| x.as_str());
                let fvalue = item.get("value").and_then(|x| x.as_str());
                let (Some(n), Some(v)) = (fname, fvalue) else {
                    return Err(invalid_param(
                        "fields",
                        "items must be { name, value } strings",
                    ));
                };
                let (nt, vt) = (n.trim(), v.trim());
                if nt.is_empty() || vt.is_empty() {
                    continue;
                }
                out.push(json!({ "name": nt, "value": vt }));
            }
            Some(Value::Array(out))
        }
        Some(_) => return Err(invalid_param("fields", "expected an array")),
    };

    // followedMessage: varchar(256)
    let followed_message = with_str_limits(
        opt_nullable_str(body, "followedMessage"),
        "followedMessage",
        1,
        256,
    )?;
    // JSON passthrough fields (array/object checks only; shape validation is
    // the consuming features' business, users.ts fixtures send valid values)
    let json_field = |key: &str| -> Result<Option<Value>, Response> {
        match body.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v @ Value::Array(_)) | Some(v @ Value::Object(_)) => Ok(Some(v.clone())),
            Some(_) => Err(invalid_param(key, "expected an array or object")),
        }
    };
    let muted_words = json_field("mutedWords")?;
    let hard_muted_words = json_field("hardMutedWords")?;
    let muted_instances = json_field("mutedInstances")?;
    let email_notification_types = json_field("emailNotificationTypes")?;
    let notification_recieve_config = json_field("notificationRecieveConfig")?;

    let visibility = ["public", "followers", "private"];
    let chat_scope = ["everyone", "followers", "following", "mutual", "none"];
    Ok(mk_db::user::ProfileUpdate {
        name,
        is_locked: opt_bool(body, "isLocked")?,
        is_explorable: opt_bool(body, "isExplorable")?,
        hide_online_status: opt_bool(body, "hideOnlineStatus")?,
        is_bot: opt_bool(body, "isBot")?,
        is_cat: opt_bool(body, "isCat")?,
        chat_scope: opt_enum(body, "chatScope", &chat_scope)?,
        description,
        location,
        birthday,
        lang,
        url,
        fields,
        no_crawle: opt_bool(body, "noCrawle")?,
        prevent_ai_learning: opt_bool(body, "preventAiLearning")?,
        inject_featured_note: opt_bool(body, "injectFeaturedNote")?,
        receive_announcement_email: opt_bool(body, "receiveAnnouncementEmail")?,
        always_mark_nsfw: opt_bool(body, "alwaysMarkNsfw")?,
        auto_sensitive: opt_bool(body, "autoSensitive")?,
        careful_bot: opt_bool(body, "carefulBot")?,
        auto_accept_followed: opt_bool(body, "autoAcceptFollowed")?,
        public_reactions: opt_bool(body, "publicReactions")?,
        following_visibility: opt_enum(body, "followingVisibility", &visibility)?,
        followers_visibility: opt_enum(body, "followersVisibility", &visibility)?,
        followed_message,
        muted_words,
        hard_muted_words,
        muted_instances,
        email_notification_types,
        notification_recieve_config,
    })
}

/// `POST /api/i/update` — requireCredential profile update, M1 subset of
/// upstream `endpoints/i/update.ts` (name/description/location/birthday/lang/
/// url/fields + the boolean/visibility switch set incl. isBot/isCat; avatar,
/// banner, decorations, mutedWords, room, pinnedPageId, alsoKnownAs etc. are
/// TODO).
async fn i_update(state: &AppState, body: &Value, token: Option<String>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };

    // requireCredential: only native 16-char tokens resolve to a local user;
    // anything else is the 401 AUTHENTICATION_FAILED shape.
    let Some(tok) = token.as_deref().filter(|t| t.len() == 16) else {
        return authentication_failed();
    };
    let me_id: Option<String> = match mk_db::user::find_local_user_id_by_token(pool, tok).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(target: "dbg_i_update", error = %e, "find_local_user_id_by_token");
            return api_err(ApiError::internal_error());
        }
    };
    let Some(me_id) = me_id else {
        return authentication_failed();
    };

    let row = match mk_db::user::find_user_by_id(pool, &me_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_i_update", error = %e, "find_user_by_id");
            return api_err(ApiError::internal_error());
        }
    };
    let Some(row) = row else {
        return authentication_failed();
    };
    // call(): requireCredential suspended gate.
    if row.is_suspended {
        return api_err(ApiError::new(
            403,
            mk_types::api_error::ApiErrorKind::Permission,
            "YOUR_ACCOUNT_SUSPENDED",
            "a8c724b3-6e9c-4b46-b1a8-bc3ed6258370",
            "Your account has been suspended.",
        ));
    }

    // --- paramDef subset ---
    let update = match parse_profile_update(body) {
        Ok(u) => u,
        Err(resp) => return resp,
    };

    // --- avatar/banner drive-file swap (i/update.ts L377-411) ---
    // `if (ps.avatarId)` → drive lookup (owned + image/*) → denormal
    // avatarUrl/avatarBlurhash written onto the user row; `=== null` → the
    // triple is nulled (pack falls back to the identicon URL).
    let avatar_op = match avatar_banner_op(state, pool, &me_id, body.get("avatarId"), true).await {
        Ok(op) => op,
        Err(resp) => return resp,
    };
    let banner_op = match avatar_banner_op(state, pool, &me_id, body.get("bannerId"), false).await {
        Ok(op) => op,
        Err(resp) => return resp,
    };

    if let Err(e) = mk_db::user::update_profile_subset(pool, &me_id, &update).await {
        tracing::error!(target: "dbg_i_update", error = %e, "update_profile_subset");
        return api_err(ApiError::internal_error());
    }
    if let Err(e) = mk_db::user::update_avatar_banner(pool, &me_id, avatar_op, banner_op).await {
        tracing::error!(target: "dbg_i_update", error = %e, "update_avatar_banner");
        return api_err(ApiError::internal_error());
    }

    // i/update.ts L483-531: hashtags from the new description join user.tags
    // (normalized lowercase, capped at 32; de-dup in SQL).
    if let Some(desc) = body.get("description").and_then(|v| v.as_str()) {
        let tags = mk_core::services::misc::extract_hashtags(desc);
        if !tags.is_empty() {
            if let Err(e) = mk_db::user::merge_user_tags(pool, &me_id, &tags).await {
                tracing::error!(target: "dbg_i_update", error = %e, "merge_user_tags");
            }
        }
    }

    // Fresh persist + pack (MeDetailed, includeSecrets path).
    let row = match mk_db::user::find_user_by_id(pool, &me_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_i_update", error = %e, "find_user_by_id[fresh]");
            return api_err(ApiError::internal_error());
        }
    };
    let Some(row) = row else {
        return api_err(ApiError::internal_error());
    };
    let profile = match mk_db::user::find_profile(pool, &row.id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(target: "dbg_i_update", error = %e, "find_profile[fresh]");
            return api_err(ApiError::internal_error());
        }
    };

    let ctx = mk_core::services::user_pack::InstanceCtx {
        url: &state.config.url,
        host: &state.config.host(),
    };

    (
        StatusCode::OK,
        Json({
            let mut v = mk_core::services::user_pack::pack_inline(&row, &profile, true, None, ctx);
            apply_me_unread(pool, &mut v).await;
            apply_me_role_compat(&mut v);
            v
        }),
    )
        .into_response()
}

/// Me-view unread flag injection: `hasUnreadNotification` reflects the
/// notification table at request time (pack is pure; upstream computes it
/// in UserEntityService.pack per call).
async fn apply_me_unread(pool: &mk_db::sqlx::PgPool, packed: &mut Value) {
    match mk_db::notification::has_unread(pool, packed["id"].as_str().unwrap_or("")).await {
        Ok(v) => {
            if let Some(obj) = packed.as_object_mut() {
                obj.insert("hasUnreadNotification".into(), Value::Bool(v));
            }
        }
        Err(e) => tracing::error!(target: "dbg_i", error = %e, "has_unread"),
    }
}

// ---------------------------------------------------------------------------
// i/update long-tail wave: Me-detailed role-compat injection + i/pin +
// i/unpin + users/update-memo (users.ts を書き換えることができる census tail)
// ---------------------------------------------------------------------------

/// `RoleService.DEFAULT_POLICIES` (RoleService.ts L78). `getUserPolicies` =
/// DEFAULT merged with meta.policies; no roles infrastructure exists yet, and
/// the e2e asserts `response.policies` deep-equals DEFAULT_POLICIES verbatim.
/// TODO(M4): derive from role rows once admin/roles lands.
pub(crate) fn default_policies_value() -> Value {
    json!({
        "gtlAvailable": true,
        "ltlAvailable": true,
        "canPublicNote": true,
        "mentionLimit": 20,
        "canInvite": false,
        "inviteLimit": 0,
        "inviteLimitCycle": 60 * 24 * 7,
        "inviteExpirationTime": 0,
        "canManageCustomEmojis": false,
        "canManageAvatarDecorations": false,
        "canSearchNotes": false,
        "canSearchUsers": true,
        "canUseTranslator": true,
        "canHideAds": false,
        "canCreateChannel": true,
        "driveCapacityMb": 100,
        "maxFileSizeMb": 30,
        "alwaysMarkNsfw": false,
        "canUpdateBioMedia": true,
        "pinLimit": 5,
        "antennaLimit": 5,
        "wordMuteLimit": 200,
        "webhookLimit": 3,
        "clipLimit": 10,
        "noteEachClipsLimit": 200,
        "userListLimit": 10,
        "userEachUserListsLimit": 50,
        "rateLimitFactor": 1,
        "avatarDecorationLimit": 1,
        "canImportAntennas": false,
        "canImportBlocking": false,
        "canImportFollowing": false,
        "canImportMuting": false,
        "canImportUserLists": false,
        "chatAvailability": "available",
        "uploadableFileTypes": [
            "text/*",
            "application/json",
            "image/*",
            "video/*",
            "audio/*",
        ],
        "noteDraftLimit": 10,
        "scheduledNoteLimit": 1,
        "watermarkAvailable": true,
    })
}

/// MeDetailed key-set completions the pack struct does not carry yet:
/// `policies: getUserPolicies(...)` (UserEntityService.pack L623) and the
/// backward-compat `mutingNotificationTypes: []` (pack L618, 後方互換のため).
/// Applied at every MeDetailed emit point (signup, i, i/update, i/pin,
/// i/unpin) because the e2e `meDetailed()` helper builds `expected` from the
/// signup/i responses, so ALL me-shaped dicts must carry the same keys.
/// PACK-SIDE TODO (mk-core user_pack + me_detailed_fresh): move these into
/// the pack structs when that surface reopens; upstream truth is pack
/// L618/L623.
pub(crate) fn apply_me_role_compat(packed: &mut Value) {
    if let Some(obj) = packed.as_object_mut() {
        obj.insert("mutingNotificationTypes".into(), json!([]));
        obj.insert("policies".into(), default_policies_value());
    }
}

/// Shared requireCredential chain for the tail endpoints (same order as
/// i_update): native 16-char token → local user id → suspended gate.
pub(crate) async fn require_credential_me_id(
    pool: &mk_db::sqlx::PgPool,
    token: Option<&str>,
    dbg: &'static str,
) -> Result<String, Response> {
    let Some(tok) = token.filter(|t| t.len() == 16) else {
        return Err(authentication_failed());
    };
    let me_id = match mk_db::user::find_local_user_id_by_token(pool, tok).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(target: "dbg_tail", endpoint = dbg, error = %e, "find_local_user_id_by_token");
            return Err(api_err(ApiError::internal_error()));
        }
    };
    let Some(me_id) = me_id else {
        return Err(authentication_failed());
    };
    match mk_db::user::find_user_by_id(pool, &me_id).await {
        Ok(Some(row)) => {
            if row.is_suspended {
                return Err(api_err(ApiError::new(
                    403,
                    mk_types::api_error::ApiErrorKind::Permission,
                    "YOUR_ACCOUNT_SUSPENDED",
                    "a8c724b3-6e9c-4b46-b1a8-bc3ed6258370",
                    "Your account has been suspended.",
                )));
            }
        }
        Ok(None) => return Err(authentication_failed()),
        Err(e) => {
            tracing::error!(target: "dbg_tail", endpoint = dbg, error = %e, "find_user_by_id");
            return Err(api_err(ApiError::internal_error()));
        }
    }
    Ok(me_id)
}

/// Packed MeDetailed of `me` (no secrets — upstream i/pin/i/unpin pack with
/// the default opts, includeSecrets default false) with the user's pinned
/// note ids/notes injected (pack L552-556; pins live in `user_note_pining`,
/// packed `pinnedNotes` re-packs each note with `{ detail: true }` — the
/// same shape `notes/show` returns).
async fn pack_me_with_pins(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    me_id: &str,
) -> Result<Value, Response> {
    let row = mk_db::user::find_user_by_id(pool, me_id)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_pin", error = %e, "find_user_by_id");
            api_err(ApiError::internal_error())
        })?
        .ok_or_else(authentication_failed)?;
    let profile = mk_db::user::find_profile(pool, &row.id)
        .await
        .map_err(|e| {
            tracing::error!(target: "dbg_pin", error = %e, "find_profile");
            api_err(ApiError::internal_error())
        })?;
    let ctx = mk_core::services::user_pack::InstanceCtx {
        url: &state.config.url,
        host: &state.config.host(),
    };
    let mut packed =
        mk_core::services::user_pack::pack_inline_opts(&row, &profile, true, None, ctx, false);
    apply_me_unread(pool, &mut packed).await;
    apply_me_role_compat(&mut packed);
    match mk_db::pin::pinned_note_ids(pool, me_id).await {
        Ok(ids) => {
            let mut notes = Vec::with_capacity(ids.len());
            for id in &ids {
                match pack_note(state, pool, id).await {
                    Ok(n) => notes.push(n),
                    Err(r) => {
                        // Deleted note under a pin: upstream innerJoins note so
                        // the pin simply disappears from the pack.
                        let _ = r;
                    }
                }
            }
            if let Some(obj) = packed.as_object_mut() {
                obj.insert("pinnedNoteIds".into(), json!(ids));
                obj.insert("pinnedNotes".into(), json!(notes));
            }
        }
        Err(e) => {
            tracing::error!(target: "dbg_pin", error = %e, "pinned_note_ids");
        }
    }
    Ok(packed)
}

/// `POST /api/i/pin` — `NotePiningService.addPinned` + packed MeDetailed.
async fn i_pin(state: &AppState, body: &Value, token: Option<String>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = match require_credential_me_id(pool, token.as_deref(), "dbg_pin").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let Some(note_id) = body.get("noteId").and_then(|v| v.as_str()) else {
        return api_err(ApiError::invalid_param(json!({
            "param": "noteId",
            "reason": "required",
        })));
    };

    // addPinned: the note must exist and belong to the caller.
    let note = match mk_db::note::find_note_by_id(pool, note_id).await {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(target: "dbg_pin", error = %e, "find_note_by_id");
            return api_err(ApiError::internal_error());
        }
    };
    let note = match note {
        Some(n) if n.user_id == me_id => n,
        _ => {
            return api_err(ApiError::new(
                400,
                mk_types::api_error::ApiErrorKind::Client,
                "NO_SUCH_NOTE",
                "56734f8b-3928-431e-bf80-6ff87df40cb3",
                "No such note.",
            ));
        }
    };
    match mk_db::pin::pining_exists(pool, &me_id, note_id).await {
        Ok(true) => {
            return api_err(ApiError::new(
                400,
                mk_types::api_error::ApiErrorKind::Client,
                "ALREADY_PINNED",
                "8b18c2b7-68fe-4edb-9892-c0cbaeb6c913",
                "That note has already been pinned.",
            ));
        }
        Ok(false) => {}
        Err(e) => {
            tracing::error!(target: "dbg_pin", error = %e, "pining_exists");
            return api_err(ApiError::internal_error());
        }
    }
    // DEFAULT_POLICIES.pinLimit (no role infra yet)
    match mk_db::pin::count_pinings(pool, &me_id).await {
        Ok(n) if n >= 5 => {
            return api_err(ApiError::new(
                400,
                mk_types::api_error::ApiErrorKind::Client,
                "PIN_LIMIT_EXCEEDED",
                "72dab508-c64d-498f-8740-a8eec1ba385a",
                "You can not pin notes any more.",
            ));
        }
        Ok(_) => {}
        Err(e) => {
            tracing::error!(target: "dbg_pin", error = %e, "count_pinings");
            return api_err(ApiError::internal_error());
        }
    }
    if let Err(e) =
        mk_db::pin::insert_pining(pool, &mk_types::id::gen_aid_now(), &me_id, &note.id).await
    {
        tracing::error!(target: "dbg_pin", error = %e, "insert_pining");
        return api_err(ApiError::internal_error());
    }

    match pack_me_with_pins(state, pool, &me_id).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(r) => r,
    }
}

/// `POST /api/i/unpin` — `NotePiningService.removePinned` + packed MeDetailed.
async fn i_unpin(state: &AppState, body: &Value, token: Option<String>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = match require_credential_me_id(pool, token.as_deref(), "dbg_pin").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let Some(note_id) = body.get("noteId").and_then(|v| v.as_str()) else {
        return api_err(ApiError::invalid_param(json!({
            "param": "noteId",
            "reason": "required",
        })));
    };

    // removePinned: same ownership gate, error id differs from addPinned.
    let note = match mk_db::note::find_note_by_id(pool, note_id).await {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(target: "dbg_pin", error = %e, "find_note_by_id");
            return api_err(ApiError::internal_error());
        }
    };
    match note {
        Some(n) if n.user_id == me_id => {
            if let Err(e) = mk_db::pin::delete_pining(pool, &me_id, &n.id).await {
                tracing::error!(target: "dbg_pin", error = %e, "delete_pining");
                return api_err(ApiError::internal_error());
            }
        }
        _ => {
            return api_err(ApiError::new(
                400,
                mk_types::api_error::ApiErrorKind::Client,
                "NO_SUCH_NOTE",
                "454170ce-9d63-4a43-9da1-ea10afe81e21",
                "No such note.",
            ));
        }
    }

    match pack_me_with_pins(state, pool, &me_id).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(r) => r,
    }
}

/// `POST /api/users/update-memo` — endpoints/users/update-memo.ts: upsert or
/// delete the viewer's personal memo on a target user; void response
/// (endpoint-base replies 204 for the undefined return).
async fn users_update_memo(state: &AppState, body: &Value, token: Option<String>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = match require_credential_me_id(pool, token.as_deref(), "dbg_memo").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let Some(user_id) = body.get("userId").and_then(|v| v.as_str()) else {
        return api_err(ApiError::invalid_param(json!({
            "param": "userId",
            "reason": "required",
        })));
    };
    // paramDef requires `memo` (string, nullable); absence is INVALID_PARAM.
    let memo = match body.get("memo") {
        Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            return api_err(ApiError::invalid_param(json!({
                "param": "memo",
                "reason": "expected a string or null",
            })));
        }
        None => {
            return api_err(ApiError::invalid_param(json!({
                "param": "memo",
                "reason": "required",
            })));
        }
    };

    // GetterService.getUser → NO_SUCH_USER on unknown target
    let target = match mk_db::user::find_user_by_id(pool, user_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_memo", error = %e, "find_user_by_id");
            return api_err(ApiError::internal_error());
        }
    };
    match target {
        Some(_) => {}
        None => {
            return api_err(ApiError::new(
                404,
                mk_types::api_error::ApiErrorKind::Client,
                "NO_SUCH_USER",
                "6fef56f3-e765-4957-88e5-c6f65329b8a5",
                "No such user.",
            ));
        }
    }

    // 引数がnullか空文字であれば、パーソナルメモを削除する
    // (written as `!is_empty` so the guard never reads a binding the `None`
    // alternative of an or-pattern would not produce)
    let result = match memo {
        Some(ref m) if !m.is_empty() => {
            mk_db::memo::set_user_memo(pool, &mk_types::id::gen_aid_now(), &me_id, user_id, m).await
        }
        _ => mk_db::memo::delete_user_memo(pool, &me_id, user_id).await,
    };
    match result {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!(target: "dbg_memo", error = %e, "update_memo");
            api_err(ApiError::internal_error())
        }
    }
}

/// users/show memo completion: upstream pack reads the viewer's `user_memo`
/// row for an other-person view (UserEntityService.pack L445-448) and keeps
/// null on self/anonymous views. Post-pack injection until the pack struct
/// grows a viewer-aware memo input (PACK-SIDE TODO, mk-core user_pack).
async fn set_viewers_memo(
    pool: &mk_db::sqlx::PgPool,
    me_id: Option<&str>,
    target_id: &str,
    packed: &mut Value,
) {
    let Some(me_id) = me_id else {
        return; // anonymous viewer → pack's memo: null stands
    };
    match mk_db::memo::find_user_memo(pool, me_id, target_id).await {
        Ok(memo) => {
            if let Some(obj) = packed.as_object_mut() {
                obj.insert("memo".into(), json!(memo));
            }
        }
        Err(e) => {
            tracing::error!(target: "dbg_users_show", error = %e, "find_user_memo");
        }
    }
}

/// `i/update.ts` L377-411: parse the avatarId/bannerId param into the
/// drive-file swap op, validating existence/ownership (`NO_SUCH_AVATAR`) and
/// MIME (`AVATAR_NOT_AN_IMAGE`), and resolving the public URL like
/// `DriveFileEntityService.getPublicUrl` (avatar mode goes through the media
/// proxy, `{url}/proxy/avatar.webp?url=…&avatar=1`).
async fn avatar_banner_op(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    me_id: &str,
    param: Option<&Value>,
    is_avatar: bool,
) -> Result<mk_db::user::AvatarBannerOp, Response> {
    let Some(param) = param else {
        return Ok(mk_db::user::AvatarBannerOp::Untouched);
    };
    match param {
        Value::Null => Ok(mk_db::user::AvatarBannerOp::Clear),
        Value::String(id) => {
            let file = match mk_db::drive::find_drive_file(pool, id).await {
                Ok(f) => f,
                Err(e) => {
                    tracing::error!(target: "dbg_i_update", error = %e, "find_drive_file");
                    return Err(api_err(ApiError::internal_error()));
                }
            };
            let file = match file {
                Some(f) if f.user_id.as_deref() == Some(me_id) => f,
                _ => {
                    return Err(api_err(ApiError::new(
                        400,
                        mk_types::api_error::ApiErrorKind::Client,
                        if is_avatar {
                            "NO_SUCH_AVATAR"
                        } else {
                            "NO_SUCH_BANNER"
                        },
                        if is_avatar {
                            "539f3a45-f215-4f81-a9a8-31293640207f"
                        } else {
                            "0d8f5629-f210-41c2-9433-735831a58595"
                        },
                        if is_avatar {
                            "No such avatar file."
                        } else {
                            "No such banner file."
                        },
                    )));
                }
            };
            if !file.file_type.starts_with("image/") {
                return Err(api_err(ApiError::new(
                    400,
                    mk_types::api_error::ApiErrorKind::Client,
                    if is_avatar {
                        "AVATAR_NOT_AN_IMAGE"
                    } else {
                        "BANNER_NOT_AN_IMAGE"
                    },
                    if is_avatar {
                        "f419f9f8-2f4d-46b1-9fb4-49d3a2fd7191"
                    } else {
                        "75aedb19-2afd-4e6d-87fc-67941256fa60"
                    },
                    if is_avatar {
                        "The file specified as an avatar is not an image."
                    } else {
                        "The file specified as a banner is not an image."
                    },
                )));
            }
            let url = drive::get_public_url(&state.config.url, &file, is_avatar);
            Ok(mk_db::user::AvatarBannerOp::Set {
                id: file.id.clone(),
                url,
                blurhash: file.blurhash.clone(),
            })
        }
        _ => Err(api_err(ApiError::invalid_param(json!({
            "param": if is_avatar { "avatarId" } else { "bannerId" },
            "reason": "expected a string or null",
        })))),
    }
}

/// `POST /api/users` — explorable-user list with the sort/state/origin matrix.
async fn users_list(state: &AppState, body: &Value, token: Option<String>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = resolve_me(pool, token.as_deref()).await;
    let sort = body.get("sort").and_then(|v| v.as_str());
    let state_p = body.get("state").and_then(|v| v.as_str());
    let origin = body
        .get("origin")
        .and_then(|v| v.as_str())
        .or(Some("local"));
    let hostname = body
        .get("hostname")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let limit = body
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(10)
        .clamp(1, 100);
    let offset = body
        .get("offset")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0);

    let rows = match mk_db::user::users_explore(
        pool,
        sort,
        state_p,
        origin,
        hostname.as_deref(),
        me_id.as_deref(),
        limit,
        offset,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_search", error = %e, "users_explore");
            return api_err(ApiError::internal_error());
        }
    };

    let packed = match pack_search_rows(state, pool, rows, me_id.as_deref(), true).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    (StatusCode::OK, Json(json!(packed))).into_response()
}

/// `POST /api/hashtags/users` — tag membership + sort/state/origin switch.
async fn hashtags_users(state: &AppState, body: &Value, token: Option<String>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = resolve_me(pool, token.as_deref()).await;
    let Some(tag) = body.get("tag").and_then(|v| v.as_str()) else {
        return api_err(ApiError::invalid_param(json!({
            "param": "tag",
            "reason": "required",
        })));
    };
    let sort = body
        .get("sort")
        .and_then(|v| v.as_str())
        .unwrap_or("+follower");
    let state_p = body.get("state").and_then(|v| v.as_str());
    let origin = body.get("origin").and_then(|v| v.as_str());
    let limit = body
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(10)
        .clamp(1, 100);
    let offset = body
        .get("offset")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0);

    let rows =
        match mk_db::user::users_by_tag(pool, tag, sort, state_p, origin, limit, offset).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(target: "dbg_search", error = %e, "users_by_tag");
                return api_err(ApiError::internal_error());
            }
        };

    let packed = match pack_search_rows(state, pool, rows, me_id.as_deref(), true).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    (StatusCode::OK, Json(json!(packed))).into_response()
}

/// Shared result packing for user-search endpoints (upstream packs with the
/// requesting me; schema 'UserDetailed' by default, 'UserLite' when
/// `detail: false`). gate-roles: thread each target's assigned roles and the
/// viewer-moderator flag into the pack.
async fn pack_search_rows(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    rows: Vec<mk_db::user::LocalUserRow>,
    me_id: Option<&str>,
    detail: bool,
) -> Result<Vec<Value>, Box<Response>> {
    let ctx = mk_core::services::user_pack::InstanceCtx {
        url: &state.config.url,
        host: &state.config.host(),
    };
    let viewer_mod = viewer_is_moderator(pool, me_id).await;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let is_me = me_id == Some(row.id.as_str());
        let profile = match mk_db::user::find_profile(pool, &row.id).await {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(target: "dbg_search", error = %e, "find_profile");
                return Err(Box::new(api_err(ApiError::internal_error())));
            }
        };
        if !detail {
            // schema 'UserLite': badge roles only, no relations
            let assigned = match mk_db::role::user_assigned_roles(pool, &row.id).await {
                Ok(a) => a,
                Err(e) => {
                    tracing::error!(target: "dbg_search", error = %e, "user_assigned_roles");
                    return Err(Box::new(api_err(ApiError::internal_error())));
                }
            };
            let role_pack = mk_core::services::user_pack::RolePack {
                assigned: &assigned,
                target_is_root: row.username_lower == "root",
                viewer_is_moderator: viewer_mod,
            };
            out.push(mk_core::services::user_pack::user_lite_value_with_roles(
                row,
                ctx,
                Some(&role_pack),
            ));
            continue;
        }
        let relation = match build_relation(pool, me_id, &row.id, is_me).await {
            Ok(r) => r,
            Err(resp) => return Err(resp),
        };
        let assigned = match mk_db::role::user_assigned_roles(pool, &row.id).await {
            Ok(a) => a,
            Err(e) => {
                tracing::error!(target: "dbg_search", error = %e, "user_assigned_roles");
                return Err(Box::new(api_err(ApiError::internal_error())));
            }
        };
        let role_pack = mk_core::services::user_pack::RolePack {
            assigned: &assigned,
            target_is_root: row.username_lower == "root",
            viewer_is_moderator: viewer_mod,
        };
        let mut packed = mk_core::services::user_pack::pack_inline_with_roles(
            row,
            &profile,
            is_me,
            relation.as_ref(),
            ctx,
            false,
            Some(&role_pack),
        );
        // Parity with the users/show arm: the memo column (viewer memo on the
        // target) and the Me-role compat keys apply the same way here — the
        // e2e compares list rows and hashtags rows against users/show output.
        set_viewers_memo(pool, me_id, &row.id, &mut packed).await;
        if is_me {
            apply_me_role_compat(&mut packed);
        }
        out.push(packed);
    }
    Ok(out)
}

/// `POST /api/users/search-by-username-and-host`
async fn users_search_by_username_and_host(
    state: &AppState,
    body: &Value,
    token: Option<String>,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = resolve_me(pool, token.as_deref()).await;

    let username = body.get("username").and_then(|v| v.as_str());
    let host = body
        .get("host")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let detail = body.get("detail").and_then(|v| v.as_bool()).unwrap_or(true);
    let limit = body
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(10)
        .clamp(1, 100);
    let own_host = state.config.host();

    let rows = match mk_db::user::search_users_by_username_and_host(
        pool,
        username,
        host.as_deref(),
        &own_host,
        me_id.as_deref(),
        limit,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_search", error = %e, "search_username_host");
            return api_err(ApiError::internal_error());
        }
    };

    let packed = match pack_search_rows(state, pool, rows, me_id.as_deref(), detail).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    (StatusCode::OK, Json(json!(packed))).into_response()
}

/// `POST /api/users/search`
async fn users_search(state: &AppState, body: &Value, token: Option<String>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = resolve_me(pool, token.as_deref()).await;

    let Some(query) = body.get("query").and_then(|v| v.as_str()) else {
        return api_err(ApiError::invalid_param(json!({
            "param": "query",
            "reason": "required",
        })));
    };
    if query.is_empty() || query.chars().count() > 100 {
        return api_err(ApiError::invalid_param(json!({
            "param": "query",
            "reason": "1..100 chars",
        })));
    }
    let limit = body
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(10)
        .clamp(1, 100);
    let offset = body
        .get("offset")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0);
    let origin = body.get("origin").and_then(|v| v.as_str());
    // paramDef default `detail: true` — false flips the pack to 'UserLite'
    // (e2e "を検索することができる(UserLite)").
    let detail = body.get("detail").and_then(|v| v.as_bool()).unwrap_or(true);

    let rows = match mk_db::user::search_users(pool, query, me_id.as_deref(), origin, limit, offset)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_search", error = %e, "search_users");
            return api_err(ApiError::internal_error());
        }
    };

    let packed = match pack_search_rows(state, pool, rows, me_id.as_deref(), detail).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    (StatusCode::OK, Json(json!(packed))).into_response()
}

/// Shared note-packing for the M1 slice: user row + profile → UserLite value.
async fn note_user_lite(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    user_id: &str,
) -> Result<Value, Response> {
    let row = match mk_db::user::find_user_by_id(pool, user_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "note find_user_by_id");
            return Err(api_err(ApiError::internal_error()));
        }
    };
    let Some(row) = row else {
        return Err(api_err(ApiError::new(
            404,
            mk_types::api_error::ApiErrorKind::Client,
            "NO_SUCH_USER",
            "4362f8dc-731f-4ad8-a694-be5a88922a24",
            "No such user.",
        )));
    };
    let profile = match mk_db::user::find_profile(pool, user_id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "note find_profile");
            return Err(api_err(ApiError::internal_error()));
        }
    };
    let ctx = mk_core::services::user_pack::InstanceCtx {
        url: &state.config.url,
        host: &state.config.host(),
    };
    Ok(mk_core::services::user_pack::user_lite_value(
        &row, &profile, ctx,
    ))
}

/// `POST /api/notes/create` — upstream `endpoints/notes/create.ts` +
/// `NoteCreateService.fetchAndCreate/create` (note.ts acceptance ladder):
/// paramDef validation, owned-file check (NO_SUCH_FILE on count mismatch,
/// `misskey:id` = plain alnum so malformed ids fall through to the service
/// check), reply-target ladder (NO_SUCH_REPLY_TARGET → pure-renote rejection
/// → invisible-note → specified-visibility extension → block), poll expiry,
/// sensitiveWords force-home + prohibitedWords rejection, mention
/// resolution to user ids + mentionLimit policy, repliesCount bump.
async fn notes_create(state: &AppState, body: &Value, token: Option<String>) -> Response {
    // single request-time clock (upstream computes expiresAt from one
    // Date.now() at the endpoint, L230): relative poll expiries never race
    // the already-expired guard, absolute ones still reject below
    let request_ms = now_ms() as i64;
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let Some(me_id) = resolve_me(pool, token.as_deref()).await else {
        return api_err(ApiError::new(
            401,
            mk_types::api_error::ApiErrorKind::Client,
            "AUTHENTICATION_FAILED",
            "b0a7f5f8-dc2f-4171-b91f-de88ad238e14",
            "Authentication failed. Please ensure your token is correct.",
        ));
    };

    // social-agent: renote path — upstream notes/create renoteId param with
    // NoteCreateService renote rules. Upstream trims text inside create()
    // before isQuote is evaluated, so whitespace-only text on a quote degrades
    // to a pure renote with text: null — normalized on the (cloned) body here
    // since the renote arm re-reads it.
    if let Some(rid) = body.get("renoteId").and_then(|v| v.as_str()) {
        if !is_alnum_id(rid) {
            return invalid_param("renoteId", "must be an alphanumeric id");
        }
        let blank_text = matches!(body.get("text"), Some(Value::String(t)) if t.trim().is_empty());
        if blank_text {
            let mut normalized = body.clone();
            normalized["text"] = Value::Null;
            return social::notes_create_renote(state, &normalized, &me_id).await;
        }
        return social::notes_create_renote(state, body, &me_id).await;
    }

    // ---- paramDef (endpoints/notes/create.ts) ----
    let visibility = match body.get("visibility").and_then(|v| v.as_str()) {
        None | Some("public") => "public",
        Some(v @ ("home" | "followers" | "specified")) => v,
        Some(_) => {
            return invalid_param(
                "visibility",
                "must be one of public/home/followers/specified",
            );
        }
    };
    let local_only = match body.get("localOnly") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return invalid_param("localOnly", "must be a boolean"),
    };
    // reactionAcceptance is validated (enum) and carried by the row column
    // once the insert grows the binding; paramDef validation happens here.
    let _reaction_acceptance = match body.get("reactionAcceptance").and_then(|v| v.as_str()) {
        None => None,
        Some(
            v @ ("likeOnly"
            | "likeOnlyForRemote"
            | "nonSensitiveOnly"
            | "nonSensitiveOnlyForLocalLikeOnlyForRemote"),
        ) => Some(v.to_string()),
        Some(_) => return invalid_param("reactionAcceptance", "invalid enum value"),
    };
    let cw = match body.get("cw") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if (1..=100).contains(&s.chars().count()) => Some(s.as_str()),
        Some(_) => return invalid_param("cw", "must be a string of 1..100 chars"),
    };
    // text: nullable, 1..=3000 — blank-but-nonempty strings pass the schema
    // and trim to null inside NoteCreateService.create.
    let raw_text = match body.get("text") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if (1..=3000).contains(&s.chars().count()) => Some(s.as_str()),
        Some(_) => {
            return invalid_param("text", "must be a string of 1..3000 chars");
        }
    };
    let reply_id = match body.get("replyId") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if is_alnum_id(s) => Some(s.as_str()),
        Some(_) => return invalid_param("replyId", "must be an alphanumeric id"),
    };
    // chan-note2: channelId param (create.ts paramDef L144 — a nullable
    // `misskey:id` string, so malformed ids fall through to the service
    // existence check like the other id params).
    let channel_id_in: Option<String> = match body.get("channelId") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if is_alnum_id(s) => Some(s.to_string()),
        Some(_) => return invalid_param("channelId", "must be an alphanumeric id"),
    };
    let visible_user_ids_in: Vec<String> = match body.get("visibleUserIds") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => {
            let mut out = Vec::with_capacity(a.len());
            for v in a {
                match v.as_str() {
                    Some(s) if is_alnum_id(s) && !out.contains(&s.to_string()) => {
                        out.push(s.to_string())
                    }
                    _ => return invalid_param("visibleUserIds", "must be unique alphanumeric ids"),
                }
            }
            out
        }
        Some(_) => return invalid_param("visibleUserIds", "must be an array of ids"),
    };
    let file_ids: Vec<String> = match body.get("fileIds").or_else(|| body.get("mediaIds")) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => {
            if a.is_empty() || a.len() > 16 {
                return invalid_param("fileIds", "must have 1..16 items");
            }
            let mut out: Vec<String> = Vec::with_capacity(a.len());
            for v in a {
                match v.as_str() {
                    Some(s) if is_alnum_id(s) => {
                        if out.contains(&s.to_string()) {
                            return invalid_param("fileIds", "must be unique");
                        }
                        out.push(s.to_string());
                    }
                    _ => return invalid_param("fileIds", "items must be alphanumeric ids"),
                }
            }
            out
        }
        Some(_) => return invalid_param("fileIds", "must be an array of ids"),
    };
    struct PollInput {
        choices: Vec<String>,
        multiple: bool,
        expires_at_ms: Option<i64>,
    }
    let poll: Option<PollInput> = match body.get("poll") {
        None | Some(Value::Null) => None,
        Some(Value::Object(o)) => {
            let Some(Value::Array(choices_raw)) = o.get("choices") else {
                return invalid_param("poll.choices", "required");
            };
            if !(2..=10).contains(&choices_raw.len()) {
                return invalid_param("poll.choices", "must have 2..10 items");
            }
            let mut choices: Vec<String> = Vec::with_capacity(choices_raw.len());
            for v in choices_raw {
                match v.as_str() {
                    Some(c) if (1..=50).contains(&c.chars().count()) => {
                        if choices.contains(&c.to_string()) {
                            return invalid_param("poll.choices", "must be unique");
                        }
                        choices.push(c.to_string());
                    }
                    _ => return invalid_param("poll.choices", "must be 1..50 char strings"),
                }
            }
            let multiple = match o.get("multiple") {
                None | Some(Value::Null) => false,
                Some(Value::Bool(b)) => *b,
                Some(_) => return invalid_param("poll.multiple", "must be a boolean"),
            };
            let expires_at = match o.get("expiresAt") {
                None | Some(Value::Null) => None,
                Some(Value::Number(n)) => n.as_i64(),
                Some(_) => return invalid_param("poll.expiresAt", "must be an integer"),
            };
            let expired_after = match o.get("expiredAfter") {
                None | Some(Value::Null) => None,
                Some(Value::Number(n)) => match n.as_i64() {
                    Some(ms) if ms >= 1 => Some(ms),
                    _ => return invalid_param("poll.expiredAfter", "must be >= 1"),
                },
                Some(_) => return invalid_param("poll.expiredAfter", "must be an integer"),
            };
            let expires_at_ms = match (expires_at, expired_after) {
                (_, Some(ms)) => Some(request_ms + ms),
                (e, None) => e,
            };
            Some(PollInput {
                choices,
                multiple,
                expires_at_ms,
            })
        }
        Some(_) => return invalid_param("poll", "must be an object"),
    };

    // (re)note with text, files and poll are optional (schema if/then):
    // everything else requires a non-blank text — blank strings that pass the
    // schema trim to null inside create() instead.
    if raw_text.is_none() && file_ids.is_empty() && poll.is_none() {
        return invalid_param("text", "required");
    }

    // ---- fetchAndCreate ----
    // author host (mention fallback `m.host ?? user.host`).
    let me_host = match mk_db::note::user_host_of(pool, &me_id).await {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "user_host_of");
            return api_err(ApiError::internal_error());
        }
    };

    // files: owned + existing rows (upstream orders by array_position; the
    // count-mismatch IS the NO_SUCH_FILE check).
    let files = if file_ids.is_empty() {
        Vec::new()
    } else {
        match mk_db::drive_note::fetch_owned_files(pool, &me_id, &file_ids).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(target: "dbg_notes", error = %e, "fetch_owned_files");
                return api_err(ApiError::internal_error());
            }
        }
    };
    if files.len() != file_ids.len() {
        return api_err(ApiError::new(
            400,
            mk_types::api_error::ApiErrorKind::Client,
            "NO_SUCH_FILE",
            "b6992544-63e7-67f0-fa7f-32444b1b5306",
            "Some files are not found.",
        ));
    }

    // reply target ladder.
    let reply: Option<mk_db::note::NoteRow> = match reply_id {
        Some(rid) => {
            let target = match mk_db::note::find_note_by_id(pool, rid).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(target: "dbg_notes", error = %e, "find_note_by_id[reply]");
                    return api_err(ApiError::internal_error());
                }
            };
            let Some(reply_row) = target else {
                return api_err(ApiError::new(
                    400,
                    mk_types::api_error::ApiErrorKind::Client,
                    "NO_SUCH_REPLY_TARGET",
                    "60142edb-1519-408e-926d-4f108d27bee0",
                    "No such reply target.",
                ));
            };
            // reply to a pure renote
            let reply_is_quote =
                reply_row.text.is_some() || !reply_row.file_ids.is_empty() || reply_row.has_poll;
            if reply_row.renote_id.is_some() && !reply_is_quote {
                return api_err(ApiError::new(
                    400,
                    mk_types::api_error::ApiErrorKind::Client,
                    "CANNOT_REPLY_TO_A_PURE_RENOTE",
                    "f089e4e2-c0e7-4f60-8a23-e5a6bf786b36",
                    "You can not reply to a pure Renote.",
                ));
            }
            let visible = match note_is_visible_for_me(pool, &reply_row, &me_id).await {
                Ok(v) => v,
                Err(r) => return r,
            };
            if !visible {
                return api_err(ApiError::new(
                    400,
                    mk_types::api_error::ApiErrorKind::Client,
                    "CANNOT_REPLY_TO_AN_INVISIBLE_NOTE",
                    "11cd37b3-a411-4f77-8633-c580ce6a8dce",
                    "You cannot reply to an invisible Note.",
                ));
            }
            if reply_row.visibility == "specified" && visibility != "specified" {
                return api_err(ApiError::new(
                    400,
                    mk_types::api_error::ApiErrorKind::Client,
                    "CANNOT_REPLY_TO_SPECIFIED_VISIBILITY_NOTE_WITH_EXTENDED_VISIBILITY",
                    "ced780a1-2012-4caf-bc7e-a95a291294cb",
                    "You cannot reply to a specified visibility note with extended visibility.",
                ));
            }
            // blocking (blocker = reply target author) — upstream
            // NoteCreateService.fetchAndCreate L391-401 throws IdentifiableError
            // b0df6025 and the notes/create endpoint catch-arm (create.ts
            // L280-282) remaps it to meta.errors.youHaveBeenBlocked; the pure
            // RENOTE direction is the separate checkBlock path (L338-347,
            // IdentifiableError 2b4fe776 remapped at create.ts L262-263). Both
            // surfaces resolve to the same meta id the client sees.
            if reply_row.user_id != me_id {
                match mk_db::social::blocking_exists(pool, &reply_row.user_id, &me_id).await {
                    Ok(true) => {
                        return api_err(ApiError::new(
                            400,
                            mk_types::api_error::ApiErrorKind::Client,
                            "YOU_HAVE_BEEN_BLOCKED",
                            "b390d7e1-8a5e-46ed-b625-06271cafd3d3",
                            "You have been blocked by this user.",
                        ));
                    }
                    Ok(false) => {}
                    Err(e) => {
                        tracing::error!(target: "dbg_notes", error = %e, "blocking_exists[reply]");
                        return api_err(ApiError::internal_error());
                    }
                }
            }
            Some(reply_row)
        }
        None => None,
    };

    // poll expiry (CANNOT_CREATE_ALREADY_EXPIRED_POLL) against the request
    // clock: absolute past dates reject, relative expiredAfter (now+ms) can
    // never start expired (upstream create.ts L230 + NoteCreateService
    // L406-408 single-tick semantics)
    if let Some(p) = &poll {
        if let Some(exp) = p.expires_at_ms {
            if exp < request_ms {
                return api_err(ApiError::new(
                    400,
                    mk_types::api_error::ApiErrorKind::Client,
                    "CANNOT_CREATE_ALREADY_EXPIRED_POLL",
                    "04da457d-b083-4055-9082-955525eda5a5",
                    "Poll is already expired.",
                ));
            }
        }
    }

    // chan-note2: fetchAndCreate channel leg (NoteCreateService L413-420) —
    // findOneBy({ id, isArchived: false }); an archived channel resolves the
    // same NO_SUCH_CHANNEL as a missing one (create.ts meta.errors
    // .noSuchChannel, thrown off IdentifiableError bfa3905b).
    if let Some(cid) = &channel_id_in {
        match mk_db::channel::find_channel(pool, cid).await {
            Ok(Some(c)) if !c.is_archived => {}
            Ok(_) => {
                return api_err(ApiError::new(
                    400,
                    mk_types::api_error::ApiErrorKind::Client,
                    "NO_SUCH_CHANNEL",
                    "b1653923-5453-4edc-b786-7c4f39bb0bbb",
                    "No such channel.",
                ));
            }
            Err(e) => {
                tracing::error!(target: "dbg_notes", error = %e, "find_channel");
                return api_err(ApiError::internal_error());
            }
        }
    }

    // ---- create() ----
    let mut text: Option<String> = raw_text.map(|t| t.trim().to_string());
    if let Some(t) = &text {
        if t.is_empty() {
            text = None;
        }
    }

    // chan-note2: create() channel scope/force legs (NoteCreateService
    // L449-470). A reply re-scopes the note into the reply target's channel —
    // the L451-457 (out-of-channel reply) and L461-463 (into-channel reply)
    // branches both reduce to "the reply's channelId wins" (an equal-id
    // payload channel never survives the existence check above). Any channel
    // note is forced public (L468) with its visibleUsers dropped (L469) and
    // localOnly on (L470); the sensitiveWords/canPublicNote home downgrade
    // below keeps the upstream `channel == null` gate (L472).
    let mut channel_id = channel_id_in;
    if let Some(reply_row) = &reply {
        channel_id = reply_row.channel_id.clone();
    }
    let visible_user_ids_in = if channel_id.is_some() {
        Vec::new()
    } else {
        visible_user_ids_in
    };

    // assigned-role policies (mentionLimit / canPublicNote).
    let assigned = match mk_db::role::user_assigned_roles(pool, &me_id).await {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "user_assigned_roles");
            return api_err(ApiError::internal_error());
        }
    };
    let policy = mk_core::services::role::UserPolicy::compute(&assigned);

    let (sensitive_words, prohibited_words) =
        match mk_db::meta::note_content_moderation_words(pool).await {
            Ok((s, p, _)) => (s, p),
            Err(e) => {
                tracing::error!(target: "dbg_notes", error = %e, "note_content_moderation_words");
                return api_err(ApiError::internal_error());
            }
        };

    // chan-note2: L468 — a channel note is always public, and the
    // sensitiveWords/canPublicNote force-home only ever applies to
    // non-channel notes (L472 `channel == null`).
    let mut visibility = if channel_id.is_some() {
        "public".to_string()
    } else {
        visibility.to_string()
    };
    if visibility == "public" && channel_id.is_none() {
        let cw_or_text = cw.or(raw_text).unwrap_or("");
        if mk_core::services::misc::is_keyword_included(cw_or_text, &sensitive_words) {
            visibility = "home".to_string();
        } else if !policy.can_public_note {
            visibility = "home".to_string();
        }
    }

    // prohibited words: cw+text joined, then the poll choices (concat helper).
    let prohibited_content = mk_core::services::misc::concat_note_contents_for_keyword_check(
        cw,
        raw_text,
        poll.as_ref().map(|p| p.choices.as_slice()).unwrap_or(&[]),
    );
    if mk_core::services::misc::is_keyword_included(&prohibited_content, &prohibited_words) {
        return api_err(ApiError::new(
            400,
            mk_types::api_error::ApiErrorKind::Client,
            "CONTAINS_PROHIBITED_WORDS",
            "aa6e01d3-a85c-669d-758a-76aab43af334",
            "Cannot post because it contains prohibited words.",
        ));
    }

    // reply visibility cascade (NoteCreateService data.reply switch).
    // chan-note2: L470 forces localOnly on channel notes; the reply localOnly
    // leg carries the upstream `channel == null` gate (L568).
    let mut local_only = local_only || channel_id.is_some();
    if let Some(reply_row) = &reply {
        match reply_row.visibility.as_str() {
            "home" if visibility == "public" => visibility = "home".to_string(),
            "followers" if visibility == "public" || visibility == "home" => {
                visibility = "followers".to_string();
            }
            _ => {}
        }
        // ローカルのみにリプライしたらローカルのみにする
        if reply_row.local_only && channel_id.is_none() {
            local_only = true;
        }
    }

    // mentions: text + cw + poll choices tokens, resolved to user ids
    // (resolveUser(m.username, m.host ?? user.host)), deduped in order.
    let mut mention_sources = String::new();
    if let Some(t) = raw_text {
        mention_sources.push_str(t);
    }
    if let Some(c) = cw {
        mention_sources.push_str(c);
    }
    if let Some(p) = &poll {
        for c in &p.choices {
            mention_sources.push_str(c);
        }
    }
    let mut mentioned_user_ids: Vec<String> = Vec::new();
    for (username, host) in mk_core::services::misc::extract_mentions(&mention_sources) {
        let host = host.or_else(|| me_host.clone());
        let id = match host.as_deref() {
            None => mk_db::user::find_local_user_by_username(pool, &username.to_lowercase())
                .await
                .ok()
                .flatten()
                .map(|u| u.id),
            Some(h) => mk_db::freq::find_user_id_by_acct(pool, &username.to_lowercase(), Some(h))
                .await
                .ok()
                .flatten(),
        };
        if let Some(id) = id {
            if !mentioned_user_ids.contains(&id) {
                mentioned_user_ids.push(id);
            }
        }
    }
    // reply author rides the mentions when the author is not the replier.
    if let Some(reply_row) = &reply {
        if reply_row.user_id != me_id && !mentioned_user_ids.contains(&reply_row.user_id) {
            mentioned_user_ids.push(reply_row.user_id.clone());
        }
    }

    // visibleUsers: resolved subset of visibleUserIds; specified notes merge
    // them into the mentions and pull the reply author into the targets.
    let mut visible_user_ids: Vec<String> = Vec::new();
    for uid in &visible_user_ids_in {
        match mk_db::user::find_user_by_id(pool, uid).await {
            Ok(Some(_)) => {
                if !mentioned_user_ids.contains(uid) {
                    mentioned_user_ids.push(uid.clone());
                }
                if !visible_user_ids.contains(uid) {
                    visible_user_ids.push(uid.clone());
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!(target: "dbg_notes", error = %e, "find_user_by_id[visible]");
                return api_err(ApiError::internal_error());
            }
        }
    }
    if visibility == "specified" {
        if let Some(reply_row) = &reply {
            if !visible_user_ids.contains(&reply_row.user_id) {
                visible_user_ids.push(reply_row.user_id.clone());
            }
        }
    }

    // mentionLimit policy (effectiveMentionCount > 0 && > mentionLimit).
    if !mentioned_user_ids.is_empty() && mentioned_user_ids.len() as i64 > policy.mention_limit {
        return api_err(ApiError::new(
            400,
            mk_types::api_error::ApiErrorKind::Client,
            "CONTAINS_TOO_MANY_MENTIONS",
            "4de0363a-3046-481b-9b0f-feff3e211025",
            "Cannot post because it exceeds the allowed number of mentions.",
        ));
    }

    let hashtags = mk_core::services::misc::extract_hashtags(&text.clone().unwrap_or_default());

    // ---- insertNote (+ poll, repliesCount bump) ----
    let id = mk_types::id::gen_aid_now();
    // upstream `threadId = reply.threadId ?? reply.id` (NoteCreateService):
    // reply chains keep the ROOT id, one-level replies take the parent id.
    let (reply_user_id, reply_user_host, thread_id) = match &reply {
        Some(r) => (
            Some(r.user_id.clone()),
            r.user_host.clone(),
            Some(r.thread_id.clone().unwrap_or_else(|| r.id.clone())),
        ),
        None => (None, None, None),
    };
    let attached_types: Vec<String> = files.iter().map(|f| f.file_type.clone()).collect();
    let no_visible: Vec<String> = Vec::new();
    if let Err(e) = mk_db::note::insert_note(
        pool,
        &mk_db::note::NewNote {
            id: &id,
            text: text.as_deref(),
            cw,
            user_id: &me_id,
            visibility: &visibility,
            reply_id,
            reply_user_id: reply_user_id.as_deref(),
            reply_user_host: reply_user_host.as_deref(),
            thread_id: thread_id.as_deref(),
            mentions: &mentioned_user_ids,
            hashtags: &hashtags,
            file_ids: &file_ids,
            attached_file_types: &attached_types,
            visible_user_ids: if visibility == "specified" {
                &visible_user_ids
            } else {
                &no_visible
            },
            has_poll: poll.is_some(),
            local_only,
            // chan-note2
            channel_id: channel_id.as_deref(),
        },
    )
    .await
    {
        tracing::error!(target: "dbg_notes", error = %e, "insert_note");
        return api_err(ApiError::internal_error());
    }
    // chan-note2: channel counters (NoteCreateService L927-943) — notesCount
    // +1 with the lastNotedAt touch, usersCount +1 when this is the author's
    // first note in the channel.
    if let Some(cid) = &channel_id {
        if let Err(e) = mk_db::channel::count_note_in_channel(pool, cid, &me_id).await {
            tracing::error!(target: "dbg_notes", error = %e, "count_note_in_channel");
        }
    }
    if let Some(p) = &poll {
        if let Err(e) = mk_db::poll::insert_poll(
            pool,
            &id,
            &p.choices,
            p.multiple,
            p.expires_at_ms,
            &visibility,
            &me_id,
        )
        .await
        {
            tracing::error!(target: "dbg_notes", error = %e, "insert_poll");
            return api_err(ApiError::internal_error());
        }
    }
    if let Some(rid) = reply_id {
        if let Err(e) = mk_db::note::bump_replies_count(pool, rid, 1).await {
            tracing::error!(target: "dbg_notes", error = %e, "bump_replies_count");
        }
    }
    if let Err(e) = mk_db::note::credit_note_to_user(pool, &me_id, &hashtags).await {
        tracing::error!(target: "dbg_notes", error = %e, "credit_note_to_user");
    }

    // ftt2: NoteCreateService.pushToTl (create() L779 — right after
    // incNotesCountOfUser; body L1068) — the upstream `FTT:` probes read the
    // `list:homeTimeline:*` Redis lists literally; the flag gate is
    // meta."enableFanoutTimeline" (timelines.ts L104 arm switch). Best-effort:
    // never fails the note creation.
    ftt::push_to_tl(
        state,
        pool,
        &id,
        &me_id,
        me_host.as_deref(),
        &visibility,
        &visible_user_ids,
        reply.as_ref().map(|r| r.user_id.as_str()),
        file_ids.len(),
        channel_id.as_deref(),
    )
    .await;

    // NoteCreateService non-AP notify branch (create() L845-869 +
    // createMentionedEvents L991-1035): the reply-target author and every
    // mentioned local user run through the NotificationManager queue — at
    // most ONE notification per notifiee, and a queued 'mention' is
    // upgraded to 'reply' (push() L88-101), so both surfaces collapse to a
    // single 'reply' row. Thread muting gates BOTH legs BEFORE the queue:
    // the reply leg on `data.reply.threadId ?? data.reply.id` (L856) and the
    // mention leg on `note.threadId ?? note.id` (L993) — the same key (the
    // new row's threadId IS the parent's). Muting/self drops ride the
    // notification helper (NotificationService.createNotification — the
    // muting gate is type-agnostic, NotificationService.ts L111-114).
    let notify_thread_key = thread_id.clone().unwrap_or_else(|| id.clone());
    let mut notify_queue: Vec<(String, &'static str)> = Vec::new();
    if let Some(rid) = reply_id {
        if let Ok(Some(target)) = mk_db::note::find_note_by_id(pool, rid).await {
            match mk_db::note::thread_muted(pool, &target.user_id, &notify_thread_key).await {
                Ok(false) => notify_queue.push((target.user_id.clone(), "reply")),
                Ok(true) => {}
                Err(e) => {
                    tracing::error!(target: "dbg_notes", error = %e, "thread_muted[reply-leg]");
                }
            }
        }
    }
    for uid in &mentioned_user_ids {
        if uid == &me_id {
            continue;
        }
        match mk_db::note::thread_muted(pool, uid, &notify_thread_key).await {
            Ok(false) => {
                if !notify_queue.iter().any(|(u, _)| u == uid) {
                    notify_queue.push((uid.clone(), "mention"));
                }
            }
            Ok(true) => {}
            Err(e) => {
                tracing::error!(target: "dbg_notes", error = %e, "thread_muted[mention-leg]");
            }
        }
    }
    for (notifiee, reason) in &notify_queue {
        social::notify_note_event(pool, notifiee, &me_id, reason, &id).await;
    }

    let note = match pack_note_for(state, pool, &id, Some(&me_id)).await {
        Ok(n) => n,
        Err(r) => return *r,
    };
    // stream-rtm: NoteCreateService.create L842 — `publishNotesStream(noteObj)`
    // fans the pack out to the /streaming timeline channels (api::streaming).
    crate::api::streaming::publish_notes(pool, &note).await;
    (StatusCode::OK, Json(json!({ "createdNote": note }))).into_response()
}

/// `NoteEntityService.isVisibleForMe` (author / specified-listed /
/// replied-to-me / mentioned / follower ladder; the remote-follow fallback
/// the e2e never exercises stays out).
pub(crate) async fn note_is_visible_for_me(
    pool: &mk_db::sqlx::PgPool,
    note: &mk_db::note::NoteRow,
    me_id: &str,
) -> Result<bool, Response> {
    match note.visibility.as_str() {
        "specified" => {
            Ok(note.user_id == me_id || note.visible_user_ids.iter().any(|v| v == me_id))
        }
        "followers" => {
            if note.user_id == me_id
                || note.reply_user_id.as_deref() == Some(me_id)
                || note.mentions.iter().any(|m| m == me_id)
            {
                return Ok(true);
            }
            let followees = match mk_db::social::followee_ids(pool, me_id).await {
                Ok(f) => f,
                Err(e) => {
                    tracing::error!(target: "dbg_notes", error = %e, "followee_ids");
                    return Err(api_err(ApiError::internal_error()));
                }
            };
            Ok(followees.iter().any(|f| f == &note.user_id))
        }
        _ => Ok(true),
    }
}

/// ajv `misskey:id` format (`/^[a-zA-Z0-9]+$/` — endpoint-base.ts L20).
pub(crate) fn is_alnum_id(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Note-pack pipeline shared by every note-shaped endpoint
/// (`NoteEntityService.pack(note, me, { detail })`): the row + author lite
/// with the db follow-throughs — `files` (by `fileIds`, aid-derived
/// createdAt), `poll` (Detail), nested `reply` (packed detail:false) /
/// `renote` (packed detail:true), `emojis` (remote-only `{}` per
/// populateEmoji's local-host null), and `myReaction` for the self-view.
/// Ok(None) = row missing (top level maps to NO_SUCH_NOTE).
/// The reply/renote recursion needs indirection: boxed future via the
/// `load_pack_note` wrapper (E0733), body lives in `load_pack_note_inner`.
/// Named pinned lifetime (multi-ref args defeat `'_` elision, E0106) and
/// `+ Send` (endpoint handlers are tokio-spawned by axum).
fn load_pack_note<'a>(
    state: &'a AppState,
    pool: &'a mk_db::sqlx::PgPool,
    note_id: &'a str,
    me_id: Option<&'a str>,
    detail: bool,
    depth: u32,
) -> std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<Option<Value>, Box<Response>>>
            + std::marker::Send
            + 'a,
    >,
> {
    Box::pin(load_pack_note_inner(
        state, pool, note_id, me_id, detail, depth,
    ))
}

async fn load_pack_note_inner(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    note_id: &str,
    me_id: Option<&str>,
    detail: bool,
    depth: u32,
) -> Result<Option<Value>, Box<Response>> {
    const MAX_PACK_DEPTH: u32 = 8;
    let row = match mk_db::note::find_note_by_id(pool, note_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "find_note_by_id");
            return Err(Box::new(api_err(ApiError::internal_error())));
        }
    };
    let Some(row) = row else {
        return Ok(None);
    };
    let user_lite = match note_user_lite(state, pool, &row.user_id).await {
        Ok(v) => v,
        Err(r) => return Err(Box::new(r)),
    };
    let created_at = mk_types::id::parse_aid_date(&row.id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default();
    // populateEmojis resolves against (name, noteUserHost); local notes pass
    // host null → upstream keeps the field `undefined`; remote authors resolve
    // nothing against the (empty) local emoji table → `{}`.
    let emojis = row.user_host.is_some().then(|| json!({}));

    // files: DriveFile pack (detail:false) in fileIds order.
    let mut files: Vec<Value> = Vec::with_capacity(row.file_ids.len());
    if !row.file_ids.is_empty() {
        let frows =
            match mk_db::drive_note::fetch_owned_files(pool, &row.user_id, &row.file_ids).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(target: "dbg_notes", error = %e, "fetch_owned_files");
                    return Err(Box::new(api_err(ApiError::internal_error())));
                }
            };
        for fid in &row.file_ids {
            if let Some(f) = frows.iter().find(|f| &f.id == fid) {
                let f_created = mk_types::id::parse_aid_date(&f.id)
                    .map(mk_core::services::signup::ms_to_iso)
                    .unwrap_or_default();
                files.push(mk_core::packers::note::drive_file_pack(f, f_created));
            }
        }
    }

    // poll (Detail): populatePoll with the viewer's votes.
    let poll = if detail && depth < MAX_PACK_DEPTH && row.has_poll {
        match mk_db::poll::find_poll_by_note(pool, &row.id).await {
            Ok(p) => p.map(|p| {
                let expires = p.expires_at_ms.map(mk_core::services::signup::ms_to_iso);
                (p, expires)
            }),
            Err(e) => {
                tracing::error!(target: "dbg_notes", error = %e, "find_poll_by_note");
                return Err(Box::new(api_err(ApiError::internal_error())));
            }
        }
    } else {
        None
    };

    // reply (detail:false recursion) / renote (detail:true recursion):
    // JOINed-but-missing rows pack as null upstream.
    let (reply, renote) = if detail && depth < MAX_PACK_DEPTH {
        // async fn 自再帰は Box 化が必要 (E0733) — nested packs box the future.
        let reply = if let Some(rid) = &row.reply_id {
            match Box::pin(load_pack_note(state, pool, rid, me_id, false, depth + 1)).await? {
                Some(v) => Some(v),
                None => Some(Value::Null),
            }
        } else {
            None
        };
        let renote = if let Some(rid) = &row.renote_id {
            match Box::pin(load_pack_note(state, pool, rid, me_id, true, depth + 1)).await? {
                Some(v) => Some(v),
                None => Some(Value::Null),
            }
        } else {
            None
        };
        (reply, renote)
    } else {
        (None, None)
    };

    // myReaction: Detail + me + non-empty reactions only (populateMyReaction).
    let my_reaction = if detail && depth < MAX_PACK_DEPTH {
        let reactions: Value = serde_json::from_str(&row.reactions).unwrap_or_else(|_| json!({}));
        let total: i64 = reactions
            .as_object()
            .map(|m| m.values().filter_map(|v| v.as_i64()).sum())
            .unwrap_or(0);
        if total > 0 {
            if let Some(me) = me_id {
                match mk_db::note::find_note_reaction(pool, &row.id, me).await {
                    Ok(Some(r)) => Some(json!(r.reaction)),
                    Ok(None) => None,
                    Err(e) => {
                        tracing::error!(target: "dbg_notes", error = %e, "find_note_reaction");
                        return Err(Box::new(api_err(ApiError::internal_error())));
                    }
                }
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    let poll = match (poll, me_id, detail && depth < MAX_PACK_DEPTH) {
        (Some((p, expires)), Some(me), true) => {
            let my_choices = mk_db::poll::poll_votes_of_user(pool, &row.id, me)
                .await
                .unwrap_or_default();
            Some(mk_core::packers::note::poll_pack(&p, &my_choices, expires))
        }
        (Some((p, expires)), _, true) => Some(mk_core::packers::note::poll_pack(&p, &[], expires)),
        _ => None,
    };

    Ok(Some(mk_core::packers::note::note_pack_full(
        &row,
        &mk_core::packers::note::NotePackParts {
            detail,
            user_lite,
            created_at_iso: created_at,
            emojis,
            files,
            poll,
            reply,
            renote,
            my_reaction,
        },
    )))
}

/// Viewer-less pack (notes/show + the social/freq slices that round-trip it).
async fn pack_note(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    note_id: &str,
) -> Result<Value, Box<Response>> {
    match load_pack_note(state, pool, note_id, None, true, 0).await? {
        Some(v) => Ok(v),
        None => Err(Box::new(api_err(ApiError::new(
            404,
            mk_types::api_error::ApiErrorKind::Client,
            "NO_SUCH_NOTE",
            "24fcbfc6-2e37-42b6-8388-c29b3861a08d",
            "No such note.",
        )))),
    }
}

/// Me-aware pack (poll isVoted + self-view myReaction need the viewer).
async fn pack_note_for(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    note_id: &str,
    me_id: Option<&str>,
) -> Result<Value, Box<Response>> {
    match load_pack_note(state, pool, note_id, me_id, true, 0).await? {
        Some(v) => Ok(v),
        None => Err(Box::new(api_err(ApiError::new(
            // show.ts noSuchNote has NO httpStatusCode → ApiError default 400
            400,
            mk_types::api_error::ApiErrorKind::Client,
            "NO_SUCH_NOTE",
            "24fcbfc6-2e37-42b6-8388-c29b3861a08d",
            "No such note.",
        )))),
    }
}

/// `POST /api/notes/show` — noteId lookup. Guest-ok, but a riding credential
/// packs the viewer in (populatePoll isVoted / myReaction require it; an
/// invalid riding credential still 401s through the ApiCallService auth).
async fn notes_show(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let Some(note_id) = body.get("noteId").and_then(|v| v.as_str()) else {
        return api_err(ApiError::invalid_param(json!({
            "param": "noteId",
            "reason": "required",
        })));
    };
    let me_id = match token {
        Some(t) => match mk_db::user::find_local_user_id_by_token(pool, t).await {
            Ok(Some(id)) => Some(id),
            Ok(None) => return authentication_failed(),
            Err(e) => {
                tracing::error!(target: "dbg_notes", error = %e, "find_local_user_id_by_token[show]");
                return api_err(ApiError::internal_error());
            }
        },
        None => None,
    };
    match pack_note_for(state, pool, note_id, me_id.as_deref()).await {
        Ok(n) => (StatusCode::OK, Json(n)).into_response(),
        Err(r) => *r,
    }
}

// ---------------------------------------------------------------------------
// notifications slice (M2) — truthful subset of upstream
// endpoints/i/notifications.ts (pinned upstream keeps the `i/` namespacing;
// the coordinator-facing name is `notifications`) + NotificationEntityService.
// ---------------------------------------------------------------------------

/// Optional nullable integer param (`{ type: 'integer' }`).
fn opt_int_param(body: &Value, key: &str) -> Result<Option<i64>, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_i64() {
            Some(n) => Ok(Some(n)),
            None => Err(invalid_param(key, "expected an integer")),
        },
    }
}

/// Optional nullable string param for misskey:id Cursors.
fn opt_id_param<'a>(body: &'a Value, key: &str) -> Result<Option<&'a str>, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(invalid_param(key, "expected a string")),
    }
}

/// Optional nullable string param (e.g. notes/mentions `visibility` —
/// paramDef `{ type: 'string' }`, any value, null = unset).
fn opt_str_param(body: &Value, key: &str) -> Result<Option<String>, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(invalid_param(key, "expected a string")),
    }
}

/// `POST /api/notifications` — requireCredential list + mark-as-read flow.
/// Upstream order: fetch → `readAllNotification` when `markAsRead` (default
/// true) → pack. Params: limit (1..=100, default 10), sinceId/untilId
/// (exclusive aid cursors), markAsRead, unreadOnly, includeTypes ([] → []
/// short-circuit BEFORE marking read; unknown type strings tolerated as
/// no-ops in the pinned upstream). `following` not ported (TODO(M2+):
/// needs a following-table join). Stored types outside this slice's
/// producible set (mention/reply/renote/reaction/follow) degrade to the
/// id/createdAt/type/userId/user shape.
async fn notifications_route(state: &AppState, body: &Value, token: Option<String>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };

    // requireCredential chain, mirrors i_route's call() order.
    let Some(token) = token else {
        return api_err(ApiError::new(
            401,
            mk_types::api_error::ApiErrorKind::Client,
            "CREDENTIAL_REQUIRED",
            "1384574d-a912-4b81-8601-c7b1c4085df1",
            "Credential required.",
        ));
    };
    if token.len() != 16 {
        return authentication_failed();
    }
    let Some(me_id) = resolve_me(pool, Some(token.as_str())).await else {
        return authentication_failed();
    };
    match mk_db::user::find_user_by_id(pool, &me_id).await {
        Ok(Some(row)) => {
            if row.is_suspended {
                return api_err(ApiError::new(
                    403,
                    mk_types::api_error::ApiErrorKind::Permission,
                    "YOUR_ACCOUNT_SUSPENDED",
                    "a8c724b3-6e9c-4b46-b1a8-bc3ed6258370",
                    "Your account has been suspended.",
                ));
            }
        }
        Ok(None) => return authentication_failed(),
        Err(e) => {
            tracing::error!(target: "dbg_notifications", error = %e, "find_user_by_id");
            return api_err(ApiError::internal_error());
        }
    }

    // paramDef: limit minimum 1, maximum 100, default 10.
    // Helpers return Result<_, Response>; this handler returns Response, so
    // parse in a scoped block and unwrap the first error (lease-safe: no `?`).
    let (limit, since_id, until_id, mark_as_read, unread_only) = {
        let limit = opt_int_param(body, "limit").map(|v| v.unwrap_or(10).clamp(1, 100));
        let since_id = opt_id_param(body, "sinceId").map(|v| v.map(String::from));
        let until_id = opt_id_param(body, "untilId").map(|v| v.map(String::from));
        let mark_as_read = opt_bool(body, "markAsRead").map(|v| v.unwrap_or(true));
        let unread_only = opt_bool(body, "unreadOnly").map(|v| v.unwrap_or(false));
        match (limit, since_id, until_id, mark_as_read, unread_only) {
            (Ok(l), Ok(s), Ok(u), Ok(m), Ok(r)) => (l, s, u, m, r),
            (Err(resp), _, _, _, _)
            | (_, Err(resp), _, _, _)
            | (_, _, Err(resp), _, _)
            | (_, _, _, Err(resp), _)
            | (_, _, _, _, Err(resp)) => return resp,
        }
    };
    let include_types: Vec<String> = match body.get("includeTypes") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        Some(_) => return invalid_param("includeTypes", "expected an array of strings"),
    };

    // upstream: empty includeTypes queries nothing and skips mark-as-read.
    if body
        .get("includeTypes")
        .is_some_and(|v| v.as_array().is_some_and(|a| a.is_empty()))
    {
        return (StatusCode::OK, Json(json!([]))).into_response();
    }

    let rows = match mk_db::notification::list_notifications(
        pool,
        &me_id,
        since_id.as_deref(),
        until_id.as_deref(),
        limit,
        unread_only,
        &include_types,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_notifications", error = %e, "list_notifications");
            return api_err(ApiError::internal_error());
        }
    };

    if mark_as_read {
        if let Err(e) = mk_db::notification::mark_as_read_all(pool, &me_id).await {
            tracing::error!(target: "dbg_notifications", error = %e, "mark_as_read_all");
        }
    }

    let cfg_url = state.config.url.clone();
    let cfg_host = state.config.host();

    let mut out: Vec<Value> = Vec::with_capacity(rows.len());
    for n in &rows {
        // Note-bound types: a deleted note hides the notification (upstream
        // #packInternal "if the note has been deleted, don't show this").
        let note_val = if mk_core::services::notification_pack::is_note_bound(&n.r#type) {
            let Some(note_id) = n.note_id.as_deref() else {
                continue;
            };
            let Some(nrow) = (match mk_db::note::find_note_by_id(pool, note_id).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(target: "dbg_notifications", error = %e, "find_note_by_id");
                    return api_err(ApiError::internal_error());
                }
            }) else {
                continue;
            };
            let Ok(lite) = note_user_lite(state, pool, &nrow.user_id).await else {
                // deleted author → pack null upstream → notification dropped
                continue;
            };
            let note_created = mk_types::id::parse_aid_date(&nrow.id)
                .map(mk_core::services::signup::ms_to_iso)
                .unwrap_or_default();
            Some(mk_core::packers::note::note_pack_min(
                &nrow,
                lite,
                note_created,
                &nrow.mentions,
                &nrow.tags,
                json!({}),
            ))
        } else {
            None
        };

        // Notifier: deleted or suspended users hide the notification
        // (upstream #validateNotifier: isSuspended → false).
        let lite_val = if let Some(notifier_id) = n.notifier_id.as_deref() {
            let Some(urow) = (match mk_db::user::find_user_by_id(pool, notifier_id).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(target: "dbg_notifications", error = %e, "find_user_by_id[notifier]");
                    return api_err(ApiError::internal_error());
                }
            }) else {
                continue;
            };
            if urow.is_suspended {
                continue;
            }
            let Ok(profile) = mk_db::user::find_profile(pool, notifier_id).await else {
                continue;
            };
            let ctx = mk_core::services::user_pack::InstanceCtx {
                url: &cfg_url,
                host: &cfg_host,
            };
            Some(mk_core::services::user_pack::user_lite_value(
                &urow, &profile, ctx,
            ))
        } else {
            None
        };

        let created_at = mk_types::id::parse_aid_date(&n.id)
            .map(mk_core::services::signup::ms_to_iso)
            .unwrap_or_default();
        out.push(mk_core::services::notification_pack::pack_notification(
            n, lite_val, note_val, created_at,
        ));
    }

    (StatusCode::OK, Json(json!(out))).into_response()
}

// mute lane: notes/mentions + notes/thread-muting/{create,delete}
// ---------------------------------------------------------------------------

/// requireCredential chain copied from notifications_route (mentions.ts
/// meta.requireCredential + ApiCallService call() order): no token →
/// CREDENTIAL_REQUIRED (401), unresolvable → AUTHENTICATION_FAILED,
/// suspended → YOUR_ACCOUNT_SUSPENDED (403).
async fn mute_lane_me_id(
    pool: &mk_db::sqlx::PgPool,
    token: Option<&str>,
) -> Result<String, Response> {
    let Some(token) = token else {
        return Err(api_err(ApiError::new(
            401,
            mk_types::api_error::ApiErrorKind::Client,
            "CREDENTIAL_REQUIRED",
            "1384574d-a912-4b81-8601-c7b1c4085df1",
            "Credential required.",
        )));
    };
    if token.len() != 16 {
        return Err(authentication_failed());
    }
    let Some(me_id) = resolve_me(pool, Some(token)).await else {
        return Err(authentication_failed());
    };
    match mk_db::user::find_user_by_id(pool, &me_id).await {
        Ok(Some(row)) => {
            if row.is_suspended {
                return Err(api_err(ApiError::new(
                    403,
                    mk_types::api_error::ApiErrorKind::Permission,
                    "YOUR_ACCOUNT_SUSPENDED",
                    "a8c724b3-6e9c-4b46-b1a8-bc3ed6258370",
                    "Your account has been suspended.",
                )));
            }
        }
        Ok(None) => return Err(authentication_failed()),
        Err(e) => {
            tracing::error!(target: "dbg_mentions", error = %e, "find_user_by_id");
            return Err(api_err(ApiError::internal_error()));
        }
    }
    Ok(me_id)
}

/// `POST /api/notes/mentions` (endpoints/notes/mentions.ts, requireCredential
/// read:account): self-addressed notes — `me <@ mentions ∨ me <@
/// visibleUserIds` — one aid page (sinceId-only → ASC, else DESC), each row
/// through visibility + base-filtering (muted/blocked author — the gate the
/// Mute e2e 「自分宛ての投稿」 leg reads) + the muted-note-THREAD gates
/// (QueryService.generateMutedNoteThreadQuery, which the thread-mute e2e
/// notes/mentions leg reads). Params: limit (1..=100, default 10),
/// sinceId/untilId (exclusive), visibility (exact match). Upstream's
/// `following` (followings join) and sinceDate/untilDate remain TODO(M2+).
async fn notes_mentions(state: &AppState, body: &Value, token: Option<String>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = match mute_lane_me_id(pool, token.as_deref()).await {
        Ok(m) => m,
        Err(r) => return r,
    };

    let (limit, since_id, until_id, visibility) = {
        let limit = opt_int_param(body, "limit").map(|v| v.unwrap_or(10).clamp(1, 100));
        let since_id = opt_id_param(body, "sinceId").map(|v| v.map(String::from));
        let until_id = opt_id_param(body, "untilId").map(|v| v.map(String::from));
        // paramDef: plain `{ type: 'string' }` — any string, null = unset.
        let visibility = opt_str_param(body, "visibility");
        match (limit, since_id, until_id, visibility) {
            (Ok(l), Ok(s), Ok(u), Ok(v)) => (l, s, u, v),
            (Err(r), _, _, _) | (_, Err(r), _, _) | (_, _, Err(r), _) | (_, _, _, Err(r)) => {
                return r;
            }
        }
    };

    let rows = match mk_db::timelines::mentions_notes(
        pool,
        &me_id,
        since_id.as_deref(),
        until_id.as_deref(),
        limit,
        visibility.as_deref(),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_mentions", error = %e, "mentions_notes");
            return api_err(ApiError::internal_error());
        }
    };

    // packMany(mentions, me) — the me-aware detail pack (poll isVoted /
    // myReaction ride the viewer).
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        match pack_note_for(state, pool, &row.id, Some(&me_id)).await {
            Ok(v) => out.push(v),
            Err(r) => return *r,
        }
    }
    (StatusCode::OK, Json(json!(out))).into_response()
}

/// `POST /api/notes/thread-muting/{create,delete}`
/// (endpoints/notes/thread-muting/, requireCredential write:account).
/// create: getNote (invisible/un existing → NO_SUCH_NOTE), existing mute →
/// ALREADY_MUTING, insert `{ id: gen(), threadId: note.threadId ?? note.id,
/// userId }` → void (204). delete: getNote then delete by
/// (threadId, userId) — no NOT_MUTING check in this pinned tree → 204.
async fn notes_thread_muting(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = match mute_lane_me_id(pool, token.as_deref()).await {
        Ok(m) => m,
        Err(r) => return r,
    };
    let Some(note_id) = body.get("noteId").and_then(|v| v.as_str()) else {
        return api_err(ApiError::invalid_param(json!({
            "param": "noteId",
            "reason": "required",
        })));
    };
    // GetterService.getNote: missing (or invisible-to-me — the helper mirrors
    // NoteEntityService.isVisibleForMe) → NO_SUCH_NOTE.
    let note = match mk_db::note::find_note_by_id(pool, note_id).await {
        Ok(Some(n)) => n,
        Ok(None) => {
            return api_err(ApiError::new(
                400,
                mk_types::api_error::ApiErrorKind::Client,
                "NO_SUCH_NOTE",
                "5ff67ada-ed3b-2e71-8e87-a1a421e177d2",
                "No such note.",
            ));
        }
        Err(e) => {
            tracing::error!(target: "dbg_mentions", error = %e, "find_note_by_id");
            return api_err(ApiError::internal_error());
        }
    };
    if !matches!(note_is_visible_for_me(pool, &note, &me_id).await, Ok(true)) {
        return api_err(ApiError::new(
            400,
            mk_types::api_error::ApiErrorKind::Client,
            "NO_SUCH_NOTE",
            "5ff67ada-ed3b-2e71-8e87-a1a421e177d2",
            "No such note.",
        ));
    }
    // mute key = `note.threadId ?? note.id` (create.ts L46/L49) — NoteRow
    // carries the decoded threadId (root notes → note.id). // thread-cap
    let thread_id = note.thread_id.clone().unwrap_or_else(|| note.id.clone());

    if name == "notes/thread-muting/create" {
        match mk_db::note::thread_muted(pool, &me_id, &thread_id).await {
            Ok(true) => {
                return api_err(ApiError::new(
                    400,
                    mk_types::api_error::ApiErrorKind::Client,
                    "ALREADY_MUTING",
                    "c146e22d-1141-4b31-b28d-176371014d18",
                    "You are already muting that thread.",
                ));
            }
            Ok(false) => {}
            Err(e) => {
                tracing::error!(target: "dbg_mentions", error = %e, "thread_muted");
                return api_err(ApiError::internal_error());
            }
        }
        if let Err(e) = mk_db::note::insert_thread_muting(
            pool,
            &mk_types::id::gen_aid_now(),
            &me_id,
            &thread_id,
        )
        .await
        {
            tracing::error!(target: "dbg_mentions", error = %e, "insert_thread_muting");
            return api_err(ApiError::internal_error());
        }
    } else if let Err(e) = mk_db::note::delete_thread_muting(pool, &me_id, &thread_id).await {
        tracing::error!(target: "dbg_mentions", error = %e, "delete_thread_muting");
        return api_err(ApiError::internal_error());
    }
    (StatusCode::NO_CONTENT).into_response()
}

/// `POST /api/notifications/mark-all-as-read`
/// (endpoints/notifications/mark-all-as-read.ts, requireCredential, void →
/// 204): `NotificationService.readAllNotification` — every unread row of the
/// caller flips isRead. The Mute e2e's stream legs run this reset before
/// each waitFire.
async fn notifications_mark_all_as_read(state: &AppState, token: Option<String>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = match mute_lane_me_id(pool, token.as_deref()).await {
        Ok(m) => m,
        Err(r) => return r,
    };
    if let Err(e) = mk_db::notification::mark_as_read_all(pool, &me_id).await {
        tracing::error!(target: "dbg_notifications", error = %e, "mark_as_read_all");
        return api_err(ApiError::internal_error());
    }
    (StatusCode::NO_CONTENT).into_response()
}

/// `GET /url` — the URL preview route (upstream ClientServerService
/// fastify.get '/url' → UrlPreviewService.handle). 400 when the `url` query
/// param is missing (UrlPreviewService.ts L59-63 — empty body); otherwise the
/// summaly fetch is out of scope for this server (no outbound HTTP client),
/// so the request mirrors the catch-arm L124-136: 422 +
/// `URL_PREVIEW_FAILED` with `Cache-Control: max-age=86400, immutable`
/// (endpoints.ts URL preview 422 assert hits the invalid host e:xample.com).
pub(crate) async fn url_preview(
    axum::extract::RawQuery(raw_query): axum::extract::RawQuery,
) -> Response {
    let has_url = raw_query
        .as_deref()
        .map(|q| {
            q.split('&')
                .any(|pair| pair.split('=').next().map(|k| k == "url").unwrap_or(false))
        })
        .unwrap_or(false);
    if !has_url {
        return StatusCode::BAD_REQUEST.into_response();
    }
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        [(header::CACHE_CONTROL, "max-age=86400, immutable")],
        Json(json!({
            "error": {
                "message": "Failed to get preview",
                "code": "URL_PREVIEW_FAILED",
                "id": "09d01cb5-53b9-4856-82e5-38a50c290a3b",
            }
        })),
    )
        .into_response()
}

/// `GET /notes/{id}` — the AP Document route the drive/files/create
/// getWebpublicType probes hit (`simpleGet('notes/{id}',
/// 'application/activity+json')`): 404 unless the Accept header asks for
/// activity+json (upstream serves the HTML player otherwise), 404 for
/// non-public notes. Renders the minimal Note document — the e2e reads
/// `attachment[0].mediaType` (file.webpublicType ?? file.type).
pub(crate) async fn ap_note(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
) -> Response {
    let accepts_ap = headers
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|a| a.contains("application/activity+json") || a.contains("application/ld+json"))
        .unwrap_or(false);
    if !accepts_ap {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let note = match mk_db::note::find_note_by_id(pool, &id).await {
        Ok(Some(n)) => n,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "ap_note find");
            return api_err(ApiError::internal_error());
        }
    };
    // AP visibility: anonymous viewer; only public/home notes render.
    if matches!(note.visibility.as_str(), "followers" | "specified") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let origin = state.config.url.trim_end_matches('/');
    let created_at = mk_types::id::parse_aid_date(&note.id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default();
    let mut attachment = Vec::new();
    if let Some(fid) = note.file_ids.first() {
        match mk_db::drive::find_drive_file(pool, fid).await {
            Ok(Some(f)) => {
                attachment.push(json!({
                    "type": "Document",
                    "mediaType": f.file_type,
                    "url": f.url,
                    "name": f.comment,
                }));
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!(target: "dbg_drive", error = %e, "ap_note attachment");
                return api_err(ApiError::internal_error());
            }
        }
    }
    let body = json!({
        "@context": [
            "https://www.w3.org/ns/activitystreams",
            "https://w3id.org/security/v1",
        ],
        "id": format!("{origin}/notes/{}", note.id),
        "type": "Note",
        "attributedTo": format!("{origin}/users/{}", note.user_id),
        "published": created_at,
        "to": ["https://www.w3.org/ns/activitystreams#Public"],
        "cc": [format!("{origin}/users/{}/followers", note.user_id)],
        "content": note
            .text
            .as_deref()
            .map(|t| format!("<p>{}</p>", t.replace('<', "&lt;").replace('>', "&gt;")))
            .unwrap_or_default(),
        "attachment": attachment,
    });
    (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "application/activity+json; charset=utf-8",
        )],
        Json(body),
    )
        .into_response()
}

#[cfg(test)]
mod tests;

/// `/api/admin/*` family (M2 slice). Module file `api/admin.rs` owns the
/// endpoint table, the ApiCallService-style auth chain (credential gate →
/// suspended gate → requireModerator/requireAdmin ROLE_PERMISSION_DENIED
/// gates with the meta.rootUserId bypass) and every admin handler; mod.rs
/// only registers the module and the single `admin/*` dispatch arm above.
mod admin;
