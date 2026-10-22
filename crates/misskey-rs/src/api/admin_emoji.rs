//! `admin/emoji/*` family — ports of
//! `endpoints/admin/emoji/{add,delete,list,update,copy,set-category-bulk,
//! set-license-bulk,add-aliases-bulk,remove-aliases-bulk,set-aliases-bulk,
//! delete-bulk,list-remote}.ts` through `CustomEmojiService` +
//! `EmojiEntityService.packDetailed`. Db paths live in `mk_db::emoji` (the
//! table ships in the 0001 baseline; no migration).
//!
//! Gate: upstream `requireCredential` + `requiredRolePolicy:
//! canManageCustomEmojis` (administrator). The shared `admin::gate` runs the
//! credential → suspended → requireAdmin chain, so a non-admin gets the same
//! ROLE_PERMISSION_DENIED shape as the other admin arms.
//!
//! DOCUMENTED SKIPS:
//! - `admin/emoji/import-zip`: needs the drive/zip storage lane
//!   (ExtractImagesService), absent here. The name falls through to the
//!   admin/* catch-all → UNKNOWN_API_ENDPOINT.
//! - `admin/emoji/copy`: upstream re-uploads the source image through
//!   `DriveService.uploadFromUrl` before inserting. That storage lane is
//!   absent, so the copy inserts a fresh local row referencing the source's
//!   existing `originalUrl`/`publicUrl` (no drive-file duplicate). The
//!   duplicate-name gate and the returned EmojiDetailed shape are unchanged.
//!
//! Status mapping: these meta ApiErrors carry no httpStatusCode → kind
//! 'client' defaults to 400; void handlers answer 204 NO_CONTENT. The
//! upstream `delete` uses `findOneByOrFail`, so a missing id is a plain
//! thrown error → 500 INTERNAL_ERROR (the declared NO_SUCH_EMOJI is unused
//! there); `update`/`copy` return NO_SUCH_EMOJI 400 as declared.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

use super::admin::{Gate, gate};

/// Endpoint names owned by this module (all POST-only).
pub(super) fn handles(name: &str) -> bool {
    matches!(
        name,
        "admin/emoji/add"
            | "admin/emoji/delete"
            | "admin/emoji/list"
            | "admin/emoji/update"
            | "admin/emoji/copy"
            | "admin/emoji/set-category-bulk"
            | "admin/emoji/set-license-bulk"
            | "admin/emoji/add-aliases-bulk"
            | "admin/emoji/remove-aliases-bulk"
            | "admin/emoji/set-aliases-bulk"
            | "admin/emoji/delete-bulk"
            | "admin/emoji/list-remote"
            | "v2/admin/emoji/list"
    )
}

type Check<T = Response> = Result<T, Response>;

