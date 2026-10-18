//! Channels domain (upstream `endpoints/channels/*.ts` slice):
//! create, update, owned, search, featured, followed, my-favorites,
//! timeline, show, follow, unfollow, favorite, unfavorite.
//!
//! Upstream refs (read-only): the endpoints themselves,
//! `core/entities/ChannelEntityService.ts` (pack), `core/ChannelFollowingService.ts`
//! (ALREADY_FOLLOWING), `core/QueryService.ts` (pagination + base note
//! filtering). Db paths live in `mk_db::channel`.
//!
//! M2 scope: local-only; `channels/create`'s rate limit (10/hour) and the
//! `requiredRolePolicy: canCreateChannel` role check stay with the M2+ role
//! wave. `channels/mute/*` are a separate slice (not routed here).

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, api_err};

/// The endpoint names routed into this module (single source of truth for
/// the `// channels-agent` dispatch arm in mod.rs).
pub(crate) const CHANNEL_ENDPOINTS: &[&str] = &[
    "channels/create",
    "channels/update",
    "channels/owned",
    "channels/search",
    "channels/featured",
    "channels/followed",
    "channels/my-favorites",
    "channels/timeline",
    "channels/show",
    "channels/follow",
    "channels/unfollow",
    "channels/favorite",
    "channels/unfavorite",
];

pub(crate) fn handles(name: &str) -> bool {
    CHANNEL_ENDPOINTS.contains(&name)
}

/// requireCredential endpoints (upstream meta.requireCredential).
const REQUIRES_CREDENTIAL: &[&str] = &[
    "channels/create",
    "channels/update",
    "channels/owned",
    "channels/followed",
    "channels/my-favorites",
    "channels/follow",
    "channels/unfollow",
    "channels/favorite",
    "channels/unfavorite",
];

fn db_err(context: &str, e: mk_db::sqlx::Error) -> Response {
    tracing::error!(target: "dbg_channels", error = %e, "db error: {context}");
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

fn no_such_channel() -> Response {
    // each endpoint carries its own error id upstream, but the code+message
    // pair is identical — the id below is channels/show's (the shapes the
    // e2e clients assert on are `error.code` and `error.message`).
    meta_err(
        "NO_SUCH_CHANNEL",
        "6f6c314b-7486-4897-8966-c04a66a02923",
        "No such channel.",
    )
}

fn ok_empty() -> Response {
    // upstream `send(reply, undefined)` → 204
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

fn opt_str<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(|v| v.as_str())
}

fn opt_id<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(|v| v.as_str())
}

fn opt_i(body: &Value, key: &str, dflt: i64) -> i64 {
    body.get(key).and_then(|v| v.as_i64()).unwrap_or(dflt)
}

