//! Antennas domain (upstream `endpoints/antennas/{create,delete,list,show,
//! update,notes,remove-note}.ts`).
//!
//! All seven endpoints are `requireCredential: true` and every one scopes the
//! antenna to its OWNER (`findOneBy({ id, userId: me.id })` — another user's
//! antennaId resolves the endpoint's own NO_SUCH_ANTENNA error id, never
//! 403). The db paths live in `mk_db::antennas` (CRUD + the 0004
//! `antenna_note` fanout projection + the shared lists.rs visibility/base
//! fragment set); the note-creation fanout hook is the single
//! `mk_db::antennas::fanout_created_note` call in mod.rs notes_create
//! (NoteCreateService.postNoteCreated → antennaService.addNoteToAntennas).
//!
//! meta error ids are per-endpoint upstream constants (show/delete/notes/
//! remove-note all carry a DIFFERENT noSuchAntenna id; do not merge them).

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// The endpoint names routed into this module (single source of truth for the
/// `// antennas-v2` dispatch arm in mod.rs).
pub(crate) const ANTENNA_ENDPOINTS: &[&str] = &[
    "antennas/create",
    "antennas/delete",
    "antennas/list",
    "antennas/notes",
    "antennas/remove-note",
    "antennas/show",
    "antennas/update",
];

pub(crate) fn handles(name: &str) -> bool {
    ANTENNA_ENDPOINTS.contains(&name)
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_antennas", error = %e, "db error: {context}");
    api_err(ApiError::internal_error())
}

/// Meta error without httpStatusCode: kind=client → default 400.
fn meta_err(code: &str, id: &str, message: &str) -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        code,
        id,
        message,
    ))
}

fn ok_empty() -> Response {
    // upstream `send(reply, undefined)` → 204 (delete/remove-note)
    StatusCode::NO_CONTENT.into_response()
}

fn ok_json(v: Value) -> Response {
    (StatusCode::OK, Json(v)).into_response()
}

fn invalid_param(param: &str, reason: &str) -> Response {
    api_err(ApiError::invalid_param(json!({
        "param": param,
        "reason": reason,
    })))
}

fn opt_id<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(|v| v.as_str())
}

fn required_id<'a>(body: &'a Value, key: &'a str) -> Result<&'a str, Response> {
    opt_id(body, key).ok_or_else(|| invalid_param(key, "required (misskey:id format)"))
}

fn opt_bool(body: &Value, key: &str, dflt: bool) -> Result<bool, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(dflt),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(invalid_param(key, "expected a boolean")),
    }
}