/// Entry from the admin-emoji dispatch arm in mod.rs (body pre-parsed, token
/// pre-extracted).
pub(super) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<&str>,
) -> Response {
    // Shared requireAdmin gate (root bypass + role check). Every endpoint in
    // this module is administrator-only.
    if let Err(resp) = gate(state, token, Gate::Admin).await {
        return resp;
    }
    let Some(pool) = &state.pool else {
        return internal();
    };
    let result: Check<Response> = match name {
        "admin/emoji/add" => add(pool, body).await,
        "admin/emoji/delete" => delete(pool, body).await,
        "admin/emoji/list" => list(pool, body).await,
        "admin/emoji/update" => update(pool, body).await,
        "admin/emoji/copy" => copy(pool, body).await,
        "admin/emoji/set-category-bulk" => set_category_bulk(pool, body).await,
        "admin/emoji/set-license-bulk" => set_license_bulk(pool, body).await,
        "admin/emoji/add-aliases-bulk" => add_aliases_bulk(pool, body).await,
        "admin/emoji/remove-aliases-bulk" => remove_aliases_bulk(pool, body).await,
        "admin/emoji/set-aliases-bulk" => set_aliases_bulk(pool, body).await,
        "admin/emoji/delete-bulk" => delete_bulk(pool, body).await,
        "admin/emoji/list-remote" => list_remote(pool, body).await,
        "v2/admin/emoji/list" => v2_list(pool, body).await,
        _ => unreachable!("checked by handles()"),
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

// --- error shapes ---

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_admin_emoji", error = %e, "db error: {context}");
    internal()
}

/// Meta error without httpStatusCode: kind 'client' → default 400.
fn meta_err(code: &str, id: &str, message: &str) -> Response {
    api_err(ApiError::new(
        400,
        mk_types::api_error::ApiErrorKind::Client,
        code,
        id,
        message,
    ))
}

fn no_such_file(id: &str) -> Response {
    meta_err("NO_SUCH_FILE", id, "No such file.")
}

fn unsupported_file_type() -> Response {
    meta_err(
        "UNSUPPORTED_FILE_TYPE",
        "f7599d96-8750-af68-1633-9575d625c1a7",
        "Unsupported file type.",
    )
}

fn duplicate_name() -> Response {
    meta_err(
        "DUPLICATE_NAME",
        "f7a3462c-4e6e-4069-8421-b9bd4f4c3975",
        "Duplicate name.",
    )
}

fn no_such_emoji(id: &str) -> Response {
    meta_err("NO_SUCH_EMOJI", id, "No such emoji.")
}

fn same_name_emoji_exists() -> Response {
    meta_err(
        "SAME_NAME_EMOJI_EXISTS",
        "7180fe9d-1ee3-bff9-647d-fe9896d2ffb8",
        "Emoji that have same name already exists.",
    )
}

// --- params ---

/// `FILE_TYPE_IMAGE` (const.ts L29-39) — the add.ts gate runs on the raw
/// `driveFile.type`, before the webpublic fallback.
const FILE_TYPE_IMAGE: &[&str] = &[
    "image/png",
    "image/gif",
    "image/jpeg",
    "image/webp",
    "image/avif",
    "image/apng",
    "image/bmp",
    "image/tiff",
    "image/x-icon",
];

fn require_str(body: &Value, key: &str) -> Check<String> {
    match body.get(key) {
        None | Some(Value::Null) => Err(super::invalid_param(
            key,
            &format!("must have required property '{key}'"),
        )),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(super::invalid_param(key, "must be string")),
    }
}

fn require_name(body: &Value) -> Check<String> {
    let name = require_str(body, "name")?;
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(super::invalid_param(
            "name",
            "must match pattern \"^[a-zA-Z0-9_]+$\"",
        ));
    }
    Ok(name)
}

fn opt_string(body: &Value, key: &str) -> Check<Option<String>> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(super::invalid_param(key, "must be string")),
    }
}

fn opt_i64(body: &Value, key: &str) -> Check<Option<i64>> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_i64() {
            Some(n) => Ok(Some(n)),
            None => Err(super::invalid_param(key, "must be integer")),
        },
    }
}

fn opt_string_array(body: &Value, key: &str) -> Check<Option<Vec<String>>> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::String(s) => out.push(s.clone()),
                    _ => return Err(super::invalid_param(key, "must be array of strings")),
                }
            }
            Ok(Some(out))
        }
        Some(_) => Err(super::invalid_param(key, "must be array")),
    }
}

fn require_string_array(body: &Value, key: &str) -> Check<Vec<String>> {
    match opt_string_array(body, key)? {
        Some(v) => Ok(v),
        None => Err(super::invalid_param(
            key,
            &format!("must have required property '{key}'"),
        )),
    }
}

/// `{ type: 'integer', minimum: 1, maximum: 100, default: 10 }`.
fn parse_limit(body: &Value) -> Check<i64> {
    match body.get("limit") {
        None | Some(Value::Null) => Ok(10),
        Some(v) => match v.as_i64() {
            Some(n) if (1..=100).contains(&n) => Ok(n),
            _ => Err(super::invalid_param(
                "limit",
                "must be an integer between 1 and 100",
            )),
        },
    }
}