/// Entry from the `// channels-agent` dispatch arm in mod.rs (body
/// pre-parsed, token + credential-declared pre-extracted).
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
    let mut me_id: Option<String> = None;
    if REQUIRES_CREDENTIAL.contains(&name) {
        // ApiCallService.call: CREDENTIAL_REQUIRED / AUTHENTICATION_FAILED /
        // YOUR_ACCOUNT_SUSPENDED chain (social.rs require_checked_user).
        match super::social::require_checked_user(pool, token.as_deref(), credential_declared).await
        {
            Ok(id) => me_id = Some(id),
            Err(resp) => return resp,
        }
    } else if token.is_some() || credential_declared {
        // Read endpoints accept guests; a provided credential still resolves
        // (or fails) through the same chain (me == null on guest failure).
        me_id = super::social::require_checked_user(pool, token.as_deref(), credential_declared)
            .await
            .ok();
    }

    let result: Result<Response, Response> = match name {
        "channels/create" => create(state, pool, body, me_id.as_deref().unwrap()).await,
        "channels/update" => update(state, pool, body, me_id.as_deref().unwrap()).await,
        "channels/owned" => owned(state, pool, body, me_id.as_deref().unwrap()).await,
        "channels/followed" => followed(state, pool, body, me_id.as_deref().unwrap()).await,
        "channels/my-favorites" => my_favorites(state, pool, me_id.as_deref().unwrap()).await,
        "channels/follow" => follow(pool, body, me_id.as_deref().unwrap()).await,
        "channels/unfollow" => unfollow(pool, body, me_id.as_deref().unwrap()).await,
        "channels/favorite" => favorite(pool, body, me_id.as_deref().unwrap()).await,
        "channels/unfavorite" => unfavorite(pool, body, me_id.as_deref().unwrap()).await,
        "channels/search" => search(state, pool, body, me_id.as_deref()).await,
        "channels/featured" => featured(state, pool, me_id.as_deref()).await,
        "channels/show" => show(state, pool, body, me_id.as_deref()).await,
        "channels/timeline" => timeline(state, pool, body, me_id.as_deref()).await,
        _ => Ok(api_err(ApiError::unknown_endpoint())),
    };
    match result {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

// ---------------------------------------------------------------------------
// ChannelEntityService.pack
// ---------------------------------------------------------------------------

/// The shared note pipeline (M1): row + author lite + derived createdAt —
/// the same 5-line shape `social.rs pack_note_row_social` uses.
async fn pack_note_row(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    row: &mk_db::note::NoteRow,
) -> Result<Value, Response> {
    let user_lite = super::note_user_lite(state, pool, &row.user_id).await?;
    let created_at = mk_types::id::parse_aid_date(&row.id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default();
    let emojis = json!({});
    Ok(mk_core::packers::note::note_pack_min(
        row,
        user_lite,
        created_at,
        &row.mentions,
        &row.tags,
        emojis,
    ))
}

async fn pack_channel(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    row: &mk_db::channel::ChannelRow,
    me_id: Option<&str>,
    detailed: bool,
) -> Result<Value, Response> {
    // bannerUrl: DriveFileEntityService.getPublicUrl(file) = webpublicUrl ?? url
    let banner_url = match &row.banner_id {
        None => None,
        Some(bid) => match mk_db::drive::find_drive_file(pool, bid).await {
            Ok(Some(file)) => Some(crate::api::drive::get_public_url(
                &state.config.url,
                &file,
                false,
            )),
            Ok(None) => None, // findOneByOrFail upstream → 500; M2-lite packs null
            Err(e) => return Err(db_err("pack find_drive_file", e)),
        },
    };
    let created_at = mk_types::id::parse_aid_date(&row.id)
        .map(mk_core::services::signup::ms_to_iso)
        .unwrap_or_default();
    let last_noted_at = row
        .last_noted_at_ms
        .map(mk_core::services::signup::ms_to_iso);

    let mut out = json!({
        "id": row.id,
        "createdAt": created_at,
        "lastNotedAt": last_noted_at,
        "name": row.name,
        "description": row.description,
        "userId": row.user_id,
        "bannerUrl": banner_url,
        "bannerId": row.banner_id,
        "pinnedNoteIds": row.pinned_note_ids,
        "color": row.color,
        "isArchived": row.is_archived,
        "usersCount": row.users_count,
        "notesCount": row.notes_count,
        "isSensitive": row.is_sensitive,
        "allowRenoteToExternal": row.allow_renote_to_external,
    });
    if let Some(me) = me_id {
        let (is_following, is_favorited, is_muting) = (
            mk_db::channel::is_following(pool, me, &row.id).await,
            mk_db::channel::is_favorited(pool, me, &row.id).await,
            mk_db::channel::is_muting(pool, me, &row.id).await,
        );
        let (is_following, is_favorited, is_muting) = match (is_following, is_favorited, is_muting)
        {
            (Ok(a), Ok(b), Ok(c)) => (a, b, c),
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
                return Err(db_err("pack relation flags", e));
            }
        };
        let obj = out.as_object_mut().unwrap();
        obj.insert("isFollowing".into(), json!(is_following));
        obj.insert("isFavorited".into(), json!(is_favorited));
        obj.insert("isMuting".into(), json!(is_muting));
        obj.insert("hasUnreadNote".into(), json!(false)); // 後方互換性のため
    }
    if detailed {
        let notes = match mk_db::channel::pinned_notes(pool, &row.pinned_note_ids).await {
            Ok(n) => n,
            Err(e) => return Err(db_err("pack pinned_notes", e)),
        };
        let mut packed = Vec::with_capacity(notes.len());
        for nrow in &notes {
            packed.push(pack_note_row(state, pool, nrow).await?);
        }
        out.as_object_mut()
            .unwrap()
            .insert("pinnedNotes".into(), json!(packed));
    }
    Ok(out)
}

async fn load_channel(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
) -> Result<mk_db::channel::ChannelRow, Response> {
    let Some(channel_id) = opt_id(body, "channelId") else {
        return Err(invalid_param("channelId", "required (misskey:id format)"));
    };
    match mk_db::channel::find_channel(pool, channel_id).await {
        Ok(Some(c)) => Ok(c),
        Ok(None) => Err(no_such_channel()),
        Err(e) => Err(db_err("find_channel", e)),
    }
}

// ---------------------------------------------------------------------------
// endpoints
// ---------------------------------------------------------------------------

/// `POST /api/channels/create` (endpoints/channels/create.ts).
async fn create(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let Some(name) = opt_str(body, "name") else {
        return Err(invalid_param("name", "required"));
    };
    if name.is_empty() || name.chars().count() > 128 {
        return Err(invalid_param("name", "must be 1..128 characters"));
    }
    let description = match body.get("description") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            if s.chars().count() > 2048 {
                return Err(invalid_param(
                    "description",
                    "must be up to 2048 characters",
                ));
            }
            Some(s.as_str())
        }
        Some(_) => return Err(invalid_param("description", "expected a string or null")),
    };
    // bannerId: owned by me or NO_SUCH_FILE (create.ts banner lookup).
    let banner_id = match body.get("bannerId") {
        None | Some(Value::Null) => None,
        Some(Value::String(bid)) => {
            let owned = mk_db::drive::find_drive_file(pool, bid)
                .await
                .map_err(|e| db_err("create find_drive_file", e))?
                .filter(|f| f.user_id.as_deref() == Some(me_id));
            if owned.is_none() {
                return Err(meta_err(
                    "NO_SUCH_FILE",
                    "cd1e9f3e-5a12-4ab4-96f6-5d0a2cc32050",
                    "No such file.",
                ));
            }
            Some(bid.as_str())
        }
        Some(_) => return Err(invalid_param("bannerId", "expected a string or null")),
    };
    let color = match body.get("color") {
        None | Some(Value::Null) => "#86b300", // ColorService.getDefaultChannelColor
        Some(Value::String(s)) => {
            if s.is_empty() || s.chars().count() > 16 {
                return Err(invalid_param("color", "must be 1..16 characters"));
            }
            s.as_str()
        }
        Some(_) => return Err(invalid_param("color", "expected a string")),
    };
    let is_sensitive = body
        .get("isSensitive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let allow_renote_to_external = body
        .get("allowRenoteToExternal")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let id = mk_types::id::gen_aid_now();
    mk_db::channel::insert_channel(
        pool,
        &mk_db::channel::NewChannel {
            id: &id,
            user_id: me_id,
            name,
            description,
            banner_id,
            color,
            is_sensitive,
            allow_renote_to_external,
        },
    )
    .await
    .map_err(|e| db_err("insert_channel", e))?;
    let row = mk_db::channel::find_channel(pool, &id)
        .await
        .map_err(|e| db_err("create refetch", e))?
        .ok_or_else(api_err_internal)?;
    Ok(ok_json(
        pack_channel(state, pool, &row, Some(me_id), false).await?,
    ))
}

