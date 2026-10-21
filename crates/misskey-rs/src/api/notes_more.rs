//! notes/* remainder batch: `notes/favorites/{create,delete}`,
//! `notes/reactions/delete`, `notes/renotes`, `notes/conversation`,
//! `notes/children`, `notes/state` and `notes/unrenote`.
//!
//! Upstream refs: `endpoints/notes/favorites/{create,delete}.ts`
//! (NO_SUCH_NOTE / ALREADY_FAVORITED / NOT_FAVORITED),
//! `endpoints/notes/reactions/delete.ts` (NOT_REACTED), `endpoints/notes/
//! renotes.ts` + `conversation.ts` + `children.ts` (guest-ok pagination over
//! the note table; conversation walks the reply chain, children unions direct
//! replies with quotes), `endpoints/notes/state.ts` (isFavorited /
//! isMutedThread) and `endpoints/notes/unrenote.ts` (delete every own renote
//! of the target through NoteDeleteService).
//!
//! Skipped + documented: notes/featured (no featured-note table at the pinned
//! schema), notes/edit (the 0001 note table has no `updatedAt` column),
//! notes/watching (not an endpoint at the pinned upstream list; the table is
//! present but the name is absent from /tmp/up.txt), notes/drafts/* and
//! notes/clips/show-partial-bulk/user-list-timeline/global-timeline/
//! polls/recommendation (outside this batch).

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// Endpoint names routed to this module (the api/mod.rs name-list mirrors it).
pub(crate) const ENDPOINTS: &[&str] = &[
    "notes/favorites/create",
    "notes/favorites/delete",
    "notes/reactions/delete",
    "notes/renotes",
    "notes/conversation",
    "notes/children",
    "notes/state",
    "notes/unrenote",
];

pub(crate) fn handles(name: &str) -> bool {
    ENDPOINTS.contains(&name)
}

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

fn required_note_id(body: &Value) -> Result<&str, Response> {
    match body.get("noteId") {
        None | Some(Value::Null) => Err(invalid("noteId", "required")),
        Some(Value::String(s)) if is_alnum_id(s) => Ok(s.as_str()),
        Some(_) => Err(invalid("noteId", "must be an alphanumeric id")),
    }
}

/// Optional-credential chain (guest-ok endpoints): absent token → None, a
/// present-but-unresolvable token still 401s (ApiCallService auth).
async fn optional_me(
    pool: &mk_db::sqlx::PgPool,
    token: Option<&str>,
) -> Result<Option<String>, Response> {
    match token {
        None => Ok(None),
        Some(t) => {
            if t.len() != 16 {
                return Err(super::authentication_failed());
            }
            match super::resolve_me(pool, Some(t)).await {
                Some(id) => Ok(Some(id)),
                None => Err(super::authentication_failed()),
            }
        }
    }
}

/// makePaginationQuery inputs (limit 1..100 default 10, id/date cursors).
struct Page {
    limit: i64,
    since_id: Option<String>,
    until_id: Option<String>,
    since_date: Option<i64>,
    until_date: Option<i64>,
}

fn page_params(body: &Value, default_limit: i64) -> Result<Page, Response> {
    let limit = match body.get("limit") {
        None | Some(Value::Null) => default_limit,
        Some(v) => match v.as_i64() {
            Some(n) if (1..=100).contains(&n) => n,
            Some(_) => return Err(invalid("limit", "must be 1..100")),
            None => return Err(invalid("limit", "must be an integer")),
        },
    };
    let id = |key: &str| -> Result<Option<String>, Response> {
        match body.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if is_alnum_id(s) => Ok(Some(s.clone())),
            Some(_) => Err(invalid(key, "must be an alphanumeric id")),
        }
    };
    let date = |key: &str| -> Result<Option<i64>, Response> {
        match body.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_i64()
                .map(Some)
                .ok_or_else(|| invalid(key, "must be an integer")),
        }
    };
    let (since_id, until_id, since_date, until_date) = match (
        id("sinceId"),
        id("untilId"),
        date("sinceDate"),
        date("untilDate"),
    ) {
        (Ok(a), Ok(b), Ok(c), Ok(d)) => (a, b, c, d),
        (Err(r), _, _, _) | (_, Err(r), _, _) | (_, _, Err(r), _) | (_, _, _, Err(r)) => {
            return Err(r);
        }
    };
    Ok(Page {
        limit,
        since_id,
        until_id,
        since_date,
        until_date,
    })
}

