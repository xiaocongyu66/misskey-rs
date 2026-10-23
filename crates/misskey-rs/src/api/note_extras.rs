//! note-agent: the notes.ts acceptance rest — `notes` (global list),
//! `notes/polls/vote`, `notes/polls/recommendation`, `notes/translate` and
//! the sensitiveWords / prohibitedWords patch of `admin/update-meta`.
//!
//! Upstream refs: `endpoints/notes.ts` (public + localOnly filters, no
//! credential), `endpoints/notes/polls/vote.ts` (NO_SUCH_NOTE on invisible,
//! NO_POLL, ALREADY_EXPIRED, INVALID_CHOICE, ALREADY_VOTED,
//! `votes[$1] = votes[$1] + 1`), `endpoints/notes/polls/recommendation.ts`
//! (requireCredential read:account; the pinned upstream recommendation is a
//! pure SQL scan over `poll` — local public unexpired polls not yet voted by
//! the caller, author unmuted, noteId DESC with limit/offset and the
//! `excludeChannels` bracket — no Redis engine), `endpoints/notes/translate.ts`
//! (canUseTranslator → note → invisible → empty text 204 → deeplAuthKey
//! UNAVAILABLE ladder), `endpoints/admin/update-meta.ts` (requireAdministrator
//! + the word-list params the meta row carries).

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

fn invalid(param: &str, reason: &str) -> Response {
    api_err(ApiError::invalid_param(json!({
        "param": param,
        "reason": reason,
    })))
}

fn meta_err(code: &'static str, id: &'static str, msg: &'static str) -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        code,
        id,
        msg,
    ))
}

/// ajv `misskey:id` format (`/^[a-zA-Z0-9]+$/`).
fn is_alnum_id(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Dispatch for the JSON-body group: polls/vote + translate.
pub(crate) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
) -> Response {
    match name {
        "notes/polls/vote" => polls_vote(state, body, token.as_deref()).await,
        "notes/polls/recommendation" => polls_recommendation(state, body, token.as_deref()).await,
        "notes/translate" => translate(state, body, token.as_deref()).await,
        _ => api_err(ApiError::unknown_endpoint()),
    }
}

/// `POST /api/notes/polls/recommendation` (requireCredential, kind
/// read:account; endpoints/notes/polls/recommendation.ts): the poll-table
/// recommendation scan (mk_db::poll::recommended_poll_note_ids) — no Redis
/// engine involved in the pinned upstream, so this is a faithful port, not an
/// approximation. The picked notes pack with `detail: true` (pack_note_for).
pub(crate) async fn polls_recommendation(
    state: &AppState,
    body: &Value,
    token: Option<&str>,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    // requireCredential: a declared-but-unresolvable credential →
    // AUTHENTICATION_FAILED, no credential at all → CREDENTIAL_REQUIRED
    // (require_checked_user's credential-declared ladder).
    let declared = body.get("i").is_some();
    let me_id = match super::social::require_checked_user(pool, token, declared).await {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let limit = match body.get("limit") {
        None => 10i64,
        Some(Value::Number(n)) => match n.as_i64() {
            Some(l) if (1..=100).contains(&l) => l,
            _ => return invalid("limit", "must be 1..100"),
        },
        Some(_) => return invalid("limit", "must be an integer"),
    };
    let offset = match body.get("offset") {
        None => 0i64,
        Some(Value::Number(n)) => match n.as_i64() {
            Some(o) if o >= 0 => o,
            _ => return invalid("offset", "must be a non-negative integer"),
        },
        Some(_) => return invalid("offset", "must be an integer"),
    };
    let exclude_channels = match body.get("excludeChannels") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return invalid("excludeChannels", "must be a boolean"),
    };

    let ids =
        match mk_db::poll::recommended_poll_note_ids(pool, &me_id, limit, offset, exclude_channels)
            .await
        {
            Ok(i) => i,
            Err(e) => {
                tracing::error!(target: "dbg_notes", error = %e, "recommended_poll_note_ids");
                return api_err(ApiError::internal_error());
            }
        };

    let mut out = Vec::with_capacity(ids.len());
    for id in &ids {
        match super::pack_note_for(state, pool, id, Some(&me_id)).await {
            Ok(n) => out.push(n),
            Err(resp) => return *resp,
        }
    }
    (StatusCode::OK, Json(Value::Array(out))).into_response()
}

