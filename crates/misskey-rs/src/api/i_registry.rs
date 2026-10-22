//! `i/registry/*` + `i/signin-history` — ports of
//! `endpoints/i/registry/{get,set,remove,get-all,get-detail,keys}.ts` and
//! `endpoints/i/signin-history.ts` (via `core/RegistryApiService.ts`).
//!
//! All names are `requireCredential: true` (`kind: read:account` /
//! `write:account`) and read/write only the caller's rows. Auth runs before
//! paramDef validation (ApiCallService authenticates first), so `{}` answers
//! CREDENTIAL_REQUIRED.
//!
//! STORAGE (0001 `registry_item`): `key` is the caller's `string[]` joined
//! with `/` (`key.join('/')`); `scope` is the array column compared by exact
//! array equality. `i/registry/set` accepts the upstream array scope and the
//! brief's single `'client' | 'server'` spelling; reads default the scope to
//! `['client']` (upstream paramDef default). `remove` deletes the exact key
//! plus its `key/%` subtree.
//!
//! RESPONSES: `get` → the stored JSON value, or 204 when absent;
//! `get-all` → the nested object rebuilt from the `/`-joined keys;
//! `get-detail` → `{ item: <row>, scopes: string[][] }` (all scopes that hold
//! the key); `keys` → `string[][]`; `set`/`remove` → 204. `signin-history`
//! pages the caller's `signin` rows id DESC with the standard
//! `limit`/`sinceId`/`untilId` shape; the row's createdAt is derived from the
//! AID id (the 0001 table has no createdAt column).

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

pub(crate) const ENDPOINTS: &[&str] = &[
    "i/registry/get",
    "i/registry/set",
    "i/registry/remove",
    "i/registry/get-all",
    "i/registry/get-detail",
    "i/registry/keys",
    "i/signin-history",
];

pub(crate) fn handles(name: &str) -> bool {
    ENDPOINTS.contains(&name)
}

/// Entry from the `// i-registry` dispatch arm in mod.rs (body pre-parsed,
/// token pre-extracted).
pub(crate) async fn handle(
    state: &AppState,
    name: &str,
    body: &Value,
    token: Option<String>,
) -> Response {
    let Some(pool) = &state.pool else {
        return internal();
    };
    let result: Check<Response> = async {
        let me_id = super::social::require_checked_user(pool, token.as_deref(), false).await?;
        match name {
            "i/registry/get" => registry_get(pool, &me_id, body).await,
            "i/registry/set" => registry_set(pool, &me_id, body).await,
            "i/registry/remove" => registry_remove(pool, &me_id, body).await,
            "i/registry/get-all" => registry_get_all(pool, &me_id, body).await,
            "i/registry/get-detail" => registry_get_detail(pool, &me_id, body).await,
            "i/registry/keys" => registry_keys(pool, &me_id, body).await,
            "i/signin-history" => signin_history(pool, &me_id, body).await,
            _ => Ok(api_err(ApiError::unknown_endpoint())),
        }
    }
    .await;
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

type Check<T = Response> = Result<T, Response>;

fn internal() -> Response {
    api_err(ApiError::internal_error())
}

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_i_registry", error = %e, "db error: {context}");
    internal()
}

fn invalid_param(param: &str, reason: &str) -> Response {
    api_err(ApiError::invalid_param(json!({
        "param": param,
        "reason": reason,
    })))
}

fn ok_empty() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

fn ok_json(v: Value) -> Response {
    (StatusCode::OK, Json(v)).into_response()
}

/// `key: string[]` → the upstream `/`-joined storage key.
fn key_param(body: &Value) -> Result<String, Response> {
    let Some(Value::Array(items)) = body.get("key") else {
        return Err(invalid_param("key", "required"));
    };
    let mut parts = Vec::with_capacity(items.len());
    for item in items {
        match item.as_str() {
            Some(s) => parts.push(s),
            None => return Err(invalid_param("key", "expected a string array")),
        }
    }
    Ok(parts.join("/"))
}

/// `scope` — upstream is `('client'|'server')[]`; the brief also allows the
/// single-string spelling. Absent/null defaults to `['client']` unless the
/// param is required.
fn scope_param(body: &Value, required: bool) -> Result<Vec<String>, Response> {
    match body.get("scope") {
        None | Some(Value::Null) => {
            if required {
                Err(invalid_param("scope", "required"))
            } else {
                Ok(vec!["client".to_string()])
            }
        }
        Some(Value::String(s)) => scope_one(s).map(|s| vec![s]),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(s) => out.push(scope_one(s)?),
                    None => return Err(invalid_param("scope", "expected a string array")),
                }
            }
            Ok(out)
        }
        Some(_) => Err(invalid_param("scope", "expected a string or string array")),
    }
}

fn scope_one(s: &str) -> Result<String, Response> {
    match s {
        "client" | "server" => Ok(s.to_string()),
        _ => Err(invalid_param("scope", "must be 'client' or 'server'")),
    }
}

fn limit_param(body: &Value) -> Result<i64, Response> {
    match body.get("limit") {
        None | Some(Value::Null) => Ok(10),
        Some(Value::Number(n)) => match n.as_i64() {
            Some(v) if (1..=100).contains(&v) => Ok(v),
            _ => Err(invalid_param("limit", "must be an integer in 1..=100")),
        },
        Some(_) => Err(invalid_param("limit", "expected an integer")),
    }
}