/// Entry from the `// antennas-v2` dispatch arm in mod.rs (body pre-parsed,
/// token + credential-declared pre-extracted).
pub(crate) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
    credential_declared: bool,
) -> Response {
    let Some(pool) = &state.pool else {
        return api_err(ApiError::internal_error());
    };
    // Every endpoint in this module is requireCredential upstream.
    let me_id = match super::social::require_checked_user(
        pool,
        token.as_deref(),
        credential_declared,
    )
    .await
    {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let result: Result<Response, Response> = match name {
        "antennas/create" => antennas_create(pool, body, &me_id).await,
        "antennas/update" => antennas_update(pool, body, &me_id).await,
        "antennas/show" => antennas_show(pool, body, &me_id).await,
        "antennas/list" => antennas_list(pool, &me_id).await,
        "antennas/delete" => antennas_delete(pool, body, &me_id).await,
        "antennas/remove-note" => antennas_remove_note(pool, body, &me_id).await,
        "antennas/notes" => antennas_notes(state, pool, body, &me_id).await,
        _ => Ok(api_err(ApiError::unknown_endpoint())),
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

// ---------------------------------------------------------------------------
// AntennaEntityService.pack
// ---------------------------------------------------------------------------

pub(crate) fn pack_antenna(row: &mk_db::antennas::AntennaRow) -> Value {
    let keywords: Value = serde_json::from_str(&row.keywords).unwrap_or_else(|_| json!([]));
    let exclude_keywords: Value =
        serde_json::from_str(&row.exclude_keywords).unwrap_or_else(|_| json!([]));
    let created_at = mk_types::id::parse_aid_date(&row.id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default();
    json!({
        "id": row.id,
        "createdAt": created_at,
        "name": row.name,
        "keywords": keywords,
        "excludeKeywords": exclude_keywords,
        "src": row.src,
        "userListId": row.user_list_id,
        "users": row.users,
        "caseSensitive": row.case_sensitive,
        "localOnly": row.local_only,
        "excludeBots": row.exclude_bots,
        "withReplies": row.with_replies,
        "withFile": row.with_file,
        "excludeNotesInSensitiveChannel": row.exclude_notes_in_sensitive_channel,
        "isActive": row.is_active,
        "hasUnreadNote": false, // TODO upstream
        "notify": false,        // 後方互換性のため (pinned pack)
    })
}

// ---------------------------------------------------------------------------
// Param parsing — paramDef shapes shared by create/update
// ---------------------------------------------------------------------------

const SRC_ENUM: &[&str] = &["home", "all", "users", "list", "users_blacklist"];

fn parse_name(body: &Value) -> Result<String, Response> {
    match body.get("name") {
        Some(Value::String(s)) if !s.is_empty() && s.chars().count() <= 100 => Ok(s.clone()),
        Some(Value::String(_)) => Err(invalid_param("name", "must be 1..100 characters")),
        _ => Err(invalid_param("name", "required (string 1..100 characters)")),
    }
}

fn parse_src(body: &Value) -> Result<String, Response> {
    match body.get("src").and_then(|v| v.as_str()) {
        Some(s) if SRC_ENUM.contains(&s) => Ok(s.to_string()),
        Some(_) => Err(invalid_param(
            "src",
            "must be one of home/all/users/list/users_blacklist",
        )),
        None => Err(invalid_param("src", "required (string enum)")),
    }
}

/// `string[][]` param (keywords / excludeKeywords): each member an array of
/// strings — re-serialized exactly as received so the pack round-trips.
fn parse_str_matrix(body: &Value, key: &str) -> Result<Vec<Vec<String>>, Response> {
    let Some(arr) = body.get(key).and_then(|v| v.as_array()) else {
        return Err(invalid_param(key, "required (array of string arrays)"));
    };
    let mut out = Vec::with_capacity(arr.len());
    for group in arr {
        let Some(inner) = group.as_array() else {
            return Err(invalid_param(key, "each group must be an array"));
        };
        let mut g = Vec::with_capacity(inner.len());
        for k in inner {
            match k.as_str() {
                Some(s) => g.push(s.to_string()),
                None => return Err(invalid_param(key, "keywords must be strings")),
            }
        }
        out.push(g);
    }
    Ok(out)
}

fn parse_str_vec(body: &Value, key: &str) -> Result<Vec<String>, Response> {
    let Some(arr) = body.get(key).and_then(|v| v.as_array()) else {
        return Err(invalid_param(key, "required (array of strings)"));
    };
    let mut out = Vec::with_capacity(arr.len());
    for k in arr {
        match k.as_str() {
            Some(s) => out.push(s.to_string()),
            None => return Err(invalid_param(key, "users must be strings")),
        }
    }
    Ok(out)
}

/// required boolean (create paramDef: caseSensitive/withReplies/withFile);
/// `null` is NOT a valid boolean upstream.
fn required_bool(body: &Value, key: &str) -> Result<bool, Response> {
    body.get(key)
        .and_then(|v| v.as_bool())
        .ok_or_else(|| invalid_param(key, "required (boolean)"))
}

/// create/update EMPTY_KEYWORD —
/// `ps.keywords.flat().every(x => x === '') && ps.excludeKeywords.flat()
/// .every(x => x === '')`: vacuous `every` on an emptied group is TRUE, so
/// `[[]]` on both sides blunders into the error.
fn both_matrices_blank(k: &[Vec<String>], e: &[Vec<String>]) -> bool {
    k.iter().flatten().all(|s| s.is_empty()) && e.iter().flatten().all(|s| s.is_empty())
}

const EMPTY_KEYWORD_MSG: &str = "Either keywords or excludeKeywords is required.";
const NO_SUCH_USER_LIST_MSG: &str = "No such user list.";
const NO_SUCH_ANTENNA_MSG: &str = "No such antenna.";

// ---------------------------------------------------------------------------
// antennas/create — create.ts
// ---------------------------------------------------------------------------

async fn antennas_create(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let name = parse_name(body)?;
    let src = parse_src(body)?;
    let keywords = parse_str_matrix(body, "keywords")?;
    let exclude_keywords = parse_str_matrix(body, "excludeKeywords")?;
    let users = parse_str_vec(body, "users")?;
    let case_sensitive = required_bool(body, "caseSensitive")?;
    let with_replies = required_bool(body, "withReplies")?;
    let with_file = required_bool(body, "withFile")?;
    let local_only = opt_bool(body, "localOnly", false)?;
    let exclude_bots = opt_bool(body, "excludeBots", false)?;
    let exclude_notes_in_sensitive_channel =
        opt_bool(body, "excludeNotesInSensitiveChannel", false)?;
    // `userListId: { type: 'string', format: 'misskey:id', nullable: true }`;
    // JS truthiness excludes '' from the list gate (lands NULL)
    let user_list_param: Option<&str> = body
        .get("userListId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());

    // meta.errors.emptyKeyword gate runs BEFORE the limit + list gates.
    if both_matrices_blank(&keywords, &exclude_keywords) {
        return Err(meta_err(
            "EMPTY_KEYWORD",
            "53ee222e-1ddd-4f9a-92e5-9fb82ddb463a",
            EMPTY_KEYWORD_MSG,
        ));
    }

    // roleService.getUserPolicies(me.id).antennaLimit (DEFAULT_POLICIES 5,
    // meta.policies/assigned-role merges ride the shared UserPolicy ladder).
    let count = mk_db::antennas::count_antennas_owned(pool, me_id)
        .await
        .map_err(|e| db_err("antennas_create count", e))?;
    let assigned = mk_db::role::user_assigned_roles(pool, me_id)
        .await
        .map_err(|e| db_err("antennas_create assigned_roles", e))?;
    let policy = mk_core::services::role::UserPolicy::compute(&assigned);
    if count >= policy.antenna_limit {
        return Err(meta_err(
            "TOO_MANY_ANTENNAS",
            "faf47050-e8b5-438c-913c-db2b1576fde4",
            "You cannot create antenna any more.",
        ));
    }

    // `ps.src === 'list' && ps.userListId` ownership gate; a non-list src
    // stores NULL even when the param carried an id (insertOne spread uses
    // `userList ? userList.id : null`).
    let user_list_id: Option<String> = if src == "list" {
        match user_list_param {
            Some(id) => {
                let owned = mk_db::lists::find_list(pool, id, me_id)
                    .await
                    .map_err(|e| db_err("antennas_create find_list", e))?;
                if owned.is_none() {
                    return Err(meta_err(
                        "NO_SUCH_USER_LIST",
                        "95063e93-a283-4b8b-9aa5-bcdb8df69a7f",
                        NO_SUCH_USER_LIST_MSG,
                    ));
                }
                Some(id.to_string())
            }
            None => None,
        }
    } else {
        None
    };

    let id = mk_types::id::gen_aid_now();
    let keywords_json = serde_json::to_string(&keywords)
        .map_err(|_| invalid_param("keywords", "unserializable"))?;
    let exclude_json = serde_json::to_string(&exclude_keywords)
        .map_err(|_| invalid_param("excludeKeywords", "unserializable"))?;
    mk_db::antennas::insert_antenna(
        pool,
        &id,
        me_id,
        &name,
        &src,
        user_list_id.as_deref(),
        &keywords_json,
        &exclude_json,
        &users,
        case_sensitive,
        local_only,
        exclude_bots,
        with_replies,
        with_file,
        exclude_notes_in_sensitive_channel,
    )
    .await
    .map_err(|e| db_err("antennas_create insert", e))?;
    let row = mk_db::antennas::find_antenna_by_id(pool, &id)
        .await
        .map_err(|e| db_err("antennas_create refetch", e))?
        .ok_or_else(|| api_err(ApiError::internal_error()))?;
    Ok(ok_json(pack_antenna(&row)))
}

// ---------------------------------------------------------------------------
// antennas/update — update.ts
// ---------------------------------------------------------------------------

async fn antennas_update(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let antenna_id = required_id(body, "antennaId")?;
    // optional arms (paramDef: only antennaId is required)
    let name = match body.get("name") {
        None | Some(Value::Null) => None,
        Some(_) => Some(parse_name(body)?),
    };
    let src = match body.get("src") {
        None | Some(Value::Null) => None,
        Some(_) => Some(parse_src(body)?),
    };
    let keywords = match body.get("keywords") {
        None | Some(Value::Null) => None,
        Some(_) => Some(parse_str_matrix(body, "keywords")?),
    };
    let exclude_keywords = match body.get("excludeKeywords") {
        None | Some(Value::Null) => None,
        Some(_) => Some(parse_str_matrix(body, "excludeKeywords")?),
    };
    let users = match body.get("users") {
        None | Some(Value::Null) => None,
        Some(_) => Some(parse_str_vec(body, "users")?),
    };
    let case_sensitive = match body.get("caseSensitive") {
        None | Some(Value::Null) => None,
        Some(_) => Some(required_bool(body, "caseSensitive")?),
    };
    let local_only = match body.get("localOnly") {
        None | Some(Value::Null) => None,
        Some(_) => Some(opt_bool(body, "localOnly", false)?),
    };
    let exclude_bots = match body.get("excludeBots") {
        None | Some(Value::Null) => None,
        Some(_) => Some(opt_bool(body, "excludeBots", false)?),
    };
    let with_replies = match body.get("withReplies") {
        None | Some(Value::Null) => None,
        Some(_) => Some(required_bool(body, "withReplies")?),
    };
    let with_file = match body.get("withFile") {
        None | Some(Value::Null) => None,
        Some(_) => Some(required_bool(body, "withFile")?),
    };
    let exclude_notes_in_sensitive_channel = match body.get("excludeNotesInSensitiveChannel") {
        None | Some(Value::Null) => None,
        Some(_) => Some(opt_bool(body, "excludeNotesInSensitiveChannel", false)?),
    };

    // `ps.keywords && ps.excludeKeywords` — the gate only fires when BOTH
    // matrices arrived.
    if let (Some(k), Some(e)) = (&keywords, &exclude_keywords) {
        if both_matrices_blank(k, e) {
            return Err(meta_err(
                "EMPTY_KEYWORD",
                "721aaff6-4e1b-4d88-8de6-877fae9f68c4",
                EMPTY_KEYWORD_MSG,
            ));
        }
    }

    let antenna = mk_db::antennas::find_antenna(pool, antenna_id, me_id)
        .await
        .map_err(|e| db_err("antennas_update find", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_ANTENNA",
                "10c673ac-8852-48eb-aa1f-f5b67f069290",
                NO_SUCH_ANTENNA_MSG,
            )
        })?;

    // `ps.userListId !== undefined` — a PRESENT param (null included) lands
    // the write; an ABSENT key leaves the column untouched. The ownership
    // gate keys on `ps.src === 'list' || antenna.src === 'list'` AND a truthy
    // (string) param — an explicit null or a non-list src resolves NULL
    // (`userList ? userList.id : null`).
    let user_list_arm: Option<Option<String>> = match body.get("userListId") {
        None => None,
        Some(Value::Null) => Some(None),
        // JS truthiness of `ps.userListId` in the gate: '' is falsy → lands NULL
        Some(Value::String(s)) if s.is_empty() => Some(None),
        Some(Value::String(s)) => Some(Some(s.clone())),
        Some(_) => return Err(invalid_param("userListId", "expected a string or null")),
    };
    let user_list_id: Option<Option<String>> = match &user_list_arm {
        Some(Some(id)) => {
            if (src.as_deref() == Some("list")) || antenna.src == "list" {
                let owned = mk_db::lists::find_list(pool, id, me_id)
                    .await
                    .map_err(|e| db_err("antennas_update find_list", e))?;
                if owned.is_none() {
                    return Err(meta_err(
                        "NO_SUCH_USER_LIST",
                        "1c6b35c9-943e-48c2-81e4-2844989407f7",
                        NO_SUCH_USER_LIST_MSG,
                    ));
                }
                Some(Some(id.clone()))
            } else {
                // `userList ? userList.id : null` — the un-probed param lands NULL.
                Some(None)
            }
        }
        other => other.clone(),
    };

    let keywords_json = match &keywords {
        Some(k) => Some(
            serde_json::to_string(k).map_err(|_| invalid_param("keywords", "unserializable"))?,
        ),
        None => None,
    };
    let exclude_json = match &exclude_keywords {
        Some(e) => Some(
            serde_json::to_string(e)
                .map_err(|_| invalid_param("excludeKeywords", "unserializable"))?,
        ),
        None => None,
    };
    mk_db::antennas::apply_patch(
        pool,
        &mk_db::antennas::AntennaPatch {
            owner_id: me_id.to_string(),
            antenna_id: antenna_id.to_string(),
            name,
            src,
            user_list_id,
            keywords: keywords_json,
            exclude_keywords: exclude_json,
            users,
            case_sensitive,
            local_only,
            exclude_bots,
            with_replies,
            with_file,
            exclude_notes_in_sensitive_channel,
        },
    )
    .await
    .map_err(|e| db_err("antennas_update patch", e))?;
    // `return await this.antennaEntityService.pack(antenna.id)` — the UPDATED
    // row re-read.
    let row = mk_db::antennas::find_antenna_by_id(pool, antenna_id)
        .await
        .map_err(|e| db_err("antennas_update refetch", e))?
        .ok_or_else(|| api_err(ApiError::internal_error()))?;
    Ok(ok_json(pack_antenna(&row)))
}

// ---------------------------------------------------------------------------
// antennas/show — show.ts (its own noSuchAntenna id)
// ---------------------------------------------------------------------------

async fn antennas_show(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let antenna_id = required_id(body, "antennaId")?;
    let row = mk_db::antennas::find_antenna(pool, antenna_id, me_id)
        .await
        .map_err(|e| db_err("antennas_show find", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_ANTENNA",
                "c06569fb-b025-4f23-b22d-1fcd20d2816b",
                NO_SUCH_ANTENNA_MSG,
            )
        })?;
    Ok(ok_json(pack_antenna(&row)))
}

// ---------------------------------------------------------------------------
// antennas/list — list.ts
// ---------------------------------------------------------------------------

async fn antennas_list(pool: &mk_db::sqlx::PgPool, me_id: &str) -> Result<Response, Response> {
    let rows = mk_db::antennas::list_antennas(pool, me_id)
        .await
        .map_err(|e| db_err("antennas_list", e))?;
    let out: Vec<Value> = rows.iter().map(pack_antenna).collect();
    Ok(ok_json(json!(out)))
}

// ---------------------------------------------------------------------------
// antennas/delete — delete.ts (its own noSuchAntenna id, 204 response)
// ---------------------------------------------------------------------------

async fn antennas_delete(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let antenna_id = required_id(body, "antennaId")?;
    let row = mk_db::antennas::find_antenna(pool, antenna_id, me_id)
        .await
        .map_err(|e| db_err("antennas_delete find", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_ANTENNA",
                "b34dcf9d-348f-44bb-99d0-6c9314cfe2df",
                NO_SUCH_ANTENNA_MSG,
            )
        })?;
    mk_db::antennas::delete_antenna(pool, &row.id)
        .await
        .map_err(|e| db_err("antennas_delete", e))?;
    Ok(ok_empty())
}

// ---------------------------------------------------------------------------
// antennas/remove-note — remove-note.ts (notes-side noSuchAntenna id, 204)
// ---------------------------------------------------------------------------

async fn antennas_remove_note(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let antenna_id = required_id(body, "antennaId")?;
    let note_id = required_id(body, "noteId")?;
    let row = mk_db::antennas::find_antenna(pool, antenna_id, me_id)
        .await
        .map_err(|e| db_err("antennas_remove_note find", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_ANTENNA",
                "850926e0-fd3b-49b6-b69a-b28a5dbd82fe",
                NO_SUCH_ANTENNA_MSG,
            )
        })?;
    // fanoutTimelineService.remove — an unknown noteId still succeeds.
    mk_db::antennas::remove_note_from_antenna(pool, &row.id, note_id)
        .await
        .map_err(|e| db_err("antennas_remove_note", e))?;
    Ok(ok_empty())
}

// ---------------------------------------------------------------------------
// antennas/notes — notes.ts (feed read; requireCredential + owner scope +
// the re-activation write; `excludeNotesInSensitiveChannel` is a FANOUT-only
// flag and is ignored here, per the upstream comment)
// ---------------------------------------------------------------------------

async fn antennas_notes(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let antenna_id = required_id(body, "antennaId")?;
    let row = mk_db::antennas::find_antenna(pool, antenna_id, me_id)
        .await
        .map_err(|e| db_err("antennas_notes find", e))?
        .ok_or_else(|| {
            meta_err(
                "NO_SUCH_ANTENNA",
                "850926e0-fd3b-49b6-b69a-b28a5dbd82fe",
                NO_SUCH_ANTENNA_MSG,
            )
        })?;
    // `antenna.isActive = true; antenna.lastUsedAt = new Date();`
    mk_db::antennas::mark_used(pool, &row.id)
        .await
        .map_err(|e| db_err("antennas_notes mark_used", e))?;

    // paramDef pager: limit 1..100 default 10; the id form wins over the date
    // form (`ps.untilId ?? idService.gen(ps.untilDate)`, a 0 date is falsy).
    let limit = body
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(10)
        .clamp(1, 100);
    let until_id = match opt_id(body, "untilId") {
        Some(u) => Some(u.to_string()),
        None => body
            .get("untilDate")
            .and_then(|v| v.as_i64())
            .filter(|&ms| ms != 0)
            .map(mk_types::id::gen_aid),
    };
    let since_id = match opt_id(body, "sinceId") {
        Some(s) => Some(s.to_string()),
        None => body
            .get("sinceDate")
            .and_then(|v| v.as_i64())
            .filter(|&ms| ms != 0)
            .map(mk_types::id::gen_aid),
    };

    let followees = mk_db::social::followee_ids(pool, me_id)
        .await
        .map_err(|e| db_err("antennas_notes followee_ids", e))?;
    let ids = mk_db::antennas::antenna_notes_page(
        pool,
        &row.id,
        me_id,
        &followees,
        since_id.as_deref(),
        until_id.as_deref(),
        limit,
    )
    .await
    .map_err(|e| db_err("antenna_notes_page", e))?;

    // Shared me-aware pack pipeline (the notes/show surface the census
    // deepStrictEqual-compares against).
    let mut out = Vec::with_capacity(ids.len());
    for id in &ids {
        match super::pack_note_for(state, pool, id, Some(me_id)).await {
            Ok(v) => out.push(v),
            Err(r) => return Err(*r),
        }
    }
    Ok(ok_json(json!(out)))
}