/// Fold sinceDate/untilDate into id boundaries exactly like
/// `makePaginationQuery`: the id params win, and a date pair is only
/// consulted when its id counterpart is absent. Returns `(since, until, asc)`.
fn pagination(body: &Value) -> Check<(Option<String>, Option<String>, bool)> {
    let since_id = opt_string(body, "sinceId")?;
    let until_id = opt_string(body, "untilId")?;
    let since_date = opt_i64(body, "sinceDate")?;
    let until_date = opt_i64(body, "untilDate")?;

    let (since, until) = if since_id.is_some() && until_id.is_some() {
        (since_id, until_id)
    } else if since_id.is_some() {
        (since_id, None)
    } else if until_id.is_some() {
        (None, until_id)
    } else if since_date.is_some() && until_date.is_some() {
        (
            since_date.map(mk_types::id::gen_aid),
            until_date.map(mk_types::id::gen_aid),
        )
    } else if since_date.is_some() {
        (since_date.map(mk_types::id::gen_aid), None)
    } else if until_date.is_some() {
        (None, until_date.map(mk_types::id::gen_aid))
    } else {
        (None, None)
    };
    let asc = since.is_some() && until.is_none();
    Ok((since, until, asc))
}

// --- pack ---

/// `EmojiEntityService.packDetailed` (the list/add/copy response shape).
fn pack(e: &mk_db::emoji::EmojiRow) -> Value {
    json!({
        "id": e.id,
        "aliases": e.aliases,
        "name": e.name,
        "category": e.category,
        "host": e.host,
        // `emoji.publicUrl || emoji.originalUrl` (publicUrl is NOT NULL).
        "url": if e.public_url.is_empty() { e.original_url.clone() } else { e.public_url.clone() },
        "license": e.license,
        "isSensitive": e.is_sensitive,
        "localOnly": e.local_only,
        "roleIdsThatCanBeUsedThisEmojiAsReaction": e.role_ids,
    })
}

/// `EmojiDetailedAdmin` pack used by the v2 list (superset of `pack`).
fn v2_pack(e: &mk_db::emoji::EmojiRow) -> Value {
    json!({
        "id": e.id,
        "aliases": e.aliases,
        "name": e.name,
        "category": e.category,
        "host": e.host,
        "url": if e.public_url.is_empty() { e.original_url.clone() } else { e.public_url.clone() },
        "license": e.license,
        "isSensitive": e.is_sensitive,
        "localOnly": e.local_only,
        "roleIdsThatCanBeUsedThisEmojiAsReaction": e.role_ids,
        "originalUrl": e.original_url,
        "publicUrl": e.public_url,
        "uri": e.uri,
        "type": e.file_type,
    })
}