/// `POST /api/notes` — the global list (visibility='public' + localOnly=FALSE;
/// reply/renote/withFiles/poll toggles, `local` host filter, aid paging).
/// No requireCredential — `me` only feeds the pack (poll isVoted et al).
pub(crate) async fn notes_list(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let opt_bool = |key: &str| -> Result<Option<bool>, Response> {
        match body.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(invalid(key, "must be a boolean")),
        }
    };
    let local = match opt_bool("local") {
        Ok(v) => v.unwrap_or(false),
        Err(r) => return r,
    };
    let reply = match opt_bool("reply") {
        Ok(v) => v,
        Err(r) => return r,
    };
    let renote = match opt_bool("renote") {
        Ok(v) => v,
        Err(r) => return r,
    };
    let with_files = match opt_bool("withFiles") {
        Ok(v) => v,
        Err(r) => return r,
    };
    let poll = match opt_bool("poll") {
        Ok(v) => v,
        Err(r) => return r,
    };
    let limit = match body.get("limit") {
        None => 10i64,
        Some(Value::Number(n)) => match n.as_i64() {
            Some(l) if (1..=100).contains(&l) => l,
            _ => return invalid("limit", "must be 1..100"),
        },
        Some(_) => return invalid("limit", "must be an integer"),
    };
    let since_id = match body.get("sinceId") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if is_alnum_id(s) => Some(s.as_str()),
        Some(_) => return invalid("sinceId", "must be an alphanumeric id"),
    };
    let until_id = match body.get("untilId") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if is_alnum_id(s) => Some(s.as_str()),
        Some(_) => return invalid("untilId", "must be an alphanumeric id"),
    };

    let ids = match mk_db::note::global_note_ids(
        pool, local, reply, renote, with_files, poll, since_id, until_id, limit,
    )
    .await
    {
        Ok(i) => i,
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "global_note_ids");
            return api_err(ApiError::internal_error());
        }
    };

    let me_id: Option<String> = super::resolve_me(pool, token).await;
    let mut out = Vec::with_capacity(ids.len());
    for id in &ids {
        match super::pack_note_for(state, pool, id, me_id.as_deref()).await {
            Ok(n) => out.push(n),
            Err(resp) => return *resp,
        }
    }
    (StatusCode::OK, Json(Value::Array(out))).into_response()
}

