//! avatar decorations — ports of `endpoints/get-avatar-decorations.ts` and
//! `endpoints/admin/avatar-decorations/{create,delete,list,update}.ts`
//! through `core/AvatarDecorationService.ts`. Db paths live in
//! `mk_db::avatar_decoration` (the table ships in the 0001 baseline; no
//! migration).
//!
//! Gate: upstream `get-avatar-decorations` is `requireCredential: false`
//! (guest-ok, optional me for role visibility); the admin/* endpoints are
//! `requireCredential` + `requiredRolePolicy: canManageAvatarDecorations`.
//! The shared `admin::gate` runs the credential → suspended → requireAdmin
//! chain, so a non-admin gets the same ROLE_PERMISSION_DENIED shape as the
//! other admin arms.
//!
//! Status mapping: void handlers answer 204 NO_CONTENT. The upstream
//! `update`/`delete` use `findOneByOrFail`, so a missing id is a plain
//! thrown error → 500 INTERNAL_ERROR (no declared NO_SUCH_* meta). The
//! admin `list` ignores its pagination params upstream (`getAll(true)`), and
//! `createdAt` is derived from the AID (`IdService.parse`).

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
        "get-avatar-decorations"
            | "admin/avatar-decorations/create"
            | "admin/avatar-decorations/delete"
            | "admin/avatar-decorations/list"
            | "admin/avatar-decorations/update"
    )
}

type Check<T = Response> = Result<T, Response>;

/// Entry from the avatar-decorations dispatch arm in mod.rs (body pre-parsed,
/// token pre-extracted).
pub(super) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<&str>,
) -> Response {
    let Some(pool) = &state.pool else {
        return internal();
    };
    // Every admin/* name rides the shared requireAdmin gate.
    if name.starts_with("admin/") {
        if let Err(resp) = gate(state, token, Gate::Admin).await {
            return resp;
        }
    }
    let result: Check<Response> = match name {
        "get-avatar-decorations" => public_list(pool, token).await,
        "admin/avatar-decorations/create" => create(pool, body).await,
        "admin/avatar-decorations/delete" => delete(pool, body).await,
        "admin/avatar-decorations/list" => admin_list(pool).await,
        "admin/avatar-decorations/update" => update(pool, body).await,
        _ => Ok(api_err(ApiError::unknown_endpoint())),
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

// --- shared helpers ---

/// Plain handler Error → ApiCallService.#onExecError → 500 INTERNAL_ERROR.
fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_avatar_decorations", error = %e, "db error: {context}");
    internal()
}

fn invalid_param(param: &str, reason: &str) -> Response {
    api_err(ApiError::invalid_param(json!({
        "param": param,
        "reason": reason,
    })))
}

fn iso_from_id(id: &str) -> String {
    mk_types::id::parse_aid_date(id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default()
}

fn ms_iso(ms: i64) -> String {
    mk_core::services::signup::ms_to_iso(ms)
}

// --- params ---

fn require_str(body: &Value, key: &str) -> Check<String> {
    match body.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        Some(Value::String(_)) => Err(invalid_param(key, "must NOT be empty")),
        None | Some(Value::Null) => Err(invalid_param(
            key,
            &format!("must have required property '{key}'"),
        )),
        Some(_) => Err(invalid_param(key, "must be string")),
    }
}

/// Required string, empty allowed (`description` has no minLength).
fn require_string(body: &Value, key: &str) -> Check<String> {
    match body.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        None | Some(Value::Null) => Err(invalid_param(
            key,
            &format!("must have required property '{key}'"),
        )),
        Some(_) => Err(invalid_param(key, "must be string")),
    }
}

/// Optional string with `minLength: 1` (name/url).
fn opt_non_empty(body: &Value, key: &str) -> Check<Option<String>> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
        Some(Value::String(_)) => Err(invalid_param(key, "must NOT be empty")),
        Some(_) => Err(invalid_param(key, "must be string")),
    }
}

/// Optional string, empty allowed (description).
fn opt_string(body: &Value, key: &str) -> Check<Option<String>> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(invalid_param(key, "must be string")),
    }
}

/// `nullable: true` string: absent/null → None, string → Some.
fn opt_nullable_string(body: &Value, key: &str) -> Check<Option<String>> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(invalid_param(key, "must be string")),
    }
}

/// `{ type: 'array', items: { type: 'string' } }` optional param.
fn opt_string_array(body: &Value, key: &str) -> Check<Option<Vec<String>>> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::String(s) => out.push(s.clone()),
                    _ => return Err(invalid_param(key, "must be an array of strings")),
                }
            }
            Ok(Some(out))
        }
        Some(_) => Err(invalid_param(key, "must be an array")),
    }
}