fn api_err_internal() -> Response {
    api_err(ApiError::internal_error())
}

/// `POST /api/channels/update` — owner or moderator; only provided params.
async fn update(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let row = load_channel(pool, body).await?;
    let is_moderator = super::viewer_is_moderator(pool, Some(me_id)).await;
    if row.user_id.as_deref() != Some(me_id) && !is_moderator {
        return Err(meta_err(
            "ACCESS_DENIED",
            "1fb7cb09-d46a-4fdf-b8df-057788cce513",
            "You do not have edit privilege of the channel.",
        ));
    }
    let name = match body.get("name") {
        None => None,
        Some(Value::Null) => return Err(invalid_param("name", "must be 1..128 characters")),
        Some(Value::String(s)) => {
            if s.is_empty() || s.chars().count() > 128 {
                return Err(invalid_param("name", "must be 1..128 characters"));
            }
            Some(s.as_str())
        }
        Some(_) => return Err(invalid_param("name", "expected a string")),
    };
    let description = match body.get("description") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(s)) => {
            if s.chars().count() > 2048 {
                return Err(invalid_param(
                    "description",
                    "must be up to 2048 characters",
                ));
            }
            Some(Some(s.as_str()))
        }
        Some(_) => return Err(invalid_param("description", "expected a string or null")),
    };
    // bannerId tri-state: null clears, a value must be one of my files, an
    // absent param leaves the column untouched.
    let banner_id = match body.get("bannerId") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(bid)) => {
            let owned = mk_db::drive::find_drive_file(pool, bid)
                .await
                .map_err(|e| db_err("update find_drive_file", e))?
                .filter(|f| f.user_id.as_deref() == Some(me_id));
            if owned.is_none() {
                return Err(meta_err(
                    "NO_SUCH_FILE",
                    "e86c14a4-0da2-4032-8df3-e737a04c7f3b",
                    "No such file.",
                ));
            }
            Some(Some(bid.as_str()))
        }
        Some(_) => return Err(invalid_param("bannerId", "expected a string or null")),
    };
    let pinned_note_ids = match body.get("pinnedNoteIds") {
        None => None,
        Some(Value::Array(a)) => {
            let mut ids = Vec::with_capacity(a.len());
            for v in a {
                let Some(s) = v.as_str() else {
                    return Err(invalid_param("pinnedNoteIds", "expected an array of ids"));
                };
                ids.push(s.to_string());
            }
            Some(ids)
        }
        Some(_) => return Err(invalid_param("pinnedNoteIds", "expected an array")),
    };
    let color = match body.get("color") {
        None => None,
        Some(Value::String(s)) => {
            if s.is_empty() || s.chars().count() > 16 {
                return Err(invalid_param("color", "must be 1..16 characters"));
            }
            Some(s.as_str())
        }
        Some(_) => return Err(invalid_param("color", "expected a string")),
    };
    let is_archived = match body.get("isArchived") {
        None | Some(Value::Null) => None,
        Some(v @ Value::Bool(_)) => v.as_bool(),
        Some(_) => return Err(invalid_param("isArchived", "expected a boolean")),
    };
    let is_sensitive = match body.get("isSensitive") {
        None | Some(Value::Null) => None,
        Some(v @ Value::Bool(_)) => v.as_bool(),
        Some(_) => return Err(invalid_param("isSensitive", "expected a boolean")),
    };
    let allow_renote_to_external = match body.get("allowRenoteToExternal") {
        None | Some(Value::Null) => None,
        Some(v @ Value::Bool(_)) => v.as_bool(),
        Some(_) => return Err(invalid_param("allowRenoteToExternal", "expected a boolean")),
    };
    mk_db::channel::update_channel(
        pool,
        &row.id,
        &mk_db::channel::UpdateChannel {
            name,
            description,
            banner_id,
            pinned_note_ids: pinned_note_ids.as_deref(),
            color,
            is_archived,
            is_sensitive,
            allow_renote_to_external,
        },
    )
    .await
    .map_err(|e| db_err("update_channel", e))?;
    // upstream re-packs `channel.id` (the fresh row).
    let row = load_channel(pool, body).await?;
    Ok(ok_json(
        pack_channel(state, pool, &row, Some(me_id), false).await?,
    ))
}