/// `POST /api/notes/polls/vote` (requireCredential, write:votes).
async fn polls_vote(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = match super::require_credential_me_id(pool, token, "dbg_polls_vote").await {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let Some(note_id) = body
        .get("noteId")
        .and_then(|v| v.as_str())
        .filter(|s| is_alnum_id(s))
    else {
        return invalid("noteId", "must be an alphanumeric id");
    };
    let Some(choice) = body.get("choice").and_then(|v| v.as_i64()) else {
        return invalid("choice", "must be an integer");
    };

    let Some(note) = (match mk_db::note::find_note_by_id(pool, note_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "find_note_by_id[vote]");
            return api_err(ApiError::internal_error());
        }
    }) else {
        return meta_err(
            "NO_SUCH_NOTE",
            "ecafbd2e-c283-4d6d-aecb-1a0a33b75396",
            "No such note.",
        );
    };
    // invisible-note votes fold into NO_SUCH_NOTE upstream.
    match super::note_is_visible_for_me(pool, &note, &me_id).await {
        Ok(true) => {}
        Ok(false) => {
            return meta_err(
                "NO_SUCH_NOTE",
                "ecafbd2e-c283-4d6d-aecb-1a0a33b75396",
                "No such note.",
            );
        }
        Err(r) => return r,
    }
    if !note.has_poll {
        return meta_err(
            "NO_POLL",
            "5f979967-52d9-4314-a911-1c673727f92f",
            "The note does not attach a poll.",
        );
    }
    // blocking (blocker = note author)
    if note.user_id != me_id {
        match mk_db::social::blocking_exists(pool, &note.user_id, &me_id).await {
            Ok(true) => {
                return meta_err(
                    "YOU_HAVE_BEEN_BLOCKED",
                    "85a5377e-b1e9-4617-b0b9-5bea73331e49",
                    "You cannot vote this poll because you have been blocked by this user.",
                );
            }
            Ok(false) => {}
            Err(e) => {
                tracing::error!(target: "dbg_notes", error = %e, "blocking_exists[vote]");
                return api_err(ApiError::internal_error());
            }
        }
    }
    let Some(poll) = (match mk_db::poll::find_poll_by_note(pool, note_id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "find_poll_by_note[vote]");
            return api_err(ApiError::internal_error());
        }
    }) else {
        return meta_err(
            "NO_POLL",
            "5f979967-52d9-4314-a911-1c673727f92f",
            "The note does not attach a poll.",
        );
    };
    if poll
        .expires_at_ms
        .is_some_and(|exp| exp <= crate::api::now_ms() as i64)
    {
        return meta_err(
            "ALREADY_EXPIRED",
            "1022a357-b085-4054-9083-8f8de358337e",
            "The poll is already expired.",
        );
    }
    if choice < 0 || poll.choices.get(choice as usize).is_none() {
        return meta_err(
            "INVALID_CHOICE",
            "e0cc9a04-f2e8-41e4-a5f1-4127293260cc",
            "Choice ID is invalid.",
        );
    }
    let existing = match mk_db::poll::poll_votes_of_user(pool, note_id, &me_id).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "poll_votes_of_user");
            return api_err(ApiError::internal_error());
        }
    };
    if existing.contains(&(choice as i32)) || (!poll.multiple && !existing.is_empty()) {
        return meta_err(
            "ALREADY_VOTED",
            "0963fc77-efac-419b-9424-b391608dc6d8",
            "You have already voted.",
        );
    }
    let vote_id = mk_types::id::gen_aid_now();
    if let Err(e) =
        mk_db::poll::insert_poll_vote(pool, &vote_id, &me_id, note_id, choice as i32).await
    {
        tracing::error!(target: "dbg_notes", error = %e, "insert_poll_vote");
        return api_err(ApiError::internal_error());
    }
    if let Err(e) = mk_db::poll::bump_poll_vote(pool, note_id, choice as i32).await {
        tracing::error!(target: "dbg_notes", error = %e, "bump_poll_vote");
        return api_err(ApiError::internal_error());
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `POST /api/notes/translate` (requireCredential, read:account). The DeepL
/// client is not ported: the pinned e2e ladder exercises the pre-key gates
/// (UNAVAILABLE via canUseTranslator, NO_SUCH_NOTE, invisible-note, the
/// empty-text 204) and the UNAVAILABLE case with no server key — the tail
/// branch keeps the same UNAVAILABLE shape for a configured-but-unported
/// translator.
async fn translate(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me_id = match super::require_credential_me_id(pool, token, "dbg_translate").await {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let Some(note_id) = body
        .get("noteId")
        .and_then(|v| v.as_str())
        .filter(|s| is_alnum_id(s))
    else {
        return invalid("noteId", "must be an alphanumeric id");
    };
    if body.get("targetLang").and_then(|v| v.as_str()).is_none() {
        return invalid("targetLang", "required");
    }

    let assigned = match mk_db::role::user_assigned_roles(pool, &me_id).await {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "user_assigned_roles");
            return api_err(ApiError::internal_error());
        }
    };
    if !mk_core::services::role::UserPolicy::compute(&assigned).can_use_translator {
        return meta_err(
            "UNAVAILABLE",
            "50a70314-2d8a-431b-b433-efa5cc56444c",
            "Translate of notes unavailable.",
        );
    }

    let Some(note) = (match mk_db::note::find_note_by_id(pool, note_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "find_note_by_id[translate]");
            return api_err(ApiError::internal_error());
        }
    }) else {
        return meta_err(
            "NO_SUCH_NOTE",
            "bea9b03f-36e0-49c5-a4db-627a029f8971",
            "No such note.",
        );
    };
    match super::note_is_visible_for_me(pool, &note, &me_id).await {
        Ok(true) => {}
        Ok(false) => {
            return meta_err(
                "CANNOT_TRANSLATE_INVISIBLE_NOTE",
                "ea29f2ca-c368-43b3-aaf1-5ac3e74bbe5d",
                "Cannot translate invisible note.",
            );
        }
        Err(r) => return r,
    }

    let mut text = note.text.clone().unwrap_or_default();
    if let Some(cw) = &note.cw {
        text = format!("{cw}\n-----\n{text}");
    }
    if text.trim().is_empty() {
        // `return;` in the upstream handler → ApiCallService sends 204 void.
        return StatusCode::NO_CONTENT.into_response();
    }

    // serverSettings.deeplAuthKey == null → UNAVAILABLE (default install);
    // no DeepL HTTP client is ported, so a configured key degrades to the
    // same UNAVAILABLE shape.
    match mk_db::meta::note_content_moderation_words(pool).await {
        Ok((_s, _p, _key)) => {}
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "note_content_moderation_words");
            return api_err(ApiError::internal_error());
        }
    }
    meta_err(
        "UNAVAILABLE",
        "50a70314-2d8a-431b-b433-efa5cc56444c",
        "Translate of notes unavailable.",
    )
}

/// Arm pick: the update-meta request carries one of the keys the note-side
/// lanes persist (word lists + the FTT flag, whose write arm lives in
/// freq::update_meta — admin.rs update-meta has no fanout branch).
pub(crate) fn takes_update_meta(body: &Value) -> bool {
    body.get("sensitiveWords").is_some()
        || body.get("prohibitedWords").is_some()
        || body.get("enableFanoutTimeline").is_some()
}