impl Page {
    /// (since bound, until bound) after the aid date conversion.
    fn bounds(&self) -> (Option<String>, Option<String>) {
        let (from, until, _desc) = super::ff_vis::paginate_bounds(
            self.since_id.as_deref(),
            self.until_id.as_deref(),
            self.since_date,
            self.until_date,
        );
        (from, until)
    }
}

async fn pack_rows(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    rows: &[mk_db::note::NoteRow],
    me: Option<&str>,
) -> Result<Vec<Value>, Response> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        match super::pack_note_for(state, pool, &row.id, me).await {
            Ok(v) => out.push(v),
            Err(r) => return Err(*r),
        }
    }
    Ok(out)
}

/// Dispatch for the notes/* remainder group.
pub(crate) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
) -> Response {
    match name {
        "notes/favorites/create" => favorites_create(state, body, token.as_deref()).await,
        "notes/favorites/delete" => favorites_delete(state, body, token.as_deref()).await,
        "notes/reactions/delete" => reactions_delete(state, body, token.as_deref()).await,
        "notes/renotes" => renotes(state, body, token.as_deref()).await,
        "notes/conversation" => conversation(state, body, token.as_deref()).await,
        "notes/children" => children(state, body, token.as_deref()).await,
        "notes/state" => state_flags(state, body, token.as_deref()).await,
        "notes/unrenote" => unrenote(state, body, token.as_deref()).await,
        _ => api_err(ApiError::unknown_endpoint()),
    }
}

/// `POST /api/notes/favorites/create` — requireCredential, prohibitMoved.
async fn favorites_create(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me = match super::require_credential_me_id(pool, token, "dbg_favorites_create").await {
        Ok(id) => id,
        Err(r) => return r,
    };
    let note_id = match required_note_id(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let note = match mk_db::note::find_note_by_id(pool, note_id).await {
        Ok(Some(n)) => n,
        Ok(None) => {
            return meta_err(
                "NO_SUCH_NOTE",
                "6dd26674-e060-4816-909a-45ba3f4da458",
                "No such note.",
            );
        }
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "favorites_create find_note_by_id");
            return api_err(ApiError::internal_error());
        }
    };
    match super::note_is_visible_for_me(pool, &note, &me).await {
        Ok(true) => {}
        Ok(false) => {
            return meta_err(
                "NO_SUCH_NOTE",
                "6dd26674-e060-4816-909a-45ba3f4da458",
                "No such note.",
            );
        }
        Err(r) => return r,
    }
    match mk_db::note::favorite_exists(pool, &me, note_id).await {
        Ok(true) => {
            return meta_err(
                "ALREADY_FAVORITED",
                "a402c12b-34dd-41d2-97d8-4d2ffd96a1a6",
                "The note has already been marked as a favorite.",
            );
        }
        Ok(false) => {}
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "favorites_create favorite_exists");
            return api_err(ApiError::internal_error());
        }
    }
    if let Err(e) =
        mk_db::note::insert_note_favorite(pool, &mk_types::id::gen_aid_now(), &me, note_id).await
    {
        tracing::error!(target: "dbg_notes", error = %e, "insert_note_favorite");
        return api_err(ApiError::internal_error());
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `POST /api/notes/favorites/delete` — requireCredential.
async fn favorites_delete(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me = match super::require_credential_me_id(pool, token, "dbg_favorites_delete").await {
        Ok(id) => id,
        Err(r) => return r,
    };
    let note_id = match required_note_id(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    match mk_db::note::find_note_by_id(pool, note_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return meta_err(
                "NO_SUCH_NOTE",
                "80848a2c-398f-4343-baa9-df1d57696c56",
                "No such note.",
            );
        }
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "favorites_delete find_note_by_id");
            return api_err(ApiError::internal_error());
        }
    }
    let favorite_id = match mk_db::note::find_note_favorite_id(pool, &me, note_id).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            return meta_err(
                "NOT_FAVORITED",
                "b625fc69-635e-45e9-86f4-dbefbef35af5",
                "You have not marked that note a favorite.",
            );
        }
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "favorites_delete find_note_favorite_id");
            return api_err(ApiError::internal_error());
        }
    };
    if let Err(e) = mk_db::note::delete_note_favorite(pool, &favorite_id).await {
        tracing::error!(target: "dbg_notes", error = %e, "delete_note_favorite");
        return api_err(ApiError::internal_error());
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `POST /api/notes/reactions/delete` — ReactionService.delete (NOT_REACTED
/// when the viewer has no row) + the reactions jsonb decrement.
async fn reactions_delete(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me = match super::require_credential_me_id(pool, token, "dbg_reactions_delete").await {
        Ok(id) => id,
        Err(r) => return r,
    };
    let note_id = match required_note_id(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    match mk_db::note::find_note_by_id(pool, note_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return meta_err(
                "NO_SUCH_NOTE",
                "764d9fce-f9f2-4a0e-92b1-6ceac9a7ad37",
                "No such note.",
            );
        }
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "reactions_delete find_note_by_id");
            return api_err(ApiError::internal_error());
        }
    }
    let mine = match mk_db::note::find_note_reaction(pool, note_id, &me).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return meta_err(
                "NOT_REACTED",
                "92f4426d-4196-4125-aa5b-02943e2ec8fc",
                "You are not reacting to that note.",
            );
        }
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "reactions_delete find_note_reaction");
            return api_err(ApiError::internal_error());
        }
    };
    if let Err(e) = mk_db::note::delete_note_reaction(pool, note_id, &me).await {
        tracing::error!(target: "dbg_notes", error = %e, "reactions_delete delete_note_reaction");
        return api_err(ApiError::internal_error());
    }
    if let Err(e) = mk_db::note::bump_reactions_map(pool, note_id, &mine.reaction, -1).await {
        tracing::error!(target: "dbg_notes", error = %e, "reactions_delete bump_reactions_map");
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `POST /api/notes/renotes` — guest-ok renote list of the target.
async fn renotes(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me = match optional_me(pool, token).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let note_id = match required_note_id(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    match mk_db::note::find_note_by_id(pool, note_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return meta_err(
                "NO_SUCH_NOTE",
                "12908022-2e21-46cd-ba6a-3edaf6093f46",
                "No such note.",
            );
        }
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "renotes find_note_by_id");
            return api_err(ApiError::internal_error());
        }
    }
    let page = match page_params(body, 10) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let (since, until) = page.bounds();
    let followees = match &me {
        Some(me_id) => match mk_db::social::followee_ids(pool, me_id).await {
            Ok(f) => f,
            Err(e) => {
                tracing::error!(target: "dbg_notes", error = %e, "renotes followee_ids");
                return api_err(ApiError::internal_error());
            }
        },
        None => Vec::new(),
    };
    let rows = match mk_db::note::renotes_of_note(
        pool,
        note_id,
        me.as_deref(),
        &followees,
        since.as_deref(),
        until.as_deref(),
        page.limit,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "renotes_of_note");
            return api_err(ApiError::internal_error());
        }
    };
    match pack_rows(state, pool, &rows, me.as_deref()).await {
        Ok(out) => (StatusCode::OK, Json(Value::Array(out))).into_response(),
        Err(r) => r,
    }
}