/// Update-only `category` patch: absent → skip, null → write NULL.
fn category_patch(body: &Value) -> Check<Option<Option<String>>> {
    match body.get("category") {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(s)) => Ok(Some(Some(s.clone()))),
        Some(_) => Err(invalid_param("category", "must be string")),
    }
}

// --- handlers ---

/// `POST /api/get-avatar-decorations` — guest-ok; a signed-in caller sees all
/// role ids, a guest only the public-role subset (upstream visibleRoles).
async fn public_list(pool: &mk_db::sqlx::PgPool, token: Option<&str>) -> Check<Response> {
    let me_id = match token {
        Some(t) => Some(super::social::require_checked_user(pool, Some(t), false).await?),
        None => None,
    };
    let rows = mk_db::avatar_decoration::list_all(pool)
        .await
        .map_err(|e| db_err("list_all[get-avatar-decorations]", e))?;
    let visible = mk_db::avatar_decoration::visible_role_ids(pool, me_id.is_none())
        .await
        .map_err(|e| db_err("visible_role_ids[get-avatar-decorations]", e))?;
    let arr: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let role_ids: Vec<&String> = r
                .role_ids
                .iter()
                .filter(|role_id| visible.contains(role_id))
                .collect();
            json!({
                "id": r.id,
                "name": r.name,
                "description": r.description,
                "url": r.url,
                "roleIdsThatCanBeUsedThisDecoration": role_ids,
                "category": r.category,
            })
        })
        .collect();
    Ok(Json(arr).into_response())
}

/// `POST /api/admin/avatar-decorations/create` — inserts the row and returns
/// the entity shape (`createdAt` from the AID, `updatedAt` null).
async fn create(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let name = require_str(body, "name")?;
    let description = require_string(body, "description")?;
    let url = require_str(body, "url")?;
    let role_ids =
        opt_string_array(body, "roleIdsThatCanBeUsedThisDecoration")?.unwrap_or_default();
    let category = opt_nullable_string(body, "category")?;

    let id = mk_types::id::gen_aid_now();
    mk_db::avatar_decoration::insert(
        pool,
        &id,
        &url,
        &name,
        &description,
        &role_ids,
        category.as_deref(),
    )
    .await
    .map_err(|e| db_err("insert[create]", e))?;

    Ok(Json(json!({
        "id": id,
        "createdAt": iso_from_id(&id),
        "updatedAt": Value::Null,
        "name": name,
        "description": description,
        "url": url,
        "roleIdsThatCanBeUsedThisDecoration": role_ids,
        "category": category,
    }))
    .into_response())
}

/// `POST /api/admin/avatar-decorations/list` — every row (the upstream params
/// are accepted but unused: `getAll(true)`).
async fn admin_list(pool: &mk_db::sqlx::PgPool) -> Check<Response> {
    let rows = mk_db::avatar_decoration::list_all(pool)
        .await
        .map_err(|e| db_err("list_all[admin]", e))?;
    let arr: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "createdAt": iso_from_id(&r.id),
                "updatedAt": r.updated_at_ms.map(ms_iso),
                "name": r.name,
                "description": r.description,
                "url": r.url,
                "roleIdsThatCanBeUsedThisDecoration": r.role_ids,
                "category": r.category,
            })
        })
        .collect();
    Ok(Json(arr).into_response())
}

/// `POST /api/admin/avatar-decorations/update` — `findOneByOrFail` then the
/// partial patch (always bumps `updatedAt`); void → 204.
async fn update(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let id = require_str(body, "id")?;
    let patch = mk_db::avatar_decoration::AvatarDecorationPatch {
        name: opt_non_empty(body, "name")?,
        description: opt_string(body, "description")?,
        url: opt_non_empty(body, "url")?,
        role_ids: opt_string_array(body, "roleIdsThatCanBeUsedThisDecoration")?,
        category: category_patch(body)?,
    };
    let found = mk_db::avatar_decoration::find_by_id(pool, &id)
        .await
        .map_err(|e| db_err("find_by_id[update]", e))?;
    if found.is_none() {
        // findOneByOrFail throws → 500 INTERNAL_ERROR.
        return Err(internal());
    }
    mk_db::avatar_decoration::update(pool, &id, &patch)
        .await
        .map_err(|e| db_err("update", e))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `POST /api/admin/avatar-decorations/delete` — `findOneByOrFail` then the
/// delete; void → 204.
async fn delete(pool: &mk_db::sqlx::PgPool, body: &Value) -> Check<Response> {
    let id = require_str(body, "id")?;
    let found = mk_db::avatar_decoration::find_by_id(pool, &id)
        .await
        .map_err(|e| db_err("find_by_id[delete]", e))?;
    if found.is_none() {
        return Err(internal());
    }
    mk_db::avatar_decoration::delete(pool, &id)
        .await
        .map_err(|e| db_err("delete", e))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