/// `v2/admin/emoji/list.ts`: filtered, page-based listing returning
/// `{emojis, count, allCount, allPages}`. Divergences: `sinceId`/`untilId`
/// id cursors are accepted but only `page` is used for paging; `roleIds`
/// filtering is not applied; `updatedAtFrom/To` are not applied.
async fn v2_list(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let q = match body.get("query") {
        None | Some(Value::Null) => None,
        Some(v @ Value::Object(_)) => Some(v),
        Some(_) => return Err(super::invalid_param("query", "must be object")),
    };
    let qs = |k: &str| -> Option<String> {
        q.and_then(|o| o.get(k))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    let qb = |k: &str| -> Option<bool> { q.and_then(|o| o.get(k)).and_then(|v| v.as_bool()) };
    let host_type = qs("hostType").unwrap_or_else(|| "all".to_string());
    let (name, host, uri, type_, aliases, category, license) = (
        qs("name"),
        qs("host"),
        qs("uri"),
        qs("type"),
        qs("aliases"),
        qs("category"),
        qs("license"),
    );
    let filter = mk_db::emoji::AdminEmojiFilter {
        name: name.as_deref(),
        host: host.as_deref(),
        uri: uri.as_deref(),
        type_: type_.as_deref(),
        aliases: aliases.as_deref(),
        category: category.as_deref(),
        license: license.as_deref(),
        is_sensitive: qb("isSensitive"),
        local_only: qb("localOnly"),
        host_type: Some(host_type.as_str()),
    };
    let limit = parse_limit(body)?;
    let page = match body.get("page") {
        None | Some(Value::Null) => 0i64,
        Some(v) => v
            .as_i64()
            .filter(|n| *n >= 0)
            .ok_or_else(|| super::invalid_param("page", "must be a non-negative integer"))?,
    };
    let sort = body
        .get("sortKeys")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .unwrap_or("-id");
    let (col, asc) = match sort {
        "-id" => ("id", false),
        "+id" => ("id", true),
        "-name" => ("name", false),
        "+name" => ("name", true),
        "-aliases" => ("aliases", false),
        "+aliases" => ("aliases", true),
        "-license" => ("license", false),
        "+license" => ("license", true),
        "-isSensitive" => ("isSensitive", false),
        "+isSensitive" => ("isSensitive", true),
        "-category" => ("category", false),
        "+category" => ("category", true),
        "-host" => ("host", false),
        "+host" => ("host", true),
        _ => return Err(super::invalid_param("sortKeys", "unsupported sort key")),
    };
    let (rows, total) = mk_db::emoji::list_admin(pool, &filter, limit, page * limit, col, asc)
        .await
        .map_err(|e| db_err("list_admin", e))?;
    let all_pages = (total + limit - 1) / limit;
    Ok(ok_json(json!({
        "emojis": rows.iter().map(v2_pack).collect::<Vec<_>>(),
        "count": rows.len(),
        "allCount": total,
        "allPages": all_pages,
    })))
}

fn ok_json(v: Value) -> Response {
    Json(v).into_response()
}

fn no_content() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

// --- endpoints ---

/// `admin/emoji/add.ts`: drive file → duplicate-name → image-type checks,
/// then insert a local emoji from the file's URL/type.
async fn add(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let name = require_name(body)?;
    let file_id = require_str(body, "fileId")?;
    let category = super::opt_nullable_str(body, "category")?.flatten();
    let aliases = opt_string_array(body, "aliases")?.unwrap_or_default();
    let license = super::opt_nullable_str(body, "license")?.flatten();
    let is_sensitive = super::opt_bool(body, "isSensitive")?.unwrap_or(false);
    let local_only = super::opt_bool(body, "localOnly")?.unwrap_or(false);
    let role_ids =
        opt_string_array(body, "roleIdsThatCanBeUsedThisEmojiAsReaction")?.unwrap_or_default();

    let file = mk_db::drive::find_drive_file(pool, &file_id)
        .await
        .map_err(|e| db_err("find_drive_file", e))?;
    let Some(file) = file else {
        return Err(no_such_file("fc46b5a4-6b92-4c33-ac66-b806659bb5cf"));
    };
    if mk_db::emoji::local_name_exists(pool, &name)
        .await
        .map_err(|e| db_err("local_name_exists", e))?
    {
        return Err(duplicate_name());
    }
    if !FILE_TYPE_IMAGE.contains(&file.file_type.as_str()) {
        return Err(unsupported_file_type());
    }

    let public_url = file
        .webpublic_url
        .clone()
        .unwrap_or_else(|| file.url.clone());
    let file_type = file
        .webpublic_type
        .clone()
        .unwrap_or_else(|| file.file_type.clone());
    let row = mk_db::emoji::EmojiRow {
        id: mk_types::id::gen_aid_now(),
        name,
        host: None,
        original_url: file.url.clone(),
        uri: None,
        file_type: Some(file_type),
        aliases,
        category,
        public_url,
        license,
        local_only,
        is_sensitive,
        role_ids,
    };
    mk_db::emoji::insert(pool, &row)
        .await
        .map_err(|e| db_err("insert", e))?;
    Ok(ok_json(pack(&row)))
}

/// `admin/emoji/delete.ts`: `findOneByOrFail` (missing → 500) then delete;
/// void → 204.
async fn delete(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let id = require_str(body, "id")?;
    let found = mk_db::emoji::find_by_id(pool, &id)
        .await
        .map_err(|e| db_err("find_by_id", e))?;
    if found.is_none() {
        // Upstream findOneByOrFail throws a plain Error → #onExecError 500.
        return Err(internal());
    }
    mk_db::emoji::delete(pool, &id)
        .await
        .map_err(|e| db_err("delete", e))?;
    Ok(no_content())
}

/// `admin/emoji/list.ts`: local rows, optional query token/substring filter.
async fn list(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let query = super::opt_nullable_str(body, "query")?.flatten();
    let limit = parse_limit(body)?;
    let (since, until, asc) = pagination(body)?;
    let rows = mk_db::emoji::list_local(
        pool,
        query.as_deref(),
        limit,
        since.as_deref(),
        until.as_deref(),
        asc,
    )
    .await
    .map_err(|e| db_err("list_local", e))?;
    Ok(ok_json(Value::Array(rows.iter().map(pack).collect())))
}

/// `admin/emoji/update.ts`: anyOf(id | name) lookup, optional fileId swap,
/// duplicate-name check only when the id+name pair renames the emoji; the
/// service returns null → void → 204.
async fn update(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let id = match body.get("id") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return Err(super::invalid_param("id", "must be string")),
    };
    let name = match body.get("name") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            if s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(super::invalid_param(
                    "name",
                    "must match pattern \"^[a-zA-Z0-9_]+$\"",
                ));
            }
            Some(s.clone())
        }
        Some(_) => return Err(super::invalid_param("name", "must be string")),
    };
    if id.is_none() && name.is_none() {
        return Err(super::invalid_param("", "must match a schema in anyOf"));
    }

    // Optional file swap (looked up before the emoji, like upstream).
    let file = match opt_string(body, "fileId")? {
        Some(file_id) => {
            let file = mk_db::drive::find_drive_file(pool, &file_id)
                .await
                .map_err(|e| db_err("find_drive_file", e))?;
            match file {
                Some(f) => Some(f),
                None => return Err(no_such_file("14fb9fd9-0731-4e2f-aeb9-f09e4740333d")),
            }
        }
        None => None,
    };

    let current = if let Some(id) = &id {
        mk_db::emoji::find_by_id(pool, id)
            .await
            .map_err(|e| db_err("find_by_id", e))?
    } else {
        mk_db::emoji::find_local_by_name(pool, name.as_deref().unwrap())
            .await
            .map_err(|e| db_err("find_local_by_name", e))?
    };
    let Some(current) = current else {
        return Err(no_such_emoji("684dec9d-a8c2-4364-9aa8-456c49cb1dc8"));
    };

    // Rename duplicate check: only id + a *different* name (name-only updates
    // look the emoji up by that same name).
    if let (Some(_), Some(new_name)) = (&id, &name) {
        if *new_name != current.name
            && mk_db::emoji::local_name_exists(pool, new_name)
                .await
                .map_err(|e| db_err("local_name_exists", e))?
        {
            return Err(same_name_emoji_exists());
        }
    }

    let patch = mk_db::emoji::EmojiPatch {
        name: name.clone(),
        category: super::opt_nullable_str(body, "category")?,
        aliases: opt_string_array(body, "aliases")?,
        license: super::opt_nullable_str(body, "license")?,
        is_sensitive: super::opt_bool(body, "isSensitive")?,
        local_only: super::opt_bool(body, "localOnly")?,
        role_ids: opt_string_array(body, "roleIdsThatCanBeUsedThisEmojiAsReaction")?,
        original_url: file.as_ref().map(|f| f.url.clone()),
        public_url: file
            .as_ref()
            .map(|f| f.webpublic_url.clone().unwrap_or_else(|| f.url.clone())),
        file_type: file.as_ref().map(|f| {
            f.webpublic_type
                .clone()
                .unwrap_or_else(|| f.file_type.clone())
        }),
    };
    mk_db::emoji::update(pool, &current.id, &patch)
        .await
        .map_err(|e| db_err("update", e))?;
    Ok(no_content())
}