/// `POST /api/notes/conversation` — walk the reply chain upward from the
/// target's reply target (offset/limit, no visibility query — the pack hides).
async fn conversation(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me = match optional_me(pool, token).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if me.is_none() {
        // serverSettings.ugcVisibilityForVisitor === 'none' → [] for guests.
        let ugc: Option<String> = match mk_db::sqlx::query_scalar(
            r#"SELECT "ugcVisibilityForVisitor" FROM "meta" ORDER BY "id" DESC LIMIT 1"#,
        )
        .fetch_optional(pool)
        .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(target: "dbg_notes", error = %e, "conversation ugc visibility");
                return api_err(ApiError::internal_error());
            }
        };
        if ugc.as_deref() == Some("none") {
            return (StatusCode::OK, Json(Value::Array(Vec::new()))).into_response();
        }
    }
    let note_id = match required_note_id(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let note = match mk_db::note::find_note_by_id(pool, note_id).await {
        Ok(Some(n)) => n,
        Ok(None) => {
            return meta_err(
                "NO_SUCH_NOTE",
                "e1035875-9551-45ec-afa8-1ded1fcb53c8",
                "No such note.",
            );
        }
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "conversation find_note_by_id");
            return api_err(ApiError::internal_error());
        }
    };
    let page = match page_params(body, 10) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let offset = match body.get("offset") {
        None | Some(Value::Null) => 0i64,
        Some(v) => match v.as_i64() {
            Some(n) if n >= 0 => n,
            _ => return invalid("offset", "must be a non-negative integer"),
        },
    };

    let mut conversation: Vec<mk_db::note::NoteRow> = Vec::new();
    let mut i: i64 = 0;
    let mut next = note.reply_id.clone();
    while let Some(id) = next {
        if conversation.len() as i64 >= page.limit {
            break;
        }
        i += 1;
        let p = match mk_db::note::find_note_by_id(pool, &id).await {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(e) => {
                tracing::error!(target: "dbg_notes", error = %e, "conversation parent");
                return api_err(ApiError::internal_error());
            }
        };
        let parent = p.reply_id.clone();
        if i > offset {
            conversation.push(p);
        }
        next = parent;
    }
    match pack_rows(state, pool, &conversation, me.as_deref()).await {
        Ok(out) => (StatusCode::OK, Json(Value::Array(out))).into_response(),
        Err(r) => r,
    }
}