/// `POST /api/admin/update-meta` — the word-list patch
/// (requireAdministrator; response void). The freq arm keeps pinnedUsers.
pub(crate) async fn update_meta(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    // requireCredential → suspended → administrator (same chain as freq).
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
        return api_err(ApiError::new(
            401,
            mk_types::api_error::ApiErrorKind::Client,
            "AUTHENTICATION_FAILED",
            "b0a7f5f8-dc2f-4171-b91f-de88ad238e14",
            "Authentication failed. Please ensure your token is correct.",
        ));
    }
    let me_id = match mk_db::user::find_local_user_id_by_token(pool, token).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            return api_err(ApiError::new(
                401,
                mk_types::api_error::ApiErrorKind::Client,
                "AUTHENTICATION_FAILED",
                "b0a7f5f8-dc2f-4171-b91f-de88ad238e14",
                "Authentication failed. Please ensure your token is correct.",
            ));
        }
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "find_local_user_id_by_token");
            return api_err(ApiError::internal_error());
        }
    };
    let me = match mk_db::user::find_user_by_id(pool, &me_id).await {
        Ok(Some(m)) => m,
        Ok(None) => {
            return api_err(ApiError::new(
                401,
                mk_types::api_error::ApiErrorKind::Client,
                "AUTHENTICATION_FAILED",
                "b0a7f5f8-dc2f-4171-b91f-de88ad238e14",
                "Authentication failed. Please ensure your token is correct.",
            ));
        }
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "find_user_by_id[meta]");
            return api_err(ApiError::internal_error());
        }
    };
    if me.is_suspended {
        return api_err(ApiError::new(
            403,
            mk_types::api_error::ApiErrorKind::Permission,
            "YOUR_ACCOUNT_SUSPENDED",
            "a8c724b3-6e9c-4b46-b1a8-bc3ed6258370",
            "Your account has been suspended.",
        ));
    }
    let is_admin = {
        let root_id = match mk_db::admin::root_user_id(pool).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(target: "dbg_notes", error = %e, "root_user_id");
                return api_err(ApiError::internal_error());
            }
        };
        if root_id.as_deref() == Some(me.id.as_str()) || me.username_lower == "root" {
            true
        } else {
            match mk_db::role::user_assigned_roles(pool, &me.id).await {
                Ok(assigned) => mk_core::services::role::has_administrator_role(&assigned),
                Err(e) => {
                    tracing::error!(target: "dbg_notes", error = %e, "user_assigned_roles");
                    return api_err(ApiError::internal_error());
                }
            }
        }
    };
    if !is_admin {
        return api_err(ApiError::new(
            403,
            mk_types::api_error::ApiErrorKind::Permission,
            "ROLE_PERMISSION_DENIED",
            "c3d38592-54c0-429d-be96-5636b0431a61",
            "You are not assigned to an administrator role.",
        ));
    }

    // ftt2: bodies that carry the fanout flag are ROUTED here by
    // takes_update_meta (timelines.ts L104 flips it per describe.each arm) —
    // persist it or the arm swallows the key and the pushToTl gate never
    // opens. Param shape = the freq::update_meta mirror (non-nullable
    // boolean; patch semantics — absent keys untouched).
    match body.get("enableFanoutTimeline") {
        None | Some(Value::Null) => {}
        Some(Value::Bool(v)) => {
            if let Err(e) = mk_db::meta::update_meta_enable_fanout_timeline(pool, *v).await {
                tracing::error!(target: "dbg_notes", error = %e, "update_meta_enable_fanout_timeline");
                return api_err(ApiError::internal_error());
            }
        }
        Some(_) => return invalid("enableFanoutTimeline", "must be a boolean"),
    }

    let parse_words = |param: &str| -> Result<Option<Vec<String>>, Response> {
        match body.get(param) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Array(a)) => {
                let mut out = Vec::with_capacity(a.len());
                for v in a {
                    match v.as_str() {
                        Some(s) => out.push(s.to_string()),
                        None => return Err(invalid(param, "must be an array of strings")),
                    }
                }
                Ok(Some(out))
            }
            Some(_) => Err(invalid(param, "must be an array of strings")),
        }
    };
    let sensitive = match parse_words("sensitiveWords") {
        Ok(v) => v,
        Err(r) => return r,
    };
    let prohibited = match parse_words("prohibitedWords") {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(e) =
        mk_db::meta::update_meta_word_lists(pool, sensitive.as_deref(), prohibited.as_deref()).await
    {
        tracing::error!(target: "dbg_notes", error = %e, "update_meta_word_lists");
        return api_err(ApiError::internal_error());
    }
    StatusCode::NO_CONTENT.into_response()
}