/// `POST /api/channels/owned` — my non-archived channels (`pack(x, me)`).
async fn owned(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let rows = mk_db::channel::list_owned(
        pool,
        me_id,
        opt_id(body, "sinceId"),
        opt_id(body, "untilId"),
        opt_i(body, "limit", 5).clamp(1, 100),
    )
    .await
    .map_err(|e| db_err("list_owned", e))?;
    pack_many(state, pool, &rows, Some(me_id), false)
        .await
        .map(ok_json)
}

/// `POST /api/channels/search` — ILIKE name ± description.
async fn search(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: Option<&str>,
) -> Result<Response, Response> {
    let Some(query) = opt_str(body, "query") else {
        return Err(invalid_param("query", "required"));
    };
    let name_only = match body.get("type") {
        None | Some(Value::Null) => false,
        Some(Value::String(t)) => match t.as_str() {
            "nameAndDescription" => false,
            "nameOnly" => true,
            _ => {
                return Err(invalid_param(
                    "type",
                    "must be nameAndDescription or nameOnly",
                ));
            }
        },
        Some(_) => return Err(invalid_param("type", "expected a string")),
    };
    let rows = mk_db::channel::search_channels(
        pool,
        query,
        name_only,
        opt_id(body, "sinceId"),
        opt_id(body, "untilId"),
        opt_i(body, "limit", 5).clamp(1, 100),
    )
    .await
    .map_err(|e| db_err("search_channels", e))?;
    pack_many(state, pool, &rows, me_id, false)
        .await
        .map(ok_json)
}

/// `POST /api/channels/featured` — lastNotedAt DESC top 10.
async fn featured(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    me_id: Option<&str>,
) -> Result<Response, Response> {
    let rows = mk_db::channel::list_featured(pool)
        .await
        .map_err(|e| db_err("list_featured", e))?;
    pack_many(state, pool, &rows, me_id, false)
        .await
        .map(ok_json)
}