/// `admin/emoji/copy.ts` — storage-lane deviation documented in the module
/// docs: no drive re-upload, the local row reuses the source URLs. The
/// duplicate-name gate is unchanged (so copying a local emoji 400s, as
/// upstream's checkDuplicate does).
async fn copy(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let emoji_id = require_str(body, "emojiId")?;
    let source = mk_db::emoji::find_by_id(pool, &emoji_id)
        .await
        .map_err(|e| db_err("find_by_id", e))?;
    let Some(source) = source else {
        return Err(no_such_emoji("e2785b66-dca3-4087-9cac-b93c541cc425"));
    };
    if mk_db::emoji::local_name_exists(pool, &source.name)
        .await
        .map_err(|e| db_err("local_name_exists", e))?
    {
        return Err(duplicate_name());
    }
    let row = mk_db::emoji::EmojiRow {
        id: mk_types::id::gen_aid_now(),
        name: source.name,
        host: None,
        original_url: source.original_url,
        uri: None,
        file_type: source.file_type,
        aliases: source.aliases,
        category: source.category,
        public_url: source.public_url,
        license: source.license,
        local_only: source.local_only,
        is_sensitive: source.is_sensitive,
        role_ids: source.role_ids,
    };
    mk_db::emoji::insert(pool, &row)
        .await
        .map_err(|e| db_err("insert[copy]", e))?;
    Ok(ok_json(pack(&row)))
}