/// `POST /api/notes/children` — direct replies + quote/renotes of the target
/// (no target existence gate upstream).
async fn children(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me = match optional_me(pool, token).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let note_id = match required_note_id(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let page = match page_params(body, 10) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let (since, until) = page.bounds();
    let followees = match &me {
        Some(me_id) => match mk_db::social::followee_ids(pool, me_id).await {
            Ok(f) => f,
            Err(e) => {
                tracing::error!(target: "dbg_notes", error = %e, "children followee_ids");
                return api_err(ApiError::internal_error());
            }
        },
        None => Vec::new(),
    };
    let rows = match mk_db::note::children_of_note(
        pool,
        note_id,
        me.as_deref(),
        &followees,
        since.as_deref(),
        until.as_deref(),
        page.limit,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "children_of_note");
            return api_err(ApiError::internal_error());
        }
    };
    match pack_rows(state, pool, &rows, me.as_deref()).await {
        Ok(out) => (StatusCode::OK, Json(Value::Array(out))).into_response(),
        Err(r) => r,
    }
}

/// `POST /api/notes/state` — { isFavorited, isMutedThread } for the viewer.
async fn state_flags(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me = match super::require_credential_me_id(pool, token, "dbg_notes_state").await {
        Ok(id) => id,
        Err(r) => return r,
    };
    let note_id = match required_note_id(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    // `findOneByOrFail` → EntityNotFound surfaces as an internal error.
    let note = match mk_db::note::find_note_by_id(pool, note_id).await {
        Ok(Some(n)) => n,
        Ok(None) => return api_err(ApiError::internal_error()),
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "notes_state find_note_by_id");
            return api_err(ApiError::internal_error());
        }
    };
    let is_favorited = match mk_db::note::favorite_exists(pool, &me, note_id).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "notes_state favorite_exists");
            return api_err(ApiError::internal_error());
        }
    };
    let thread_id = note.thread_id.clone().unwrap_or_else(|| note.id.clone());
    let is_muted = match mk_db::note::thread_muted(pool, &me, &thread_id).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "notes_state thread_muted");
            return api_err(ApiError::internal_error());
        }
    };
    (
        StatusCode::OK,
        Json(json!({
            "isFavorited": is_favorited,
            "isMutedThread": is_muted,
        })),
    )
        .into_response()
}

/// `POST /api/notes/unrenote` — delete every own renote of the target
/// (NoteDeleteService per row; upstream does not await the loop, we do and
/// log failures).
async fn unrenote(state: &AppState, body: &Value, token: Option<&str>) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    let me = match super::require_credential_me_id(pool, token, "dbg_unrenote").await {
        Ok(id) => id,
        Err(r) => return r,
    };
    let note_id = match required_note_id(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    match mk_db::note::find_note_by_id(pool, note_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return meta_err(
                "NO_SUCH_NOTE",
                "efd4a259-2442-496b-8dd7-b255aa1a160f",
                "No such note.",
            );
        }
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "unrenote find_note_by_id");
            return api_err(ApiError::internal_error());
        }
    }
    let ids = match mk_db::note::find_renotes_by_user(pool, &me, note_id).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(target: "dbg_notes", error = %e, "find_renotes_by_user");
            return api_err(ApiError::internal_error());
        }
    };
    for id in ids {
        let resp = super::social::notes_delete(state, &json!({ "noteId": id }), &me).await;
        if resp.status() != StatusCode::NO_CONTENT {
            tracing::error!(target: "dbg_notes", note = %id, "unrenote delete failed");
        }
    }
    StatusCode::NO_CONTENT.into_response()
}