/// `POST /api/channels/followed` — paged channel_followings, packed like
/// `pack(x.followeeId, me)`.
async fn followed(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let rows = mk_db::channel::list_followed(
        pool,
        me_id,
        opt_id(body, "sinceId"),
        opt_id(body, "untilId"),
        opt_i(body, "limit", 5).clamp(1, 100),
    )
    .await
    .map_err(|e| db_err("list_followed", e))?;
    pack_many(state, pool, &rows, Some(me_id), false)
        .await
        .map(ok_json)
}

/// `POST /api/channels/my-favorites`.
async fn my_favorites(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    me_id: &str,
) -> Result<Response, Response> {
    let rows = mk_db::channel::list_favorites(pool, me_id)
        .await
        .map_err(|e| db_err("list_favorites", e))?;
    pack_many(state, pool, &rows, Some(me_id), false)
        .await
        .map(ok_json)
}

/// `POST /api/channels/show` — `pack(channel, me, true)` (detailed adds
/// `pinnedNotes`).
async fn show(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: Option<&str>,
) -> Result<Response, Response> {
    let row = load_channel(pool, body).await?;
    Ok(ok_json(pack_channel(state, pool, &row, me_id, true).await?))
}

/// `POST /api/channels/timeline` — db path of timeline.ts getFromDb packed
/// through the /api/notes/show pipeline.
async fn timeline(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: Option<&str>,
) -> Result<Response, Response> {
    // channel existence gate (timeline.ts NO_SUCH_CHANNEL) reads the row
    // before the note query.
    let Some(channel_id) = opt_id(body, "channelId") else {
        return Err(invalid_param("channelId", "required (misskey:id format)"));
    };
    let exists = mk_db::channel::find_channel(pool, channel_id)
        .await
        .map_err(|e| db_err("timeline find_channel", e))?;
    if exists.is_none() {
        return Err(meta_err(
            "NO_SUCH_CHANNEL",
            "4d0eeeba-a02c-4c3c-9966-ef60d38d2e7f",
            "No such channel.",
        ));
    }
    let rows = mk_db::channel::timeline_notes(
        pool,
        channel_id,
        me_id,
        opt_id(body, "sinceId"),
        opt_id(body, "untilId"),
        opt_i(body, "limit", 10).clamp(1, 100),
    )
    .await
    .map_err(|e| db_err("timeline_notes", e))?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(pack_note_row(state, pool, row).await?);
    }
    Ok(ok_json(json!(out)))
}

/// `POST /api/channels/follow` — ALREADY_FOLLOWING on the duplicate.
async fn follow(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let row = load_channel(pool, body).await?;
    let id = mk_types::id::gen_aid_now();
    let already = mk_db::channel::follow(pool, &id, me_id, &row.id)
        .await
        .map_err(|e| db_err("follow", e))?;
    if already {
        return Err(meta_err(
            "ALREADY_FOLLOWING",
            "7db31665-651e-40c1-8e6e-28e9ad829a2d",
            "You are already following that channel.",
        ));
    }
    Ok(ok_empty())
}

/// `POST /api/channels/unfollow`.
async fn unfollow(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let row = load_channel(pool, body).await?;
    mk_db::channel::unfollow(pool, me_id, &row.id)
        .await
        .map_err(|e| db_err("unfollow", e))?;
    Ok(ok_empty())
}

/// `POST /api/channels/favorite`.
async fn favorite(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let row = load_channel(pool, body).await?;
    let id = mk_types::id::gen_aid_now();
    let duplicate = mk_db::channel::favorite(pool, &id, me_id, &row.id)
        .await
        .map_err(|e| db_err("favorite", e))?;
    if duplicate {
        // upstream lets the PG unique violation rise as 500 ISE
        return Err(api_err(ApiError::internal_error()));
    }
    Ok(ok_empty())
}

/// `POST /api/channels/unfavorite`.
async fn unfavorite(
    pool: &mk_db::sqlx::PgPool,
    body: &Value,
    me_id: &str,
) -> Result<Response, Response> {
    let row = load_channel(pool, body).await?;
    mk_db::channel::unfavorite(pool, me_id, &row.id)
        .await
        .map_err(|e| db_err("unfavorite", e))?;
    Ok(ok_empty())
}

// ---------------------------------------------------------------------------
// shared pack-many
// ---------------------------------------------------------------------------

async fn pack_many(
    state: &AppState,
    pool: &mk_db::sqlx::PgPool,
    rows: &[mk_db::channel::ChannelRow],
    me_id: Option<&str>,
    detailed: bool,
) -> Result<Value, Response> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(pack_channel(state, pool, row, me_id, detailed).await?);
    }
    Ok(json!(out))
}