fn opt_id_param(body: &Value, key: &str) -> Result<Option<String>, Response> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(invalid_param(key, "expected a string")),
    }
}

fn paging(body: &Value) -> Result<(i64, Option<String>, Option<String>), Response> {
    Ok((
        limit_param(body)?,
        opt_id_param(body, "sinceId")?,
        opt_id_param(body, "untilId")?,
    ))
}

fn created_at(id: &str) -> Value {
    match mk_types::id::parse_aid_date(id) {
        Some(ms) => json!(mk_core::services::signup::ms_to_iso(ms)),
        None => Value::Null,
    }
}

async fn registry_get(pool: &mk_db::sqlx::PgPool, me_id: &str, body: &Value) -> Check<Response> {
    let key = key_param(body)?;
    let scope = scope_param(body, false)?;
    let value = mk_db::registry::get(pool, me_id, &scope, &key)
        .await
        .map_err(|e| db_err("i/registry/get", e))?;
    match value {
        Some(v) => Ok(ok_json(v)),
        None => Ok(ok_empty()),
    }
}

async fn registry_set(pool: &mk_db::sqlx::PgPool, me_id: &str, body: &Value) -> Check<Response> {
    let key = key_param(body)?;
    let Some(value) = body.get("value") else {
        return Err(invalid_param("value", "required"));
    };
    let scope = scope_param(body, true)?;
    let id = mk_types::id::gen_aid_now();
    mk_db::registry::set(pool, &id, me_id, &scope, &key, value)
        .await
        .map_err(|e| db_err("i/registry/set", e))?;
    Ok(ok_empty())
}

async fn registry_remove(pool: &mk_db::sqlx::PgPool, me_id: &str, body: &Value) -> Check<Response> {
    let key = key_param(body)?;
    let scope = scope_param(body, false)?;
    mk_db::registry::remove(pool, me_id, &scope, &key)
        .await
        .map_err(|e| db_err("i/registry/remove", e))?;
    Ok(ok_empty())
}

async fn registry_get_all(
    pool: &mk_db::sqlx::PgPool,
    me_id: &str,
    body: &Value,
) -> Check<Response> {
    let scope = scope_param(body, false)?;
    let rows = mk_db::registry::get_all(pool, me_id, &scope)
        .await
        .map_err(|e| db_err("i/registry/get-all", e))?;
    let mut root = json!({});
    for (key, value) in rows {
        let parts: Vec<&str> = key.split('/').collect();
        insert_nested(&mut root, &parts, value);
    }
    Ok(ok_json(root))
}

/// `RegistryApiService.getAll`'s key-splitting fold.
fn insert_nested(root: &mut Value, parts: &[&str], value: Value) {
    let Some((head, rest)) = parts.split_first() else {
        return;
    };
    let obj = root.as_object_mut().expect("registry root object");
    if rest.is_empty() {
        obj.insert((*head).to_string(), value);
    } else {
        let entry = obj.entry((*head).to_string()).or_insert_with(|| json!({}));
        if !entry.is_object() {
            *entry = json!({});
        }
        insert_nested(entry, rest, value);
    }
}

async fn registry_get_detail(
    pool: &mk_db::sqlx::PgPool,
    me_id: &str,
    body: &Value,
) -> Check<Response> {
    let key = key_param(body)?;
    let items = mk_db::registry::get_detail(pool, me_id, &key)
        .await
        .map_err(|e| db_err("i/registry/get-detail", e))?;
    let Some(item) = items.first() else {
        return Err(api_err(ApiError::new(
            404,
            mk_types::api_error::ApiErrorKind::Client,
            "REGISTRY_ITEM_NOT_FOUND",
            "98ea1c89-6e9e-4b1b-b5c1-8b3e2d5e2f7a",
            "No such registry item.",
        )));
    };
    let scopes: Vec<Value> = items
        .iter()
        .map(|row| row.get("scope").cloned().unwrap_or_else(|| json!([])))
        .collect();
    Ok(ok_json(json!({ "item": item, "scopes": scopes })))
}

async fn registry_keys(pool: &mk_db::sqlx::PgPool, me_id: &str, body: &Value) -> Check<Response> {
    let scope = scope_param(body, false)?;
    let keys = mk_db::registry::keys(pool, me_id, &scope)
        .await
        .map_err(|e| db_err("i/registry/keys", e))?;
    let out: Vec<Value> = keys
        .iter()
        .map(|k| json!(k.split('/').collect::<Vec<_>>()))
        .collect();
    Ok(ok_json(json!(out)))
}

async fn signin_history(pool: &mk_db::sqlx::PgPool, me_id: &str, body: &Value) -> Check<Response> {
    let (limit, since_id, until_id) = paging(body)?;
    let rows =
        mk_db::signin::signin_history(pool, me_id, limit, since_id.as_deref(), until_id.as_deref())
            .await
            .map_err(|e| db_err("i/signin-history", e))?;
    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "ip": r.ip,
                "createdAt": created_at(&r.id),
                "headers": r.headers,
                "success": r.success,
            })
        })
        .collect();
    Ok(ok_json(json!(out)))
}