/// `admin/emoji/set-category-bulk.ts` — absent/null category both reset.
async fn set_category_bulk(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let ids = require_string_array(body, "ids")?;
    let category = super::opt_nullable_str(body, "category")?.flatten();
    mk_db::emoji::set_category_bulk(pool, &ids, category.as_deref())
        .await
        .map_err(|e| db_err("set_category_bulk", e))?;
    Ok(no_content())
}

/// `admin/emoji/set-license-bulk.ts`.
async fn set_license_bulk(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let ids = require_string_array(body, "ids")?;
    let license = super::opt_nullable_str(body, "license")?.flatten();
    mk_db::emoji::set_license_bulk(pool, &ids, license.as_deref())
        .await
        .map_err(|e| db_err("set_license_bulk", e))?;
    Ok(no_content())
}

/// `admin/emoji/add-aliases-bulk.ts`.
async fn add_aliases_bulk(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let ids = require_string_array(body, "ids")?;
    let aliases = require_string_array(body, "aliases")?;
    mk_db::emoji::add_aliases_bulk(pool, &ids, &aliases)
        .await
        .map_err(|e| db_err("add_aliases_bulk", e))?;
    Ok(no_content())
}

/// `admin/emoji/remove-aliases-bulk.ts`.
async fn remove_aliases_bulk(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let ids = require_string_array(body, "ids")?;
    let aliases = require_string_array(body, "aliases")?;
    mk_db::emoji::remove_aliases_bulk(pool, &ids, &aliases)
        .await
        .map_err(|e| db_err("remove_aliases_bulk", e))?;
    Ok(no_content())
}

/// `admin/emoji/set-aliases-bulk.ts`.
async fn set_aliases_bulk(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let ids = require_string_array(body, "ids")?;
    let aliases = require_string_array(body, "aliases")?;
    mk_db::emoji::set_aliases_bulk(pool, &ids, &aliases)
        .await
        .map_err(|e| db_err("set_aliases_bulk", e))?;
    Ok(no_content())
}

/// `admin/emoji/delete-bulk.ts`.
async fn delete_bulk(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let ids = require_string_array(body, "ids")?;
    mk_db::emoji::delete_bulk(pool, &ids)
        .await
        .map_err(|e| db_err("delete_bulk", e))?;
    Ok(no_content())
}

/// `admin/emoji/list-remote.ts`.
async fn list_remote(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let query = super::opt_nullable_str(body, "query")?.flatten();
    let host = super::opt_nullable_str(body, "host")?.flatten();
    let limit = parse_limit(body)?;
    let (since, until, _asc) = pagination(body)?;
    let rows = mk_db::emoji::list_remote(
        pool,
        query.as_deref(),
        host.as_deref(),
        since.as_deref(),
        until.as_deref(),
        limit,
    )
    .await
    .map_err(|e| db_err("list_remote", e))?;
    Ok(ok_json(Value::Array(rows.iter().map(pack).collect())))
}
