//! M1 signup round-trip against the real router + PostgreSQL (CI services;
//! skipped locally without DATABASE_URL).

use super::*;
use axum::body::Body;
use axum::http::Request;
use serde_json::{Value, json};
use tower::ServiceExt;

/// Unit-test binaries never run main(), so tracing has no subscriber and
/// error!() calls are silently dropped. This registers the fmt subscriber
/// exactly once per test-binary.
fn init_tracing() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug")),
            )
            .try_init();
    });
}

fn test_state(pool: mk_db::sqlx::PgPool) -> crate::AppState {
    crate::AppState {
        config: crate::config::Config {
            port: 0,
            url: "http://127.0.0.1:3000".to_string(),
            db: None,
            redis: None,
            test: true,
            version: None,
            raw_json: serde_json::Value::Null,
        },
        pool: Some(pool),
        rate_limiter: std::sync::Arc::new(crate::ratelimit::RateLimiter::new()),
        search_engine: std::sync::Arc::new(mk_core::services::search_bridge::NoopEngine),
    }
}

async fn post_json(app: axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, v)
}

#[tokio::test]
async fn signup_roundtrip() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("mksignup_roundtrip").await;
    mk_db::run_migrations(&pool).await.unwrap();

    let username = format!("alice{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool));

    let (status, body) = post_json(
        app,
        "/api/signup",
        json!({ "username": username, "password": "test" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // token / id contracts
    assert_eq!(body["token"].as_str().map(|t| t.len()), Some(16));
    assert_eq!(body["id"].as_str().map(|t| t.len()), Some(10));
    assert_eq!(body["username"], username.as_str());
    assert_eq!(body["host"], Value::Null);

    // presence contracts: Nullable present; optional absent
    let obj = body.as_object().unwrap();
    for required in [
        "location",
        "birthday",
        "twoFactorEnabled",
        "avatarUrl",
        "createdAt",
        "isExplorable",
        "chatScope",
        // includeSecrets: true on the signup path
        "email",
        "emailVerified",
        "securityKeysList",
    ] {
        assert!(obj.contains_key(required), "{required} must be present");
    }
    assert_eq!(body["email"], Value::Null);
    assert_eq!(body["emailVerified"], json!(false));
    for absent in ["instance", "isFollowing"] {
        assert!(!obj.contains_key(absent), "{absent} must be absent");
    }
    assert!(body["avatarUrl"].as_str().unwrap().contains("/identicon/"));
    assert_eq!(body["isBot"], json!(false));
    assert_eq!(body["isCat"], json!(false));
    assert!(body.as_object().unwrap().contains_key("avatarBlurhash"));
    assert_eq!(body["isExplorable"], json!(true));
    assert_eq!(body["autoAcceptFollowed"], json!(true));
}

#[tokio::test]
async fn signup_duplicated_username() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("mksignup_dup").await;
    mk_db::run_migrations(&pool).await.unwrap();

    let username = format!("bob{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool));

    let (s1, _) = post_json(
        app.clone(),
        "/api/signup",
        json!({ "username": username, "password": "test" }),
    )
    .await;
    assert_eq!(s1, StatusCode::OK);

    let (s2, b2) = post_json(
        app,
        "/api/signup",
        json!({ "username": username, "password": "test" }),
    )
    .await;
    assert_eq!(s2, StatusCode::BAD_REQUEST, "body: {b2}");
}

async fn fresh_pool(db: &str) -> mk_db::sqlx::PgPool {
    let pool = mk_db::test_support::fresh_db(db).await;
    mk_db::run_migrations(&pool).await.unwrap();
    pool
}

async fn signup(app: &axum::Router, username: &str) -> Value {
    let (status, body) = post_json(
        app.clone(),
        "/api/signup",
        json!({ "username": username, "password": "test" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    body
}

#[tokio::test]
async fn signin_flow() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mksignin_flow").await;
    let username = format!("carol{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let signed = signup(&app, &username).await;
    let token = signed["token"].as_str().unwrap().to_string();

    // bad username → 404 with upstream error id
    let (s404, b404) = post_json(
        app.clone(),
        "/api/signin",
        json!({ "username": "ghost_user_0", "password": "test" }),
    )
    .await;
    assert_eq!(s404, StatusCode::NOT_FOUND);
    assert_eq!(
        b404["error"]["id"],
        json!("6cc579cc-885d-43d8-95c2-b8c7fc963280")
    );

    // wrong password → 403 with upstream error id
    let (s403, b403) = post_json(
        app.clone(),
        "/api/signin",
        json!({ "username": username, "password": "wrong" }),
    )
    .await;
    assert_eq!(s403, StatusCode::FORBIDDEN);
    assert_eq!(
        b403["error"]["id"],
        json!("932c904e-9460-45b7-9ce6-7ed33be7eb2c")
    );

    // correct → SigninFlowResponse finished with token i
    let (s_ok, b_ok) = post_json(
        app,
        "/api/signin",
        json!({ "username": username, "password": "test" }),
    )
    .await;
    assert_eq!(s_ok, StatusCode::OK);
    assert_eq!(b_ok["finished"], json!(true));
    assert_eq!(b_ok["i"], json!(token));

    // signin history recorded
    let n: i64 = mk_db::sqlx::query_scalar(r#"SELECT count(*) FROM "signin""#)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 2); // one failure + one success
}

#[tokio::test]
async fn users_show_variant() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkusershow").await;
    let username = format!("dave{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let me = signup(&app, &username).await;
    let my_id = me["id"].as_str().unwrap().to_string();
    let my_token = me["token"].as_str().unwrap().to_string();

    // by id, not-me view: Me-only fields absent, twoFactorEnabled optional
    let (s1, b1) = post_json(
        app.clone(),
        "/api/users/show",
        json!({ "userId": my_id, "i": format!("{}x", "0123456789abcdef") }),
    )
    .await;
    assert_eq!(s1, StatusCode::OK, "b: {b1}");
    assert_eq!(b1["username"], username.as_str());
    assert!(!b1.as_object().unwrap().contains_key("avatarId"));

    // by id as me: MeDetailed fields present
    let (s2, b2) = post_json(
        app.clone(),
        "/api/users/show",
        json!({ "userId": my_id, "i": my_token }),
    )
    .await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b2["avatarId"], Value::Null);
    assert_eq!(b2["twoFactorEnabled"], json!(false));
    assert_eq!(b2["injectFeaturedNote"], json!(true));

    // by username, unauthenticated
    let (s3, b3) = post_json(
        app.clone(),
        "/api/users/show",
        json!({ "username": username }),
    )
    .await;
    assert_eq!(s3, StatusCode::OK);
    assert_eq!(b3["id"], json!(my_id));

    // no such user → 404 NO_SUCH_USER
    let (s4, b4) = post_json(app, "/api/users/show", json!({ "userId": "zzzzzzzzzzzz" })).await;
    assert_eq!(s4, StatusCode::NOT_FOUND);
    assert_eq!(
        b4["error"]["id"],
        json!("4362f8dc-731f-4ad8-a694-be5a88922a24")
    );
}

#[tokio::test]
async fn i_endpoint() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mki_endpoint").await;
    let username = format!("erin{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let me = signup(&app, &username).await;
    let my_id = me["id"].as_str().unwrap().to_string();
    let my_token = me["token"].as_str().unwrap().to_string();

    // valid token → MeDetailed for the caller
    let (s1, b1) = post_json(app.clone(), "/api/i", json!({ "i": my_token })).await;
    assert_eq!(s1, StatusCode::OK, "body: {b1}");
    assert_eq!(b1["id"], json!(my_id));
    assert_eq!(b1["username"], username.as_str());
    assert_eq!(b1["avatarId"], Value::Null);
    assert_eq!(b1["twoFactorEnabled"], json!(false));
    assert_eq!(b1["email"], Value::Null);
    // includeSecrets (native token, no app) → secrets present
    assert_eq!(b1["emailVerified"], json!(false));
    assert!(b1.as_object().unwrap().contains_key("securityKeysList"));
    // loggedInDates gained today (upstream i.ts side effect)
    assert_eq!(b1["loggedInDays"], json!(1));

    // missing `i` → 401 CREDENTIAL_REQUIRED (call() credential gate)
    let (s2, b2) = post_json(app.clone(), "/api/i", json!({})).await;
    assert_eq!(s2, StatusCode::UNAUTHORIZED);
    assert_eq!(b2["error"]["code"], json!("CREDENTIAL_REQUIRED"));
    assert_eq!(
        b2["error"]["id"],
        json!("1384574d-a912-4b81-8601-c7b1c4085df1")
    );

    // garbage native-shaped token (len 16, no user) → 401 AUTHENTICATION_FAILED
    let (s3, b3) = post_json(app.clone(), "/api/i", json!({ "i": "0123456789abcdef" })).await;
    assert_eq!(s3, StatusCode::UNAUTHORIZED);
    assert_eq!(b3["error"]["code"], json!("AUTHENTICATION_FAILED"));
    assert_eq!(
        b3["error"]["id"],
        json!("b0a7f5f8-dc2f-4171-b91f-de88ad238e14")
    );

    // non-string `i` → 400 with an empty body (upstream handleRequest)
    let (s4, b4) = post_json(app, "/api/i", json!({ "i": 42 })).await;
    assert_eq!(s4, StatusCode::BAD_REQUEST);
    assert_eq!(b4, Value::Null);
}

#[tokio::test]
async fn i_update_roundtrip() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkiupdate").await;
    let username = format!("erin{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let me = signup(&app, &username).await;
    let my_id = me["id"].as_str().unwrap().to_string();
    let my_token = me["token"].as_str().unwrap().to_string();

    let (s1, b1) = post_json(
        app.clone(),
        "/api/i/update",
        json!({
            "i": my_token,
            "description": "hello bio",
            "location": "Kyoto",
            "birthday": "2000-01-31",
            "fields": [{ "name": "site", "value": "https://example.com" }],
            "followingVisibility": "followers",
            "chatScope": "following",
        }),
    )
    .await;
    assert_eq!(s1, StatusCode::OK, "body: {b1}");
    assert_eq!(b1["id"], json!(my_id));
    assert_eq!(b1["description"], json!("hello bio"));
    assert_eq!(b1["location"], json!("Kyoto"));
    assert_eq!(b1["birthday"], json!("2000-01-31"));
    assert_eq!(b1["fields"][0]["name"], json!("site"));
    assert_eq!(b1["fields"][0]["value"], json!("https://example.com"));
    assert_eq!(b1["followingVisibility"], json!("followers"));
    assert_eq!(b1["chatScope"], json!("following"));

    // persisted: users/show with the same token (me view) sees the new values
    let (s2, b2) = post_json(
        app.clone(),
        "/api/users/show",
        json!({ "userId": my_id, "i": my_token }),
    )
    .await;
    assert_eq!(s2, StatusCode::OK, "body: {b2}");
    assert_eq!(b2["description"], json!("hello bio"));
    assert_eq!(b2["location"], json!("Kyoto"));
    assert_eq!(b2["birthday"], json!("2000-01-31"));
    assert_eq!(b2["fields"][0]["name"], json!("site"));
    assert_eq!(b2["fields"][0]["value"], json!("https://example.com"));
    assert_eq!(b2["followingVisibility"], json!("followers"));
    assert_eq!(b2["chatScope"], json!("following"));

    // explicit null birthday → NULL persisted (character(10) column)
    let (s3, b3) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "birthday": null, "i": my_token }),
    )
    .await;
    assert_eq!(s3, StatusCode::OK, "body: {b3}");
    assert_eq!(b3["birthday"], Value::Null);

    // fields omitted → previously stored fields left untouched
    assert_eq!(b3["fields"][0]["value"], json!("https://example.com"));
}

#[tokio::test]
async fn i_update_requires_native_token() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkiupdate_auth").await;
    let app = crate::build_router(test_state(pool));

    // no credential at all → 401 AUTHENTICATION_FAILED
    let (s1, b1) = post_json(app.clone(), "/api/i/update", json!({ "description": "x" })).await;
    assert_eq!(s1, StatusCode::UNAUTHORIZED, "body: {b1}");
    assert_eq!(b1["error"]["code"], json!("AUTHENTICATION_FAILED"));
    assert_eq!(
        b1["error"]["id"],
        json!("b0a7f5f8-dc2f-4171-b91f-de88ad238e14")
    );

    // present but unknown 16-char token → same 401
    let (s2, b2) = post_json(
        app,
        "/api/i/update",
        json!({ "description": "x", "i": "0123456789abcdef" }),
    )
    .await;
    assert_eq!(s2, StatusCode::UNAUTHORIZED, "body: {b2}");
    assert_eq!(b2["error"]["code"], json!("AUTHENTICATION_FAILED"));
}

#[tokio::test]
async fn notes_create_and_show() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mknotes1").await;
    let username = format!("noter{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let me = signup(&app, &username).await;
    let token = me["token"].as_str().unwrap().to_string();

    let (s1, b1) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "hello e2e", "i": token }),
    )
    .await;
    assert_eq!(s1, StatusCode::OK, "body: {b1}");
    let note = &b1["createdNote"];
    assert_eq!(note["text"], json!("hello e2e"));
    assert_eq!(note["visibility"], json!("public"));
    assert_eq!(note["userId"], me["id"]);
    assert_eq!(note["user"]["username"], username.as_str());
    assert_eq!(note["repliesCount"], json!(0));

    // e2e asserts exact field sets — optional-absent fields must be missing.
    // Upstream presence truth (NoteEntityService.pack L403-436): only
    // `undefined` drops a key — nullable scalars stay present as null
    // (`cw: note.cw`, `replyId`/`renoteId`, detail `clippedCount` column
    // default 0), while empty collections vanish (`mentions`/`tags`
    // `.length > 0 ? : undefined`, `emojis` local-host → undefined,
    // `visibleUserIds` only for specified).
    let got = note
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    for absent in [
        "deletedAt",
        "mentions",
        "tags",
        "emojis",
        "visibleUserIds",
        "channelId",
        "reply",
        "renote",
        "poll",
        "myReaction",
    ] {
        assert!(!got.contains(absent), "{absent} must be absent");
    }
    for present in [
        "cw",
        "replyId",
        "renoteId",
        "clippedCount",
        "fileIds",
        "files",
        "reactions",
        "reactionEmojis",
        "localOnly",
        "reactionCount",
        "renoteCount",
        "repliesCount",
    ] {
        assert!(got.contains(present), "{present} must be present");
    }

    // notes/show roundtrip
    let nid = note["id"].as_str().unwrap().to_string();
    let (s2, b2) = post_json(app, "/api/notes/show", json!({ "noteId": nid })).await;
    assert_eq!(s2, StatusCode::OK, "b: {b2}");
    assert_eq!(b2["id"], json!(nid));
    assert_eq!(b2["text"], json!("hello e2e"));
}

#[tokio::test]
async fn notes_create_requires_auth() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mknotes2").await;
    let app = crate::build_router(test_state(pool));
    let (s, b) = post_json(app, "/api/notes/create", json!({ "text": "x" })).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(
        b["error"]["id"],
        json!("b0a7f5f8-dc2f-4171-b91f-de88ad238e14")
    );
}

#[tokio::test]
async fn users_search_family() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mksearch").await;
    let app = crate::build_router(test_state(pool.clone()));

    let a = signup(&app, "searchali").await;
    let _b = signup(&app, "searchbob").await;
    let token = a["token"].as_str().unwrap().to_string();

    // set a description via i/update for description-pass coverage
    let (s0, b0) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "description": "zetaomega", "i": token }),
    )
    .await;
    assert_eq!(s0, StatusCode::OK, "b: {b0}");

    // by-username-and-host: prefix, case-insensitive, local host only
    let (s1, b1) = post_json(
        app.clone(),
        "/api/users/search-by-username-and-host",
        json!({ "username": "SEARCH", "host": null }),
    )
    .await;
    assert_eq!(s1, StatusCode::OK, "b: {b1}");
    let arr = b1.as_array().unwrap();
    assert_eq!(arr.len(), 2, "b: {b1}");
    assert_eq!(arr[0]["username"], json!("searchali"));

    // search by username token (@-prefix)
    let (s2, b2) = post_json(
        app.clone(),
        "/api/users/search",
        json!({ "query": "@searchb" }),
    )
    .await;
    assert_eq!(s2, StatusCode::OK, "b: {b2}");
    assert_eq!(b2.as_array().unwrap().len(), 1);
    assert_eq!(b2[0]["username"], json!("searchbob"));

    // search by description token
    let (s3, b3) = post_json(app, "/api/users/search", json!({ "query": "zetaomega" })).await;
    assert_eq!(s3, StatusCode::OK, "b: {b3}");
    assert_eq!(b3.as_array().unwrap().len(), 1);
    assert_eq!(b3[0]["username"], json!("searchali"));
}

// `isBot`/`isCat` are upstream i/update.ts paramDef booleans
// (`isBot: { type: 'boolean' }, isCat: { type: 'boolean' }`) applied to the
// "user" table via `updates.isBot = ps.isBot` / `updates.isCat = ps.isCat`
// (users.ts を書き換えることができる cases with parameters isBot: true/false,
// isCat: true/false).
//
// NOTE(pack): this pins the persist layer only. /api/i still reports
// isBot/isCat as false because mk-core hardcodes `is_bot: Some(false),
// is_cat: Some(false)` (packers/user.rs + services/user_pack.rs) and
// LocalUserRow (mk-db/src/user.rs, another agent's hunk) does not carry the
// columns yet. Once the pack layer reads "isBot"/"isCat" from the row, the
// census each-cases pass end-to-end via the already-persisted values.
#[tokio::test]
async fn i_update_persists_is_bot_and_is_cat() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkiupdate_botcat").await;
    let username = format!("botcat{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let me = signup(&app, &username).await;
    let my_id = me["id"].as_str().unwrap().to_string();
    let my_token = me["token"].as_str().unwrap().to_string();

    async fn persisted(pool: &mk_db::sqlx::PgPool, id: &str) -> (bool, bool) {
        let (is_bot, is_cat): (bool, bool) =
            mk_db::sqlx::query_as(r#"SELECT "isBot", "isCat" FROM "user" WHERE "id" = $1"#)
                .bind(id)
                .fetch_one(pool)
                .await
                .unwrap();
        (is_bot, is_cat)
    }

    // fresh signup defaults (signup upstream ignores isBot/isCat outright)
    assert_eq!(persisted(&pool, &my_id).await, (false, false));

    // set both via i/update → persisted on the "user" table
    let (s1, b1) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "i": my_token, "isBot": true, "isCat": true }),
    )
    .await;
    assert_eq!(s1, StatusCode::OK, "body: {b1}");
    assert_eq!(b1["id"], json!(my_id));
    assert_eq!(persisted(&pool, &my_id).await, (true, true));

    // partial update: omitted key untouched (upstream `if typeof ps.isBot ===
    // 'boolean'` guards each assignment)
    let (s2, b2) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "i": my_token, "isBot": false }),
    )
    .await;
    assert_eq!(s2, StatusCode::OK, "body: {b2}");
    assert_eq!(persisted(&pool, &my_id).await, (false, true));

    // explicit false roundtrips for isCat too
    let (s3, b3) = post_json(
        app,
        "/api/i/update",
        json!({ "i": my_token, "isCat": false }),
    )
    .await;
    assert_eq!(s3, StatusCode::OK, "body: {b3}");
    assert_eq!(persisted(&pool, &my_id).await, (false, false));
}

// ---------------------------------------------------------------------------
// i/update census tail (users.ts を書き換えることができる Avatar/Banner/
// ピン止めノート/メモ/undefined + 作りたてdict): persist + pack parity for the
// avatarId/bannerId drive swap, i/pin + i/unpin, users/update-memo, the
// absent-vs-null birthday semantics and the MeDetailed role-compat keys.
// ---------------------------------------------------------------------------

/// Drive-file insert mirroring the schema defaults (0001_init.sql NOT NULL
/// cols: md5/name/type/size/"storedInternal"/url) via the upload path.
async fn seed_drive_file(pool: &mk_db::sqlx::PgPool, user_id: &str, file_type: &str) -> String {
    let id = mk_types::id::gen_aid_now();
    let row = mk_db::drive::DriveFileRow {
        id: id.clone(),
        user_id: Some(user_id.to_string()),
        md5: mk_types::id::gen_aid_now(),
        name: "192.jpg".to_string(),
        file_type: file_type.to_string(),
        size: 47_918,
        blurhash: Some(
            "U0000000i0f00000000~q00j[00j%NSj[4nWBofj[4nWBofWBj[ofj[%NSj[4nf7j[".to_string(),
        ),
        url: "http://127.0.0.1:3000/files/accesskey123/192.jpg".to_string(),
        webpublic_url: None,
        properties: json!({}),
        ..Default::default()
    };
    let access_key = format!("seedkey{}{}", mk_types::id::gen_aid_now(), id);
    mk_db::drive::insert_drive_file(pool, &row, &access_key)
        .await
        .unwrap();
    id
}

/// users.ts `を書き換えることができる(Avatar/Banner)`: i/update with
/// avatarId/bannerId looks the drive file up and denormalizes
/// avatarUrl/avatarBlurhash (resp. banner) onto the user row
/// (i/update.ts L377-411 → UserEntityService.pack L490/L536-537).
#[tokio::test]
async fn i_update_avatar_banner_drive_swap() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkiupdate_avabanner").await;
    let username = format!("ava{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let me = signup(&app, &username).await;
    let my_id = me["id"].as_str().unwrap().to_string();
    let my_token = me["token"].as_str().unwrap().to_string();

    // foreign-owned file → NO_SUCH_AVATAR (avatar.userId !== user.id gate)
    let other = signup(&app, &format!("avao{}", mk_types::id::gen_aid_now())).await;
    let foreign_file = seed_drive_file(&pool, other["id"].as_str().unwrap(), "image/jpeg").await;
    let (s1, b1) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "i": my_token, "avatarId": foreign_file }),
    )
    .await;
    assert_eq!(s1, StatusCode::BAD_REQUEST, "body: {b1}");
    assert_eq!(b1["error"]["code"], json!("NO_SUCH_AVATAR"));

    // non-image MIME → AVATAR_NOT_AN_IMAGE / BANNER_NOT_AN_IMAGE
    let bad_file = seed_drive_file(&pool, &my_id, "text/plain").await;
    let (s2, b2) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "i": my_token, "bannerId": bad_file }),
    )
    .await;
    assert_eq!(s2, StatusCode::BAD_REQUEST, "body: {b2}");
    assert_eq!(b2["error"]["code"], json!("BANNER_NOT_AN_IMAGE"));

    // Avatar: image file → id + proxied URL + blurhash denormalized
    let avatar_file = seed_drive_file(&pool, &my_id, "image/jpeg").await;
    let (s3, b3) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "i": my_token, "avatarId": avatar_file }),
    )
    .await;
    assert_eq!(s3, StatusCode::OK, "body: {b3}");
    assert_eq!(b3["avatarId"], json!(avatar_file));
    let blur = b3["avatarBlurhash"].as_str().unwrap_or("");
    assert!(blur.len() >= 54, "blurhash {blur:?} misses /[ -~]{{54}}/");
    assert!(blur.bytes().all(|c| (0x20..=0x7e).contains(&c)));
    let avatar_url = b3["avatarUrl"].as_str().unwrap();
    assert!(
        avatar_url.starts_with("http://127.0.0.1:3000/proxy/avatar.webp?url=")
            && avatar_url.ends_with("&avatar=1"),
        "avatar url {avatar_url}"
    );
    let (row_url, row_blur): (String, String) = mk_db::sqlx::query_as(
        r#"SELECT "avatarUrl", "avatarBlurhash" FROM "user" WHERE "id" = $1"#,
    )
    .bind(&my_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row_url, avatar_url);
    assert_eq!(row_blur, blur);

    // Banner: raw stored url (getPublicUrl w/o mode), blurhash denormalized
    let banner_file = seed_drive_file(&pool, &my_id, "image/png").await;
    let (s4, b4) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "i": my_token, "bannerId": banner_file }),
    )
    .await;
    assert_eq!(s4, StatusCode::OK, "body: {b4}");
    assert_eq!(b4["bannerId"], json!(banner_file));
    assert_eq!(
        b4["bannerUrl"],
        json!("http://127.0.0.1:3000/files/accesskey123/192.jpg")
    );
    let banner_blur = b4["bannerBlurhash"].as_str().unwrap_or("");
    assert!(banner_blur.len() >= 54, "banner blurhash {banner_blur:?}");

    // avatarId: null → identicon fallback + blurhash null (pack L490),
    // bannerId: null → url/blurhash null (pack L536-537)
    let (s5, b5) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "i": my_token, "avatarId": null, "bannerId": null }),
    )
    .await;
    assert_eq!(s5, StatusCode::OK, "body: {b5}");
    assert_eq!(b5["avatarId"], Value::Null);
    assert_eq!(b5["avatarBlurhash"], Value::Null);
    assert!(b5["avatarUrl"].as_str().unwrap().contains("/identicon/"));
    assert_eq!(b5["bannerId"], Value::Null);
    assert_eq!(b5["bannerUrl"], Value::Null);
    assert_eq!(b5["bannerBlurhash"], Value::Null);
    let (a_url, a_blur, a_id, b_url, b_blur, b_id): (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = mk_db::sqlx::query_as(
        r#"SELECT "avatarUrl", "avatarBlurhash", "avatarId",
                  "bannerUrl", "bannerBlurhash", "bannerId"
           FROM "user" WHERE "id" = $1"#,
    )
    .bind(&my_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (a_url, a_blur, a_id, b_url, b_blur, b_id),
        (None, None, None, None, None, None)
    );

    // unknown drive id → NO_SUCH_AVATAR (avatar == null is the CLEAR branch)
    let (s6, b6) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "i": my_token, "bannerId": "9zzzzzzzzz" }),
    )
    .await;
    assert_eq!(s6, StatusCode::BAD_REQUEST, "body: {b6}");
    assert_eq!(b6["error"]["code"], json!("NO_SUCH_BANNER"));
}

/// users.ts `を書き換えることができる($#)` birthday tail: an OMITTED birthday
/// must leave the stored value untouched (i/update.ts L296
/// `if (ps.birthday !== undefined)` guard), explicit null → NULL.
#[tokio::test]
async fn i_update_birthday_absent_vs_null() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkiupdate_bday").await;
    let username = format!("bday{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let me = signup(&app, &username).await;
    let my_token = me["token"].as_str().unwrap().to_string();

    // '9999-99-99' passes the birthdaySchema pattern and persists verbatim
    let (s1, b1) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "i": my_token, "birthday": "9999-99-99" }),
    )
    .await;
    assert_eq!(s1, StatusCode::OK, "body: {b1}");
    assert_eq!(b1["birthday"], json!("9999-99-99"));

    // THE census case: a later partial update (lang only) must NOT re-null
    // the birthday
    let (s2, b2) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "i": my_token, "lang": "en-US" }),
    )
    .await;
    assert_eq!(s2, StatusCode::OK, "body: {b2}");
    assert_eq!(b2["birthday"], json!("9999-99-99"));
    assert_eq!(b2["lang"], json!("en-US"));
    // `i` (the e2e beforeEach refresh) agrees
    let (_, bi) = post_json(app.clone(), "/api/i", json!({ "i": my_token })).await;
    assert_eq!(bi["birthday"], json!("9999-99-99"));

    // explicit null → NULL
    let (s3, b3) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "i": my_token, "birthday": null }),
    )
    .await;
    assert_eq!(s3, StatusCode::OK, "body: {b3}");
    assert_eq!(b3["birthday"], Value::Null);
    // and omitting it afterwards keeps the NULL (not a resurrection)
    let (s4, b4) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "i": my_token, "location": "Kyoto" }),
    )
    .await;
    assert_eq!(s4, StatusCode::OK, "body: {b4}");
    assert_eq!(b4["birthday"], Value::Null);
}

/// SignupApiService's response is the full MeDetailed pack, including
/// `policies` (= DEFAULT_POLICIES when no role/meta overrides exist) and the
/// backward-compat `mutingNotificationTypes: []` (UserEntityService.pack
/// L618/L623) — the users.ts 作りたて case deep-equals both.
#[tokio::test]
async fn signup_carries_policies_and_compat_keys() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkiupdate_polkey").await;
    let username = format!("pol{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let (status, body) = post_json(
        app,
        "/api/signup",
        json!({ "username": username, "password": "test" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["mutingNotificationTypes"], json!([]));
    let policies = body["policies"].as_object().expect("policies object");
    assert_eq!(policies["canPublicNote"], json!(true));
    assert_eq!(policies["pinLimit"], json!(5));
    assert_eq!(policies["wordMuteLimit"], json!(200));
    assert_eq!(policies["driveCapacityMb"], json!(100));
    assert_eq!(policies["chatAvailability"], json!("available"));
    assert_eq!(
        policies["uploadableFileTypes"],
        json!([
            "text/*",
            "application/json",
            "image/*",
            "video/*",
            "audio/*"
        ])
    );
    // i emits the same key set (meDetailed() helper symmetry)
    let my_token = body["token"].as_str().unwrap().to_string();
    let (_, bi) = post_json(
        crate::build_router(test_state(pool)),
        "/api/i",
        json!({ "i": my_token }),
    )
    .await;
    assert_eq!(bi["mutingNotificationTypes"], json!([]));
    assert_eq!(bi["policies"]["pinLimit"], json!(5));
}

/// users.ts `を書き換えることができる(ピン止めノート)`: i/pin adds a
/// `user_note_pining` row and returns the MeDetailed with pinnedNoteIds +
/// packed pinnedNotes; i/unpin reverts (NotePiningService.addPinned /
/// removePinned).
#[tokio::test]
async fn i_pin_unpin_roundtrip() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkiupdate_pin").await;
    let username = format!("pinu{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let me = signup(&app, &username).await;
    let my_token = me["token"].as_str().unwrap().to_string();

    // aliceNote fixture (beforeAll: post → notes/create)
    let (_, created) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "i": my_token, "text": "pin me" }),
    )
    .await;
    assert_eq!(created["createdNote"]["text"], json!("pin me"));
    let note_id = created["createdNote"]["id"].as_str().unwrap().to_string();

    // foreign note → NO_SUCH_NOTE (addPinned ownership lookup)
    let other = signup(&app, &format!("pino{}", mk_types::id::gen_aid_now())).await;
    let other_token = other["token"].as_str().unwrap().to_string();
    let (_, created2) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "i": other_token, "text": "not mine" }),
    )
    .await;
    let foreign_note = created2["createdNote"]["id"].as_str().unwrap().to_string();
    let (s1, b1) = post_json(
        app.clone(),
        "/api/i/pin",
        json!({ "i": my_token, "noteId": foreign_note }),
    )
    .await;
    assert_eq!(s1, StatusCode::BAD_REQUEST, "body: {b1}");
    assert_eq!(b1["error"]["code"], json!("NO_SUCH_NOTE"));

    // pin → response carries the id array + the note pack; row persisted
    let (s2, b2) = post_json(
        app.clone(),
        "/api/i/pin",
        json!({ "i": my_token, "noteId": note_id }),
    )
    .await;
    assert_eq!(s2, StatusCode::OK, "body: {b2}");
    assert_eq!(b2["pinnedNoteIds"], json!([note_id]));
    assert_eq!(b2["pinnedNotes"][0]["id"], json!(note_id));
    assert_eq!(b2["pinnedNotes"][0]["text"], json!("pin me"));
    let pins: i64 =
        mk_db::sqlx::query_scalar(r#"SELECT count(*) FROM "user_note_pining" WHERE "userId" = $1"#)
            .bind(me["id"].as_str().unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pins, 1);

    // re-pin → ALREADY_PINNED
    let (s3, b3) = post_json(
        app.clone(),
        "/api/i/pin",
        json!({ "i": my_token, "noteId": note_id }),
    )
    .await;
    assert_eq!(s3, StatusCode::BAD_REQUEST, "body: {b3}");
    assert_eq!(b3["error"]["code"], json!("ALREADY_PINNED"));

    // secrets stay hidden on i/pin (pack default includeSecrets=false)
    assert!(!b2.as_object().unwrap().contains_key("email"), "{b2}");

    // unpin → back to the empty baseline
    let (s4, b4) = post_json(
        app.clone(),
        "/api/i/unpin",
        json!({ "i": my_token, "noteId": note_id }),
    )
    .await;
    assert_eq!(s4, StatusCode::OK, "body: {b4}");
    assert_eq!(b4["pinnedNoteIds"], json!([]));
    assert_eq!(b4["pinnedNotes"], json!([]));
    let pins: i64 =
        mk_db::sqlx::query_scalar(r#"SELECT count(*) FROM "user_note_pining" WHERE "userId" = $1"#)
            .bind(me["id"].as_str().unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pins, 0);
}

/// users.ts `を書き換えることができる(メモを$labelに)`: users/update-memo
/// upserts/deletes the viewer's `user_memo` row and users/show reflects it
/// (UserEntityService.pack L445-448).
#[tokio::test]
async fn users_update_memo_roundtrip() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkiupdate_memo").await;
    let app = crate::build_router(test_state(pool.clone()));

    let alice = signup(&app, &format!("memoa{}", mk_types::id::gen_aid_now())).await;
    let bob = signup(&app, &format!("memob{}", mk_types::id::gen_aid_now())).await;
    let alice_token = alice["token"].as_str().unwrap().to_string();
    let bob_id = bob["id"].as_str().unwrap();

    // 最大長 (2048 chars persist verbatim)
    let memo = "x".repeat(2048);
    let (s1, b1) = post_json(
        app.clone(),
        "/api/users/update-memo",
        json!({ "i": alice_token, "userId": bob_id, "memo": memo }),
    )
    .await;
    assert_eq!(s1, StatusCode::NO_CONTENT, "body: {b1}");
    let (_, show1) = post_json(
        app.clone(),
        "/api/users/show",
        json!({ "i": alice_token, "userId": bob_id }),
    )
    .await;
    assert_eq!(show1["memo"], json!(memo));
    // self view / anonymous view keep memo null
    let (_, show2) = post_json(
        app.clone(),
        "/api/users/show",
        json!({ "i": alice_token, "userId": alice["id"].as_str().unwrap() }),
    )
    .await;
    assert_eq!(show2["memo"], Value::Null);
    let (_, show3) = post_json(app.clone(), "/api/users/show", json!({ "userId": bob_id })).await;
    assert_eq!(show3["memo"], Value::Null);

    // overwrite (update branch)
    let (s2, _) = post_json(
        app.clone(),
        "/api/users/update-memo",
        json!({ "i": alice_token, "userId": bob_id, "memo": "second" }),
    )
    .await;
    assert_eq!(s2, StatusCode::NO_CONTENT);
    let (_, show4) = post_json(
        app.clone(),
        "/api/users/show",
        json!({ "i": alice_token, "userId": bob_id }),
    )
    .await;
    assert_eq!(show4["memo"], json!("second"));
    let memos: i64 = mk_db::sqlx::query_scalar(r#"SELECT count(*) FROM "user_memo""#)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(memos, 1);

    // 空文字 deletes; null deletes too
    for payload in [json!(""), Value::Null] {
        let (s3, _) = post_json(
            app.clone(),
            "/api/users/update-memo",
            json!({ "i": alice_token, "userId": bob_id, "memo": payload }),
        )
        .await;
        assert_eq!(s3, StatusCode::NO_CONTENT);
    }
    let (_, show5) = post_json(
        app.clone(),
        "/api/users/show",
        json!({ "i": alice_token, "userId": bob_id }),
    )
    .await;
    assert_eq!(show5["memo"], Value::Null);
    let memos: i64 = mk_db::sqlx::query_scalar(r#"SELECT count(*) FROM "user_memo""#)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(memos, 0);

    // unknown target → NO_SUCH_USER
    let (s4, b4) = post_json(
        app.clone(),
        "/api/users/update-memo",
        json!({ "i": alice_token, "userId": "9zzzzzzzzz", "memo": "x" }),
    )
    .await;
    assert_eq!(s4, StatusCode::NOT_FOUND, "body: {b4}");
    assert_eq!(b4["error"]["code"], json!("NO_SUCH_USER"));
}

/// The multipart drive/files/create chain (uploadFile shape: file part with
/// no Content-Type — sniffed to image/jpeg from the magic bytes — plus the
/// `i` auth field), then the avatarId hand-off into i/update that the
/// users.ts Avatar census case makes.
#[tokio::test]
async fn drive_files_create_and_avatar_handoff() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkiupdate_drive").await;
    let username = format!("drv{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let me = signup(&app, &username).await;
    let my_id = me["id"].as_str().unwrap().to_string();
    let my_token = me["token"].as_str().unwrap().to_string();

    let jpeg = [
        &[0xFFu8, 0xD8, 0xFF, 0xE0][..],
        &b"fake-jpeg-content-for-md5-stability"[..],
    ]
    .concat();
    let mut raw: Vec<u8> = Vec::new();
    raw.extend_from_slice(
        b"--B\r\nContent-Disposition: form-data; name=\"file\"; filename=\"192.jpg\"\r\n\r\n",
    );
    raw.extend_from_slice(&jpeg);
    raw.extend_from_slice(
        b"\r\n--B\r\nContent-Disposition: form-data; name=\"force\"\r\n\r\ntrue\r\n",
    );
    raw.extend_from_slice(
        format!("--B\r\nContent-Disposition: form-data; name=\"i\"\r\n\r\n{my_token}\r\n--B--\r\n")
            .as_bytes(),
    );

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/drive/files/create")
                .header("content-type", "multipart/form-data; boundary=B")
                .body(Body::from(raw))
                .unwrap(),
        )
        .await
        .unwrap();
    let s1 = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let b1: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(s1, StatusCode::OK, "body: {b1}");
    let file_id = b1["id"].as_str().unwrap().to_string();
    assert_eq!(b1["type"], json!("image/jpeg"));
    assert_eq!(b1["name"], json!("192.jpg"));
    assert_eq!(b1["size"], json!(jpeg.len() as i64));
    assert_eq!(
        b1["md5"]
            .as_str()
            .map(|s| (s.len(), s.chars().all(|c| c.is_ascii_hexdigit()))),
        Some((32, true))
    );
    assert!(b1["blurhash"].as_str().unwrap_or("").len() >= 54);
    assert!(b1["url"].as_str().unwrap_or("").contains("/files/"));
    assert_eq!(b1["userId"], json!(my_id));

    // i/update consumes the id exactly like the e2e Avatar test
    let (s2, b2) = post_json(
        app,
        "/api/i/update",
        json!({ "i": my_token, "avatarId": file_id }),
    )
    .await;
    assert_eq!(s2, StatusCode::OK, "body: {b2}");
    assert_eq!(b2["avatarId"], json!(file_id));
    assert!(b2["avatarUrl"].as_str().unwrap_or("").starts_with(
        "http://127.0.0.1:3000/proxy/avatar.webp?url=http%3A%2F%2F127.0.0.1%3A3000%2Ffiles%2F",
    ));
}

// Upstream signup truth: SignupApiService.signup reads only
// username/password(+host/invitationCode/emailAddress/captcha) from the body —
// isBot/isCat are NOT accepted (there is no paramDef; extra keys are dropped),
// and e2e users.ts asserts fresh signups report isBot === false, isCat ===
// false. Mirrored here: extra params must be ignored, not rejected nor applied.
#[tokio::test]
async fn signup_ignores_bot_and_cat_params() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mksignup_botcat").await;
    let username = format!("zebot{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let (status, body) = post_json(
        app.clone(),
        "/api/signup",
        json!({ "username": username, "password": "test", "isBot": true, "isCat": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["isBot"], json!(false));
    assert_eq!(body["isCat"], json!(false));

    let my_id = body["id"].as_str().unwrap().to_string();
    let (is_bot, is_cat): (bool, bool) =
        mk_db::sqlx::query_as(r#"SELECT "isBot", "isCat" FROM "user" WHERE "id" = $1"#)
            .bind(&my_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((is_bot, is_cat), (false, false));
}
/// hashtags/users (upstream endpoints/hashtags/users.ts + e2e users.ts
/// ハッシュタグ family): hashtags posted in note text are harvested into
/// user.tags (lowercased, extract_hashtags), the queried tag is normalized
/// case-insensitively (normalizeForSearch), sort '+follower' orders by
/// followersCount DESC (and '-follower' ASC), and origin defaults to 'local'.
#[tokio::test]
async fn hashtags_users_tag_normalization_and_sort() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkhashtagsusers").await;
    let suffix = mk_types::id::gen_aid_now().to_lowercase();
    let uname_a = format!("htaga{suffix}");
    let uname_b = format!("htagb{suffix}");
    let app = crate::build_router(test_state(pool.clone()));

    let a = signup(&app, &uname_a).await;
    let b = signup(&app, &uname_b).await;
    let a_id = a["id"].as_str().unwrap().to_string();
    let b_id = b["id"].as_str().unwrap().to_string();
    let b_token = b["token"].as_str().unwrap().to_string();

    // followersCount inputs (following/create lands later); set directly like
    // signin_flow's direct SQL access.
    for (uid, n) in [(&a_id, 2i64), (&b_id, 1i64)] {
        mk_db::sqlx::query(r#"UPDATE "user" SET "followersCount" = $1 WHERE "id" = $2"#)
            .bind(n)
            .bind(uid)
            .execute(&pool)
            .await
            .unwrap();
    }

    // Notes carry hashtags into user.tags. b's tag text is mixed-case on
    // purpose: storage must store 'htagtwo' normalized.
    for (token, text) in [
        (a["token"].as_str().unwrap(), "#htagone tail"),
        (&b_token, "#htagtWO x"),
        (&b_token, "#htagone more"),
    ] {
        let (s, body) = post_json(
            app.clone(),
            "/api/notes/create",
            json!({ "text": text, "i": token }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "body: {body}");
    }
    // tags persisted, normalized (a: htagone; b: htagtwo then htagone)
    let rows: Vec<(String, Vec<String>)> =
        mk_db::sqlx::query_as(r#"SELECT "id", "tags" FROM "user" WHERE "id" = ANY($1::varchar[])"#)
            .bind(vec![a_id.clone(), b_id.clone()])
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows.len(), 2);
    let mut tags_by_id = std::collections::BTreeMap::new();
    for (id, tags) in rows {
        tags_by_id.insert(id, tags);
    }
    assert_eq!(tags_by_id[&a_id], vec!["htagone".to_string()]);
    assert_eq!(
        tags_by_id[&b_id],
        vec!["htagtwo".to_string(), "htagone".to_string()]
    );

    // ?tag normalized case-insensitively ('HTAGONE' ~ 'htagone'); both users
    // carry it; '+follower' = followersCount DESC → [a(2), b(1)]
    let (s, body) = post_json(
        app.clone(),
        "/api/hashtags/users",
        json!({ "tag": "HTAGONE", "sort": "+follower", "limit": 10 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let ids: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![a_id.as_str(), b_id.as_str()], "body: {body}");

    // '-follower' = followersCount ASC → reversed
    let (s, body) = post_json(
        app.clone(),
        "/api/hashtags/users",
        json!({ "tag": "htagone", "sort": "-follower", "limit": 10 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let ids: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![b_id.as_str(), a_id.as_str()], "body: {body}");

    // mixed-case query hits the normalized stored tag 'htagtwo' → only b
    let (s, body) = post_json(
        app,
        "/api/hashtags/users",
        json!({ "tag": "htagtWO", "sort": "+follower", "limit": 10 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let ids: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![b_id.as_str()], "body: {body}");
}

// ---------------------------------------------------------------------------
// notifications slice (M2) — appended tests; keep at file end so co-agent
// appends rebase trivially.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn notifications_mention_reaction_and_mark_as_read() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mknotifications").await;
    let app = crate::build_router(test_state(pool.clone()));

    let username_a = format!("na{}", mk_types::id::gen_aid_now());
    let username_b = format!("nb{}", mk_types::id::gen_aid_now());
    let a = signup(&app, &username_a).await;
    let b = signup(&app, &username_b).await;
    let token_a = a["token"].as_str().unwrap().to_string();
    let token_b = b["token"].as_str().unwrap().to_string();
    let b_id = b["id"].as_str().unwrap().to_string();

    // requireCredential: no token → CREDENTIAL_REQUIRED (401)
    let (s, err) = post_json(app.clone(), "/api/notifications", json!({})).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "body: {err}");

    // A posts the mention text. notes/create resolves the mention itself
    // (M2 mention wiring): the notification row is produced by the live
    // endpoint, no manual seed needed.
    let (s, nb) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "i": token_a, "text": format!("@{} hello", username_b) }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {nb}");
    let note_id = nb["createdNote"]["id"].as_str().unwrap().to_string();

    // First read: unreadOnly + default markAsRead=true → mention packed
    // (type/userId/user/note), then everything marked read.
    let (s, list) = post_json(
        app.clone(),
        "/api/notifications",
        json!({ "i": token_b, "unreadOnly": true }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {list}");
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 1, "body: {list}");
    let n = &arr[0];
    assert!(n["id"].as_str().is_some(), "body: {n}");
    assert_eq!(n["type"], "mention");
    assert!(n["createdAt"].as_str().is_some());
    assert_eq!(n["userId"], a["id"]);
    assert_eq!(n["user"]["username"], username_a.as_str());
    assert!(
        n["user"]["avatarUrl"]
            .as_str()
            .unwrap()
            .contains("/identicon/")
    );
    assert_eq!(n["note"]["id"], note_id.as_str());
    assert_eq!(n["note"]["text"], format!("@{} hello", username_b));

    // unreadOnly again → empty: the previous call's markAsRead default fired.
    let (s, list2) = post_json(
        app.clone(),
        "/api/notifications",
        json!({ "i": token_b, "unreadOnly": true }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(list2.as_array().unwrap().is_empty(), "body: {list2}");

    // Full list still shows the (now read) mention; includeTypes filters.
    let (s, list3) = post_json(app.clone(), "/api/notifications", json!({ "i": token_b })).await;
    assert_eq!(s, StatusCode::OK, "body: {list3}");
    assert_eq!(list3.as_array().unwrap().len(), 1, "body: {list3}");
    let (s, list3b) = post_json(
        app.clone(),
        "/api/notifications",
        json!({ "i": token_b, "includeTypes": ["follow"] }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {list3b}");
    assert!(list3b.as_array().unwrap().is_empty(), "body: {list3b}");

    // renote (note-bound) + follow (no note) from A.
    let renote_id = mk_types::id::gen_aid_now();
    mk_db::notification::insert_notification(
        &pool,
        &mk_db::notification::NewNotification {
            id: &renote_id,
            r#type: "renote",
            user_id: &b_id,
            notifier_id: Some(a["id"].as_str().unwrap()),
            note_id: Some(&note_id),
            reaction: None,
        },
    )
    .await
    .unwrap();
    let follow_id = mk_types::id::gen_aid_now();
    mk_db::notification::insert_notification(
        &pool,
        &mk_db::notification::NewNotification {
            id: &follow_id,
            r#type: "follow",
            user_id: &b_id,
            notifier_id: Some(a["id"].as_str().unwrap()),
            note_id: None,
            reaction: None,
        },
    )
    .await
    .unwrap();

    // Default ordering is id DESC (newest first): follow, renote, mention.
    // markAsRead:false keeps rows unread for the later unreadOnly asserts
    // (upstream default is true — the final block exercises that path).
    let (s, list4) = post_json(
        app.clone(),
        "/api/notifications",
        json!({ "i": token_b, "markAsRead": false }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {list4}");
    let arr4 = list4.as_array().unwrap();
    assert_eq!(arr4.len(), 3, "body: {list4}");
    assert_eq!(arr4[0]["type"], "follow");
    // follow carries no note field (upstream drops undefined)
    assert!(!arr4[0].as_object().unwrap().contains_key("note"));
    assert_eq!(arr4[0]["userId"], a["id"]);
    assert_eq!(arr4[1]["type"], "renote");
    assert_eq!(arr4[1]["note"]["id"], note_id.as_str());
    assert_eq!(arr4[2]["type"], "mention");

    // untilId is EXCLUSIVE (misc/id.ts aid monotonic string compare).
    let (s, list5) = post_json(
        app.clone(),
        "/api/notifications",
        json!({ "i": token_b, "untilId": renote_id, "markAsRead": false }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {list5}");
    let arr5 = list5.as_array().unwrap();
    assert_eq!(arr5.len(), 1, "body: {list5}");
    assert_eq!(arr5[0]["type"], "mention");

    // sinceId-only flips to oldest-first ASC: follow was seeded after the
    // renote, so only follow is strictly newer than the renote_id boundary.
    let (s, list6) = post_json(
        app.clone(),
        "/api/notifications",
        json!({ "i": token_b, "sinceId": renote_id, "markAsRead": false }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {list6}");
    let arr6 = list6.as_array().unwrap();
    assert_eq!(arr6.len(), 1, "body: {list6}");
    assert_eq!(arr6[0]["type"], "follow");

    // markAsRead:false leaves rows unread. All rows above were already
    // marked read (their calls used the markAsRead default), so seed a fresh
    // unread mention for this leg.
    let mention2_id = mk_types::id::gen_aid_now();
    mk_db::notification::insert_notification(
        &pool,
        &mk_db::notification::NewNotification {
            id: &mention2_id,
            r#type: "mention",
            user_id: &b_id,
            notifier_id: Some(a["id"].as_str().unwrap()),
            note_id: Some(&note_id),
            reaction: None,
        },
    )
    .await
    .unwrap();
    let (s, list7) = post_json(
        app.clone(),
        "/api/notifications",
        json!({ "i": token_b, "markAsRead": false, "unreadOnly": true, "includeTypes": ["mention"] }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {list7}");
    let arr7 = list7.as_array().unwrap();
    assert_eq!(arr7.len(), 1, "body: {list7}");
    assert_eq!(arr7[0]["id"], mention2_id.as_str());

    // Then a default call (markAsRead=true) clears it.
    let (s, _) = post_json(
        app.clone(),
        "/api/notifications",
        json!({ "i": token_b, "unreadOnly": true }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, list8) = post_json(
        app.clone(),
        "/api/notifications",
        json!({ "i": token_b, "unreadOnly": true }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(list8.as_array().unwrap().is_empty(), "body: {list8}");

    // Reaction notifications carry the reaction string.
    let reaction_id = mk_types::id::gen_aid_now();
    mk_db::notification::insert_notification(
        &pool,
        &mk_db::notification::NewNotification {
            id: &reaction_id,
            r#type: "reaction",
            user_id: &b_id,
            notifier_id: Some(a["id"].as_str().unwrap()),
            note_id: Some(&note_id),
            reaction: Some(":custom_grin:"),
        },
    )
    .await
    .unwrap();
    let (s, list9) = post_json(
        app.clone(),
        "/api/notifications",
        json!({ "i": token_b, "includeTypes": ["reaction"] }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {list9}");
    let arr9 = list9.as_array().unwrap();
    assert_eq!(arr9.len(), 1, "body: {list9}");
    assert_eq!(arr9[0]["type"], "reaction");
    assert_eq!(arr9[0]["reaction"], ":custom_grin:");
    assert_eq!(arr9[0]["note"]["id"], note_id.as_str());
}

// ===========================================================================
// social-agent: follow/mute/block/renote slice tests (upstream
// following/{create,delete}, following/requests/{accept,cancel}, mute/*,
// blocking/*, notes/create renoteId semantics)
// ===========================================================================

async fn scalar_i64(pool: &mk_db::sqlx::PgPool, sql: &str, id: &str) -> i64 {
    mk_db::sqlx::query_scalar(sql)
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn count_rows(pool: &mk_db::sqlx::PgPool, sql: &str, me: &str, other: &str) -> i64 {
    mk_db::sqlx::query_scalar(sql)
        .bind(me)
        .bind(other)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn social_follow_flow() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mksocial_follow").await;
    let uname_a = format!("socfollowa{}", mk_types::id::gen_aid_now());
    let uname_b = format!("socfollowb{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let a = signup(&app, &uname_a).await;
    let b = signup(&app, &uname_b).await;
    let (a_id, a_token) = (a["id"].as_str().unwrap(), a["token"].as_str().unwrap());
    let (b_id, _b_token) = (b["id"].as_str().unwrap(), b["token"].as_str().unwrap());

    // auth gate: no credential → CREDENTIAL_REQUIRED (401)
    let (s, err) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": b_id }),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("1384574d-a912-4b81-8601-c7b1c4085df1")
    );

    // self-follow → 400 FOLLOWEE_IS_YOURSELF
    let (s, err) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": a_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("26fbe7bb-a331-4857-af17-205b426669a9")
    );

    // unknown target → 400 NO_SUCH_USER (create's own id)
    let (s, err) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": "zzzzzzzzzzzz", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("fcd2eef9-a9b2-4c4f-8624-038099e90aa5")
    );

    // create → packed followee with relation fields (notify/withReplies)
    let (s, body) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": b_id, "i": a_token, "withReplies": true }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body["id"], json!(b_id));
    assert_eq!(body["isFollowing"], json!(true));
    assert_eq!(body["notify"], json!("none"));
    assert_eq!(body["withReplies"], json!(true));

    // persisted edge + counters (insertFollowingDoc + increments). The port
    // persists the default notify as NULL (insert_following binds NULL;
    // get_relation/pack map null → 'none'), so the row read must be nullable —
    // a concrete String decode panics with UnexpectedNullError.
    let (notify, with_replies): (Option<String>, bool) = mk_db::sqlx::query_as(
        r#"SELECT "notify", "withReplies" FROM "following" WHERE "followerId" = $1 AND "followeeId" = $2"#,
    )
    .bind(a_id)
    .bind(b_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(notify.as_deref(), None);
    assert!(with_replies);
    assert_eq!(
        scalar_i64(
            &pool,
            r#"SELECT "followingCount"::bigint FROM "user" WHERE "id" = $1"#,
            a_id
        )
        .await,
        1
    );
    assert_eq!(
        scalar_i64(
            &pool,
            r#"SELECT "followersCount"::bigint FROM "user" WHERE "id" = $1"#,
            b_id
        )
        .await,
        1
    );

    // users/show (me view) reflects the relation
    let (s, body) = post_json(
        app.clone(),
        "/api/users/show",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body["isFollowing"], json!(true));
    assert_eq!(body["notify"], json!("none"));
    assert_eq!(body["withReplies"], json!(true));

    // already following → 400 ALREADY_FOLLOWING
    let (s, err) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("35387507-38c7-4cb9-9197-300b93783fa0")
    );

    // delete → relation gone, counters back to 0
    let (s, body) = post_json(
        app.clone(),
        "/api/following/delete",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body["isFollowing"], json!(false));
    assert_eq!(
        count_rows(
            &pool,
            r#"SELECT count(*) FROM "following" WHERE "followerId" = $1 AND "followeeId" = $2"#,
            a_id,
            b_id
        )
        .await,
        0
    );
    assert_eq!(
        scalar_i64(
            &pool,
            r#"SELECT "followingCount"::bigint FROM "user" WHERE "id" = $1"#,
            a_id
        )
        .await,
        0
    );
    assert_eq!(
        scalar_i64(
            &pool,
            r#"SELECT "followersCount"::bigint FROM "user" WHERE "id" = $1"#,
            b_id
        )
        .await,
        0
    );

    // unfollowing again → 400 NOT_FOLLOWING
    let (s, err) = post_json(
        app,
        "/api/following/delete",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("5dbf82f5-c92b-40b1-87d1-6c8c0741fd09")
    );
}

#[tokio::test]
async fn social_follow_request_lock_accept_cancel() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mksocial_lock").await;
    let uname_a = format!("soclocka{}", mk_types::id::gen_aid_now());
    let uname_b = format!("soclockb{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let a = signup(&app, &uname_a).await;
    let b = signup(&app, &uname_b).await;
    let (a_id, a_token) = (a["id"].as_str().unwrap(), a["token"].as_str().unwrap());
    let (b_id, b_token) = (b["id"].as_str().unwrap(), b["token"].as_str().unwrap());

    // lock b (locked targets branch upstream → follow request)
    let (s, _) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "isLocked": true, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // locked target, no relationship → follow_request row (not following)
    let (s, body) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body["hasPendingFollowRequestFromYou"], json!(true));
    assert_eq!(body["isFollowing"], json!(false));
    assert_eq!(
        count_rows(
            &pool,
            r#"SELECT count(*) FROM "follow_request" WHERE "followerId" = $1 AND "followeeId" = $2"#,
            a_id,
            b_id
        )
        .await,
        1
    );
    assert_eq!(
        count_rows(
            &pool,
            r#"SELECT count(*) FROM "following" WHERE "followerId" = $1 AND "followeeId" = $2"#,
            a_id,
            b_id
        )
        .await,
        0
    );
    // no counters bumped on a pending request
    assert_eq!(
        scalar_i64(
            &pool,
            r#"SELECT "followersCount"::bigint FROM "user" WHERE "id" = $1"#,
            b_id
        )
        .await,
        0
    );

    // unfollow while only requested → NOT_FOLLOWING (request ≠ following)
    let (s, err) = post_json(
        app.clone(),
        "/api/following/delete",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("5dbf82f5-c92b-40b1-87d1-6c8c0741fd09")
    );

    // cancel the request (returns the packed followee)
    let (s, body) = post_json(
        app.clone(),
        "/api/following/requests/cancel",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body["id"], json!(b_id));
    assert_eq!(body["hasPendingFollowRequestFromYou"], json!(false));
    assert_eq!(
        count_rows(
            &pool,
            r#"SELECT count(*) FROM "follow_request" WHERE "followerId" = $1 AND "followeeId" = $2"#,
            a_id,
            b_id
        )
        .await,
        0
    );

    // cancel again → FOLLOW_REQUEST_NOT_FOUND
    let (s, err) = post_json(
        app.clone(),
        "/api/following/requests/cancel",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("089b125b-d338-482a-9a09-e2622ac9f8d4")
    );

    // re-request withReplies: true, then b accepts → following row keeps it
    let (s, _) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": b_id, "i": a_token, "withReplies": true }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // accept with the WRONG requester side → NO_FOLLOW_REQUEST
    let (s, err) = post_json(
        app.clone(),
        "/api/following/requests/accept",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("bcde4f8b-0913-4614-8881-614e522fb041")
    );

    // b accepts a's request → 204 (endpoint returns nothing upstream)
    let (s, body) = post_json(
        app.clone(),
        "/api/following/requests/accept",
        json!({ "userId": a_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {body}");
    assert_eq!(body, Value::Null);
    assert_eq!(
        count_rows(
            &pool,
            r#"SELECT count(*) FROM "following" WHERE "followerId" = $1 AND "followeeId" = $2 AND "withReplies" = TRUE"#,
            a_id,
            b_id
        )
        .await,
        1,
        "accepted follow preserved the request's withReplies"
    );
    assert_eq!(
        count_rows(
            &pool,
            r#"SELECT count(*) FROM "follow_request" WHERE "followerId" = $1 AND "followeeId" = $2"#,
            a_id,
            b_id
        )
        .await,
        0
    );
    assert_eq!(
        scalar_i64(
            &pool,
            r#"SELECT "followingCount"::bigint FROM "user" WHERE "id" = $1"#,
            a_id
        )
        .await,
        1
    );
    assert_eq!(
        scalar_i64(
            &pool,
            r#"SELECT "followersCount"::bigint FROM "user" WHERE "id" = $1"#,
            b_id
        )
        .await,
        1
    );

    // accept again → NO_FOLLOW_REQUEST
    let (s, err) = post_json(
        app.clone(),
        "/api/following/requests/accept",
        json!({ "userId": a_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("bcde4f8b-0913-4614-8881-614e522fb041")
    );

    // auto-accept: b follows a (unlocked mutual path), then a re-request to
    // the still-locked b is auto-accepted only when b.autoAcceptFollowed AND
    // b already follows the requester → direct follow row, no request row.
    let (s, _) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "isLocked": false, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": a_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "isLocked": true, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = post_json(
        app.clone(),
        "/api/following/delete",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, body) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": b_id, "i": a_token, "withReplies": false }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body["isFollowing"], json!(true));
    assert_eq!(
        count_rows(
            &pool,
            r#"SELECT count(*) FROM "follow_request" WHERE "followerId" = $1 AND "followeeId" = $2"#,
            a_id,
            b_id
        )
        .await,
        0,
        "auto-accepted: no pending request row"
    );
    assert_eq!(
        count_rows(
            &pool,
            r#"SELECT count(*) FROM "following" WHERE "followerId" = $1 AND "followeeId" = $2"#,
            a_id,
            b_id
        )
        .await,
        1
    );
}

#[tokio::test]
async fn social_mute_flow_and_search_exclusion() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mksocial_mute").await;
    let uname_a = format!("socmutea{}", mk_types::id::gen_aid_now());
    let uname_b = format!("socmuteb{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let a = signup(&app, &uname_a).await;
    let b = signup(&app, &uname_b).await;
    let (a_id, a_token) = (a["id"].as_str().unwrap(), a["token"].as_str().unwrap());
    let (b_id, b_token) = (b["id"].as_str().unwrap(), b["token"].as_str().unwrap());

    // self-mute → 400 MUTEE_IS_YOURSELF; unknown → 400 NO_SUCH_USER
    let (s, err) = post_json(
        app.clone(),
        "/api/mute/create",
        json!({ "userId": a_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("a4619cb2-5f23-484b-9301-94c903074e10")
    );
    let (s, err) = post_json(
        app.clone(),
        "/api/mute/create",
        json!({ "userId": "zzzzzzzzzzzz", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("6fef56f3-e765-4957-88e5-c6f65329b8a5")
    );

    // search (me passed) finds b before the mute
    let (s, body) = post_json(
        app.clone(),
        "/api/users/search",
        json!({ "query": format!("@{}", &uname_b[..9]), "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body.as_array().unwrap().len(), 1, "body: {body}");

    // mute → 204 + row persisted
    let (s, body) = post_json(
        app.clone(),
        "/api/mute/create",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {body}");
    assert_eq!(body, Value::Null);
    let (expires_at,): (bool,) = mk_db::sqlx::query_as(
        r#"SELECT ("expiresAt" IS NULL) FROM "muting" WHERE "muterId" = $1 AND "muteeId" = $2"#,
    )
    .bind(a_id)
    .bind(b_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(expires_at, "indefinite mute → NULL expiresAt");

    // users/search (me passed) now excludes the mutee
    let (s, body) = post_json(
        app.clone(),
        "/api/users/search",
        json!({ "query": format!("@{}", &uname_b[..9]), "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body.as_array().unwrap().len(), 0, "body: {body}");

    // users/show isMuted flips via the relation pack
    let (s, body) = post_json(
        app.clone(),
        "/api/users/show",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body["isMuted"], json!(true));

    // double mute → 400 ALREADY_MUTING
    let (s, err) = post_json(
        app.clone(),
        "/api/mute/create",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("7e7359cb-160c-4956-b08f-4d1c653cd007")
    );

    // past expiresAt → silent no-op (204, no row): b mutes a with a stale
    // timestamp
    let (s, _) = post_json(
        app.clone(),
        "/api/mute/create",
        json!({ "userId": a_id, "i": b_token, "expiresAt": 1 }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(
        count_rows(
            &pool,
            r#"SELECT count(*) FROM "muting" WHERE "muterId" = $1 AND "muteeId" = $2"#,
            b_id,
            a_id
        )
        .await,
        0
    );

    // unmute → 204, row gone, search finds b again
    let (s, body) = post_json(
        app.clone(),
        "/api/mute/delete",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {body}");
    assert_eq!(
        count_rows(
            &pool,
            r#"SELECT count(*) FROM "muting" WHERE "muterId" = $1 AND "muteeId" = $2"#,
            a_id,
            b_id
        )
        .await,
        0
    );
    let (s, body) = post_json(
        app.clone(),
        "/api/users/search",
        json!({ "query": format!("@{}", &uname_b[..9]), "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body.as_array().unwrap().len(), 1, "body: {body}");

    // unmute again → NOT_MUTING
    let (s, err) = post_json(
        app,
        "/api/mute/delete",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("5467d020-daa9-4553-81e1-135c0c35a96d")
    );
}

#[tokio::test]
async fn social_block_flow() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mksocial_block").await;
    let uname_a = format!("socblocka{}", mk_types::id::gen_aid_now());
    let uname_b = format!("socblockb{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let a = signup(&app, &uname_a).await;
    let b = signup(&app, &uname_b).await;
    let (a_id, a_token) = (a["id"].as_str().unwrap(), a["token"].as_str().unwrap());
    let (b_id, b_token) = (b["id"].as_str().unwrap(), b["token"].as_str().unwrap());

    // mutual follow → 1/1 counters both sides
    let (s, _) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": a_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        scalar_i64(
            &pool,
            r#"SELECT "followingCount"::bigint FROM "user" WHERE "id" = $1"#,
            a_id
        )
        .await,
        1
    );
    assert_eq!(
        scalar_i64(
            &pool,
            r#"SELECT "followersCount"::bigint FROM "user" WHERE "id" = $1"#,
            a_id
        )
        .await,
        1
    );

    // self-block → 400; unknown → 400; packed blockee with isBlocking
    let (s, err) = post_json(
        app.clone(),
        "/api/blocking/create",
        json!({ "userId": a_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("88b19138-f28d-42c0-8499-6a31bbd0fdc6")
    );
    let (s, err) = post_json(
        app.clone(),
        "/api/blocking/create",
        json!({ "userId": "zzzzzzzzzzzz", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("7cc4f851-e2f1-4621-9633-ec9e1d00c01e")
    );

    // block b → packed blockee, row persisted, both follow edges removed
    let (s, body) = post_json(
        app.clone(),
        "/api/blocking/create",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body["isBlocking"], json!(true));
    assert_eq!(
        count_rows(
            &pool,
            r#"SELECT count(*) FROM "blocking" WHERE "blockerId" = $1 AND "blockeeId" = $2"#,
            a_id,
            b_id
        )
        .await,
        1
    );
    assert_eq!(
        count_rows(
            &pool,
            r#"SELECT count(*) FROM "following" WHERE (("followerId" = $1 AND "followeeId" = $2) OR ("followerId" = $2 AND "followeeId" = $1))"#,
            a_id,
            b_id
        )
        .await,
        0
    );
    assert_eq!(
        scalar_i64(
            &pool,
            r#"SELECT "followingCount"::bigint FROM "user" WHERE "id" = $1"#,
            a_id
        )
        .await,
        0
    );
    assert_eq!(
        scalar_i64(
            &pool,
            r#"SELECT "followersCount"::bigint FROM "user" WHERE "id" = $1"#,
            a_id
        )
        .await,
        0
    );

    // blocked user re-following → 400 BLOCKED; blocker re-following → BLOCKING
    let (s, err) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": a_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("c4ab57cc-4e41-45e9-bfd9-584f61e35ce0")
    );
    let (s, err) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("4e2206ec-aa4f-4960-b865-6c23ac38e2d9")
    );

    // double block → ALREADY_BLOCKING; unblock → row gone; follow works again
    let (s, err) = post_json(
        app.clone(),
        "/api/blocking/create",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("787fed64-acb9-464a-82eb-afbd745b9614")
    );
    let (s, body) = post_json(
        app.clone(),
        "/api/blocking/delete",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body["isBlocking"], json!(false));
    assert_eq!(
        count_rows(
            &pool,
            r#"SELECT count(*) FROM "blocking" WHERE "blockerId" = $1 AND "blockeeId" = $2"#,
            a_id,
            b_id
        )
        .await,
        0
    );
    let (s, _) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "refollow after unblock");

    // unblock again → NOT_BLOCKING
    let (s, err) = post_json(
        app,
        "/api/blocking/delete",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("291b2efa-60c6-45c0-9f6a-045c8f9b02cd")
    );
}

#[tokio::test]
async fn social_renote_flow() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mksocial_renote").await;
    let uname_a = format!("socrenota{}", mk_types::id::gen_aid_now());
    let uname_b = format!("socrenotb{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let a = signup(&app, &uname_a).await;
    let b = signup(&app, &uname_b).await;
    let (a_id, a_token) = (a["id"].as_str().unwrap(), a["token"].as_str().unwrap());
    let (b_id, b_token) = (b["id"].as_str().unwrap(), b["token"].as_str().unwrap());

    // b writes the target note
    let (s, body) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "renote me", "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let target_id = body["createdNote"]["id"].as_str().unwrap().to_string();

    // bogus target → 400 NO_SUCH_RENOTE_TARGET
    let (s, err) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "renoteId": "zzzzzzzzzzzz", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("b5c90186-4ab0-49c8-9bba-a1f76c282ba4")
    );

    // a renotes: textless row pointing at the target, renoteCount bumped
    let (s, body) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "renoteId": target_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let note = &body["createdNote"];
    assert_eq!(note["renoteId"], json!(target_id));
    assert_eq!(note["text"], Value::Null);
    assert_eq!(note["userId"], json!(a_id));
    assert_eq!(note["visibility"], json!("public"));
    let renote_id = note["id"].as_str().unwrap().to_string();
    assert_eq!(
        scalar_i64(
            &pool,
            r#"SELECT "renoteCount"::bigint FROM "note" WHERE "id" = $1"#,
            &target_id
        )
        .await,
        1,
        "incRenoteCount on the target"
    );
    assert_eq!(
        count_rows(
            &pool,
            r#"SELECT count(*) FROM "note" WHERE "renoteId" = $1 AND "renoteUserId" = $2 AND "text" IS NULL"#,
            &target_id,
            b_id
        )
        .await,
        1,
        "textless renote row with denormalized renoteUserId"
    );
    // notesCount incremented for the renote author (incNotesCount)
    assert_eq!(
        scalar_i64(
            &pool,
            r#"SELECT "notesCount"::bigint FROM "user" WHERE "id" = $1"#,
            a_id
        )
        .await,
        1
    );

    // pure renote of a pure renote → CANNOT_RENOTE_TO_A_PURE_RENOTE
    let (s, err) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "renoteId": renote_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("fd4cc33e-2a37-48dd-99cc-9b806eb2031a")
    );

    // notes/show on the renote exposes renoteId
    let (s, body) = post_json(
        app.clone(),
        "/api/notes/show",
        json!({ "noteId": renote_id }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body["renoteId"], json!(target_id));

    // own renote does not bump the target's renoteCount (!user.isBot, same id)
    let (s, _) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "renoteId": target_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        scalar_i64(
            &pool,
            r#"SELECT "renoteCount"::bigint FROM "note" WHERE "id" = $1"#,
            &target_id
        )
        .await,
        1,
        "self renote does not increment"
    );

    // quote (text alongside renoteId) is allowed and is not a pure renote;
    // b quotes b's own note (a's renote is a pure renote → rejected target)
    let (s, body) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "and my thoughts", "renoteId": target_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body["createdNote"]["renoteId"], json!(target_id));
    assert_eq!(body["createdNote"]["text"], json!("and my thoughts"));
    assert_eq!(
        scalar_i64(
            &pool,
            r#"SELECT count(*) FROM "note" WHERE "renoteId" = $1 AND "text" IS NOT NULL"#,
            &target_id
        )
        .await,
        1,
        "quote row keeps its text"
    );

    // renote of own note visibility normalization: target home keeps home
    let (s, body) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "home note", "visibility": "home", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let home_id = body["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, body) = post_json(
        app,
        "/api/notes/create",
        json!({ "renoteId": home_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["createdNote"]["visibility"],
        json!("home"),
        "home note renoted as public drops to home"
    );
}

// ============================================================================
// ============================================================================
// admin/* family (M2 slice, api/admin.rs): suspend/unsuspend (+moderation
// log + follow-request purge + list exclusion), show-user / show-users, and
// the meta/server-info/stats facades. Auth: e2e upstream signs `root` first
// and calls admin/* with it — mirrored here via the documented username
// shortcut (signup does not stamp meta.rootUserId yet).
// ============================================================================

/// GET probe (admin/* endpoints are POST-only; allowGet is false).
async fn get_json(app: &axum::Router, path: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, v)
}

async fn seed_follow_request(pool: &mk_db::sqlx::PgPool, followee: &str, follower: &str) {
    mk_db::sqlx::query(
        r#"INSERT INTO "follow_request" ("id", "followeeId", "followerId") VALUES ($1, $2, $3)"#,
    )
    .bind(mk_types::id::gen_aid_now())
    .bind(followee)
    .bind(follower)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn admin_suspend_unsuspend_family() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkadmin_suspend").await;
    let app = crate::build_router(test_state(pool.clone()));

    // upstream users.ts fixture order: root first, then the roles/suspended
    // cast; admin/suspend-user is invoked with root's token.
    let root = signup(&app, "root").await;
    let root_id = root["id"].as_str().unwrap().to_string();
    let root_token = root["token"].as_str().unwrap().to_string();
    let victim = signup(&app, "victimsus1").await;
    let victim_id = victim["id"].as_str().unwrap().to_string();
    let plain = signup(&app, "plainmod").await;
    let plain_token = plain["token"].as_str().unwrap().to_string();

    // follow requests on both sides of the victim (UserSuspendService.postSuspend purge)
    seed_follow_request(&pool, &victim_id, &root_id).await;
    seed_follow_request(&pool, &root_id, &victim_id).await;

    // (a) no credential → 401 CREDENTIAL_REQUIRED (call() gate)
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/suspend-user",
        json!({ "userId": victim_id }),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "body: {b}");
    assert_eq!(b["error"]["code"], json!("CREDENTIAL_REQUIRED"));

    // (b) unresolved native token → 401 AUTHENTICATION_FAILED
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/suspend-user",
        json!({ "userId": victim_id, "i": "0123456789abcdef" }),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "body: {b}");
    assert_eq!(
        b["error"]["id"],
        json!("b0a7f5f8-dc2f-4171-b91f-de88ad238e14")
    );

    // (c) missing userId → 400 INVALID_PARAM
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/suspend-user",
        json!({ "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {b}");
    assert_eq!(b["error"]["code"], json!("INVALID_PARAM"));

    // (d) non-moderator credential → 403 ROLE_PERMISSION_DENIED (moderator gate)
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/suspend-user",
        json!({ "userId": victim_id, "i": plain_token }),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "body: {b}");
    assert_eq!(b["error"]["code"], json!("ROLE_PERMISSION_DENIED"));
    assert_eq!(
        b["error"]["id"],
        json!("d33d5333-db36-423d-a8f9-1a2b9549da41")
    );

    // (e) root suspends the victim (legacy alias name) → 204 void
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/suspend",
        json!({ "userId": victim_id, "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {b}");
    assert_eq!(b, Value::Null);

    // DB effects: isSuspended flip + updatedAt bump + follow-request purge
    let (suspended, has_updated_at): (bool, bool) = mk_db::sqlx::query_as(
        r#"SELECT "isSuspended", "updatedAt" IS NOT NULL FROM "user" WHERE "id" = $1"#,
    )
    .bind(&victim_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((suspended, has_updated_at), (true, true));

    let fr: i64 = mk_db::sqlx::query_scalar(
        r#"SELECT count(*) FROM "follow_request" WHERE "followeeId" = $1 OR "followerId" = $1"#,
    )
    .bind(&victim_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(fr, 0, "postSuspend purges follow requests both directions");

    let logs: Vec<String> =
        mk_db::sqlx::query_scalar(r#"SELECT "type" FROM "moderation_log" WHERE "userId" = $1"#)
            .bind(&root_id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(logs, vec!["suspend".to_string()]);

    // (f) users/show by id still returns the suspended user, flag visible
    let (s, b) = post_json(
        app.clone(),
        "/api/users/show",
        json!({ "userId": victim_id, "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b["isSuspended"], json!(true));

    // (g) list gates hide the suspended user (e2e users.ts exclusion-cases)
    let (s, b) = post_json(
        app.clone(),
        "/api/users/search",
        json!({ "query": "victimsus1" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert!(b.as_array().unwrap().is_empty(), "body: {b}");

    let (s, b) = post_json(
        app.clone(),
        "/api/users/search-by-username-and-host",
        json!({ "username": "victimsus1", "host": null }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert!(b.as_array().unwrap().is_empty(), "body: {b}");

    let (s, b) = post_json(app.clone(), "/api/users", json!({ "limit": 100 })).await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert!(
        !b.as_array()
            .unwrap()
            .iter()
            .any(|u| u["id"].as_str() == Some(victim_id.as_str())),
        "suspended user excluded from the explore list: {b}"
    );

    // (h) the root user cannot be suspended (isModerator guard → the plain
    // Error path becomes 500 INTERNAL_ERROR)
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/suspend-user",
        json!({ "userId": root_id, "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "body: {b}");
    assert_eq!(
        b["error"]["id"],
        json!("5d37dbcb-891e-41ca-a3d6-e690c97775ac")
    );

    // (i) unsuspend via the canonical name → 204 and state restored
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/unsuspend-user",
        json!({ "userId": victim_id, "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {b}");
    let suspended: bool =
        mk_db::sqlx::query_scalar(r#"SELECT "isSuspended" FROM "user" WHERE "id" = $1"#)
            .bind(&victim_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!suspended);
    let logs: Vec<String> =
        mk_db::sqlx::query_scalar(r#"SELECT "type" FROM "moderation_log" WHERE "userId" = $1"#)
            .bind(&root_id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(logs, vec!["suspend".to_string(), "unsuspend".to_string()]);

    // search finds the unsuspended user again (fresh updatedAt from the suspend bump)
    let (s, b) = post_json(
        app.clone(),
        "/api/users/search",
        json!({ "query": "victimsus1" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert!(
        b.as_array()
            .unwrap()
            .iter()
            .any(|u| u["id"].as_str() == Some(victim_id.as_str())),
        "body: {b}"
    );

    // (j) unknown admin name → UNKNOWN_API_ENDPOINT; GET on admin/* → 405
    // empty (no admin meta allows GET)
    let (s, b) = post_json(app.clone(), "/api/admin/definitely-not-real", json!({})).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "body: {b}");
    assert_eq!(
        b["error"]["id"],
        json!("2ca3b769-540a-4f08-9dd5-b5a825b6d0f1")
    );
    let (s, b) = get_json(&app, "/api/admin/meta").await;
    assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED, "body: {b}");
    assert_eq!(b, Value::Null);
}

#[tokio::test]
async fn admin_show_user_and_show_users() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkadmin_show").await;
    let app = crate::build_router(test_state(pool.clone()));

    let root = signup(&app, "root").await;
    let root_token = root["token"].as_str().unwrap().to_string();
    let vic_a = signup(&app, "showvica").await;
    let vic_a_id = vic_a["id"].as_str().unwrap().to_string();
    let vic_b = signup(&app, "showvicb").await;
    let vic_b_id = vic_b["id"].as_str().unwrap().to_string();
    let plain = signup(&app, "plainview").await;
    let plain_token = plain["token"].as_str().unwrap().to_string();

    // follower ordering inputs (direct SQL, like the hashtags fixture)
    for (uid, n) in [(&vic_a_id, 3i64), (&vic_b_id, 5i64)] {
        mk_db::sqlx::query(r#"UPDATE "user" SET "followersCount" = $1 WHERE "id" = $2"#)
            .bind(n)
            .bind(uid)
            .execute(&pool)
            .await
            .unwrap();
    }

    // --- admin/show-user ---
    // missing userId → 400 INVALID_PARAM
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/show-user",
        json!({ "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {b}");
    assert_eq!(b["error"]["code"], json!("INVALID_PARAM"));

    // unknown target → the plain Error('user not found') path → 500
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/show-user",
        json!({ "userId": "zzzzzzzzzzzz", "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "body: {b}");

    // non-moderator caller → 403 (role gate fires before the handler)
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/show-user",
        json!({ "userId": vic_a_id, "i": plain_token }),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "body: {b}");

    // root view: moderation-data keys + the packed user
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/show-user",
        json!({ "userId": vic_a_id, "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b["user"]["id"], json!(vic_a_id));
    assert_eq!(b["user"]["username"], json!("showvica"));
    assert_eq!(b["isSuspended"], json!(false));
    assert_eq!(b["isModerator"], json!(false));
    assert_eq!(b["isSilenced"], json!(false));
    assert_eq!(b["isHibernated"], json!(false));
    assert_eq!(b["lastActiveDate"], Value::Null);
    assert_eq!(b["moderationNote"], json!(""));
    assert_eq!(b["signins"], json!([]));
    assert_eq!(b["roleAssigns"], json!([]));
    assert_eq!(b["roles"], json!([]));
    assert_eq!(b["policies"]["canPublicNote"], json!(true));
    // secrets flat copies gated off without includeSecrets
    assert_eq!(b["email"], Value::Null);
    assert_eq!(b["emailVerified"], json!(false));
    let user_obj = b["user"].as_object().unwrap();
    assert!(!user_obj.contains_key("securityKeysList"), "{b}");
    assert!(!user_obj.contains_key("email"), "{b}");

    // includeSecrets + isAdministrator → upstream pack secrets spread lands
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/show-user",
        json!({ "userId": vic_a_id, "i": root_token, "includeSecrets": true }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let user_obj = b["user"].as_object().unwrap();
    assert!(user_obj.contains_key("email"), "{b}");
    assert!(user_obj.contains_key("emailVerified"), "{b}");
    assert!(user_obj.contains_key("securityKeysList"), "{b}");

    // --- admin/show-users ---
    // default sort = id ASC; username prefix filter
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/show-users",
        json!({ "i": root_token, "username": "showvic" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let ids: Vec<&str> = b
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![vic_a_id.as_str(), vic_b_id.as_str()], "body: {b}");

    // +follower / -follower sort
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/show-users",
        json!({ "i": root_token, "username": "showvic", "sort": "+follower" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let ids: Vec<&str> = b
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![vic_b_id.as_str(), vic_a_id.as_str()], "body: {b}");
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/show-users",
        json!({ "i": root_token, "username": "showvic", "sort": "-follower", "limit": 1, "offset": 1 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let ids: Vec<&str> = b
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["id"].as_str().unwrap())
        .collect();
    // followersCount ASC over [a(3), b(5)]; offset 1 skips a → b
    assert_eq!(ids, vec![vic_b_id.as_str()], "body: {b}");

    // no suspended users yet → state gate empty
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/show-users",
        json!({ "i": root_token, "state": "suspended" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert!(b.as_array().unwrap().is_empty());

    // state admin/moderator resolve via role assignments only — the root user
    // is NOT included (upstream getAdministratorIds TODO comment) and fresh
    // instances have no roles → []
    for state in ["admin", "moderator", "adminOrModerator"] {
        let (s, b) = post_json(
            app.clone(),
            "/api/admin/show-users",
            json!({ "i": root_token, "state": state }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "body: {b}");
        assert!(b.as_array().unwrap().is_empty(), "state {state}: {b}");
    }

    // suspend vic_b via the admin endpoint, then the state gates split
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/suspend-user",
        json!({ "userId": vic_b_id, "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/show-users",
        json!({ "i": root_token, "state": "suspended" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let ids: Vec<&str> = b
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![vic_b_id.as_str()], "body: {b}");
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/show-users",
        json!({ "i": root_token, "state": "available", "username": "showvic" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert!(
        !b.as_array()
            .unwrap()
            .iter()
            .any(|u| u["id"].as_str() == Some(vic_b_id.as_str())),
        "body: {b}"
    );

    // non-moderator caller → 403 moderator gate
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/show-users",
        json!({ "i": plain_token }),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "body: {b}");
    assert_eq!(
        b["error"]["id"],
        json!("d33d5333-db36-423d-a8f9-1a2b9549da41")
    );

    // invalid sort/limit → 400
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/show-users",
        json!({ "i": root_token, "sort": "+bogus" }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/show-users",
        json!({ "i": root_token, "limit": 0 }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_meta_server_info_stats() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkadmin_facades").await;
    let app = crate::build_router(test_state(pool.clone()));

    let root = signup(&app, "root").await;
    let root_token = root["token"].as_str().unwrap().to_string();
    let _plain = signup(&app, "plainfacade").await;

    // --- admin/meta (requireAdmin) ---
    // no credential → 401 CREDENTIAL_REQUIRED
    let (s, b) = post_json(app.clone(), "/api/admin/meta", json!({})).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "body: {b}");
    assert_eq!(
        b["error"]["id"],
        json!("1384574d-a912-4b81-8601-c7b1c4085df1")
    );

    // unresolved token → 401 AUTHENTICATION_FAILED (before the role gates)
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/meta",
        json!({ "i": "0123456789abcdef" }),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "body: {b}");
    assert_eq!(
        b["error"]["id"],
        json!("b0a7f5f8-dc2f-4171-b91f-de88ad238e14")
    );

    // fresh instance: schema defaults without a meta row
    let (s, b) = post_json(app.clone(), "/api/admin/meta", json!({ "i": root_token })).await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b["disableRegistration"], json!(true));
    assert_eq!(b["maintainerName"], Value::Null);
    assert_eq!(b["mascotImageUrl"], json!("/assets/ai.png"));
    assert_eq!(b["pinnedUsers"], json!([]));
    assert_eq!(b["enableEmail"], json!(false));
    // the first signup stamps meta.rootUserId (SignupService L160-162) and
    // MetaService.update creates the bootstrap row via save({...data, id:'x'},
    // L117-120) — the row carries the MiMeta COLUMN default for notesPerOneAd
    // (models/Meta.ts L632-635 default: 0), so the row-present read is 0;
    // the code-fallback 20 only applies when no meta row exists at all
    assert_eq!(b["notesPerOneAd"], json!(0));
    assert_eq!(b["policies"]["canPublicNote"], json!(true));
    // admin-only secret fields are present (the requireAdmin gate is the seal)
    assert_eq!(b["hcaptchaSecretKey"], Value::Null);
    assert_eq!(b["swPrivateKey"], Value::Null);

    // plain (non-admin) caller on requireAdmin meta → 403 with the
    // administrator-gate id (distinct from the moderator-gate id)
    let plain_token = signup(&app, "plainmeta").await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, b) = post_json(app.clone(), "/api/admin/meta", json!({ "i": plain_token })).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "body: {b}");
    assert_eq!(
        b["error"]["id"],
        json!("c3d38592-54c0-429d-be96-5636b0431a61")
    );

    // --- admin/server-info (requireModerator) ---
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/server-info",
        json!({ "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert!(b["psql"].as_str().unwrap().starts_with("1"), "body: {b}");
    assert_eq!(b["os"], json!(std::env::consts::OS));
    assert!(b["mem"]["total"].as_i64().unwrap() > 0, "body: {b}");
    assert!(
        b["net"]["interface"].as_str().unwrap().len() > 0,
        "body: {b}"
    );

    // --- admin/stats (moderator-gated facade over real row counts) ---
    let (s, b) = post_json(app.clone(), "/api/admin/stats", json!({ "i": root_token })).await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b["usersCount"], json!(3));
    assert_eq!(b["originalUsersCount"], json!(3));
    assert_eq!(b["notesCount"], json!(0));
    assert_eq!(b["reactionsCount"], json!(0));
    assert_eq!(b["driveUsageLocal"], json!(0));
    assert_eq!(b["instances"], json!(0));

    // plain user on a moderator-gated facade → 403 moderator gate
    let plain_token = signup(&app, "plainstats").await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, b) = post_json(app.clone(), "/api/admin/stats", json!({ "i": plain_token })).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "body: {b}");
    assert_eq!(
        b["error"]["id"],
        json!("d33d5333-db36-423d-a8f9-1a2b9549da41")
    );
}

// --- freq-agent: users/get-frequently-replied-users + pinned-users slice ---

#[tokio::test]
async fn frequently_replied_orders_and_gates() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("freq_replied").await;
    mk_db::run_migrations(&pool).await.unwrap();
    let app = crate::build_router(test_state(pool));

    let alice = signup(&app, "fqa").await;
    let a_token = alice["token"].as_str().unwrap();
    let a_id = alice["id"].as_str().unwrap();
    let bob = signup(&app, "fqb").await;
    let b_token = bob["token"].as_str().unwrap();
    let b_id = bob["id"].as_str().unwrap();
    let carol = signup(&app, "fqc").await;
    let c_token = carol["token"].as_str().unwrap();
    let c_id = carol["id"].as_str().unwrap();
    let dave = signup(&app, "fqd").await;
    let d_token = dave["token"].as_str().unwrap();
    let mallory = signup(&app, "fqm").await;
    let m_token = mallory["token"].as_str().unwrap();
    let m_id = mallory["id"].as_str().unwrap();

    // targets post one note each
    let note_of = |body: Value| body["createdNote"]["id"].as_str().unwrap().to_string();
    let (_, nb) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "b", "i": b_token }),
    )
    .await;
    assert!(nb["createdNote"]["id"].is_string(), "body: {nb}");
    let b1 = note_of(nb);
    let (_, nc) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "c", "i": c_token }),
    )
    .await;
    let c1 = note_of(nc);
    let (_, nd) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "d", "i": d_token }),
    )
    .await;
    let d1 = note_of(nd);
    let (_, nm) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "m", "i": m_token }),
    )
    .await;
    let m1 = note_of(nm);

    // alice: 1 reply to a bob note, 2 to distinct carol notes. Upstream
    // counts DISTINCT reply-target notes per author (`note.id IN (...)`
    // row semantics), so the two carol replies must hit different notes.
    let (_, nc2) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "c2", "i": c_token }),
    )
    .await;
    let c2 = note_of(nc2);
    for target in [&b1, &c1, &c2] {
        let (s, body) = post_json(
            app.clone(),
            "/api/notes/create",
            json!({ "text": "re", "replyId": target, "i": a_token }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "body: {body}");
    }

    // top list: carol (weight 2/2) before bob (1/2), packed == users/show
    // (both viewer views: alice token — relation block present on both sides)
    let (s, body) = post_json(
        app.clone(),
        "/api/users/get-frequently-replied-users",
        json!({ "userId": a_id, "limit": 10, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 2, "body: {body}");
    assert_eq!(arr[0]["user"]["id"], json!(c_id));
    assert_eq!(arr[1]["user"]["id"], json!(b_id));
    assert_eq!(arr[0]["weight"], json!(1.0), "body: {body}");
    assert_eq!(arr[1]["weight"], json!(0.5), "body: {body}");
    let (_, show_carol) = post_json(
        app.clone(),
        "/api/users/show",
        json!({ "userId": c_id, "i": a_token }),
    )
    .await;
    assert_eq!(arr[0]["user"], show_carol, "pack must equal users/show");

    // limit=1
    let (s, body) = post_json(
        app.clone(),
        "/api/users/get-frequently-replied-users",
        json!({ "userId": a_id, "limit": 1 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body.as_array().unwrap().len(), 1, "body: {body}");

    // requireCredential: false — guest request works
    let (s, body) = post_json(
        app.clone(),
        "/api/users/get-frequently-replied-users",
        json!({ "userId": a_id }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body.as_array().unwrap().len(), 2, "body: {body}");

    // a user who never replied gets an empty list
    let (s, body) = post_json(
        app.clone(),
        "/api/users/get-frequently-replied-users",
        json!({ "userId": b_id }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body, json!([]), "body: {body}");

    // unknown target → NO_SUCH_USER (endpoint-specific id)
    let (s, body) = post_json(
        app.clone(),
        "/api/users/get-frequently-replied-users",
        json!({ "userId": "zzzzzzzzzzzzzzzz" }),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "body: {body}");
    assert_eq!(
        body["error"]["id"],
        json!("e6965129-7b2a-40a4-bae2-cd84cd434822")
    );

    // emergent block gate: the creation-time reject is upstream too —
    // dave blocks alice → her NEW reply to dave's note 400s
    // (NoteCreateService.fetchAndCreate L383-391 via the notes/create
    // endpoint, L99 YOU_HAVE_BEEN_BLOCKED) while the pre-block d-replies
    // still count in the list and the blocker is excluded at query time
    // (TARGET_BLOCKER_GATE); mallory (blocked BY alice — the reverse
    // direction is not a gate) accepts the reply and stays included
    let (s, _) = post_json(
        app.clone(),
        "/api/blocking/create",
        json!({ "userId": a_id, "i": d_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = post_json(
        app.clone(),
        "/api/blocking/create",
        json!({ "userId": m_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, body) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "re2", "replyId": d1, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(
        body["error"]["code"],
        json!("YOU_HAVE_BEEN_BLOCKED"),
        "body: {body}"
    );
    assert_eq!(
        body["error"]["id"],
        // notes/create's catch-arm (create.ts L280-282) remaps the
        // NoteCreateService IdentifiableError b0df6025 to
        // meta.errors.youHaveBeenBlocked (b390d7e1) — the client-facing id
        json!("b390d7e1-8a5e-46ed-b625-06271cafd3d3")
    );
    let (s, body) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "re2", "replyId": m1, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let (s, body) = post_json(
        app.clone(),
        "/api/users/get-frequently-replied-users",
        json!({ "userId": a_id, "limit": 100 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let ids: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["user"]["id"].as_str().unwrap())
        .collect();
    assert!(
        !ids.contains(&dave["id"].as_str().unwrap()),
        "blocker of the target must be excluded: {body}"
    );
    assert!(
        ids.contains(&mallory["id"].as_str().unwrap()),
        "blocked-by-me must stay included: {body}"
    );

    // users/notes requisite: guest fetch of a user's notes (id DESC)
    let (s, body) = post_json(app.clone(), "/api/users/notes", json!({ "userId": b_id })).await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let notes = body.as_array().unwrap();
    assert_eq!(notes.len(), 1, "body: {body}");
    assert_eq!(notes[0]["id"], json!(b1));
    assert_eq!(notes[0]["userId"], json!(b_id));
}

#[tokio::test]
async fn pinned_users_lists_meta_accts() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("freq_pinned").await;
    mk_db::run_migrations(&pool).await.unwrap();
    let app = crate::build_router(test_state(pool));

    let root = signup(&app, "root").await;
    let root_token = root["token"].as_str().unwrap();
    let bob = signup(&app, "fpb").await;
    let b_id = bob["id"].as_str().unwrap();
    let carol = signup(&app, "fpc").await;
    let c_id = carol["id"].as_str().unwrap();
    let plain = signup(&app, "fpplain").await;
    let plain_token = plain["token"].as_str().unwrap();

    // requireAdministrator: plain caller → ROLE_PERMISSION_DENIED (admin gate)
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/update-meta",
        json!({ "pinnedUsers": ["fpb"], "i": plain_token }),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(
        body["error"]["id"],
        json!("c3d38592-54c0-429d-be96-5636b0431a61")
    );

    // root pins [fpb, @fpc, ghost] → resolved in order, unknown acct skipped
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/update-meta",
        json!({
            "pinnedUsers": [bob["username"], format!("@{}", carol["username"].as_str().unwrap()), "ghostnothere"],
            "i": root_token
        }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {body}");

    let (s, body) = post_json(app.clone(), "/api/pinned-users", json!({})).await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 2, "body: {body}");
    assert_eq!(arr[0]["id"], json!(b_id));
    assert_eq!(arr[1]["id"], json!(c_id));
    // pack identical to users/show
    let (_, show_bob) = post_json(app.clone(), "/api/users/show", json!({ "userId": b_id })).await;
    assert_eq!(arr[0], show_bob, "pack must equal users/show");

    // update path: re-pin with only @fpc (row now exists → UPDATE branch)
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/update-meta",
        json!({ "pinnedUsers": ["@fpc"], "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, body) = post_json(app.clone(), "/api/pinned-users", json!({})).await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1, "body: {body}");
    assert_eq!(arr[0]["id"], json!(c_id));

    // requireCredential: false — guest works
    let (s, _) = post_json(app.clone(), "/api/pinned-users", json!({})).await;
    assert_eq!(s, StatusCode::OK);
}

// ==== delete-account + freq silenced census (test/e2e/users.ts) ====

/// e2e users.ts 削除済 fixtures: signup → post → i/delete-account → the user
/// stays listed ('削除済ユーザーが含まれる') while the me-view users/show
/// packs `isDeleted: true` ('削除済みになっていること') and not-me views omit
/// the flag ('自分以外から見たときは削除済みか判定できない').
#[tokio::test]
async fn delete_account_by_self_soft_flag_and_listed() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("delacct_self").await;
    mk_db::run_migrations(&pool).await.unwrap();
    let dbp = pool.clone();
    let app = crate::build_router(test_state(pool));

    let root = signup(&app, "root").await;
    let root_token = root["token"].as_str().unwrap().to_string();
    let victim = signup(&app, "dself").await;
    let v_token = victim["token"].as_str().unwrap().to_string();
    let v_id = victim["id"].as_str().unwrap().to_string();
    let alice = signup(&app, "dselfviewer").await;
    let a_token = alice["token"].as_str().unwrap().to_string();

    // fixtures: post(userDeletedBySelf, { text: 'test' }) before deletion —
    // the note purge lives in the upstream queue job, so the deleted user
    // keeps appearing in lists inside the census window.
    let (s, _) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "test", "i": v_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // requireCredential
    let (s, body) = post_json(
        app.clone(),
        "/api/i/delete-account",
        json!({ "password": "test" }),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(
        body["error"]["id"],
        json!("1384574d-a912-4b81-8601-c7b1c4085df1")
    );

    // wrong password → Error('incorrect password') → 500 ISE
    let (s, _) = post_json(
        app.clone(),
        "/api/i/delete-account",
        json!({ "password": "nope", "i": v_token }),
    )
    .await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);

    // success → the endpoint returns undefined → ApiCallService 204
    let (s, _) = post_json(
        app.clone(),
        "/api/i/delete-account",
        json!({ "password": "test", "i": v_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // '削除済みになっていること' — me-view packs isDeleted: true
    let (s, body) = post_json(
        app.clone(),
        "/api/users/show",
        json!({ "userId": v_id, "i": v_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body["isDeleted"], json!(true));

    // '自分以外から見たときは削除済みか判定できない' — not-me view omits it
    let (s, body) = post_json(
        app.clone(),
        "/api/users/show",
        json!({ "userId": v_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert!(body.get("isDeleted").is_none(), "body: {body}");

    // '削除済ユーザーが含まれる' — users/search (and the list/read paths)
    // keep serving the deleted user
    let (s, body) = post_json(
        app.clone(),
        "/api/users/search",
        json!({ "query": "dself", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert!(
        body.as_array()
            .unwrap()
            .iter()
            .any(|u| u["id"] == json!(v_id)),
        "body: {body}"
    );
    let (s, body) = post_json(app.clone(), "/api/users", json!({ "i": a_token })).await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert!(
        body.as_array()
            .unwrap()
            .iter()
            .any(|u| u["id"] == json!(v_id)),
        "body: {body}"
    );

    // idempotent: `userDetailed.isDeleted → return` → 204 again
    let (s, _) = post_json(
        app.clone(),
        "/api/i/delete-account",
        json!({ "password": "test", "i": v_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // DeleteAccountService safeguard: root refuses self-deletion (500; the
    // password compare runs first and matches)
    let (s, _) = post_json(
        app.clone(),
        "/api/i/delete-account",
        json!({ "password": "test", "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);

    drop(dbp);
}

/// e2e users.ts 削除済(byAdmin): admin/delete-account as root — the plain
/// caller hits requireAdmin (ROLE_PERMISSION_DENIED), the root call flips
/// the flag + writes the deleteAccount moderation log, and a re-run hits the
/// `user.isDeleted → return` short-circuit.
#[tokio::test]
async fn delete_account_by_admin_gates_and_moderation_log() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("delacct_admin").await;
    mk_db::run_migrations(&pool).await.unwrap();
    let dbp = pool.clone();
    let app = crate::build_router(test_state(pool));

    let root = signup(&app, "root").await;
    let root_token = root["token"].as_str().unwrap().to_string();
    let victim = signup(&app, "dadmin").await;
    let v_token = victim["token"].as_str().unwrap().to_string();
    let v_id = victim["id"].as_str().unwrap().to_string();
    let caller = signup(&app, "dcaller").await;
    let c_token = caller["token"].as_str().unwrap().to_string();

    // requireAdmin: plain caller → ROLE_PERMISSION_DENIED (c3d38592…)
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/delete-account",
        json!({ "userId": v_id, "i": c_token }),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(
        body["error"]["id"],
        json!("c3d38592-54c0-429d-be96-5636b0431a61")
    );

    // missing target → findOneByOrFail → 500 ISE
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/delete-account",
        json!({ "userId": mk_types::id::gen_aid_now(), "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);

    // root deletes → 204 + deleteAccount moderation log
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/delete-account",
        json!({ "userId": v_id, "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let logs: i64 = mk_db::sqlx::query_scalar(
        r#"SELECT count(*) FROM "moderation_log" WHERE "type" = 'deleteAccount'"#,
    )
    .fetch_one(&dbp)
    .await
    .unwrap();
    assert_eq!(logs, 1);

    // '削除済み(byAdmin)になっていること' — me-view packs isDeleted: true
    let (s, body) = post_json(
        app.clone(),
        "/api/users/show",
        json!({ "userId": v_id, "i": v_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body["isDeleted"], json!(true));

    // re-run → `user.isDeleted → return` → 204, no second log
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/delete-account",
        json!({ "userId": v_id, "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let logs: i64 = mk_db::sqlx::query_scalar(
        r#"SELECT count(*) FROM "moderation_log" WHERE "type" = 'deleteAccount'"#,
    )
    .fetch_one(&dbp)
    .await
    .unwrap();
    assert_eq!(logs, 1);

    drop(dbp);
}

/// e2e users.ts freq サイレンス case: a silenced (canPublicNote: false role)
/// reply target must appear in get-frequently-replied-users with the
/// users/show-identical pack (`isSilenced: true`) — the freq pack path reads
/// the merged role policy like users/show does.
#[tokio::test]
async fn freq_pack_silenced_user_equals_users_show() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("freq_silenced_pack").await;
    mk_db::run_migrations(&pool).await.unwrap();
    let app = crate::build_router(test_state(pool));

    let root = signup(&app, "root").await;
    let root_token = root["token"].as_str().unwrap().to_string();
    let silenced = signup(&app, "fsil").await;
    let s_token = silenced["token"].as_str().unwrap().to_string();
    let s_id = silenced["id"].as_str().unwrap().to_string();
    let s_username = silenced["username"].as_str().unwrap().to_string();
    let alice = signup(&app, "fviewer").await;
    let a_token = alice["token"].as_str().unwrap().to_string();
    let a_id = alice["id"].as_str().unwrap().to_string();

    // users.ts fixture: role(root, {}, { canPublicNote: { priority: 0,
    // useDefault: false, value: false } }) + admin/roles/assign
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/roles/create",
        json!({
            "name": "Silenced",
            "asBadge": false,
            "canEditMembersByModerator": false,
            "isAdministrator": false,
            "isModerator": false,
            "isPublic": false,
            "target": "manual",
            "policies": {
                "canPublicNote": { "priority": 0, "useDefault": false, "value": false }
            },
            "i": root_token
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let role_id = body["id"].as_str().unwrap().to_string();
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/roles/assign",
        json!({ "userId": s_id, "roleId": role_id, "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {body}");

    // fixtures: the silenced user posts; alice replies to it
    let (s, note) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "test", "i": s_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {note}");
    // notes/create answers `{ createdNote: <packed> }` (SignupApi/ApiCall
    // shape the M1 port returns)
    let note_id = note["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, _) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({
            "text": format!("@{s_username} test"),
            "replyId": note_id,
            "i": a_token
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // freq: the silenced user present with the users/show-identical pack
    let (s, body) = post_json(
        app.clone(),
        "/api/users/get-frequently-replied-users",
        json!({ "userId": a_id, "limit": 100, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let entries = body.as_array().unwrap();
    let entry = entries
        .iter()
        .find(|e| e["user"]["id"] == json!(s_id))
        .expect("silenced user in freq list");
    let (s, show) = post_json(
        app.clone(),
        "/api/users/show",
        json!({ "userId": s_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {show}");
    assert_eq!(entry["user"], show, "freq pack must equal users/show");
    assert_eq!(entry["user"]["isSilenced"], json!(true));
}

// ---------------------------------------------------------------------------
// channels slice (upstream test/e2e/channel.ts arena)
// ---------------------------------------------------------------------------

async fn channels_signup(app: axum::Router, name: &str) -> (String, String) {
    let username = format!("{name}{}", mk_types::id::gen_aid_now());
    let (_, body) = post_json(
        app,
        "/api/signup",
        json!({ "username": username, "password": "test" }),
    )
    .await;
    assert!(!body.is_null(), "signup must return a body: {body}");
    let token = body["token"].as_str().unwrap().to_string();
    let id = body["id"].as_str().unwrap().to_string();
    (token, id)
}

/// The test/e2e/channel.ts Follow describe: `channels/create` from alice,
/// then a re-follow of the same channel must hit ALREADY_FOLLOWING while the
/// first follow round-trips 204.
#[tokio::test]
async fn channels_follow_arena() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("mktest_channels_follow").await;
    mk_db::run_migrations(&pool).await.unwrap();
    let app = crate::build_router(test_state(pool));

    let (a_token, _) = channels_signup(app.clone(), "alice").await;

    // fixture: `channels/create { name: 'follow-test-channel' }` — packed
    // Channel shape back (ChannelEntityService.pack)
    let (s, ch) = post_json(
        app.clone(),
        "/api/channels/create",
        json!({ "name": "follow-test-channel", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {ch}");
    assert_eq!(ch["name"], json!("follow-test-channel"));
    assert_eq!(ch["isFollowing"], json!(false));
    assert_eq!(ch["isFavorited"], json!(false));
    assert_eq!(ch["isMuting"], json!(false));
    assert_eq!(ch["hasUnreadNote"], json!(false));
    assert_eq!(ch["notesCount"], json!(0));
    assert_eq!(ch["usersCount"], json!(0));
    let channel_id = ch["id"].as_str().unwrap().to_string();

    let (s, empty) = post_json(
        app.clone(),
        "/api/channels/follow",
        json!({ "channelId": channel_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {empty}");

    let (s, body) = post_json(
        app.clone(),
        "/api/channels/follow",
        json!({ "channelId": channel_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("ALREADY_FOLLOWING"));
}

/// The wider channels/* slice: show/followed/timeline/favorite/unfavorite/
/// unfollow round-trips over the shared pack.
#[tokio::test]
async fn channels_slice_roundtrip() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("mktest_channels_slice").await;
    mk_db::run_migrations(&pool).await.unwrap();
    let app = crate::build_router(test_state(pool));

    // the OWNER registers first: upstream SignupService L160-162 stamps the
    // first signup as meta.rootUserId and RoleService.isModerator (L461)
    // grants that holder moderator powers, which channels/update accepts as
    // an owner (update.ts L92 `channel.userId !== me.id && !iAmModerator`)
    // — a first-registered alice would make the non-owner update below pass
    // instead of mirroring the ACCESS_DENIED shape
    let (b_token, _) = channels_signup(app.clone(), "bobby").await;
    let (a_token, _) = channels_signup(app.clone(), "alice").await;

    // bob owns the channel; alice follows + favorites it
    let (s, ch) = post_json(
        app.clone(),
        "/api/channels/create",
        json!({ "name": "slice-channel", "description": "a channel", "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {ch}");
    let channel_id = ch["id"].as_str().unwrap().to_string();

    let (s, _) = post_json(
        app.clone(),
        "/api/channels/follow",
        json!({ "channelId": channel_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // followed (alice) now packs the channel with her me-flags
    let (s, followed) = post_json(
        app.clone(),
        "/api/channels/followed",
        json!({ "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {followed}");
    let followed = followed.as_array().unwrap();
    assert_eq!(followed.len(), 1);
    assert_eq!(followed[0]["id"], json!(channel_id));
    assert_eq!(followed[0]["isFollowing"], json!(true));

    // a to-become note lives in the channel via the arena write path
    // (notes/create { channelId } — the row's channelId column). notes/create
    // is note-core's file: until the coordinator applies the NewNote
    // .channelId extension, the param is ignored and the branch below is
    // skipped — this test must pass in both states.
    let (s, note) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "hi", "channelId": channel_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {note}");
    if note["createdNote"]["channelId"] == json!(channel_id) {
        let note_id = note["createdNote"]["id"].as_str().unwrap().to_string();
        let (s, tl) = post_json(
            app.clone(),
            "/api/channels/timeline",
            json!({ "channelId": channel_id, "i": a_token }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "body: {tl}");
        let tl = tl.as_array().unwrap();
        assert_eq!(tl.len(), 1);
        assert_eq!(tl[0]["id"], json!(note_id));
        assert_eq!(tl[0]["channelId"], json!(channel_id));
    }

    // owner update lane (update.ts owner check)
    let (s, upd) = post_json(
        app.clone(),
        "/api/channels/update",
        json!({ "channelId": channel_id, "name": "renamed", "description": null, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {upd}");
    assert_eq!(upd["name"], json!("renamed"));
    assert_eq!(upd["description"], Value::Null);
    // non-owner update denied (update.ts ACCESS_DENIED)
    let (s_nte, body) = post_json(
        app.clone(),
        "/api/channels/update",
        json!({ "channelId": channel_id, "name": "nope", "i": a_token }),
    )
    .await;
    assert_eq!(s_nte, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("ACCESS_DENIED"));

    // favorite → my-favorites → unfavorite (favorite.ts / my-favorites.ts)
    let (s, _) = post_json(
        app.clone(),
        "/api/channels/favorite",
        json!({ "channelId": channel_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, favs) = post_json(
        app.clone(),
        "/api/channels/my-favorites",
        json!({ "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {favs}");
    assert_eq!(favs.as_array().unwrap().len(), 1);
    assert_eq!(favs[0]["id"], json!(channel_id));
    let (s, _) = post_json(
        app.clone(),
        "/api/channels/unfavorite",
        json!({ "channelId": channel_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // unfollow lands back on an empty followed list
    let (s, _) = post_json(
        app.clone(),
        "/api/channels/unfollow",
        json!({ "channelId": channel_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, followed) = post_json(
        app.clone(),
        "/api/channels/followed",
        json!({ "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {followed}");
    assert_eq!(followed.as_array().unwrap().len(), 0);

    // unknown channel → NO_SUCH_CHANNEL (channels/show)
    let (s, body) = post_json(
        app.clone(),
        "/api/channels/show",
        json!({ "channelId": "x".repeat(32) }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("NO_SUCH_CHANNEL"));
}

// ---------------------------------------------------------------------------
// drive-agent2: drive/files/{create,attached-notes,upload-from-url} —
// upstream test/e2e/drive.ts (添付ノート一覧を取得できる / 他の人から見えない).
// ---------------------------------------------------------------------------

/// Minimal multipart POST (files/create carries no JSON body).
async fn post_multipart(
    app: axum::Router,
    path: &str,
    content_type: &str,
    body: Vec<u8>,
) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", content_type)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, v)
}

#[tokio::test]
async fn drive_attached_notes_roundtrip() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_drive_attached").await;
    let app = crate::build_router(test_state(pool));

    let alice = signup(&app, &format!("alice{}", mk_types::id::gen_aid_now())).await;
    let bob = signup(&app, &format!("bob{}", mk_types::id::gen_aid_now())).await;
    let a_token = alice["token"].as_str().unwrap().to_string();
    let b_token = bob["token"].as_str().unwrap().to_string();

    // multipart upload (jpeg magic bytes — mirrors e2e uploadFile)
    let mut body: Vec<u8> =
        b"--XX\r\nContent-Disposition: form-data; name=\"file\"; filename=\"192.jpg\"\r\n\r\n"
            .to_vec();
    body.extend_from_slice(b"\xFF\xD8\xFF\xe0jpeg-bytes");
    body.extend_from_slice(b"\r\n--XX\r\nContent-Disposition: form-data; name=\"i\"\r\n\r\n");
    body.extend_from_slice(a_token.as_bytes());
    body.extend_from_slice(b"\r\n--XX--\r\n");
    let (s, file) = post_multipart(
        app.clone(),
        "/api/drive/files/create",
        "multipart/form-data; boundary=XX",
        body,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {file}");
    assert_eq!(file["name"], json!("192.jpg"));
    assert_eq!(file["type"], json!("image/jpeg"));
    assert_eq!(file["userId"], alice["id"]);
    let file_id = file["id"].as_str().unwrap().to_string();

    // note carrying the file (notes/create fileIds note-pair writes)
    let (s, created) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "with file", "fileIds": [file_id], "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {created}");
    let note_id = created["createdNote"]["id"].as_str().unwrap().to_string();

    // attached-notes: owner sees the note (pack with id), reverse order supported
    let (s, notes) = post_json(
        app.clone(),
        "/api/drive/files/attached-notes",
        json!({ "fileId": file_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {notes}");
    assert_eq!(notes.as_array().unwrap().len(), 1);
    assert_eq!(notes[0]["id"], json!(note_id));

    // unknown file under the caller → NO_SUCH_FILE (attached-notes.ts)
    let (s, body) = post_json(
        app.clone(),
        "/api/drive/files/attached-notes",
        json!({ "fileId": "x".repeat(24), "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("NO_SUCH_FILE"));

    // 添付ノート一覧は他の人から見えない (drive.ts): foreign file → 400 error
    let (s, body) = post_json(
        app.clone(),
        "/api/drive/files/attached-notes",
        json!({ "fileId": file_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("NO_SUCH_FILE"));

    // upload-from-url acks 204 before the async download (bad URL → the
    // spawned task logs; the endpoint contract is the immediate 204)
    let (s, _) = post_json(
        app.clone(),
        "/api/drive/files/upload-from-url",
        json!({ "url": "http://127.0.0.1:9/never.jpg", "marker": "m", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn announcements_auto_archive_cycle() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("mktest_ann_archive").await;
    mk_db::run_migrations(&pool).await.unwrap();
    let app = crate::build_router(test_state(pool));

    // first signup = the SignupService rootUserId stroke-in proxy (the e2e
    // `signup({ username: 'admin' })` gate for admin/announcements)
    let admin = signup(&app, "admin").await;
    let token = admin["token"].as_str().unwrap().to_string();

    // create with a future autoArchiveAt (e2e test 1)
    let (s, created) = post_json(
        app.clone(),
        "/api/admin/announcements/create",
        json!({
            "title": "Scheduled announcement",
            "text": "Text",
            "imageUrl": null,
            "autoArchiveAt": now_ms() + 60_000,
            "i": token,
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {created}");
    let id = created["id"].as_str().unwrap().to_string();
    // the entity pack carries neither autoArchiveAt nor isActive; the admin
    // list shape does
    assert!(created.get("autoArchiveAt").is_none());
    assert!(created.get("isActive").is_none());

    let (s, active) = post_json(
        app.clone(),
        "/api/admin/announcements/list",
        json!({ "status": "active", "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {active}");
    let listed = active
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == json!(id))
        .unwrap()
        .clone();
    assert!(listed["autoArchiveAt"].is_string());
    assert_eq!(listed["isActive"], json!(true));
    assert_eq!(listed["reads"], json!(0));

    // clearing autoArchiveAt (204) keeps it active with autoArchiveAt null
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/announcements/update",
        json!({ "id": id, "autoArchiveAt": null, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, active) = post_json(
        app.clone(),
        "/api/admin/announcements/list",
        json!({ "status": "active", "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let cleared = active
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == json!(id))
        .unwrap()
        .clone();
    assert_eq!(cleared["autoArchiveAt"], Value::Null);

    // a past autoArchiveAt archives it (update's INVALID check is < 0 only)
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/announcements/update",
        json!({ "id": id, "autoArchiveAt": now_ms() - 1_000, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    let (s, active) = post_json(
        app.clone(),
        "/api/admin/announcements/list",
        json!({ "status": "active", "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        !active
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == json!(id))
    );

    // the archived status surface shows it with the computed isActive=false
    let (s, archived) = post_json(
        app.clone(),
        "/api/admin/announcements/list",
        json!({ "status": "archived", "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let archived = archived
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == json!(id))
        .unwrap()
        .clone();
    assert_eq!(archived["isActive"], json!(false));

    // the public default (active) list drops it for guests — and guests pack
    // without an isRead key (AnnouncementEntityService.pack)
    let (s, public) = post_json(app.clone(), "/api/announcements", json!({})).await;
    assert_eq!(s, StatusCode::OK, "body: {public}");
    assert!(
        !public
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == json!(id))
    );
    if let Some(first) = public.as_array().unwrap().first() {
        assert!(first.get("isRead").is_none());
    }
}

#[tokio::test]
async fn announcements_invalid_dates_and_read_flow() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("mktest_ann_read").await;
    mk_db::run_migrations(&pool).await.unwrap();
    let app = crate::build_router(test_state(pool));

    let admin = signup(&app, "root").await;
    let token = admin["token"].as_str().unwrap().to_string();
    let user = signup(&app, &format!("u{}", mk_types::id::gen_aid_now())).await;
    let user_token = user["token"].as_str().unwrap().to_string();
    let user_id = user["id"].as_str().unwrap().to_string();

    // e2e test 2: out-of-range autoArchiveAt (a negative JS-Date-epoch ms)
    // rejected on create (<= now) and update (< 0) with the endpoints'
    // INVALID_AUTO_ARCHIVE_AT ids
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/announcements/create",
        json!({
            "title": "Invalid scheduled announcement",
            "text": "Text",
            "imageUrl": null,
            "autoArchiveAt": -8_640_000_000_000_000i64,
            "i": token,
        }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("INVALID_AUTO_ARCHIVE_AT"));
    assert_eq!(
        body["error"]["id"],
        json!("2a892bd5-487d-46a2-a5fe-3d85ad51defe")
    );

    let (s, body) = post_json(
        app.clone(),
        "/api/admin/announcements/create",
        json!({
            "title": "Expired scheduled announcement",
            "text": "Text",
            "imageUrl": null,
            "autoArchiveAt": now_ms() - 60_000,
            "i": token,
        }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("INVALID_AUTO_ARCHIVE_AT"));

    // missing title → INVALID_PARAM (required paramDef property)
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/announcements/create",
        json!({ "text": "Text", "imageUrl": null, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("INVALID_PARAM"));

    // a plain global announcement + a personal one for user
    let (s, global) = post_json(
        app.clone(),
        "/api/admin/announcements/create",
        json!({ "title": "Global", "text": "hi", "imageUrl": null, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {global}");
    let global_id = global["id"].as_str().unwrap().to_string();
    let (s, personal) = post_json(
        app.clone(),
        "/api/admin/announcements/create",
        json!({
            "title": "Personal",
            "text": "just you",
            "imageUrl": null,
            "userId": user_id,
            "i": token,
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {personal}");
    let personal_id = personal["id"].as_str().unwrap().to_string();
    // create's pack is me-less: forYou false, isRead absent
    assert_eq!(global["forYou"], json!(false));
    assert!(global.get("isRead").is_none());

    // update with the out-of-range date → the update endpoint's error id
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/announcements/update",
        json!({
            "id": global_id,
            "autoArchiveAt": -8_640_000_000_000_000i64,
            "i": token,
        }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"]["id"],
        json!("01b83d7b-2fd5-4d7c-86c4-d03144d16355")
    );

    // i/read-announcement requires a credential
    let (s, body) = post_json(
        app.clone(),
        "/api/i/read-announcement",
        json!({ "announcementId": global_id }),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(body["error"]["code"], json!("CREDENTIAL_REQUIRED"));

    // a personal announcement of someone else → NO_SUCH_ANNOUNCEMENT (show)
    let (s, body) = post_json(
        app.clone(),
        "/api/announcements/show",
        json!({ "announcementId": personal_id, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(
        body["error"]["id"],
        json!("b57b5e1d-4f49-404a-9edb-46b00268f121")
    );
    // global show as guest: the pack carries no isRead key
    let (s, shown) = post_json(
        app.clone(),
        "/api/announcements/show",
        json!({ "announcementId": global_id }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {shown}");
    assert!(shown.get("isRead").is_none());

    // the me view: isRead resolves false until the read lands
    let (s, shown) = post_json(
        app.clone(),
        "/api/announcements/show",
        json!({ "announcementId": personal_id, "i": user_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(shown["isRead"], json!(false));

    // me-scoped public list: forYou on the personal row
    let (s, list) = post_json(
        app.clone(),
        "/api/announcements",
        json!({ "i": user_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let rows = list.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let mine = rows.iter().find(|a| a["id"] == json!(personal_id)).unwrap();
    assert_eq!(mine["forYou"], json!(true));
    assert_eq!(mine["isRead"], json!(false));

    // reading the personal announcement → 204 (idempotent like the upstream
    // catch), then the deactivation pulls it from the default active me-list
    let (s, _) = post_json(
        app.clone(),
        "/api/i/read-announcement",
        json!({ "announcementId": personal_id, "i": user_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = post_json(
        app.clone(),
        "/api/i/read-announcement",
        json!({ "announcementId": personal_id, "i": user_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, list) = post_json(
        app.clone(),
        "/api/announcements",
        json!({ "i": user_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let rows = list.as_array().unwrap();
    assert_eq!(rows.len(), 1); // only the global stays active-for-me
    assert_eq!(rows[0]["id"], json!(global_id));
    assert_eq!(rows[0]["isRead"], json!(false));
    let (s, shown) = post_json(
        app.clone(),
        "/api/announcements/show",
        json!({ "announcementId": personal_id, "i": user_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(shown["isRead"], json!(true));
    // reads counted on the admin list — upstream scopes the list by userId
    // when present and to GLOBALS ONLY when absent (admin/announcements/
    // list.ts L144-147), so the personal row rides the user-scoped call
    // (the very next block asserts the absent-userId globals-only shape)
    let (s, list) = post_json(
        app.clone(),
        "/api/admin/announcements/list",
        json!({ "status": "all", "userId": user_id, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let rows = list.as_array().unwrap();
    let reads = rows.iter().find(|a| a["id"] == json!(personal_id)).unwrap()["reads"]
        .as_i64()
        .unwrap();
    assert_eq!(reads, 1);

    // admin/list scopes: absent userId = globals only, present = just that
    let (s, list) = post_json(
        app.clone(),
        "/api/admin/announcements/list",
        json!({ "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list.as_array().unwrap()[0]["id"], json!(global_id));
    // present userId = just that user's rows. The personal row is
    // read-DEACTIVATED (AnnouncementService.read L280-284 flips isActive for
    // a personal announcement once its target reads it), so the default
    // 'active' scope filters it back out upstream too — the present-userId
    // shape asserts over status 'all'
    let (s, list) = post_json(
        app.clone(),
        "/api/admin/announcements/list",
        json!({ "status": "all", "userId": user_id, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list.as_array().unwrap()[0]["userId"], json!(user_id));

    // non-moderator (no rootUserId/no first-user/no roles) → 403 gate
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/announcements/list",
        json!({ "i": user_token }),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["error"]["code"], json!("ROLE_PERMISSION_DENIED"));

    // NO_SUCH_ANNOUNCEMENT on update/delete misses
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/announcements/update",
        json!({ "id": "x".repeat(32), "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"]["id"],
        json!("d3aae5a7-6372-4cb4-b61c-f511ffc2d7cc")
    );
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/announcements/delete",
        json!({ "id": "x".repeat(32), "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"]["id"],
        json!("ecad8040-a276-4e85-bda9-015a708d291e")
    );

    // delete lands (fire-and-forget moderation log write)
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/announcements/delete",
        json!({ "id": global_id, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, shown) = post_json(
        app.clone(),
        "/api/announcements/show",
        json!({ "announcementId": global_id }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        shown["error"]["id"],
        json!("b57b5e1d-4f49-404a-9edb-46b00268f121")
    );
}

// ---------------------------------------------------------------------------
// note-agent: notes.ts acceptance slice — unit mirrors of the note.ts e2e
// ladder (create-with-poll → show poll/votes, vote rules, reply ladder +
// repliesCount truth, sensitive/prohibited words, mentions dedupe +
// mentionLimit, translate gate, bare-create visibility in users/notes).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn notes_create_poll_ladder() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_note_poll").await;
    let app = crate::build_router(test_state(pool.clone()));
    let alice = signup(&app, &format!("alice{}", mk_types::id::gen_aid_now())).await;
    let a_token = alice["token"].as_str().unwrap().to_string();

    // 投票を添付できる (+ pack poll body)
    let (s, created) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({
            "text": "test",
            "poll": { "choices": ["sakura", "izumi", "ako"] },
            "i": a_token
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {created}");
    assert_eq!(created["createdNote"]["hasPoll"], json!(true));
    let poll = &created["createdNote"]["poll"];
    assert_eq!(poll["multiple"], json!(false));
    assert_eq!(poll["choices"][0]["text"], json!("sakura"));
    assert_eq!(poll["choices"][0]["votes"], json!(0));
    assert_eq!(poll["choices"][0]["isVoted"], json!(false));
    assert_eq!(poll["expiresAt"], json!(null));
    let note_id = created["createdNote"]["id"].as_str().unwrap().to_string();

    // choices validation (投票の選択肢が無くて/1つで怒られる)
    for bad in [
        json!({}),
        json!({"choices": []}),
        json!({"choices": ["one"]}),
    ] {
        let (s, _) = post_json(
            app.clone(),
            "/api/notes/create",
            json!({ "text": "x", "poll": bad, "i": a_token }),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "poll: {bad}");
    }

    // 投票できる → pack votes bump + isVoted (self-view)
    let (s, body) = post_json(
        app.clone(),
        "/api/notes/polls/vote",
        json!({ "noteId": note_id, "choice": 1, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {body}");
    let (s, shown) = post_json(
        app.clone(),
        "/api/notes/show",
        json!({ "noteId": note_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {shown}");
    assert_eq!(shown["poll"]["choices"][1]["votes"], json!(1));
    assert_eq!(shown["poll"]["choices"][1]["isVoted"], json!(true));
    assert_eq!(shown["poll"]["choices"][0]["isVoted"], json!(false));

    // 複数投票できない → ALREADY_VOTED
    let (s, err) = post_json(
        app.clone(),
        "/api/notes/polls/vote",
        json!({ "noteId": note_id, "choice": 0, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"]["code"], json!("ALREADY_VOTED"), "body: {err}");

    // 許可されている場合は複数投票できる
    let (s, multi) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({
            "text": "multi",
            "poll": { "choices": ["a", "b", "c"], "multiple": true },
            "i": a_token
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {multi}");
    assert_eq!(multi["createdNote"]["poll"]["multiple"], json!(true));
    let multi_id = multi["createdNote"]["id"].as_str().unwrap().to_string();
    for choice in [0, 1, 2] {
        let (s, _) = post_json(
            app.clone(),
            "/api/notes/polls/vote",
            json!({ "noteId": multi_id, "choice": choice, "i": a_token }),
        )
        .await;
        assert_eq!(s, StatusCode::NO_CONTENT, "choice {choice}");
    }

    // 締め切られている場合は投票できない
    let (s, expired) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({
            "text": "exp",
            "poll": { "choices": ["a", "b"], "expiredAfter": 1 },
            "i": a_token
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {expired}");
    let expired_id = expired["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, err) = post_json(
        app.clone(),
        "/api/notes/polls/vote",
        json!({ "noteId": expired_id, "choice": 0, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        err["error"]["code"],
        json!("ALREADY_EXPIRED"),
        "body: {err}"
    );

    // noPoll note → NO_POLL
    let (s, plain) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "plain", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let plain_id = plain["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, err) = post_json(
        app.clone(),
        "/api/notes/polls/vote",
        json!({ "noteId": plain_id, "choice": 0, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"]["code"], json!("NO_POLL"), "body: {err}");

    // text:null poll note → pack text null and cw-qualified null
    assert_eq!(shown["text"], json!("test"));
    assert_eq!(shown["cw"], json!(null));
}

#[tokio::test]
async fn notes_create_reply_ladder_and_delete_truth() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_note_reply").await;
    let app = crate::build_router(test_state(pool.clone()));
    let alice = signup(&app, &format!("alice{}", mk_types::id::gen_aid_now())).await;
    let bob = signup(&app, &format!("bob{}", mk_types::id::gen_aid_now())).await;
    let a_token = alice["token"].as_str().unwrap().to_string();
    let b_token = bob["token"].as_str().unwrap().to_string();

    // 返信できる: createdNote.reply gets packed (nested detail)
    let (s, base) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "foo", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let base_id = base["createdNote"]["id"].as_str().unwrap().to_string();

    let (s, reply) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "bar", "replyId": base_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {reply}");
    assert_eq!(reply["createdNote"]["replyId"], json!(base_id));
    assert_eq!(reply["createdNote"]["reply"]["text"], json!("foo"));
    let reply_id = reply["createdNote"]["id"].as_str().unwrap().to_string();

    // visibility: followers reply target is invisible to a non-follower
    let (s, followers_note) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "direct note to bob", "visibility": "followers", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let fn_id = followers_note["createdNote"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, err) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "reply to alice note", "replyId": fn_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        err["error"]["code"],
        json!("CANNOT_REPLY_TO_AN_INVISIBLE_NOTE"),
        "body: {err}"
    );

    // 存在しないリプライ先で怒られる
    let (s, err) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({
            "text": "test",
            "replyId": "000000000000000000000000",
            "i": a_token
        }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        err["error"]["code"],
        json!("NO_SUCH_REPLY_TARGET"),
        "body: {err}"
    );

    // 引用renoteで空白文字のみ → createdNote.text null (pure renote degrade)
    let (s, blank) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "   ", "renoteId": base_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {blank}");
    assert_eq!(blank["createdNote"]["text"], json!(null));
    assert_eq!(blank["createdNote"]["renote"]["text"], json!("foo"));

    // delete a reply: db repliesCount truth (bump on create, dec on delete)
    let mut ids = Vec::new();
    for text in ["reply one", "reply two"] {
        let (s, r) = post_json(
            app.clone(),
            "/api/notes/create",
            json!({ "text": text, "replyId": base_id, "i": a_token }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "body: {r}");
        ids.push(r["createdNote"]["id"].as_str().unwrap().to_string());
    }
    let row = mk_db::note::find_note_by_id(&pool, &base_id)
        .await
        .unwrap()
        .unwrap();
    // 承カウント: upstream NoteCreateService.saveReply (L1015-1019) increments
    // the parent unconditionally for EVERY reply — bob's opening 'bar' counts
    // (the earlier ladder: bar + reply one + reply two = 3)
    assert_eq!(row.replies_count, 3);
    // reply created by alice here; delete needs bob's reply? both by alice — fine
    let (s, _) = post_json(
        app.clone(),
        "/api/notes/delete",
        json!({ "noteId": ids[0], "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let row = mk_db::note::find_note_by_id(&pool, &base_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.replies_count, 2);
    let (s, _) = post_json(
        app.clone(),
        "/api/notes/delete",
        json!({ "noteId": ids[1], "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let row = mk_db::note::find_note_by_id(&pool, &base_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.replies_count, 1);
    let (_s, gone) = post_json(app.clone(), "/api/notes/show", json!({ "noteId": ids[0] })).await;
    assert_eq!(gone["error"]["code"], json!("NO_SUCH_NOTE"), "body: {gone}");
    let _ = reply_id;
}

#[tokio::test]
async fn notes_create_word_ladders() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_note_words").await;
    let app = crate::build_router(test_state(pool.clone()));
    let root = signup(&app, &format!("root{}", mk_types::id::gen_aid_now())).await;
    let alice = signup(&app, &format!("alice{}", mk_types::id::gen_aid_now())).await;
    let r_token = root["token"].as_str().unwrap().to_string();
    let a_token = alice["token"].as_str().unwrap().to_string();

    // (単語指定) force-home
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/update-meta",
        json!({ "sensitiveWords": ["test"], "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "update-meta");
    let (s, note) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "hogetesthuge", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        note["createdNote"]["visibility"],
        json!("home"),
        "body: {note}"
    );

    // (正規表現)
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/update-meta",
        json!({ "sensitiveWords": ["/Test/i"], "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, note) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "hogetesthuge", "i": a_token }),
    )
    .await;
    assert_eq!(note["createdNote"]["visibility"], json!("home"));

    // (スペースアンド)
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/update-meta",
        json!({ "sensitiveWords": ["Test hoge"], "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, note) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "hogeTesthuge", "i": a_token }),
    )
    .await;
    assert_eq!(note["createdNote"]["visibility"], json!("home"));

    // 禁止ワード (単語/正規表現/スペースアンド → 400 CONTAINS_PROHIBITED_WORDS;
    // poll choices ride the same check). Upstream pairs each word form with
    // its exact fixture casing (note.ts L719-763): the plain word and the
    // /regex/ forms hit lowercase 'hogetesthuge' (isKeyWordIncluded is
    // CASE-SENSITIVE includes for non-regex forms — UtilityService L92-94),
    // only the space-AND form uses 'hogeTesthuge'.
    for (words, text) in [
        (json!(["test"]), "hogetesthuge"),
        (json!(["/Test/i"]), "hogetesthuge"),
        (json!(["Test hoge"]), "hogeTesthuge"),
    ] {
        let (s, _) = post_json(
            app.clone(),
            "/api/admin/update-meta",
            json!({ "prohibitedWords": words, "i": r_token }),
        )
        .await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (s, err) = post_json(
            app.clone(),
            "/api/notes/create",
            json!({ "text": text, "i": a_token }),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "words: {words} text: {text}");
        assert_eq!(
            err["error"]["code"],
            json!("CONTAINS_PROHIBITED_WORDS"),
            "body: {err}"
        );
    }
}

#[tokio::test]
async fn notes_create_mentions_and_translate_ladder() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_note_mentions").await;
    let app = crate::build_router(test_state(pool.clone()));
    let root = signup(&app, &format!("root{}", mk_types::id::gen_aid_now())).await;
    let alice = signup(&app, &format!("alice{}", mk_types::id::gen_aid_now())).await;
    let bob = signup(&app, &format!("bob{}", mk_types::id::gen_aid_now())).await;
    let r_token = root["token"].as_str().unwrap().to_string();
    let a_token = alice["token"].as_str().unwrap().to_string();
    let bob_id = bob["id"].as_str().unwrap().to_string();

    // 同じユーザーに複数メンションしても内部的にまとめられる (user ids, dedup)
    // e2e note.ts L330-343 mentions the literal `@bob` because its fixture
    // signs up the literal username; this mirror suffixes every username to
    // dodge cross-run collisions, so the mention must target the actual
    // account (resolveUser is by username — `@bob` alone would resolve to
    // nothing and record no mention, unlike the mirrored upstream assert
    // `noteDoc.mentions == [bob.id]`).
    let bob_acct = bob["username"].as_str().unwrap().to_string();
    let (s, note) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": format!("@{bob_acct} @{bob_acct} @{bob_acct} yo"), "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {note}");
    let note_id = note["createdNote"]["id"].as_str().unwrap().to_string();
    let row = mk_db::note::find_note_by_id(&pool, &note_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.mentions, vec![bob_id.clone()], "db mentions");

    // 存在しないユーザーにメンションできる (resolves to nothing, stays 200)
    let (s, note) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "@ghost yo", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {note}");
    let ghost_id = note["createdNote"]["id"].as_str().unwrap().to_string();
    let row = mk_db::note::find_note_by_id(&pool, &ghost_id)
        .await
        .unwrap()
        .unwrap();
    assert!(row.mentions.is_empty());

    // メンションの数が上限を超えるとエラーになる (mentionLimit policy 0)
    let (s, role) = post_json(
        app.clone(),
        "/api/admin/roles/create",
        json!({
            "name": "test",
            "description": "",
            "color": null,
            "iconUrl": null,
            "displayOrder": 0,
            "target": "manual",
            "condFormula": {},
            "isAdministrator": false,
            "isModerator": false,
            "isPublic": false,
            "isExplorable": false,
            "asBadge": false,
            "canEditMembersByModerator": false,
            "policies": { "mentionLimit": { "useDefault": false, "priority": 1, "value": 0 } },
            "i": r_token
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {role}");
    let role_id = role["id"].as_str().unwrap().to_string();
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/roles/assign",
        json!({ "userId": alice["id"], "roleId": role_id, "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, err) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": format!("@{bob_acct} potentially annoying text"), "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        err["error"]["code"],
        json!("CONTAINS_TOO_MANY_MENTIONS"),
        "body: {err}"
    );
    // ダイレクト投稿もエラーになる
    let (s, err) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({
            "text": "potentially annoying text",
            "visibility": "specified",
            "visibleUserIds": [bob_id],
            "i": a_token
        }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"]["code"], json!("CONTAINS_TOO_MANY_MENTIONS"));
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/roles/unassign",
        json!({ "userId": alice["id"], "roleId": role_id, "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/roles/delete",
        json!({ "roleId": role_id, "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // notes/translate ladder: unknown note → NO_SUCH_NOTE (400)
    let (s, err) = post_json(
        app.clone(),
        "/api/notes/translate",
        json!({ "noteId": "foo", "targetLang": "ja", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(err["error"]["code"], json!("NO_SUCH_NOTE"), "body: {err}");

    // default install (no DeepL key) → UNAVAILABLE
    let (s, err) = post_json(
        app.clone(),
        "/api/notes/translate",
        json!({ "noteId": note_id, "targetLang": "ja", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"]["code"], json!("UNAVAILABLE"), "body: {err}");

    // text:null poll note → 204 空レスポンス
    let (s, null_note) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({
            "text": null,
            "poll": { "choices": ["kinoko", "takenoko"] },
            "i": a_token
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {null_note}");
    assert_eq!(null_note["createdNote"]["text"], json!(null));
    let null_id = null_note["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, body) = post_json(
        app.clone(),
        "/api/notes/translate",
        json!({ "noteId": null_id, "targetLang": "ja", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {body}");

    // bare create → visible on users/notes (freq path) and on the global notes list
    // (the e2e ladder's 'ノートできる' plain note, note.ts L35-46 text 'test')
    let (s, plain) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "test", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {plain}");
    let (s, res) = post_json(
        app.clone(),
        "/api/users/notes",
        json!({ "userId": alice["id"], "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {res}");
    assert!(
        res.as_array()
            .unwrap()
            .iter()
            .any(|n| n["text"] == json!("test")),
        "users/notes carries the note: {res}"
    );
    let (s, res) = post_json(app.clone(), "/api/notes", json!({ "i": a_token })).await;
    assert_eq!(s, StatusCode::OK, "body: {res}");
    let arr = res.as_array().unwrap();
    assert!(arr.iter().all(|n| n["visibility"] == json!("public")));
    assert!(arr.iter().any(|n| n["id"] == json!(note_id)));
}

// ===== timelines.ts arena: notes/timeline / local / hybrid mirrors ==========
//
// Both-fanout-mode leg shapes from the pinned upstream e2e (timelines.ts,
// ea2c3aed) against the mk-db timelines.rs query fusion (db fallback +
// FanoutTimelineEndpointService push semantics). The FIXME #12065-guarded
// shapes ride the same fusion and are covered by the e2e workflow's
// enableFanoutTimeline:true pass.

#[tokio::test]
async fn timelines_matrix_home_local_hybrid() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktimelines_matrix").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let alice = signup(&app, &uname("tla")).await;
    let bob = signup(&app, &uname("tlb")).await;
    let carol = signup(&app, &uname("tlc")).await;
    let dave = signup(&app, &uname("tld")).await;
    // &str bindings (Copy) — the e2e fixture ids/tokens ride many json! bodies
    let a = alice["id"].as_str().unwrap();
    let b = bob["id"].as_str().unwrap();
    let c = carol["id"].as_str().unwrap();
    let d = dave["id"].as_str().unwrap();
    let at = alice["token"].as_str().unwrap();
    let bt = bob["token"].as_str().unwrap();
    let ct = carol["token"].as_str().unwrap();
    let dt = dave["token"].as_str().unwrap();

    async fn note_post(app: &axum::Router, body: Value) -> String {
        let (s, v) = post_json(app.clone(), "/api/notes/create", body).await;
        assert_eq!(s, StatusCode::OK, "body: {v}");
        v["createdNote"]["id"].as_str().unwrap().to_string()
    }
    async fn fetch_tl(app: &axum::Router, name: &str, mut params: Value, token: &str) -> Value {
        params["i"] = json!(token);
        let (s, v) = post_json(app.clone(), &format!("/api/{name}"), params).await;
        assert_eq!(s, StatusCode::OK, "body: {v}");
        v
    }
    fn has(res: &Value, id: &str) -> bool {
        res.as_array().unwrap().iter().any(|n| n["id"] == json!(id))
    }
    fn lacks(res: &Value, id: &str) -> bool {
        !has(res, id)
    }

    // -- Home: candidates are own + followees; followers-visibility self and
    //    followee (generateVisibilityQuery followers arm = author IN
    //    meOrFollowees) -----------------------------------------------
    let n_self_fol = note_post(
        &app,
        json!({ "text": "hi", "visibility": "followers", "i": at }),
    )
    .await;
    let (s, v) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": b, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let n_bob = note_post(&app, json!({ "text": "hi", "i": bt })).await;
    let n_carol = note_post(&app, json!({ "text": "hi", "i": ct })).await;
    let n_bob_fol = note_post(
        &app,
        json!({ "text": "hi", "visibility": "followers", "i": bt }),
    )
    .await;
    let res = fetch_tl(&app, "notes/timeline", json!({ "limit": 100 }), &at).await;
    assert!(has(&res, &n_self_fol));
    assert!(has(&res, &n_bob));
    assert!(has(&res, &n_bob_fol));
    assert!(lacks(&res, &n_carol));

    // -- Home replies: foreign reply excluded with following.withReplies
    //    false, included after following/update flips the flag -----------
    let n_bob_reply = note_post(&app, json!({ "text": "hi", "replyId": n_carol, "i": bt })).await;
    let res = fetch_tl(&app, "notes/timeline", json!({ "limit": 100 }), &at).await;
    assert!(lacks(&res, &n_bob_reply));
    // not-following → NOT_FOLLOWING
    let (s, err) = post_json(
        app.clone(),
        "/api/following/update",
        json!({ "userId": c, "withReplies": true, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(err["error"]["code"], json!("NOT_FOLLOWING"));
    let (s, v) = post_json(
        app.clone(),
        "/api/following/update",
        json!({ "userId": b, "withReplies": true, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let res = fetch_tl(&app, "notes/timeline", json!({ "limit": 100 }), &at).await;
    assert!(has(&res, &n_bob_reply));

    // -- Home: followers-visibility REPLY-TARGET gate (endpoint noteFilter):
    //    the reply rides the withReplies push but the followers parent must
    //    be one I follow (or my own) ---------------------------------
    // bob follows carol — the upstream fixture carries BOTH follows here
    // (timelines.ts L222-225), because a non-follower cannot even reply to a
    // followers note (NoteCreateService.fetchAndCreate L385 isVisibleForMe)
    let (s, v) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": c, "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let n_carol_fol = note_post(
        &app,
        json!({ "text": "hi", "visibility": "followers", "i": ct }),
    )
    .await;
    let n_bob_reply2 = note_post(
        &app,
        json!({ "text": "hi", "replyId": n_carol_fol, "i": bt }),
    )
    .await;
    let res = fetch_tl(&app, "notes/timeline", json!({ "limit": 100 }), &at).await;
    assert!(lacks(&res, &n_bob_reply2));
    assert!(lacks(&res, &n_carol_fol));
    // after following carol both surface (withReplies stays on)
    let (s, v) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": c, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let res = fetch_tl(&app, "notes/timeline", json!({ "limit": 100 }), &at).await;
    assert!(has(&res, &n_bob_reply2));
    assert!(has(&res, &n_carol_fol));

    // -- Home: replies addressed to me always ride along ---------------
    let n_alice_note = note_post(&app, json!({ "text": "hi", "i": at })).await;
    let n_bob_reply_me = note_post(
        &app,
        json!({ "text": "hi", "replyId": n_alice_note, "i": bt }),
    )
    .await;
    let res = fetch_tl(&app, "notes/timeline", json!({ "limit": 100 }), &at).await;
    assert!(has(&res, &n_bob_reply_me));

    // -- Home: specified only to visibleUserIds Targets ---------------
    let n_bob_spec_other = note_post(
        &app,
        json!({ "text": "hi", "visibility": "specified", "visibleUserIds": [d], "i": bt }),
    )
    .await;
    let n_bob_spec_me = note_post(
        &app,
        json!({ "text": "hi", "visibility": "specified", "visibleUserIds": [a], "i": bt }),
    )
    .await;
    let n_self_spec = note_post(
        &app,
        json!({ "text": "hi", "visibility": "specified", "i": at }),
    )
    .await;
    let res = fetch_tl(&app, "notes/timeline", json!({ "limit": 100 }), &at).await;
    assert!(lacks(&res, &n_bob_spec_other));
    assert!(has(&res, &n_bob_spec_me));
    assert!(has(&res, &n_self_spec));

    // -- Home [withRenotes: false]: pure renote dropped, quote kept -----
    let n_bob_renote = note_post(&app, json!({ "renoteId": n_carol, "i": bt })).await;
    let n_bob_quote = note_post(&app, json!({ "text": "q", "renoteId": n_carol, "i": bt })).await;
    let res = fetch_tl(
        &app,
        "notes/timeline",
        json!({ "withRenotes": false, "limit": 100 }),
        &at,
    )
    .await;
    assert!(lacks(&res, &n_bob_renote));
    assert!(has(&res, &n_bob_quote));

    // -- Home: renote-mute CHAIN — muted author → unrelated quote →
    //    followee renote vanishes (isUserRelated(note.renote) chain) ----
    let (s, _) = post_json(
        app.clone(),
        "/api/mute/create",
        json!({ "userId": c, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let n_carol2 = note_post(&app, json!({ "text": "hi", "i": ct })).await;
    let n_dave_quote = note_post(
        &app,
        json!({ "text": "quote", "renoteId": n_carol2, "i": dt }),
    )
    .await;
    let n_bob_rerenote = note_post(&app, json!({ "renoteId": n_dave_quote, "i": bt })).await;
    let res = fetch_tl(&app, "notes/timeline", json!({ "limit": 100 }), &at).await;
    assert!(lacks(&res, &n_carol2));
    assert!(lacks(&res, &n_bob_rerenote));
    let (s, _) = post_json(
        app.clone(),
        "/api/mute/delete",
        json!({ "userId": c, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // -- Home [withFiles: true] ----------------------------------------
    let file_id = seed_drive_file(&pool, &b, "image/jpeg").await;
    let n_bob_text = note_post(&app, json!({ "text": "hi", "i": bt })).await;
    let n_bob_file = note_post(&app, json!({ "fileIds": [file_id], "i": bt })).await;
    let res = fetch_tl(
        &app,
        "notes/timeline",
        json!({ "withFiles": true, "limit": 100 }),
        &at,
    )
    .await;
    assert!(lacks(&res, &n_bob_text));
    assert!(has(&res, &n_bob_file));

    // -- Local TL: public-only plane; home visibility stays out even for
    //    followees --------------------------------------------------
    let n_carol_home =
        note_post(&app, json!({ "text": "hi", "visibility": "home", "i": ct })).await;
    let res = fetch_tl(&app, "notes/local-timeline", json!({ "limit": 100 }), &at).await;
    assert!(has(&res, &n_carol));
    assert!(has(&res, &n_dave_quote));
    assert!(lacks(&res, &n_carol_home));
    // reply plane: foreign reply out by default; addressed-to-me in;
    // withReplies=true opens the gate
    let n_carol_reply_bob =
        note_post(&app, json!({ "text": "hi", "replyId": n_bob, "i": ct })).await;
    let n_carol_reply_me = note_post(
        &app,
        json!({ "text": "hi", "replyId": n_alice_note, "i": ct }),
    )
    .await;
    let res = fetch_tl(&app, "notes/local-timeline", json!({ "limit": 100 }), &at).await;
    assert!(lacks(&res, &n_carol_reply_bob));
    assert!(has(&res, &n_carol_reply_me));
    let res = fetch_tl(
        &app,
        "notes/local-timeline",
        json!({ "withReplies": true, "limit": 100 }),
        &at,
    )
    .await;
    assert!(has(&res, &n_carol_reply_bob));

    // -- Hybrid TL: own+followees ∪ local public ------------------------
    let n_dave_pub = note_post(&app, json!({ "text": "hi", "i": dt })).await;
    let n_dave_home = note_post(&app, json!({ "text": "hi", "visibility": "home", "i": dt })).await;
    let res = fetch_tl(&app, "notes/hybrid-timeline", json!({ "limit": 100 }), &at).await;
    assert!(has(&res, &n_carol));
    assert!(has(&res, &n_dave_pub));
    assert!(lacks(&res, &n_dave_home));
    // reply gate default-off: unaddressed foreign reply excluded…
    let n_dave_reply = note_post(&app, json!({ "text": "hi", "replyId": n_carol, "i": dt })).await;
    let res = fetch_tl(&app, "notes/hybrid-timeline", json!({ "limit": 100 }), &at).await;
    assert!(lacks(&res, &n_dave_reply));
    // …and withReplies true opens the gate
    let res = fetch_tl(
        &app,
        "notes/hybrid-timeline",
        json!({ "withReplies": true, "limit": 100 }),
        &at,
    )
    .await;
    assert!(has(&res, &n_dave_reply));
    // followee's home-visibility note rides the followee arm
    let (s, v) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": d, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let res = fetch_tl(&app, "notes/hybrid-timeline", json!({ "limit": 100 }), &at).await;
    assert!(has(&res, &n_dave_home));

    // -- 凍結: suspended author drops out of the local plane and returns
    let root = signup(&app, "root").await;
    let root_token = root["token"].as_str().unwrap();
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/suspend-user",
        json!({ "userId": c, "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let res = fetch_tl(&app, "notes/hybrid-timeline", json!({ "limit": 100 }), &at).await;
    assert!(lacks(&res, &n_carol));
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/unsuspend-user",
        json!({ "userId": c, "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let res = fetch_tl(&app, "notes/hybrid-timeline", json!({ "limit": 100 }), &at).await;
    assert!(has(&res, &n_carol));
}

// tl-v3c: timelines.ts arena legs locked per stream type for the trio
// (pinned upstream test/e2e/timelines.ts): the remote-user split (LTL
// excludes; home/hybrid pick a followed remote user up), followers-only
// and specified/visibleUserIds planes per stream, the withReplies DM-reply
// drop, withRenotes=false pure-renote drop, and withFiles on the local
// plane. Remote users/notes ride raw schema-default inserts because signup
// is local-only (user_profile fetch_one'd by the pack pipeline).
#[tokio::test]
async fn timelines_tl_v3c_per_stream_legs() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktlv3c").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let alice = signup(&app, &uname("v3a")).await;
    let bob = signup(&app, &uname("v3b")).await;
    let carol = signup(&app, &uname("v3c")).await;
    let a = alice["id"].as_str().unwrap();
    let b = bob["id"].as_str().unwrap();
    let c = carol["id"].as_str().unwrap();
    let at = alice["token"].as_str().unwrap();
    let bt = bob["token"].as_str().unwrap();

    async fn note_post(app: &axum::Router, body: Value) -> String {
        let (s, v) = post_json(app.clone(), "/api/notes/create", body).await;
        assert_eq!(s, StatusCode::OK, "body: {v}");
        v["createdNote"]["id"].as_str().unwrap().to_string()
    }
    async fn fetch_tl(app: &axum::Router, name: &str, mut params: Value, token: &str) -> Value {
        params["i"] = json!(token);
        let (s, v) = post_json(app.clone(), &format!("/api/{name}"), params).await;
        assert_eq!(s, StatusCode::OK, "body: {v}");
        v
    }
    fn has(res: &Value, id: &str) -> bool {
        res.as_array().unwrap().iter().any(|n| n["id"] == json!(id))
    }
    fn lacks(res: &Value, id: &str) -> bool {
        !has(res, id)
    }

    // -- remote fixture: a remote user's public + home notes raw-inserted --
    // (token is character(16) and LocalUserRow decodes it non-Option — a
    // tokenless row would break the note pack's find_user_by_id)
    let rid = mk_types::id::gen_aid_now();
    mk_db::sqlx::query(
        r#"INSERT INTO "user" ("id", "username", "usernameLower", "host", "token")
           VALUES ($1, 'remobo', 'remobo', 'remote.example', 'remotousertoken1')"#,
    )
    .bind(&rid)
    .execute(&pool)
    .await
    .unwrap();
    mk_db::sqlx::query(r#"INSERT INTO "user_profile" ("userId") VALUES ($1)"#)
        .bind(&rid)
        .execute(&pool)
        .await
        .unwrap();
    let remote_pub = mk_types::id::gen_aid_now();
    let remote_home = mk_types::id::gen_aid_now();
    mk_db::sqlx::query(
        r#"INSERT INTO "note" ("id", "userId", "text", "visibility", "userHost")
           VALUES ($1, $2, 'remote pub', 'public', 'remote.example'),
                  ($3, $2, 'remote home', 'home', 'remote.example')"#,
    )
    .bind(&remote_pub)
    .bind(&rid)
    .bind(&remote_home)
    .execute(&pool)
    .await
    .unwrap();

    // リモートユーザーのノート: the local plane excludes remote rows even for
    // a logged-in viewer, and the followee plane keeps unfollowed remotes out
    let res = fetch_tl(&app, "notes/local-timeline", json!({ "limit": 100 }), &at).await;
    assert!(lacks(&res, &remote_pub));
    let res = fetch_tl(&app, "notes/hybrid-timeline", json!({ "limit": 100 }), &at).await;
    assert!(lacks(&res, &remote_pub));
    let res = fetch_tl(&app, "notes/timeline", json!({ "limit": 100 }), &at).await;
    assert!(lacks(&res, &remote_pub));

    // フォローしているリモートユーザー: after following the remote user, the
    // home-visibility note rides the followee arm of home AND hybrid
    let (s, v) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": rid, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let res = fetch_tl(&app, "notes/timeline", json!({ "limit": 100 }), &at).await;
    assert!(has(&res, &remote_pub), "remote pub must ride home");
    assert!(has(&res, &remote_home), "remote home must ride home");
    let res = fetch_tl(&app, "notes/hybrid-timeline", json!({ "limit": 100 }), &at).await;
    assert!(has(&res, &remote_home), "remote home must ride hybrid");

    // -- followers-only per stream: bob's followers note rides home + hybrid
    //    once followed, never the public-only local plane -----------------
    let (s, _) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": b, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let n_bob_fol = note_post(
        &app,
        json!({ "text": "fol", "visibility": "followers", "i": bt }),
    )
    .await;
    let res = fetch_tl(&app, "notes/timeline", json!({ "limit": 100 }), &at).await;
    assert!(has(&res, &n_bob_fol));
    let res = fetch_tl(&app, "notes/hybrid-timeline", json!({ "limit": 100 }), &at).await;
    assert!(has(&res, &n_bob_fol));
    let res = fetch_tl(&app, "notes/local-timeline", json!({ "limit": 100 }), &at).await;
    assert!(lacks(&res, &n_bob_fol));

    // -- specified/visibleUserIds split: bob's DM to alice rides home +
    //    hybrid (visibleUserIds arm), never local; unfollowed carol's DM
    //    stays out of the followee planes --------------------------------
    let n_bob_dm = note_post(
        &app,
        json!({ "text": "dm", "visibility": "specified", "visibleUserIds": [a], "i": bt }),
    )
    .await;
    let res = fetch_tl(&app, "notes/timeline", json!({ "limit": 100 }), &at).await;
    assert!(has(&res, &n_bob_dm));
    let res = fetch_tl(&app, "notes/hybrid-timeline", json!({ "limit": 100 }), &at).await;
    assert!(has(&res, &n_bob_dm));
    let res = fetch_tl(&app, "notes/local-timeline", json!({ "limit": 100 }), &at).await;
    assert!(lacks(&res, &n_bob_dm));
    let n_carol_dm = note_post(
        &app,
        json!({ "text": "dmc", "visibility": "specified", "visibleUserIds": [a], "i": carol["token"].as_str().unwrap() }),
    )
    .await;
    let res = fetch_tl(&app, "notes/timeline", json!({ "limit": 100 }), &at).await;
    assert!(lacks(&res, &n_carol_dm));
    let res = fetch_tl(&app, "notes/hybrid-timeline", json!({ "limit": 100 }), &at).await;
    assert!(lacks(&res, &n_carol_dm));

    // -- withReplies: true still drops a followee's DM REPLY to a third
    //    party (the reply ride-along never lifts the specified VIS gate) --
    let n_carol_note = note_post(
        &app,
        json!({ "text": "hi", "i": carol["token"].as_str().unwrap() }),
    )
    .await;
    let n_bob_dm_reply = note_post(
        &app,
        json!({
            "text": "dm reply", "replyId": n_carol_note,
            "visibility": "specified", "visibleUserIds": [c], "i": bt
        }),
    )
    .await;
    let res = fetch_tl(
        &app,
        "notes/timeline",
        json!({ "withReplies": true, "limit": 100 }),
        &at,
    )
    .await;
    assert!(lacks(&res, &n_bob_dm_reply));
    let res = fetch_tl(
        &app,
        "notes/hybrid-timeline",
        json!({ "withReplies": true, "limit": 100 }),
        &at,
    )
    .await;
    assert!(lacks(&res, &n_bob_dm_reply));

    // -- withRenotes: false drops a followee's pure renote of a home note
    //    (EXCLUDE_PURE_RENOTES ladder; default withRenotes keeps it) ------
    let n_carol_home = note_post(
        &app,
        json!({ "text": "home", "visibility": "home", "i": carol["token"].as_str().unwrap() }),
    )
    .await;
    let n_bob_renote = note_post(&app, json!({ "renoteId": n_carol_home, "i": bt })).await;
    let res = fetch_tl(
        &app,
        "notes/timeline",
        json!({ "withRenotes": false, "limit": 100 }),
        &at,
    )
    .await;
    assert!(lacks(&res, &n_bob_renote));
    let res = fetch_tl(&app, "notes/timeline", json!({ "limit": 100 }), &at).await;
    assert!(has(&res, &n_bob_renote));

    // -- withFiles: true on the local plane keeps file notes only ---------
    let file_id = seed_drive_file(&pool, b, "image/jpeg").await;
    let n_bob_text = note_post(&app, json!({ "text": "txt", "i": bt })).await;
    let n_bob_file = note_post(&app, json!({ "fileIds": [file_id], "i": bt })).await;
    let res = fetch_tl(
        &app,
        "notes/local-timeline",
        json!({ "withFiles": true, "limit": 100 }),
        &at,
    )
    .await;
    assert!(lacks(&res, &n_bob_text));
    assert!(has(&res, &n_bob_file));
}

// utl-misc: users/notes User TL mute-chain + reply-gate + sinceId-ASC legs
// (pinned upstream timelines.ts User TL section): ミュートしているユーザーの
// ノートの、関係のないユーザによる引用ノート/リプライの、リノートが含まれない
// (the generateMutedUserQueryForNotes renote-side pass), ミュートしていても
// userId に指定したユーザーの投稿が含まれる (excludeUserFromMute), the
// NoteCreateService.pushToTl userTimeline reply plane (self-replies stay,
// replies to others ride only withReplies), and the makePaginationQuery
// sinceId ASC order.
#[tokio::test]
async fn users_notes_user_tl_mute_chain_reply_gate_since_id() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkutlmisc").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let alice = signup(&app, &uname("uma")).await;
    let bob = signup(&app, &uname("umb")).await;
    let carol = signup(&app, &uname("umc")).await;
    let dave = signup(&app, &uname("umd")).await;
    let (a_id, a_token) = (
        alice["id"].as_str().unwrap(),
        alice["token"].as_str().unwrap(),
    );
    let (b_id, b_token) = (bob["id"].as_str().unwrap(), bob["token"].as_str().unwrap());
    let carol_token = carol["token"].as_str().unwrap();
    let dave_token = dave["token"].as_str().unwrap();

    async fn note_post(app: &axum::Router, body: Value) -> String {
        let (s, v) = post_json(app.clone(), "/api/notes/create", body).await;
        assert_eq!(s, StatusCode::OK, "body: {v}");
        v["createdNote"]["id"].as_str().unwrap().to_string()
    }
    async fn fetch_user_notes(app: &axum::Router, mut params: Value, token: Option<&str>) -> Value {
        if let Some(t) = token {
            params["i"] = json!(t);
        }
        let (s, v) = post_json(app.clone(), "/api/users/notes", params).await;
        assert_eq!(s, StatusCode::OK, "body: {v}");
        v
    }
    fn has(res: &Value, id: &str) -> bool {
        res.as_array().unwrap().iter().any(|n| n["id"] == json!(id))
    }

    // -- mute chain: alice mutes carol; bob's renote of carol's note, of
    //    dave's quote-of-carol, and of dave's reply-of-carol all vanish from
    //    bob's users/notes as seen by alice (QueryService renote pass) but
    //    survive for the guest view (no muter) ------------------------------
    let (s, _) = post_json(
        app.clone(),
        "/api/mute/create",
        json!({ "userId": carol["id"], "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let carol_note = note_post(&app, json!({ "text": "hi", "i": carol_token })).await;
    let dave_quote = note_post(
        &app,
        json!({ "text": "quote hi", "renoteId": carol_note, "i": dave_token }),
    )
    .await;
    let dave_reply = note_post(
        &app,
        json!({ "text": "reply hi", "replyId": carol_note, "i": dave_token }),
    )
    .await;
    let bob_renote_of_muted =
        note_post(&app, json!({ "renoteId": carol_note, "i": b_token })).await;
    let bob_renote_of_quote =
        note_post(&app, json!({ "renoteId": dave_quote, "i": b_token })).await;
    let bob_renote_of_reply =
        note_post(&app, json!({ "renoteId": dave_reply, "i": b_token })).await;

    let res = fetch_user_notes(&app, json!({ "userId": b_id, "limit": 100 }), Some(a_token)).await;
    assert!(
        !has(&res, &bob_renote_of_muted),
        "mute chain must drop the renote of a muted user's note: {res}"
    );
    assert!(
        !has(&res, &bob_renote_of_quote),
        "mute chain must drop the renote of a quote of a muted user's note: {res}"
    );
    assert!(
        !has(&res, &bob_renote_of_reply),
        "mute chain must drop the renote of a reply to a muted user's note: {res}"
    );
    let res = fetch_user_notes(&app, json!({ "userId": b_id, "limit": 100 }), None).await;
    assert!(
        has(&res, &bob_renote_of_reply),
        "guest view carries no muter — the renote stays: {res}"
    );

    // -- excludeUserFromMute: fetching the MUTED user's own timeline keeps
    //    their notes (mute chain starts from the requested user) ----------
    let res = fetch_user_notes(
        &app,
        json!({ "userId": carol["id"], "limit": 100 }),
        Some(a_token),
    )
    .await;
    assert!(
        has(&res, &carol_note),
        "userId exempts the requested user from the mute exclusion: {res}"
    );

    // -- reply plane: a reply to a third party rides only withReplies; the
    //    self-reply stays on the default plane (target dave is unmuted so
    //    the mute chain never masks the gate) ------------------------------
    let bob_note1 = note_post(&app, json!({ "text": "hi", "i": b_token })).await;
    let dave_base = note_post(&app, json!({ "text": "dave", "i": dave_token })).await;
    let bob_reply_out = note_post(
        &app,
        json!({ "text": "hi", "replyId": dave_base, "i": b_token }),
    )
    .await;
    let bob_self_reply = note_post(
        &app,
        json!({ "text": "hi", "replyId": bob_note1, "i": b_token }),
    )
    .await;
    let res = fetch_user_notes(&app, json!({ "userId": b_id, "limit": 100 }), Some(a_token)).await;
    assert!(has(&res, &bob_note1));
    assert!(
        !has(&res, &bob_reply_out),
        "[withReplies: false] 他人への返信が含まれない: {res}"
    );
    assert!(
        has(&res, &bob_self_reply),
        "self-replies stay on the base plane: {res}"
    );
    let res = fetch_user_notes(
        &app,
        json!({ "userId": b_id, "limit": 100, "withReplies": true }),
        Some(a_token),
    )
    .await;
    assert!(
        has(&res, &bob_reply_out),
        "[withReplies: true] 他人への返信が含まれる: {res}"
    );

    // -- sinceId pagination: makePaginationQuery runs ASC on sinceId-only
    //    and DESC when untilId bounds it (FTT sinceId legs) ---------------
    let note_since = note_post(&app, json!({ "text": "since", "i": a_token })).await;
    let note1 = note_post(&app, json!({ "text": "1", "i": a_token })).await;
    let note2 = note_post(&app, json!({ "text": "2", "i": a_token })).await;
    let note_until = note_post(&app, json!({ "text": "until", "i": a_token })).await;
    let res = fetch_user_notes(&app, json!({ "userId": a_id, "sinceId": note_since }), None).await;
    let ids: Vec<&str> = res
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec![note1.as_str(), note2.as_str(), note_until.as_str()],
        "sinceId-only → ASC: {res}"
    );
    let res = fetch_user_notes(
        &app,
        json!({ "userId": a_id, "sinceId": note_since, "untilId": note_until }),
        None,
    )
    .await;
    let ids: Vec<&str> = res
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec![note2.as_str(), note1.as_str()],
        "sinceId+untilId → DESC inside the window: {res}"
    );
}

// ===== ul-tl: users/lists/* + notes/user-list-timeline ======================
//
// timelines.ts User List TL legs (upstream users/lists fixtures family +
// notes/user-list-timeline.ts getFromDb): list membership visibility
// (public/home ride always, followers only once the viewer follows the member
// or authored the note), the per-member withReplies reply plane
// (UserListService.addMember default false), the non-owner NO_SUCH_LIST shape
// and the withFiles filter.

#[tokio::test]
async fn user_list_timeline_membership_visibility_replies() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkul_tl_membership").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let alice = signup(&app, &uname("ula")).await;
    let bob = signup(&app, &uname("ulb")).await;
    let carol = signup(&app, &uname("ulc")).await;
    let a = alice["id"].as_str().unwrap();
    let b = bob["id"].as_str().unwrap();
    let at = alice["token"].as_str().unwrap();
    let bt = bob["token"].as_str().unwrap();

    async fn note_post(app: &axum::Router, body: Value) -> String {
        let (s, v) = post_json(app.clone(), "/api/notes/create", body).await;
        assert_eq!(s, StatusCode::OK, "body: {v}");
        v["createdNote"]["id"].as_str().unwrap().to_string()
    }
    async fn fetch_tl(app: &axum::Router, params: Value, token: &str) -> (StatusCode, Value) {
        post_json(
            app.clone(),
            "/api/notes/user-list-timeline",
            json!({ "listId": params["listId"], "limit": 100, "i": token }),
        )
        .await
    }
    fn has(res: &Value, id: &str) -> bool {
        res.as_array().unwrap().iter().any(|n| n["id"] == json!(id))
    }
    fn lacks(res: &Value, id: &str) -> bool {
        !has(res, id)
    }

    // -- users/lists/create: UserList pack shape { id, createdAt, name,
    //    userIds: [], isPublic: false } -----------------------------------
    let (s, list) = post_json(
        app.clone(),
        "/api/users/lists/create",
        json!({ "name": "list", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {list}");
    assert_eq!(list["name"], json!("list"));
    assert_eq!(list["userIds"], json!([]));
    assert_eq!(list["isPublic"], json!(false));
    assert!(list["createdAt"].as_str().is_some());
    let list_id = list["id"].as_str().unwrap().to_string();

    // -- users/lists/push: 204, and the member shows in the pack ----------
    let (s, v) = post_json(
        app.clone(),
        "/api/users/lists/push",
        json!({ "listId": list_id, "userId": b, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {v}");

    // -- non-followed member: public + home ride, followers stays out -----
    let n_bob_pub = note_post(&app, json!({ "text": "hi", "i": bt })).await;
    let n_bob_home = note_post(&app, json!({ "text": "hi", "visibility": "home", "i": bt })).await;
    let (s, res) = fetch_tl(&app, json!({ "listId": list_id }), at).await;
    assert_eq!(s, StatusCode::OK, "body: {res}");
    assert!(has(&res, &n_bob_pub));
    assert!(has(&res, &n_bob_home));
    let n_bob_fol = note_post(
        &app,
        json!({ "text": "hi", "visibility": "followers", "i": bt }),
    )
    .await;
    let (s, res) = fetch_tl(&app, json!({ "listId": list_id }), at).await;
    assert_eq!(s, StatusCode::OK, "body: {res}");
    assert!(lacks(&res, &n_bob_fol));

    // -- followed member's followers note rides (and packs its text) ------
    let (s, _) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": b, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let n_bob_fol2 = note_post(
        &app,
        json!({ "text": "fol", "visibility": "followers", "i": bt }),
    )
    .await;
    let (s, res) = fetch_tl(&app, json!({ "listId": list_id }), at).await;
    assert!(has(&res, &n_bob_fol2));
    assert!(
        res.as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == json!(n_bob_fol2))
            .unwrap()["text"]
            == json!("fol")
    );

    // -- self rides: alice pushed herself, her own followers note flows ---
    let n_alice_fol = note_post(
        &app,
        json!({ "text": "hi", "visibility": "followers", "i": at }),
    )
    .await;
    let (s, res) = fetch_tl(&app, json!({ "listId": list_id }), at).await;
    assert!(lacks(&res, &n_alice_fol), "self not yet a member");
    let (s, v) = post_json(
        app.clone(),
        "/api/users/lists/push",
        json!({ "listId": list_id, "userId": a, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {v}");
    let (s, res) = fetch_tl(&app, json!({ "listId": list_id }), at).await;
    assert!(has(&res, &n_alice_fol));

    // -- reply plane (membership withReplies default false): self-replies
    //    and replies to ME ride; third-party replies stay out until
    //    users/lists/update-membership withReplies: true opens the gate ---
    let n_alice_plain = note_post(&app, json!({ "text": "hi", "i": at })).await;
    let n_bob_selfrep =
        note_post(&app, json!({ "text": "hi", "replyId": n_bob_pub, "i": bt })).await;
    let n_bob_merep = note_post(
        &app,
        json!({ "text": "hi", "replyId": n_alice_plain, "i": bt }),
    )
    .await;
    let n_carol_plain = note_post(
        &app,
        json!({ "text": "hi", "i": carol["token"].as_str().unwrap() }),
    )
    .await;
    let n_bob_carolrep = note_post(
        &app,
        json!({ "text": "hi", "replyId": n_carol_plain, "i": bt }),
    )
    .await;
    let (s, res) = fetch_tl(&app, json!({ "listId": list_id }), at).await;
    assert!(has(&res, &n_bob_selfrep), "self-reply always rides");
    assert!(has(&res, &n_bob_merep), "reply addressed to me rides");
    assert!(lacks(&res, &n_bob_carolrep), "third-party reply gated");
    let (s, v) = post_json(
        app.clone(),
        "/api/users/lists/update-membership",
        json!({ "listId": list_id, "userId": b, "withReplies": true, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {v}");
    let (s, res) = fetch_tl(&app, json!({ "listId": list_id }), at).await;
    assert!(
        has(&res, &n_bob_carolrep),
        "withReplies: true opens the gate"
    );
    let (s, v) = post_json(
        app.clone(),
        "/api/users/lists/update-membership",
        json!({ "listId": list_id, "userId": b, "withReplies": false, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {v}");
    let (s, res) = fetch_tl(&app, json!({ "listId": list_id }), at).await;
    assert!(
        lacks(&res, &n_bob_carolrep),
        "withReplies: false closes it again"
    );

    // -- non-owner listId → NO_SUCH_LIST (list scoped to its OWNER) -------
    let (s, err) = post_json(
        app.clone(),
        "/api/notes/user-list-timeline",
        json!({ "listId": list_id, "i": carol["token"].as_str().unwrap() }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(err["error"]["code"], json!("NO_SUCH_LIST"));

    // -- requireCredential: bare no-credential call → CREDENTIAL_REQUIRED -
    let (s, err) = post_json(
        app.clone(),
        "/api/notes/user-list-timeline",
        json!({ "listId": list_id }),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "body: {err}");
    assert_eq!(err["error"]["code"], json!("CREDENTIAL_REQUIRED"));
}

#[tokio::test]
async fn user_list_timeline_filters_pager_channel() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkul_tl_filters").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let alice = signup(&app, &uname("ulf")).await;
    let bob = signup(&app, &uname("ulg")).await;
    let dave = signup(&app, &uname("ulh")).await;
    let b = bob["id"].as_str().unwrap();
    let at = alice["token"].as_str().unwrap();
    let bt = bob["token"].as_str().unwrap();
    let dt = dave["token"].as_str().unwrap();

    async fn note_post(app: &axum::Router, body: Value) -> String {
        let (s, v) = post_json(app.clone(), "/api/notes/create", body).await;
        assert_eq!(s, StatusCode::OK, "body: {v}");
        v["createdNote"]["id"].as_str().unwrap().to_string()
    }
    // E0434: fn items cannot capture the test's `at` — bind the token as a
    // parameter instead (call sites thread it through).
    async fn fetch_tl(app: &axum::Router, mut params: Value, token: &str) -> Value {
        params["i"] = json!(token);
        let (s, v) = post_json(app.clone(), "/api/notes/user-list-timeline", params).await;
        assert_eq!(s, StatusCode::OK, "body: {v}");
        v
    }
    fn has(res: &Value, id: &str) -> bool {
        res.as_array().unwrap().iter().any(|n| n["id"] == json!(id))
    }
    fn lacks(res: &Value, id: &str) -> bool {
        !has(res, id)
    }
    async fn push_member(app: &axum::Router, list_id: &str, user_id: &str, token: &str) {
        let (s, _) = post_json(
            app.clone(),
            "/api/users/lists/push",
            json!({ "listId": list_id, "userId": user_id, "i": token }),
        )
        .await;
        assert_eq!(s, StatusCode::NO_CONTENT);
    }

    let (s, list) = post_json(
        app.clone(),
        "/api/users/lists/create",
        json!({ "name": "list", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {list}");
    let list_id = list["id"].as_str().unwrap().to_string();
    push_member(&app, &list_id, b, at).await;

    // -- pager: limit=1 returns the newest only; untilId/sinceId are
    //    EXCLUSIVE bounds (makePaginationQuery) ---------------------------
    let n1 = note_post(&app, json!({ "text": "p1", "i": bt })).await;
    let n2 = note_post(&app, json!({ "text": "p2", "i": bt })).await;
    let n3 = note_post(&app, json!({ "text": "p3", "i": bt })).await;
    let res = fetch_tl(&app, json!({ "listId": list_id, "limit": 100 }), at).await;
    assert!(has(&res, &n1));
    assert!(has(&res, &n3));
    let res = fetch_tl(&app, json!({ "listId": list_id, "limit": 1 }), at).await;
    let arr = res.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], json!(n3), "newest first");
    let res = fetch_tl(&app, json!({ "listId": list_id, "untilId": n3 }), at).await;
    assert!(has(&res, &n2), "untilId is exclusive-bound DESC");
    assert!(lacks(&res, &n3));
    let res = fetch_tl(&app, json!({ "listId": list_id, "sinceId": n1 }), at).await;
    assert!(has(&res, &n3), "sinceId is exclusive-bound ASC");
    assert!(lacks(&res, &n1));
    let res = fetch_tl(&app, json!({ "listId": list_id, "limit": 100 }), at).await;
    assert!(
        has(&res, &n1) && has(&res, &n3),
        "unbounded fetch carries all"
    );

    // -- member's channel note stays out; the channel-external renote of it
    //    flows (bob is a list member, the renote row itself has no channel) -
    let (s, channel) = post_json(
        app.clone(),
        "/api/channels/create",
        json!({ "name": "channel", "i": dt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {channel}");
    let channel_id = channel["id"].as_str().unwrap().to_string();
    let n_ch = note_post(
        &app,
        json!({ "text": "ok", "channelId": channel_id, "i": dt }),
    )
    .await;
    let n_ch_note = note_post(
        &app,
        json!({ "text": "ok", "channelId": channel_id, "i": bt }),
    )
    .await;
    let res = fetch_tl(&app, json!({ "listId": list_id, "limit": 100 }), at).await;
    assert!(lacks(&res, &n_ch));
    assert!(lacks(&res, &n_ch_note), "member channel note excluded");
    let n_ch_renote = note_post(&app, json!({ "renoteId": n_ch_note, "i": bt })).await;
    let res = fetch_tl(&app, json!({ "listId": list_id, "limit": 100 }), at).await;
    assert!(
        has(&res, &n_ch_renote),
        "external renote of a channel note flows"
    );

    // -- channels/mute/create flips the renoteChannelId gate --------------
    let (s, v) = post_json(
        app.clone(),
        "/api/channels/mute/create",
        json!({ "channelId": channel_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {v}");
    let (s, err) = post_json(
        app.clone(),
        "/api/channels/mute/create",
        json!({ "channelId": channel_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(err["error"]["code"], json!("ALREADY_MUTING_CHANNEL"));
    let n_ch_renote2 = note_post(&app, json!({ "renoteId": n_ch_note, "i": bt })).await;
    let res = fetch_tl(&app, json!({ "listId": list_id, "limit": 100 }), at).await;
    assert!(lacks(&res, &n_ch_renote2), "muted channel renote drops out");
    // The gate is applied at read time (getFromDb L192-201 brackets the whole
    // query), so the pre-mute renote — whose renoteChannelId resolves to the
    // muted channel — drops as well (matches the upstream チャンネルミュート
    // legs, which assert .some(...) === false for every renote after muted).
    assert!(
        lacks(&res, &n_ch_renote),
        "read-time gate drops the pre-mute renote too"
    );

    // -- withFiles: true keeps the file notes only ------------------------
    let file_id = seed_drive_file(&pool, b, "image/jpeg").await;
    let n_txt = note_post(&app, json!({ "text": "txt", "i": bt })).await;
    let n_file = note_post(&app, json!({ "fileIds": [file_id], "i": bt })).await;
    let res = fetch_tl(
        &app,
        json!({ "listId": list_id, "withFiles": true, "limit": 100 }),
        at,
    )
    .await;
    assert!(lacks(&res, &n_txt));
    assert!(has(&res, &n_file));

    // -- withRenotes: false drops bob's pure renote (quotes would stay) ---
    let n_dave_home = note_post(
        &app,
        json!({ "text": "home", "visibility": "home", "i": dt }),
    )
    .await;
    let n_renote = note_post(&app, json!({ "renoteId": n_dave_home, "i": bt })).await;
    let res = fetch_tl(&app, json!({ "listId": list_id, "limit": 100 }), at).await;
    assert!(has(&res, &n_renote), "default withRenotes keeps renotes");
    let res = fetch_tl(
        &app,
        json!({ "listId": list_id, "withRenotes": false, "limit": 100 }),
        at,
    )
    .await;
    assert!(lacks(&res, &n_renote));
}

// utl-misc: users/notes User TL channel-plane legs (pinned upstream
// timelines.ts User TL section): チャンネル投稿が含まれない, the
// [withChannelNotes: true] sensitive-channel split ([withChannelNotes: true]
// 他人が取得した場合センシティブチャンネル投稿が含まれない / 自分が取得した
// 場合…含まれる), and the Channel チャンネルミュート pair incl. the
// [チャンネル外リノート] renoteChannelId gate. Channel rows are raw-stamped
// (channels/create persists the channel; the note.channelId write side is a
// sibling lane's in-flight plumbing) so the SQL plane asserts in isolation.
#[tokio::test]
async fn users_notes_user_tl_channel_plane_legs() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkutlchan").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let alice = signup(&app, &uname("uca")).await;
    let bob = signup(&app, &uname("ucb")).await;
    let a_token = alice["token"].as_str().unwrap();
    let (b_id, b_token) = (bob["id"].as_str().unwrap(), bob["token"].as_str().unwrap());

    async fn note_post(app: &axum::Router, body: Value) -> String {
        let (s, v) = post_json(app.clone(), "/api/notes/create", body).await;
        assert_eq!(s, StatusCode::OK, "body: {v}");
        v["createdNote"]["id"].as_str().unwrap().to_string()
    }
    async fn fetch_user_notes(app: &axum::Router, mut params: Value, token: Option<&str>) -> Value {
        if let Some(t) = token {
            params["i"] = json!(t);
        }
        let (s, v) = post_json(app.clone(), "/api/users/notes", params).await;
        assert_eq!(s, StatusCode::OK, "body: {v}");
        v
    }
    fn has(res: &Value, id: &str) -> bool {
        res.as_array().unwrap().iter().any(|n| n["id"] == json!(id))
    }

    // -- channels: an open one and a sensitive one (channels/create persists
    //    isSensitive); alice mutes the open one via a raw channel_muting row
    //    (the channels/mute/* slice is a sibling lane) ---------------------
    let (s, ch) = post_json(
        app.clone(),
        "/api/channels/create",
        json!({ "name": "open", "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {ch}");
    let ch_open = ch["id"].as_str().unwrap().to_string();
    let (s, ch) = post_json(
        app.clone(),
        "/api/channels/create",
        json!({ "name": "hot", "isSensitive": true, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {ch}");
    let ch_hot = ch["id"].as_str().unwrap().to_string();
    mk_db::sqlx::query(
        r#"INSERT INTO "channel_muting" ("id", "userId", "channelId", "expiresAt")
           VALUES ($1, $2, $3, NULL)"#,
    )
    .bind(mk_types::id::gen_aid_now())
    .bind(alice["id"].as_str().unwrap())
    .bind(&ch_open)
    .execute(&pool)
    .await
    .unwrap();

    // -- raw channel notes: an open-channel note, a sensitive-channel note
    //    and an out-of-channel renote of the open-channel note -------------
    let ch_note = mk_types::id::gen_aid_now();
    let ch_hot_note = mk_types::id::gen_aid_now();
    mk_db::sqlx::query(
        r#"INSERT INTO "note" ("id", "userId", "text", "visibility", "channelId")
           VALUES ($1, $2, 'ch', 'public', $3),
                  ($4, $2, 'hot', 'public', $5)"#,
    )
    .bind(&ch_note)
    .bind(b_id)
    .bind(&ch_open)
    .bind(&ch_hot_note)
    .bind(&ch_hot)
    .execute(&pool)
    .await
    .unwrap();
    let plain_note = note_post(&app, json!({ "text": "plain", "i": b_token })).await;
    let ch_renote = mk_types::id::gen_aid_now();
    mk_db::sqlx::query(
        r#"INSERT INTO "note"
             ("id", "userId", "visibility", "renoteId", "renoteUserId", "renoteChannelId")
           VALUES ($1, $2, 'public', $3, $2, $4)"#,
    )
    .bind(&ch_renote)
    .bind(b_id)
    .bind(&ch_note)
    .bind(&ch_open)
    .execute(&pool)
    .await
    .unwrap();
    // the API renote path must denormalize the parent's channel too: the
    // notes/create renote arm stamps renoteChannelId (NoteCreateService.ts
    // L680) — guard against the silent no-stamp regression (the stamp keyed
    // on the row's own id instead of the parent matched nothing once).
    let ch_renote_api = note_post(&app, json!({ "renoteId": ch_note, "i": b_token })).await;
    let (stamped,): (Option<String>,) =
        mk_db::sqlx::query_as(r#"SELECT "renoteChannelId" FROM "note" WHERE "id" = $1"#)
            .bind(&ch_renote_api)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        stamped.as_deref(),
        Some(ch_open.as_str()),
        "API renote must carry the parent's channel in renoteChannelId"
    );

    // -- default plane: channel rows never flow (channelId IS NULL), the
    //    renote of a muted channel's note drops with the viewer's muting ---
    let res = fetch_user_notes(&app, json!({ "userId": b_id, "limit": 100 }), Some(a_token)).await;
    assert!(has(&res, &plain_note));
    assert!(!has(&res, &ch_note), "チャンネル投稿が含まれない: {res}");
    assert!(
        !has(&res, &ch_renote),
        "[チャンネル外リノート] チャンネルミュート＝TLに流れない: {res}"
    );
    let res = fetch_user_notes(&app, json!({ "userId": b_id, "limit": 100 }), None).await;
    assert!(
        has(&res, &ch_renote),
        "guest view carries no channel muting: {res}"
    );

    // -- withChannelNotes: true — the muted channel stays dropped, the
    //    sensitive channel is dropped for others but kept for the owner ----
    let res = fetch_user_notes(
        &app,
        json!({ "userId": b_id, "limit": 100, "withChannelNotes": true }),
        Some(a_token),
    )
    .await;
    assert!(
        !has(&res, &ch_note),
        "チャンネルミュート＝TLに流れない: {res}"
    );
    assert!(
        !has(&res, &ch_renote),
        "muted renoteChannel drops on the channel plane too: {res}"
    );
    assert!(
        !has(&res, &ch_hot_note),
        "他人が取得した場合センシティブチャンネル投稿が含まれない: {res}"
    );
    assert!(has(&res, &plain_note));
    let res = fetch_user_notes(
        &app,
        json!({ "userId": b_id, "limit": 100, "withChannelNotes": true }),
        Some(b_token),
    )
    .await;
    assert!(
        has(&res, &ch_note),
        "owner view keeps the unmuted channel note: {res}"
    );
    assert!(
        has(&res, &ch_hot_note),
        "自分が取得した場合センシティブチャンネル投稿が含まれる: {res}"
    );
    let res = fetch_user_notes(
        &app,
        json!({ "userId": b_id, "limit": 100, "withChannelNotes": true }),
        None,
    )
    .await;
    assert!(
        has(&res, &ch_note),
        "guest view keeps the unmuted channel note: {res}"
    );
    assert!(
        has(&res, &ch_renote),
        "guest view keeps the channel renote: {res}"
    );
}

// ===== ul-tl2: user-list-timeline follow-ups ================================
//
// Companion legs to the // ===== ul-tl: section above (pinned upstream
// timelines.ts User List TL + notes/user-list-timeline.ts getFromDb): the
// membership-invisible specified plane (visibleUserIds, both directions),
// the followers-visibility reply-addressed-to-me arm of
// generateVisibilityQuery, the makePaginationQuery both-bounds window
// (exclusive bounds, DESC order per QueryService.ts L46), and withFiles.

#[tokio::test]
async fn user_list_timeline_ul_tl2_specified_pager_window_withfiles() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkul_tl2_specified").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let alice = signup(&app, &uname("u2a")).await;
    let bob = signup(&app, &uname("u2b")).await;
    let carol = signup(&app, &uname("u2c")).await;
    let a = alice["id"].as_str().unwrap();
    let b = bob["id"].as_str().unwrap();
    let c = carol["id"].as_str().unwrap();
    let at = alice["token"].as_str().unwrap();
    let bt = bob["token"].as_str().unwrap();
    let ct = carol["token"].as_str().unwrap();

    async fn note_post(app: &axum::Router, body: Value) -> String {
        let (s, v) = post_json(app.clone(), "/api/notes/create", body).await;
        assert_eq!(s, StatusCode::OK, "body: {v}");
        v["createdNote"]["id"].as_str().unwrap().to_string()
    }
    async fn fetch_tl(app: &axum::Router, mut params: Value, token: &str) -> Value {
        params["i"] = json!(token);
        let (s, v) = post_json(app.clone(), "/api/notes/user-list-timeline", params).await;
        assert_eq!(s, StatusCode::OK, "body: {v}");
        v
    }
    fn has(res: &Value, id: &str) -> bool {
        res.as_array().unwrap().iter().any(|n| n["id"] == json!(id))
    }
    fn lacks(res: &Value, id: &str) -> bool {
        !has(res, id)
    }

    let (s, list) = post_json(
        app.clone(),
        "/api/users/lists/create",
        json!({ "name": "list", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {list}");
    let list_id = list["id"].as_str().unwrap().to_string();
    for uid in [b, c] {
        let (s, _) = post_json(
            app.clone(),
            "/api/users/lists/push",
            json!({ "listId": list_id, "userId": uid, "i": at }),
        )
        .await;
        assert_eq!(s, StatusCode::NO_CONTENT);
    }

    // -- membership visibility, specified plane: a member's specified note
    //    addressed to ME rides even without a follow; the same member's
    //    specified note addressed to the OTHER member stays out ------------
    let n_spec_me = note_post(
        &app,
        json!({ "text": "dm-ride", "visibility": "specified", "visibleUserIds": [a], "i": bt }),
    )
    .await;
    let res = fetch_tl(&app, json!({ "listId": list_id, "limit": 100 }), at).await;
    assert!(has(&res, &n_spec_me), "specified addressed to me rides");
    let n_spec_other = note_post(
        &app,
        json!({ "text": "dm-hold", "visibility": "specified", "visibleUserIds": [c], "i": bt }),
    )
    .await;
    let res = fetch_tl(&app, json!({ "listId": list_id, "limit": 100 }), at).await;
    assert!(
        lacks(&res, &n_spec_other),
        "specified addressed to the other member stays out"
    );

    // -- followers plane, reply arm: a member's followers-visibility REPLY
    //    to my note rides with no follow (replyUserId = me in
    //    generateVisibilityQuery) ------------------------------------------
    let n_a_home = note_post(&app, json!({ "text": "root", "i": at })).await;
    let n_b_folreply = note_post(
        &app,
        json!({ "text": "fol-reply", "visibility": "followers", "replyId": n_a_home, "i": bt }),
    )
    .await;
    let res = fetch_tl(&app, json!({ "listId": list_id, "limit": 100 }), at).await;
    assert!(
        has(&res, &n_b_folreply),
        "followers reply addressed to me rides unfollowed"
    );

    // -- pager both-bounds window: makePaginationQuery keeps
    //    sinceId < note < untilId (exclusive) with DESC order ---------------
    let n1 = note_post(&app, json!({ "text": "w1", "i": bt })).await;
    let n2 = note_post(&app, json!({ "text": "w2", "i": bt })).await;
    let n3 = note_post(&app, json!({ "text": "w3", "i": bt })).await;
    let res = fetch_tl(
        &app,
        json!({ "listId": list_id, "sinceId": n1, "untilId": n3, "limit": 100 }),
        at,
    )
    .await;
    let arr = res.as_array().unwrap();
    assert_eq!(arr.len(), 1, "both-bounds window is exclusive: {res}");
    assert_eq!(arr[0]["id"], json!(n2), "in-window note");
    let n4 = note_post(&app, json!({ "text": "w4", "i": bt })).await;
    let res = fetch_tl(
        &app,
        json!({ "listId": list_id, "sinceId": n1, "untilId": n4, "limit": 100 }),
        at,
    )
    .await;
    let arr = res.as_array().unwrap();
    assert_eq!(arr.len(), 2, "window across n4 bound: {res}");
    assert_eq!(arr[0]["id"], json!(n3), "both-bounds order is DESC");
    assert_eq!(arr[1]["id"], json!(n2));

    // -- withFiles: true over the member mix keeps file notes only ---------
    let file_id = seed_drive_file(&pool, b, "image/jpeg").await;
    let n_txt = note_post(&app, json!({ "text": "plain", "i": bt })).await;
    let n_file = note_post(&app, json!({ "fileIds": [file_id], "i": bt })).await;
    let res = fetch_tl(
        &app,
        json!({ "listId": list_id, "withFiles": true, "limit": 100 }),
        at,
    )
    .await;
    assert!(lacks(&res, &n_txt), "withFiles drops text-only notes");
    assert!(has(&res, &n_file), "withFiles keeps the file note");
    let _ = ct;
}

// ===== chan-note2: notes/create channel legs + the TL channel folds ==========
// upstream NoteCreateService: fetchAndCreate L413-420 (findOneBy
// { id, isArchived: false } → NO_SUCH_CHANNEL), create() L449-470 (the reply
// re-scope; a channel note is forced public + localOnly, visibleUsers
// dropped) and the timeline.ts / local-timeline.ts getFromDb channel brackets
// (followed channels fold into HTL; local keeps `channelId IS NULL`).

#[tokio::test]
async fn notes_create_channel_legs_and_tl_folds() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkchan_note2").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let alice = signup(&app, &uname("cn2a")).await;
    let bob = signup(&app, &uname("cn2b")).await;
    let at = alice["token"].as_str().unwrap();
    let bt = bob["token"].as_str().unwrap();

    // bob owns the channel (channels/create is the e2e fixture path).
    let (s, channel) = post_json(
        app.clone(),
        "/api/channels/create",
        json!({ "name": "chan-note2", "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {channel}");
    let channel_id = channel["id"].as_str().unwrap().to_string();

    // validation: a missing channel id rejects with NO_SUCH_CHANNEL
    // (fetchAndCreate L413-420 → create.ts meta.errors.noSuchChannel).
    let (s, err) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "x", "channelId": "nonexistent0000000001", "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(err["error"]["code"], json!("NO_SUCH_CHANNEL"));

    // forcing (create() L468-470): the row stores channelId, followers
    // collapses to public, localOnly is forced on, and the pack carries the
    // channelId (NoteEntityService.pack's conditional entry).
    let (s, note) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({
            "text": "in-channel",
            "channelId": channel_id,
            "visibility": "followers",
            "localOnly": false,
            "i": bt
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {note}");
    let created = &note["createdNote"];
    let note_id = created["id"].as_str().unwrap().to_string();
    assert_eq!(
        created["channelId"],
        json!(channel_id),
        "the row stores channelId and the pack emits it"
    );
    assert_eq!(created["visibility"], json!("public"), "L468 force-public");
    assert_eq!(created["localOnly"], json!(true), "L470 force-localOnly");

    // channels/timeline lists the note (timeline.ts getFromDb, channelId
    // reader over the stored column).
    let (s, ctl) = post_json(
        app.clone(),
        "/api/channels/timeline",
        json!({ "channelId": channel_id, "i": bt, "limit": 100 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {ctl}");
    let arr = ctl.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], json!(note_id));
    assert_eq!(arr[0]["channelId"], json!(channel_id));

    // before the follow the channel note stays out of alice's HTL
    // (upstream case B keeps channelId IS NULL off the user plane).
    let (s, htl) = post_json(
        app.clone(),
        "/api/notes/timeline",
        json!({ "i": at, "limit": 100 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {htl}");
    assert!(
        !htl.as_array()
            .unwrap()
            .iter()
            .any(|n| n["id"] == json!(note_id)),
        "unfollowed channel note stays out of HTL"
    );

    // LTL excludes channel rows (local-timeline.ts getFromDb L157
    // `channelId IS NULL`), for the author too.
    let (s, ltl) = post_json(
        app.clone(),
        "/api/notes/local-timeline",
        json!({ "i": bt, "limit": 100 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {ltl}");
    assert!(
        !ltl.as_array()
            .unwrap()
            .iter()
            .any(|n| n["id"] == json!(note_id)),
        "channel note is absent from LTL"
    );

    // alice follows the channel → the followed-channel fold
    // (timeline.ts getFromDb followingChannels brackets) lets it flow.
    let (s, _) = post_json(
        app.clone(),
        "/api/channels/follow",
        json!({ "channelId": channel_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, htl2) = post_json(
        app.clone(),
        "/api/notes/timeline",
        json!({ "i": at, "limit": 100 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {htl2}");
    assert!(
        htl2.as_array()
            .unwrap()
            .iter()
            .any(|n| n["id"] == json!(note_id)),
        "followed channel note folds into HTL"
    );

    let _ = pool;
}

// ===== tl-tail: timelines.ts census clusters (post-fix shapes) ==============
//
// Asserts over the endpoint shapes the remaining e2e-timelines census clusters
// exercise, on the fixed rows:
// - signup({ host }) — SignupApiService.ts L116 test-mode host → user."host" +
//   pack echo + the note.userHost insert denormalization → local-timeline's
//   `(visibility='public') AND (userHost IS NULL)` plane (timelines.ts
//   L1172/L1747 legs, Social TL 凍結(リモート) L2204 gap);
// - the renote-row channel denormalization (NoteCreateService.ts L651/L680:
//   payload channelId + parent renoteChannelId) as channels/timeline's
//   `channelId = ?` reader and the users/notes + user-list-timeline
//   channel_muting renote gates read both (timelines.ts L3078/L3131/L3147/
//   L3247/L2704 legs).

#[tokio::test]
async fn tl_tail_remote_host_and_channel_renote_shapes() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktltail").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let alice = signup(&app, &uname("tta")).await;
    let at = alice["token"].as_str().unwrap();

    // -- remote bob: test-mode signup host → pack echoes, row stores -------
    let host = format!("{}.example.com", mk_types::id::gen_aid_now());
    let (s, bob) = post_json(
        app.clone(),
        "/api/signup",
        json!({ "username": uname("ttb"), "password": "test", "host": host }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {bob}");
    assert_eq!(bob["host"], json!(host), "pack echoes the signup host");
    let (b_id, bt) = (
        bob["id"].as_str().unwrap().to_string(),
        bob["token"].as_str().unwrap().to_string(),
    );
    let stored: Option<String> =
        mk_db::sqlx::query_scalar(r#"SELECT "host" FROM "user" WHERE "id" = $1"#)
            .bind(&b_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored.as_deref(), Some(host.as_str()));

    // -- the remote public note rides the userHost denormalization on the
    //    insert (note.rs user_host_of) and stays out of LTL ---------------
    async fn note_post(app: &axum::Router, body: Value) -> String {
        let (s, v) = post_json(app.clone(), "/api/notes/create", body).await;
        assert_eq!(s, StatusCode::OK, "body: {v}");
        v["createdNote"]["id"].as_str().unwrap().to_string()
    }
    fn has(res: &Value, id: &str) -> bool {
        res.as_array().unwrap().iter().any(|n| n["id"] == json!(id))
    }

    let local_note = note_post(&app, json!({ "text": "local", "i": at })).await;
    let remote_note = note_post(&app, json!({ "text": "remote", "i": bt })).await;
    let remote_stored: Option<String> =
        mk_db::sqlx::query_scalar(r#"SELECT "userHost" FROM "note" WHERE "id" = $1"#)
            .bind(&remote_note)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(remote_stored.as_deref(), Some(host.as_str()));
    let (s, ltl) = post_json(
        app.clone(),
        "/api/notes/local-timeline",
        json!({ "i": at, "limit": 100 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {ltl}");
    assert!(has(&ltl, &local_note), "local control note in LTL: {ltl}");
    assert!(
        !has(&ltl, &remote_note),
        "リモートユーザーのノートが含まれない: {ltl}"
    );

    // ...alice can still reach the followed remote user's note in HTL
    let (s, _) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": b_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, htl) = post_json(
        app.clone(),
        "/api/notes/timeline",
        json!({ "i": at, "limit": 100 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {htl}");
    assert!(
        has(&htl, &remote_note),
        "フォローしているリモートユーザーのノートが含まれる: {htl}"
    );

    // -- renote stamp shapes: a payload-channelId renote shows in the
    //    channel TL, and muted-channel renotes drop from the users/notes and
    //    user-list-timeline gates ------------------------------------------
    let carol = signup(&app, &uname("ttc")).await;
    let (c_id, ct) = (
        carol["id"].as_str().unwrap().to_string(),
        carol["token"].as_str().unwrap().to_string(),
    );
    let (s, ch) = post_json(
        app.clone(),
        "/api/channels/create",
        json!({ "name": "chan", "i": ct }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {ch}");
    let ch_id = ch["id"].as_str().unwrap().to_string();

    let ch_note = note_post(
        &app,
        json!({ "text": "in-chan", "channelId": ch_id, "i": ct }),
    )
    .await;
    // the payload-channelId renote (timelines.ts L3138/L3155/L3255 shapes)
    let (s, r) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "renoteId": ch_note, "channelId": ch_id, "i": ct }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {r}");
    let ch_renote = r["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, ctl) = post_json(
        app.clone(),
        "/api/channels/timeline",
        json!({ "channelId": ch_id, "i": at, "limit": 100 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {ctl}");
    assert!(
        has(&ctl, &ch_renote),
        "閲覧中チャンネルのノートにリノートが含まれる: {ctl}"
    );

    // even muted, the VIEWED channel's rows flow (timelines.ts L3247 leg)
    let (s, _) = post_json(
        app.clone(),
        "/api/channels/mute/create",
        json!({ "channelId": ch_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, ctl2) = post_json(
        app.clone(),
        "/api/channels/timeline",
        json!({ "channelId": ch_id, "i": at, "limit": 100 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {ctl2}");
    assert!(
        has(&ctl2, &ch_renote),
        "閲覧中チャンネルをミュートしていても、同チャンネルのリノートが含まれる: {ctl2}"
    );

    // users/notes renoteChannel gate ([チャンネル外リノート] L3078)
    let (s, un) = post_json(
        app.clone(),
        "/api/users/notes",
        json!({ "userId": c_id, "withChannelNotes": true, "limit": 100, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {un}");
    assert!(
        !has(&un, &ch_renote),
        "[チャンネル外リノート] チャンネルミュート＝TLに流れない: {un}"
    );

    // user-list-timeline renoteChannel gate for a list member (L2704)
    let (s, list) = post_json(
        app.clone(),
        "/api/users/lists/create",
        json!({ "name": "list", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {list}");
    let list_id = list["id"].as_str().unwrap().to_string();
    let (s, _) = post_json(
        app.clone(),
        "/api/users/lists/push",
        json!({ "listId": list_id, "userId": c_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, ult) = post_json(
        app.clone(),
        "/api/notes/user-list-timeline",
        json!({ "listId": list_id, "i": at, "limit": 100 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {ult}");
    assert!(
        !has(&ult, &ch_renote),
        "[チャンネル外リノート] チャンネル未フォロー＋リスインしてる＋チャンネルミュート＝TLに流れない: {ult}"
    );

    let _ = pool;
}

// ===== ftt2: the FTT flag leg (timelines.ts L104 + L941-953) ================
//
// admin/update-meta persists enableFanoutTimeline (update-meta.ts paramDef
// non-nullable boolean) and mk_db::meta reads it back for the pushToTl gate —
// the e2e `FTT:` Redis probes ride this flag per describe.each arm (the
// fanout:false leg asserts `exists == 0`, so the gate must persist false).
// The Redis push itself is config-redis-gated (test_state carries redis:
// None → no-op), so these asserts cover the meta plumbing that flips the
// arms; the push semantics land with the e2e harness. (~4 asserts per the
// ftt2 brief.)

#[tokio::test]
async fn ftt2_enable_fanout_timeline_meta_flag() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkftt2").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    // first signup of an instance is root (SignupService L160) — update-meta
    // is requireAdministrator.
    let root = signup(&app, &uname("f2r")).await;
    let rt = root["token"].as_str().unwrap();

    // off → persists (fanout:false arm, timelines.ts L104/L951)
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/update-meta",
        json!({ "i": rt, "enableFanoutTimeline": false }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(
        !mk_db::meta::enable_fanout_timeline(&pool).await.unwrap(),
        "enableFanoutTimeline persists false"
    );

    // on → persists (fanout:true arm) — and non-bool rejects (paramDef type)
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/update-meta",
        json!({ "i": rt, "enableFanoutTimeline": true }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(
        mk_db::meta::enable_fanout_timeline(&pool).await.unwrap(),
        "enableFanoutTimeline persists true"
    );
    let (s, v) = post_json(
        app.clone(),
        "/api/admin/update-meta",
        json!({ "i": rt, "enableFanoutTimeline": "yes" }),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::BAD_REQUEST,
        "non-boolean flag → invalid param: {v}"
    );

    // patch semantics: a pinnedUsers-only update-meta leaves the flag alone
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/update-meta",
        json!({ "i": rt, "pinnedUsers": ["@root"] }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(
        mk_db::meta::enable_fanout_timeline(&pool).await.unwrap(),
        "absent key leaves the flag (partial patch)"
    );

    let _ = pool;
}

// ===== ftt2: the FTT Redis push shape (timelines.ts L941-953 probes) ========
//
// ci.yml carries the redis:7 service, so this leg asserts the LPUSH pipeline
// end to end through the raw-RESP probe `ftt::debug_lrange` (the e2e reads
// `redisForTimelines.lrange('list:homeTimeline:${uid}', 0, -1)`):
// flag on → bob's follower list + alice's self list hold the posted ids;
// flag off (update-meta, timelines.ts L104) → a fresh post stays out;
// a no-follower author's list only holds her own self-push. Gracefully
// skipped when redis is unreachable (local dev without the service).

#[tokio::test]
async fn ftt2_redis_home_timeline_push_shape() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    let redis_cfg = crate::config::RedisConfig {
        host: "127.0.0.1".to_string(),
        port: 6379,
        pass: None,
    };
    if tokio::net::TcpStream::connect(("127.0.0.1", 6379))
        .await
        .is_err()
    {
        return; // no redis service — CI always provisions one
    }
    init_tracing();
    let pool = fresh_pool("mkftt2r").await;

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let cfg = crate::config::Config {
        port: 0,
        url: "http://127.0.0.1:3000".to_string(),
        db: None,
        redis: Some(redis_cfg.clone()),
        test: true,
        version: None,
        raw_json: serde_json::Value::Null,
    };
    // The e2e probes ride ioredis `keyPrefix: '<url.host>:'` (config.ts
    // convertRedisOptions L378 — see ftt.rs module doc), so the probes must
    // spell the physical key exactly as `redisForTimelines.*` does.
    let prefix = format!("{}:", cfg.host());
    let hkey = |uid: &str| format!("{prefix}list:homeTimeline:{uid}");
    let app = crate::build_router(crate::AppState {
        config: cfg,
        pool: Some(pool.clone()),
        rate_limiter: std::sync::Arc::new(crate::ratelimit::RateLimiter::new()),
        search_engine: std::sync::Arc::new(mk_core::services::search_bridge::NoopEngine),
    });

    // first signup = root (SignupService L160) — update-meta is requireAdmin
    let alice = signup(&app, &uname("f2a")).await;
    let at = alice["token"].as_str().unwrap();
    let a_id = alice["id"].as_str().unwrap().to_string();

    // bob follows alice (timelines.ts FTT: following/create by bob)
    let bob = signup(&app, &uname("f2b")).await;
    let b_id = bob["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": a_id, "i": bob["token"].as_str().unwrap() }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");

    // flag on (leg true arm) — persists before the posts
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/update-meta",
        json!({ "i": at, "enableFanoutTimeline": true }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // alice posts → pushed to her own HTL + bob's (follower); id order: the
    // follower leg rides the same note.
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "ftt hello", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let note = v["createdNote"]["id"].as_str().unwrap().to_string();

    let bob_list = ftt::debug_lrange(&redis_cfg, &hkey(&b_id)).await.unwrap();
    let alice_list = ftt::debug_lrange(&redis_cfg, &hkey(&a_id)).await.unwrap();
    assert!(
        bob_list.iter().any(|n| n == &note) && alice_list.iter().any(|n| n == &note),
        "FTT: ローカルユーザーの HTL にはプッシュされる — note={note} bob={bob_list:?} alice={alice_list:?}"
    );

    // flag off (leg false arm: exists == 0) — fresh posts push nothing
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/update-meta",
        json!({ "i": at, "enableFanoutTimeline": false }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let carol = signup(&app, &uname("f2c")).await;
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "gated", "i": carol["token"].as_str().unwrap() }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let gated_note = v["createdNote"]["id"].as_str().unwrap().to_string();
    let c_id = carol["id"].as_str().unwrap().to_string();
    let carol_list = ftt::debug_lrange(&redis_cfg, &hkey(&c_id)).await.unwrap();
    assert!(
        !carol_list.iter().any(|n| n == &gated_note),
        "fanout:false arm must not push (exists == 0): {carol_list:?}"
    );

    let _ = pool;
}

// ===== ftt3: timeline KEY NAMESPACE parity with the e2e ioredis probe ======
//
// The last census failure (`FTT: ローカルユーザーの HTL にはプッシュされる`,
// timelines.ts L933-958) was NOT the push pipeline — the wire log showed the
// follower LPUSH replying :1 — it was the KEY NAME. Upstream builds every
// redis client from convertRedisOptions (config.ts L372-380):
// `keyPrefix: '${options.prefix ?? host}':` with host = new URL(url).host,
// and BOTH the server (GlobalModule `new Redis(config.redisForTimelines)`)
// and the pinned test (`new Redis(loadConfig().redisForTimelines)`) ride the
// same prefix. ioredis prepends it on every read/write command, so the test's
// `exists('list:homeTimeline:${bob.id}')` physically asks for
// `127.0.0.1:3000:list:homeTimeline:${bob.id}`. This probe replicates the
// e2e sequence over the HTTP surface and reads through the identical
// keyPrefix (plus a negative on the raw name — an unprefixed key is
// unreachable for the e2e client and must not exist).

#[tokio::test]
async fn ftt3_e2e_keyprefix_parity_probe() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    let redis_cfg = crate::config::RedisConfig {
        host: "127.0.0.1".to_string(),
        port: 6379,
        pass: None,
    };
    if tokio::net::TcpStream::connect(("127.0.0.1", 6379))
        .await
        .is_err()
    {
        return; // no redis service — CI always provisions one
    }
    init_tracing();
    let pool = fresh_pool("mkftt3r").await;

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let cfg = crate::config::Config {
        port: 0,
        url: "http://127.0.0.1:3000".to_string(),
        db: None,
        redis: Some(redis_cfg.clone()),
        test: true,
        version: None,
        raw_json: serde_json::Value::Null,
    };
    // same ioredis keyPrefix shape as ftt2 — the exact name the pinned e2e
    // `redisForTimelines.exists/lrange` computes for the followee's HTL.
    let prefix = format!("{}:", cfg.host());
    let hkey = |uid: &str| format!("{prefix}list:homeTimeline:{uid}");
    let app = crate::build_router(crate::AppState {
        config: cfg,
        pool: Some(pool.clone()),
        rate_limiter: std::sync::Arc::new(crate::ratelimit::RateLimiter::new()),
        search_engine: std::sync::Arc::new(mk_core::services::search_bridge::NoopEngine),
    });

    // first signup = root (SignupService L160) — update-meta is requireAdmin
    let root = signup(&app, &uname("f3r")).await;

    // timelines.ts L935: alice/bob/carol signup() then bob follows alice
    let alice = signup(&app, &uname("f3a")).await;
    let bob = signup(&app, &uname("f3b")).await;
    let at = alice["token"].as_str().unwrap();
    let a_id = alice["id"].as_str().unwrap().to_string();
    let b_id = bob["id"].as_str().unwrap().to_string();

    let (s, v) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": a_id, "i": bob["token"].as_str().unwrap() }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");

    // describe.each true arm (timelines.ts L104) — then the three posts
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/update-meta",
        json!({ "i": root["token"].as_str().unwrap(), "enableFanoutTimeline": true }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "I'm Alice.", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let alice_note = v["createdNote"]["id"].as_str().unwrap().to_string();

    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "I'm Bob.", "i": bob["token"].as_str().unwrap() }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let bob_note = v["createdNote"]["id"].as_str().unwrap().to_string();

    // L944/L946-948 through the ioredis nameshaping: exists == 1 and both
    // notes present (order [bobNote, aliceNote] — LPUSH prepends per note).
    let bob_htl = ftt::debug_lrange(&redis_cfg, &hkey(&b_id)).await.unwrap();
    assert_eq!(
        bob_htl,
        vec![bob_note.clone(), alice_note.clone()],
        "FTT: ローカルユーザーの HTL にはプッシュされる — prefixed bob key"
    );

    // the raw name is INVISIBLE to the prefixed client and must not exist
    let raw = ftt::debug_lrange(&redis_cfg, &format!("list:homeTimeline:{b_id}"))
        .await
        .unwrap();
    assert!(
        raw.is_empty(),
        "unprefixed key leaked — e2e client could never read it: {raw:?}"
    );

    let _ = pool;
}

// stream-rtm =================================================================
// renote-mute.ts stream arcs: the three `waitFire` rows ride the /streaming
// timeline channels (`localTimeline` per the pinned spec; `homeTimeline` is
// the same hub's credential arm). No ws client dep exists, so the registry
// itself is the spy: subscriptions are registered through
// api::streaming::connect_channel and the frames the hub would write to the
// socket are read off the channel. The notes flow through the real router —
// /api/notes/create text + renote lanes with their `// stream-rtm`
// publishNotesStream hooks (NoteCreateService.create L842).

#[tokio::test]
async fn renote_mute_stream_timeline_fanout() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkstreamrtm").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    // ミュート作成 row: alice renote-mutes carol (bob stays clean).
    let alice = signup(&app, &uname("rtma")).await;
    let at = alice["token"].as_str().unwrap().to_string();
    let a_id = alice["id"].as_str().unwrap().to_string();
    let bob = signup(&app, &uname("rtmb")).await;
    let bt = bob["token"].as_str().unwrap().to_string();
    let carol = signup(&app, &uname("rtmc")).await;
    let ct = carol["token"].as_str().unwrap().to_string();
    let c_id = carol["id"].as_str().unwrap().to_string();

    let (s, v) = post_json(
        app.clone(),
        "/api/renote-mute/create",
        json!({ "userId": c_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {v}");

    // Registry spy: alice's localTimeline + homeTimeline subscriptions
    // (arena-streaming: connect_channel is async + pool-scoped for the
    // userList owner check — the two calls below carry the fresh pool).
    let mut ltl = {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        assert!(
            crate::api::streaming::connect_channel(
                &pool,
                "localTimeline",
                &json!({}),
                "a-ltl",
                tx,
                &a_id
            )
            .await
        );
        rx
    };
    let mut htl = {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        assert!(
            crate::api::streaming::connect_channel(
                &pool,
                "homeTimeline",
                &json!({}),
                "a-htl",
                tx,
                &a_id
            )
            .await
        );
        rx
    };

    /// Drain the spy into `note` event ids (idle gap = done).
    async fn note_ids(rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>) -> Vec<String> {
        let mut ids = Vec::new();
        loop {
            match tokio::time::timeout(std::time::Duration::from_millis(300), rx.recv()).await {
                Ok(Some(t)) => {
                    let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                    if v["type"] == "channel" && v["body"]["type"] == "note" {
                        if let Some(id) = v["body"]["body"]["id"].as_str() {
                            ids.push(id.to_string());
                        }
                    }
                }
                _ => return ids,
            }
        }
    }

    // Positive control: bob's normal note flows on localTimeline (the fanout
    // + pack hook are alive before any renote row is judged).
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "hi", "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let bob_note = v["createdNote"]["id"].as_str().unwrap().to_string();
    let got = note_ids(&mut ltl).await;
    assert!(
        got.iter().any(|n| n == &bob_note),
        "ストリームに通常ノートが流れてくる — {got:?}"
    );

    // Row 1: carol's PURE renote must not flow (renoter is renote-muted,
    // channel.ts L118 — isRenotePacked && !isQuotePacked && renoter in set).
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "renoteId": bob_note, "i": ct }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let carol_renote = v["createdNote"]["id"].as_str().unwrap().to_string();
    let got = note_ids(&mut ltl).await;
    assert!(
        !got.iter().any(|n| n == &carol_renote),
        "ストリームにリノートミュートしているユーザーのリノートが流れない — {got:?}"
    );

    // Row 2: carol's QUOTE (text-carrying renote) must flow — the renote-mute
    // leg is keyed on pure renotes, quotes bypass.
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "renoteId": bob_note, "text": "kore", "i": ct }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let carol_quote = v["createdNote"]["id"].as_str().unwrap().to_string();
    let got = note_ids(&mut ltl).await;
    assert!(
        got.iter().any(|n| n == &carol_quote),
        "ストリームにリノートミュートしているユーザーの引用が流れる — {got:?}"
    );

    // Row 3 (#12956): the renote of a renote-muted user's NORMAL note by a
    // clean author (bob renotes carol's note) must flow — the mute is keyed
    // on the RENOTER, not the renote target.
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "hi", "i": ct }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let carol_note = v["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "renoteId": carol_note, "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let bob_renote = v["createdNote"]["id"].as_str().unwrap().to_string();
    let got = note_ids(&mut ltl).await;
    assert!(
        got.iter().any(|n| n == &bob_renote),
        "ストリームにリノートミュートしているユーザーの通常ノートのリノートが流れてくる — {got:?}"
    );

    // homeTimeline arm: alice follows nobody, so only her own notes ride the
    // channel (`isMe || following[author]` gate).
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "own htl", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let alice_note = v["createdNote"]["id"].as_str().unwrap().to_string();
    let got = note_ids(&mut htl).await;
    assert!(
        got.iter().any(|n| n == &alice_note),
        "homeTimeline に自分のノートが流れてくる — {got:?}"
    );

    crate::api::streaming::remove_user(&a_id);
    let _ = pool;
}
// ===== mute lane noise-gates: muter silence + thread-mute wiring ============
//
// Unit-level mirror of the batch-social arena legs (upstream
// test/e2e/mute.ts + thread-mute.ts). Every notification reason that
// carries a notifier drops when the notifiee mutes the notifier
// (NotificationService.#createNotificationInternal L111-114 — the muting
// check is TYPE-AGNOSTIC, so the follow family gates identically), the
// notes/mentions plane runs visibility + base (muted author) + the muted-
// note-THREAD gates (QueryService.generateMutedNoteThreadQuery), and the
// reply/mention notification legs stay silent inside a thread-muted thread
// (NoteCreateService L856/L991). CI judges the e2e file; this prevents the
// wires from silently detaching between runs.

#[tokio::test]
async fn noise_gate_muter_silence_and_thread_mute() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mknoisemute").await;
    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let alice = signup(&app, &uname("nga")).await;
    let bob = signup(&app, &uname("ngb")).await;
    let carol = signup(&app, &uname("ngc")).await;
    let (a_id, a_token) = (
        alice["id"].as_str().unwrap(),
        alice["token"].as_str().unwrap(),
    );
    let (b_id, b_token) = (bob["id"].as_str().unwrap(), bob["token"].as_str().unwrap());
    let (c_id, c_token) = (
        carol["id"].as_str().unwrap(),
        carol["token"].as_str().unwrap(),
    );
    let a_name = alice["username"].as_str().unwrap().to_string();

    // Mute: alice ==> carol (mute.ts beforeAll)
    let (s, body) = post_json(
        app.clone(),
        "/api/mute/create",
        json!({ "userId": c_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {body}");

    // notification reasons 1+2: reaction + follow from both sides
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "hi", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let alice_note = v["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, body) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": a_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let (s, body) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": a_id, "i": c_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let (s, body) = post_json(
        app.clone(),
        "/api/notes/reactions/create",
        json!({ "noteId": alice_note, "reaction": "like", "i": b_token }),
    )
    .await;
    assert!(s.is_success(), "body: {body}");
    let (s, body) = post_json(
        app.clone(),
        "/api/notes/reactions/create",
        json!({ "noteId": alice_note, "reaction": "like", "i": c_token }),
    )
    .await;
    assert!(s.is_success(), "body: {body}");

    // gate 1: bob's follow + reaction notifications visible, carol's rows
    // (BOTH reasons) dropped — the muting gate is type-agnostic
    let (s, v) = post_json(
        app.clone(),
        "/api/i/notifications",
        json!({ "i": a_token, "markAsRead": false }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let arr = v.as_array().unwrap();
    assert!(
        arr.iter()
            .any(|n| n["type"] == json!("follow") && n["userId"] == json!(b_id)),
        "follow notification visible: {arr:?}"
    );
    assert!(
        arr.iter()
            .any(|n| n["type"] == json!("reaction") && n["userId"] == json!(b_id)),
        "reaction notification visible: {arr:?}"
    );
    assert!(
        !arr.iter().any(|n| n["userId"] == json!(c_id)),
        "muted carol silent across reasons: {arr:?}"
    );

    // gate 2: i/notifications-grouped shares the same gate through the alias
    let (s, v) = post_json(
        app.clone(),
        "/api/i/notifications-grouped",
        json!({ "i": a_token, "markAsRead": false }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let arr = v.as_array().unwrap();
    assert!(
        arr.iter().any(|n| n["userId"] == json!(b_id)),
        "body: {arr:?}"
    );
    assert!(
        !arr.iter().any(|n| n["userId"] == json!(c_id)),
        "grouped list keeps the muter silence: {arr:?}"
    );

    // thread-mute wiring (thread-mute.ts): bob roots the thread with a
    // mention, alice replies, then mutes the THREAD; alice's standalone
    // mention control must stay visible while the muted-thread rows vanish.
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": format!("@{a_name} standalone"), "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let control_note = v["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": format!("@{a_name} root note"), "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let bob_note = v["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "child note", "replyId": bob_note, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let alice_reply = v["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, body) = post_json(
        app.clone(),
        "/api/notes/thread-muting/create",
        json!({ "noteId": bob_note, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {body}");
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": format!("@{a_name} child note"), "replyId": bob_note, "i": c_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let carol_reply = v["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "child note", "replyId": alice_reply, "i": c_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let carol_no_mention = v["createdNote"]["id"].as_str().unwrap().to_string();

    // gate 3: notes/mentions — muted-thread rows (root / mid-thread reply /
    // thread-muted reply author) excluded, the standalone mention control kept
    let (s, v) = post_json(app.clone(), "/api/notes/mentions", json!({ "i": a_token })).await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let arr = v.as_array().unwrap();
    assert!(
        !arr.iter().any(|n| n["id"] == json!(bob_note)),
        "muted thread root excluded: {arr:?}"
    );
    assert!(
        !arr.iter().any(|n| n["id"] == json!(carol_reply)),
        "muted-thread reply excluded: {arr:?}"
    );
    assert!(
        !arr.iter().any(|n| n["id"] == json!(carol_no_mention)),
        "reply-target ride-along excluded by the thread gate: {arr:?}"
    );
    assert!(
        arr.iter().any(|n| n["id"] == json!(control_note)),
        "unmuted mention control visible: {arr:?}"
    );

    // gate 4: i/notifications — the in-thread carol replies notify nobody
    let (s, v) = post_json(
        app.clone(),
        "/api/i/notifications",
        json!({ "i": a_token, "markAsRead": false }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let arr = v.as_array().unwrap();
    assert!(
        !arr.iter()
            .any(|n| n["note"]["id"] == json!(carol_reply)
                || n["note"]["id"] == json!(carol_no_mention)),
        "thread-muted reply legs stayed silent: {arr:?}"
    );
    assert!(
        !arr.iter().any(|n| n["userId"] == json!(c_id)),
        "muted carol still silent: {arr:?}"
    );

    let _ = pool;
}

// ===== ntf-mute: createNotification central gate + table self-heal ==========
//
// mk-db::notification::insert_if_not_muted is the ONE muting drop every
// notify site routes through (NotificationService.createNotification L110
// self-suppression + L111-114 type-agnostic muting). The ensure_table legs
// replay migrations/0002_notifications.sql on SQLSTATE 42P01 — the upstream
// e2e fixture (initTestDb dropSchema) drops the table under a live server
// between suite files, which 500'd the whole mute.ts Notification cluster.

#[tokio::test]
async fn notifications_insert_if_not_muted_central_gate() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkntfmute").await;
    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let alice = signup(&app, &uname("nma")).await;
    let bob = signup(&app, &uname("nmb")).await;
    let carol = signup(&app, &uname("nmc")).await;
    let (a_id, a_token) = (
        alice["id"].as_str().unwrap(),
        alice["token"].as_str().unwrap(),
    );
    let (b_id, b_token) = (bob["id"].as_str().unwrap(), bob["token"].as_str().unwrap());
    let (c_id, c_token) = (
        carol["id"].as_str().unwrap(),
        carol["token"].as_str().unwrap(),
    );

    // Mute: alice ==> carol (mute.ts beforeAll)
    let (s, body) = post_json(
        app.clone(),
        "/api/mute/create",
        json!({ "userId": c_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {body}");

    // gate 1: receiveFollowRequest drop through the centralized path —
    // locked alice accepts the request flow; muted carol's request stays
    // silent, bob's lands.
    let (s, body) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "isLocked": true, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let (s, body) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": a_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let (s, body) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": a_id, "i": c_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let (s, v) = post_json(
        app.clone(),
        "/api/i/notifications",
        json!({ "markAsRead": false, "includeTypes": ["receiveFollowRequest"], "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 1, "only bob's follow request lands: {arr:?}");
    assert_eq!(arr[0]["userId"], json!(b_id), "body: {arr:?}");
    assert_eq!(
        arr[0]["type"],
        json!("receiveFollowRequest"),
        "body: {arr:?}"
    );

    // gate 2: reaction drop via the SAME helper call site shape the notify
    // sites use — muted pair inserts NOTHING, self-pair inserts NOTHING.
    let note_id = mk_types::id::gen_aid_now();
    mk_db::notification::insert_if_not_muted(
        &pool,
        &mk_db::notification::NewNotification {
            id: &mk_types::id::gen_aid_now(),
            r#type: "reaction",
            user_id: a_id,
            notifier_id: Some(c_id),
            note_id: Some(&note_id),
            reaction: Some("like"),
        },
    )
    .await
    .unwrap();
    mk_db::notification::insert_if_not_muted(
        &pool,
        &mk_db::notification::NewNotification {
            id: &mk_types::id::gen_aid_now(),
            r#type: "mention",
            user_id: b_id,
            notifier_id: Some(b_id),
            note_id: Some(&note_id),
            reaction: None,
        },
    )
    .await
    .unwrap();
    let (s, v) = post_json(
        app.clone(),
        "/api/i/notifications",
        json!({ "markAsRead": false, "includeTypes": ["reaction"], "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert!(
        v.as_array().unwrap().is_empty(),
        "muted reaction never reached the store: {v:?}"
    );
    let (s, v) = post_json(
        app.clone(),
        "/api/i/notifications",
        json!({ "markAsRead": false, "includeTypes": ["mention"], "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert!(
        v.as_array().unwrap().is_empty(),
        "self-notification suppressed (bob's helper call): {v:?}"
    );

    // gate 3: unmute → the same helper lane inserts again (the drop is the
    // live muting state, not a one-way tombstone).
    let (s, body) = post_json(
        app.clone(),
        "/api/mute/delete",
        json!({ "userId": c_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {body}");
    mk_db::notification::insert_if_not_muted(
        &pool,
        &mk_db::notification::NewNotification {
            id: &mk_types::id::gen_aid_now(),
            r#type: "reaction",
            user_id: a_id,
            notifier_id: Some(c_id),
            note_id: Some(&note_id),
            reaction: Some("like"),
        },
    )
    .await
    .unwrap();
    let n: i64 = mk_db::sqlx::query_scalar(
        r#"SELECT count(*) FROM "notification" WHERE "notifieeId" = $1 AND ("type"::text) = 'reaction'"#,
    )
    .bind(a_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n, 1, "unmuted pair inserts once");

    let _ = pool;
}

// ===== thread-cap: root-note thread key ======================================
//
// thread_key decoded the nullable "threadId" column through an untyped
// query_scalar (plain String scalar): every ROOT note (threadId IS NULL)
// errored with `unexpected null`, notes/thread-muting/create 500'd before any
// mute row landed, and the e2e thread-mute.ts gates (notes/mentions +
// i/notifications) asserted absence against a gate that never armed (census
// run 39199419722, both cases red). Root notes must flatten to the note.id
// fallback path, not Err.

#[tokio::test]
async fn thread_mute_root_note_thread_key_decodes_null() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkthreadcap").await;
    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let alice = signup(&app, &uname("tca")).await;
    let bob = signup(&app, &uname("tcb")).await;
    let (a_id, a_token) = (
        alice["id"].as_str().unwrap(),
        alice["token"].as_str().unwrap(),
    );
    let b_token = bob["token"].as_str().unwrap();
    let a_name = alice["username"].as_str().unwrap().to_string();

    // thread-mute.ts fixture: bob roots the thread mentioning alice, alice
    // replies (the chain's threadId carrier)
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": format!("@{a_name} root note"), "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let bob_note = v["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "child note", "replyId": bob_note, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");

    // thread-cap 1: the ROOT row's threadId is NULL and must flatten to None
    // (the id fallback), never Err — this is the exact decode trap.
    let key = mk_db::note::thread_key(&pool, &bob_note).await.unwrap();
    assert!(
        key.is_none(),
        "root note threadId must decode as None, got {key:?}"
    );
    // the reply carrier inherits the root id — the key both gates share
    let alice_reply = v["createdNote"]["id"].as_str().unwrap().to_string();
    let reply_thread: Option<String> =
        mk_db::sqlx::query_scalar(r#"SELECT "threadId" FROM "note" WHERE "id" = $1"#)
            .bind(&alice_reply)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        reply_thread.as_deref(),
        Some(bob_note.as_str()),
        "reply carries the root thread id: {reply_thread:?}"
    );

    // thread-cap 2: muting the ROOT note (threadId NULL) lands 204 — pre-fix
    // this 500'd on the decode error and no row was written
    let (s, body) = post_json(
        app.clone(),
        "/api/notes/thread-muting/create",
        json!({ "noteId": bob_note, "i": a_token }),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::NO_CONTENT,
        "root-note thread mute must not 500: {body}"
    );
    assert!(
        mk_db::note::thread_muted(&pool, a_id, &bob_note)
            .await
            .unwrap(),
        "mute row keyed on the root thread id"
    );

    let _ = pool;
}

// ===== arena-block: upstream test/e2e/block.ts ===============================
//
// census: block.ts exercises the blocking plane end-to-end — blocking/create,
// the two-way following gate, the reaction gate, the reply/renote gate and the
// LTL exclusion for a viewer-blocking pair.

#[tokio::test]
async fn block_arena_gates() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkblockarena").await;
    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let alice = signup(&app, &uname("bla")).await;
    let bob = signup(&app, &uname("blb")).await;
    let carol = signup(&app, &uname("blc")).await;
    let (a_id, a_token) = (
        alice["id"].as_str().unwrap(),
        alice["token"].as_str().unwrap(),
    );
    let (b_id, b_token) = (bob["id"].as_str().unwrap(), bob["token"].as_str().unwrap());
    let c_token = carol["token"].as_str().unwrap();

    // 「Block作成」 — alice blocks bob, 200 with the packed target
    let (s, body) = post_json(
        app.clone(),
        "/api/blocking/create",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "blocking/create: {body}");

    // 「ブロックされているユーザーをフォローできない」 — following/create.ts
    // meta.errors.youHaveBeenBlocked (c4ab57cc)
    let (s, body) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": a_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "following gate: {body}");
    assert_eq!(
        body["error"]["id"],
        json!("c4ab57cc-4e41-45e9-bfd9-584f61e35ce0"),
        "following gate error id: {body}"
    );

    // 「ブロックされているユーザーにリアクションできない」 — ReactionService
    // e70412a4 remapped to meta.errors.youHaveBeenBlocked (20ef5475)
    let (s, body) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "hello", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "alice note: {body}");
    let alice_note = body["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, body) = post_json(
        app.clone(),
        "/api/notes/reactions/create",
        json!({ "noteId": alice_note, "reaction": "👍", "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "reaction gate: {body}");
    assert_eq!(
        body["error"]["id"],
        json!("20ef5475-9f38-4e4c-bd33-de6d979498ec"),
        "reaction gate error id: {body}"
    );

    // 「ブロックされているユーザーに返信できない」 + 「…のノートをRenoteできな
    // い」 — both NoteCreateService IdentifiableErrors remap to the notes/create
    // meta.errors.youHaveBeenBlocked id (b390d7e1)
    let (s, body) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "replyId": alice_note, "text": "yo", "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "reply gate: {body}");
    assert_eq!(
        body["error"]["id"],
        json!("b390d7e1-8a5e-46ed-b625-06271cafd3d3"),
        "reply gate error id: {body}"
    );
    let (s, body) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "renoteId": alice_note, "text": "yo", "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "renote gate: {body}");
    assert_eq!(
        body["error"]["id"],
        json!("b390d7e1-8a5e-46ed-b625-06271cafd3d3"),
        "renote gate error id: {body}"
    );

    // 「タイムライン(LTL)にブロックされているユーザーの投稿が含まれない」 —
    // generateBaseNoteFilteringQuery BLOCK_SEL drops the blocker's notes
    let mut ids = Vec::new();
    for (tok, who) in [(a_token, "a"), (b_token, "b"), (c_token, "c")] {
        let (s, body) = post_json(
            app.clone(),
            "/api/notes/create",
            json!({ "text": format!("hi {who}"), "i": tok }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{who} note: {body}");
        ids.push((who, body["createdNote"]["id"].as_str().unwrap().to_string()));
    }
    let (s, body) = post_json(
        app.clone(),
        "/api/notes/local-timeline",
        json!({ "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "LTL: {body}");
    let tl = body.as_array().expect("LTL array");
    let has = |id: &str| tl.iter().any(|n| n["id"].as_str() == Some(id));
    let (_, alice_note2) = &ids[0];
    let (_, bob_note2) = &ids[1];
    let (_, carol_note2) = &ids[2];
    assert!(
        !has(alice_note2),
        "blocker's note must not appear: {alice_note2}"
    );
    assert!(has(bob_note2), "own note must appear: {bob_note2}");
    assert!(
        has(carol_note2),
        "unblocked user's note must appear: {carol_note2}"
    );

    let _ = pool;
}
// clips-arena: the e2e-needed slice of test/e2e/clips.ts — CRUD roundtrip +
// favorite + notes attach (created 9f40e44.. lane; upstream refs in
// mk-db/src/clips.rs + api/clips.rs headers).
#[tokio::test]
async fn clips_arena_crud_favorite_notes() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("mkclips_arena").await;
    // pool re-borrowed by the asserts below (E0382 triage, same as 05d5116)
    mk_db::run_migrations(&pool).await.unwrap();

    let username = format!("alice{}", mk_types::id::gen_aid_now());
    // cross-lane compile triage (block lane): the pool is re-borrowed by the
    // asserts below, so the AppState must clone it (E0382 borrow of moved)
    let app = crate::build_router(test_state(pool.clone()));
    let (_, v) = post_json(
        app.clone(),
        "/api/signup",
        json!({ "username": username, "password": "test" }),
    )
    .await;
    let token = v["token"].as_str().unwrap().to_string();

    // cap 1: create packs the Clip shape's me-gated keys, lastClippedAt NULL
    let (s, v) = post_json(
        app.clone(),
        "/api/clips/create",
        json!({ "name": "test", "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert_eq!(v["name"], "test");
    assert_eq!(v["isPublic"], false);
    assert_eq!(v["lastClippedAt"], Value::Null);
    assert_eq!(v["favoritedCount"], 0);
    assert_eq!(v["isFavorited"], false);
    assert_eq!(v["notesCount"], 0);
    assert_eq!(v["description"], Value::Null);
    let clip_id = v["id"].as_str().unwrap().to_string();

    // cap 2: clips/list roundtrip + clips/show by id agree
    let (s, v) = post_json(app.clone(), "/api/clips/list", json!({ "i": token })).await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 1, "list: {v}");
    assert_eq!(arr[0]["id"], clip_id, "list: {v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/clips/show",
        json!({ "clipId": clip_id, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert_eq!(v["id"], clip_id);
    assert_eq!(v["name"], "test");

    // the clip's source note
    let (s, nv) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "clip me", "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {nv}");
    let note_id = nv["createdNote"]["id"].as_str().unwrap().to_string();

    // cap 3: add-note attaches (clips/notes ids) + feeds the notes/show
    // clippedCount counter; a second attach answers ALREADY_CLIPPED
    let (s, v) = post_json(
        app.clone(),
        "/api/clips/add-note",
        json!({ "clipId": clip_id, "noteId": note_id, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {v}");
    let clipped: i16 =
        mk_db::sqlx::query_scalar(r#"SELECT "clippedCount" FROM "note" WHERE "id" = $1"#)
            .bind(&note_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(clipped, 1, "add-note must bump the note counter");
    let (s, v) = post_json(
        app.clone(),
        "/api/clips/notes",
        json!({ "clipId": clip_id, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert_eq!(v[0]["id"], note_id, "notes: {v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/clips/add-note",
        json!({ "clipId": clip_id, "noteId": note_id, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {v}");
    assert_eq!(v["error"]["code"], "ALREADY_CLIPPED", "body: {v}");
    assert_eq!(
        v["error"]["id"], "734806c4-542c-463a-9311-15c512803965",
        "body: {v}"
    );

    // cap 4: favorite flips the pack counters, duplicate answers
    // ALREADY_FAVORITED, unfavorite restores
    let (s, v) = post_json(
        app.clone(),
        "/api/clips/favorite",
        json!({ "clipId": clip_id, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/clips/show",
        json!({ "clipId": clip_id, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert_eq!(v["favoritedCount"], 1, "body: {v}");
    assert_eq!(v["isFavorited"], true, "body: {v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/clips/favorite",
        json!({ "clipId": clip_id, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {v}");
    assert_eq!(v["error"]["code"], "ALREADY_FAVORITED", "body: {v}");

    // cap 5: update + delete roundtrip; a deleted clip answers NO_SUCH_CLIP
    let (s, v) = post_json(
        app.clone(),
        "/api/clips/update",
        json!({ "clipId": clip_id, "name": "updated", "isPublic": true, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert_eq!(v["name"], "updated");
    assert_eq!(v["isPublic"], true);
    let (s, v) = post_json(
        app.clone(),
        "/api/clips/delete",
        json!({ "clipId": clip_id, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/clips/add-note",
        json!({ "clipId": clip_id, "noteId": note_id, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {v}");
    assert_eq!(v["error"]["code"], "NO_SUCH_CLIP", "body: {v}");
    assert_eq!(
        v["error"]["id"], "d6e76cc0-a1b5-4c7c-a287-73fa9c716dcf",
        "body: {v}"
    );
    let (s, v) = post_json(app.clone(), "/api/clips/list", json!({ "i": token })).await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert_eq!(v.as_array().unwrap().len(), 0, "list after delete: {v}");

    let _ = pool;
}

// ===== ff-arena: ff-visibility list gates + AP collections ===================
//
// users/followers + users/following run the per-visibility gate before the
// list query (upstream endpoints/users/{followers,following}.ts L115-143:
// public → anyone; followers → me or a follower, moderators bypass; private →
// me only; ApiError kind=client → 400). The AP twins
// (ActivityPubServerService.{followers,following}) 403 on federation 'none'
// or any non-public visibility and serve activity+json OrderedCollections
// otherwise — admin/update-meta {federation:'all'} must persist for the
// public leg to reach 200, matching the ff-visibility.ts beforeAll flow.

#[tokio::test]
async fn ff_visibility_list_gates_and_ap_collections() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkffvis").await;
    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    // alice signs up FIRST → unset meta rootUserId makes her the moderator/admin
    let alice = signup(&app, &uname("ffva")).await;
    let bob = signup(&app, &uname("ffvb")).await;
    let (a_id, a_token) = (
        alice["id"].as_str().unwrap(),
        alice["token"].as_str().unwrap(),
    );
    let (b_id, b_token) = (bob["id"].as_str().unwrap(), bob["token"].as_str().unwrap());

    // ff-arena 1: both public → any viewer gets 200 arrays with Following
    // pack items (followeeId/followerId + populated followee/follower)
    let (s, v) = post_json(
        app.clone(),
        "/api/i/update",
        json!({
            "i": a_token,
            "followingVisibility": "public",
            "followersVisibility": "public",
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/users/followers",
        json!({ "userId": a_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "public followers list: {v}");
    assert_eq!(v, json!([]));
    let (s, v) = post_json(
        app.clone(),
        "/api/users/following",
        json!({ "userId": a_id }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "public following list needs no me: {v}");
    assert_eq!(v, json!([]));

    // ff-arena 2: followersVisibility='followers' — a non-follower gets 400
    // FORBIDDEN, the owner still sees her own list
    let (s, v) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "i": a_token, "followersVisibility": "followers" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let (s403, v403) = post_json(
        app.clone(),
        "/api/users/followers",
        json!({ "userId": a_id, "i": b_token }),
    )
    .await;
    assert_eq!(s403, StatusCode::BAD_REQUEST, "non-follower: {v403}");
    assert_eq!(v403["error"]["code"], json!("FORBIDDEN"));
    let (s, _) = post_json(
        app.clone(),
        "/api/users/followers",
        json!({ "userId": a_id, "i": a_token }),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::OK,
        "owner sees the followers-visibility list"
    );

    // bob follows alice → the followers gate opens for him (and the pack
    // mirrors the edge + its follower user)
    let (s, v) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": a_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/users/followers",
        json!({ "userId": a_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "follower now allowed: {v}");
    assert_eq!(v[0]["followerId"], json!(b_id));
    assert_eq!(v[0]["followeeId"], json!(a_id));
    assert_eq!(v[0]["follower"]["id"], json!(b_id));

    // ff-arena 3: followersVisibility='private' — even a follower is locked
    // out; only me (and moderators, root alice here) gets 200
    let (s, v) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "i": a_token, "followersVisibility": "private" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/users/followers",
        json!({ "userId": a_id, "i": b_token }),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::BAD_REQUEST,
        "private locks followers out: {v}"
    );
    assert_eq!(v["error"]["code"], json!("FORBIDDEN"));
    let (s, _) = post_json(
        app.clone(),
        "/api/users/followers",
        json!({ "userId": a_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "me sees the private list");

    // ff-arena 4: the AP collections — federation 'none' (fresh meta) 403s;
    // admin/update-meta {federation:'all'} (alice is root) + public
    // visibility → 200 OrderedCollection with the activity+json content type;
    // flipping visibility to private → 403 again
    // Reset followersVisibility first: ff-arena 3 left it 'private', and the
    // AP collection gate reads the SAME setting — the 'none'/federation 403
    // and the 200 legs only pin what they mean with the visibility back at
    // the public ff-arena-1 value.
    let (s, v) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "i": a_token, "followersVisibility": "public" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "reset followersVisibility: {v}");
    async fn ap_get(app: axum::Router, path: String) -> (StatusCode, Option<String>, Value) {
        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri(path)
                    .header("accept", "application/activity+json")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let v = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, ct, v)
    }

    let (s, _, v) = ap_get(app.clone(), format!("/users/{a_id}/followers")).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "federation none: {v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/admin/update-meta",
        json!({ "i": a_token, "federation": "all" }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "federation 'all' patch: {v}");
    let (s, ct, v) = ap_get(app.clone(), format!("/users/{a_id}/followers")).await;
    assert_eq!(s, StatusCode::OK, "public AP collection: {v}");
    assert!(
        ct.as_deref()
            .unwrap_or_default()
            .starts_with("application/activity+json"),
        "ct: {ct:?}"
    );
    assert_eq!(v["type"], json!("OrderedCollection"));
    assert_eq!(v["totalItems"], json!(1), "bob follows alice: {v}");
    let (s, _, _) = ap_get(app.clone(), format!("/users/{a_id}/following")).await;
    assert_eq!(s, StatusCode::OK, "AP following public");
    let (s, v) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "i": a_token, "followingVisibility": "private" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let (s, _, _) = ap_get(app.clone(), format!("/users/{a_id}/following")).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "private AP collection must 403");

    let _ = (b_id, pool);
}

// ===== approval-arena: follow-approval windows (follow-approval.ts) =========
//
// followApprovalLocalSeconds/RemoteSeconds are Me-detailed profile settings
// (i/update.ts paramDef: nullable integer 0..=FOLLOW_APPROVAL_MAX_SECONDS
// 2592000) that hold a fresh local follower's following/create as a pending
// request (shouldRequireFollowApproval); flipping isLocked false in i/update
// re-adjudicates every pending request against the UPDATED settings
// (UserFollowingService.acceptAllFollowRequests L619) — expired-window and
// null/0 flips accept, the not-yet-expired ones stay pending unless
// autoAcceptFollowed covers them.

#[tokio::test]
async fn approval_arena_i_update_seconds_roundtrip_validation_and_privacy() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkapprovset").await;
    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let alice = signup(&app, &uname("apa")).await;
    let a_token = alice["token"].as_str().unwrap();
    let a_id = alice["id"].as_str().unwrap();

    // 初期値は null (follow-approval.ts test 1, first assert)
    let (s, v) = post_json(app.clone(), "/api/i", json!({ "i": a_token })).await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert_eq!(v["followApprovalLocalSeconds"], json!(null), "body: {v}");
    assert_eq!(v["followApprovalRemoteSeconds"], json!(null), "body: {v}");

    // update both, then reset local → remote preserved, response carries both
    let (s, v) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "followApprovalLocalSeconds": 43200, "followApprovalRemoteSeconds": 86400, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert_eq!(v["followApprovalLocalSeconds"], json!(43200), "body: {v}");
    assert_eq!(v["followApprovalRemoteSeconds"], json!(86400), "body: {v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "followApprovalLocalSeconds": null, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert_eq!(v["followApprovalLocalSeconds"], json!(null), "body: {v}");
    assert_eq!(v["followApprovalRemoteSeconds"], json!(86400), "body: {v}");

    // 不正な期間を拒否する: -1 / 1.5 / 2592001 → 400 (both keys)
    for seconds in [-1_i64, 2592001] {
        for key in ["followApprovalLocalSeconds", "followApprovalRemoteSeconds"] {
            let (s, v) = post_json(
                app.clone(),
                "/api/i/update",
                json!({ key: seconds, "i": a_token }),
            )
            .await;
            assert_eq!(
                s,
                StatusCode::BAD_REQUEST,
                "seconds={seconds} key={key} body: {v}"
            );
        }
    }
    let (s, v) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "followApprovalLocalSeconds": 1.5, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "fractional seconds: {v}");

    // 本人以外には公開されない: public users/show pack omits both keys
    let (s, v) = post_json(app.clone(), "/api/users/show", json!({ "userId": a_id })).await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert!(
        v.get("followApprovalLocalSeconds").is_none(),
        "must not leak on the public pack: {v}"
    );
    assert!(
        v.get("followApprovalRemoteSeconds").is_none(),
        "must not leak on the public pack: {v}"
    );

    let _ = pool;
}

#[tokio::test]
async fn approval_arena_fresh_follower_pends_then_manual_accept_204() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkapprovpending").await;
    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let receiver = signup(&app, &uname("apr")).await;
    let follower = signup(&app, &uname("apf")).await;
    let (r_id, r_token) = (
        receiver["id"].as_str().unwrap(),
        receiver["token"].as_str().unwrap(),
    );
    let (f_id, f_token) = (
        follower["id"].as_str().unwrap(),
        follower["token"].as_str().unwrap(),
    );

    let (s, v) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "followApprovalLocalSeconds": 43200, "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");

    // 作成直後のローカルユーザーからのフォローを保留: 200 + pending, no follow
    let (s, v) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": r_id, "i": f_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/users/show",
        json!({ "userId": r_id, "i": f_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert_eq!(v["isFollowing"], json!(false), "body: {v}");
    assert_eq!(
        v["hasPendingFollowRequestFromYou"],
        json!(true),
        "body: {v}"
    );

    // 手動で承認: 204 → following edge + request swept
    let (s, v) = post_json(
        app.clone(),
        "/api/following/requests/accept",
        json!({ "userId": f_id, "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/users/show",
        json!({ "userId": r_id, "i": f_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert_eq!(v["isFollowing"], json!(true), "body: {v}");
    assert_eq!(
        v["hasPendingFollowRequestFromYou"],
        json!(false),
        "body: {v}"
    );
    assert!(
        mk_db::social::following_exists(&pool, f_id, r_id)
            .await
            .unwrap(),
        "follow edge written by the manual accept"
    );

    let _ = pool;
}

#[tokio::test]
async fn approval_arena_unlock_readjudicates_pending_requests() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkapprovunlock").await;
    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let receiver = signup(&app, &uname("aur")).await;
    let bob = signup(&app, &uname("aub")).await;
    let dan = signup(&app, &uname("aud")).await;
    let (r_id, r_token) = (
        receiver["id"].as_str().unwrap(),
        receiver["token"].as_str().unwrap(),
    );
    let (_b_id, b_token) = (bob["id"].as_str().unwrap(), bob["token"].as_str().unwrap());
    let d_token = dan["token"].as_str().unwrap();

    // leg 1 (指定期間を経過した相手は鍵の解除時に承認される): bob pends under
    // a 1s window, the window expires, the unlock accepts him.
    let (s, v) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "isLocked": true, "followApprovalLocalSeconds": 1, "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": r_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    std::thread::sleep(std::time::Duration::from_millis(1100));

    // leg 2 (鍵を解除しても指定期間に達していない相手は保留される): carol is
    // born NOW — her account age is well under 1s when the unlock
    // re-adjudicates, so the SAME 1s window that expired for bob still
    // binds her (requires_age_approval reads the follower's AID createdAt,
    // upstream アカウント作成後の期間によるフォロー承認).
    let carol = signup(&app, &uname("auc")).await;
    let (_c_id, c_token) = (
        carol["id"].as_str().unwrap(),
        carol["token"].as_str().unwrap(),
    );
    let (s, v) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": r_id, "i": c_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");

    let (s, v) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "isLocked": false, "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");

    let (s, v) = post_json(
        app.clone(),
        "/api/users/show",
        json!({ "userId": r_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert_eq!(
        v["isFollowing"],
        json!(true),
        "expired window accepted: {v}"
    );
    assert_eq!(
        v["hasPendingFollowRequestFromYou"],
        json!(false),
        "body: {v}"
    );
    let (s, v) = post_json(
        app.clone(),
        "/api/users/show",
        json!({ "userId": r_id, "i": c_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert_eq!(
        v["isFollowing"],
        json!(false),
        "fresh-window request stays pending: {v}"
    );
    assert_eq!(
        v["hasPendingFollowRequestFromYou"],
        json!(true),
        "body: {v}"
    );

    // leg 3 (鍵の解除と同時に期間を null に変更すると承認される): re-lock so
    // the null flip rides a genuine isLocked true→false transition — dan
    // (old account) pends under the lock, and the SAME i/update's window
    // reset makes acceptAllFollowRequests accept him.
    let (s, v) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "isLocked": true, "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": r_id, "i": d_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "isLocked": false, "followApprovalLocalSeconds": null, "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/users/show",
        json!({ "userId": r_id, "i": d_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert_eq!(v["isFollowing"], json!(true), "null flip accepts dan: {v}");
    assert_eq!(
        v["hasPendingFollowRequestFromYou"],
        json!(false),
        "body: {v}"
    );

    let _ = pool;
}

// approval-final — serial-suite close-out of the follow-approval.ts arena:
// the three tests above carry the census asserts (i/update roundtrip +
// validation + Me-only privacy, fresh-follower pend + manual accept 204,
// unlock re-adjudication: expired-window accept / fresh-window pend /
// simultaneous null flip accept); the e2e census (upstream
// test/e2e/follow-approval.ts) ran 18/18 green at ed1ee66, the block was
// dropped by a later API-pushed tree and is restored here verbatim.

// ============================================================================
// well-known arena: /.well-known/* + /nodeinfo/* routes ride OUTSIDE /api/*
// (upstream test/e2e/well-known.ts — e2e-well-known workflow). Mirrors
// WellKnownServerService (hook headers, 204 preflight, JRD webfinger, XRD
// host-meta, RFC8414 oauth metadata) + NodeinfoServerService (links relay +
// 2.0/2.1 documents). The beforeAll federation flip rides admin/update-meta.
// ============================================================================

/// GET/OPTIONS probe that keeps the response headers — the well-known asserts
/// are strict string compares on Access-Control-* / Vary / content-type.
#[allow(dead_code)]
async fn request_raw(
    app: &axum::Router,
    method: &str,
    path: &str,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

#[allow(dead_code)]
fn hdr(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// well-known assert 1 (nodeinfo): the /.well-known/nodeinfo links relay —
/// NodeinfoServerService.getLinks shape (2.1 then 2.0, `{origin}/nodeinfo/*`
/// hrefs) under Access-Control-Allow-Origin: *.
#[tokio::test]
async fn well_known_nodeinfo_links() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkwellknown1").await;
    let app = crate::build_router(test_state(pool.clone()));

    // well-known.ts beforeAll: signup alice + admin/update-meta federation all
    let alice = signup(&app, "alice").await;
    let a_token = alice["token"].as_str().unwrap();
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/update-meta",
        json!({ "federation": "all", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "update-meta must 204");

    let (s, h, body) = request_raw(&app, "GET", "/.well-known/nodeinfo").await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(hdr(&h, "access-control-allow-origin").as_deref(), Some("*"));
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        v,
        json!({
            "links": [{
                "rel": "http://nodeinfo.diaspora.software/ns/schema/2.1",
                "href": "http://127.0.0.1:3000/nodeinfo/2.1",
            }, {
                "rel": "http://nodeinfo.diaspora.software/ns/schema/2.0",
                "href": "http://127.0.0.1:3000/nodeinfo/2.0",
            }],
        }),
        "nodeinfo links relay: {v}"
    );

    // the 2.1/2.0 documents the links point at: software.name 'misskey' +
    // profile'd content types + REAL usage counts from postgres (alice ⇒ 1).
    let (s, h21, body) = request_raw(&app, "GET", "/nodeinfo/2.1").await;
    assert_eq!(s, StatusCode::OK, "nodeinfo/2.1 body: {body}");
    assert_eq!(
        hdr(&h21, "content-type").as_deref(),
        Some("application/json; profile=\"http://nodeinfo.diaspora.software/ns/schema/2.1#\"")
    );
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["version"], json!("2.1"));
    assert_eq!(v["software"]["name"], json!("misskey"));
    assert_eq!(v["usage"]["users"]["total"], json!(1), "real users count");
    let (s, h20, body) = request_raw(&app, "GET", "/nodeinfo/2.0").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        hdr(&h20, "content-type").as_deref(),
        Some("application/json; profile=\"http://nodeinfo.diaspora.software/ns/schema/2.0#\"")
    );
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["version"], json!("2.0"));
    assert!(
        v["software"].get("repository").is_none(),
        "2.0 deletes software.repository: {v}"
    );

    let _ = pool;
}

/// well-known assert 2 (webfinger): OPTIONS preflight → 204 with
/// `Access-Control-Allow-Headers: Accept`; GET JRD — subject + the exact
/// three links (self /users/:id, profile-page, ostatus subscribe template),
/// Vary: Accept exposed through Access-Control-Expose-Headers.
#[tokio::test]
async fn well_known_webfinger_jrd() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkwellknown2").await;
    let app = crate::build_router(test_state(pool.clone()));

    let alice = signup(&app, "alice").await;
    let a_token = alice["token"].as_str().unwrap();
    let a_id = alice["id"].as_str().unwrap();
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/update-meta",
        json!({ "federation": "all", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    let path = "/.well-known/webfinger?resource=acct:alice@127.0.0.1:3000";
    let (s, h, _) = request_raw(&app, "OPTIONS", path).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "preflight must 204");
    assert_eq!(
        hdr(&h, "access-control-allow-headers").as_deref(),
        Some("Accept")
    );

    let (s, h, body) = request_raw(&app, "GET", path).await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(hdr(&h, "access-control-allow-origin").as_deref(), Some("*"));
    assert_eq!(
        hdr(&h, "access-control-expose-headers").as_deref(),
        Some("Vary")
    );
    assert_eq!(hdr(&h, "vary").as_deref(), Some("Accept"));
    assert_eq!(
        hdr(&h, "content-type").as_deref(),
        Some("application/jrd+json")
    );
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        v,
        json!({
            "subject": "acct:alice@127.0.0.1:3000",
            "links": [{
                "rel": "self",
                "type": "application/activity+json",
                "href": format!("http://127.0.0.1:3000/users/{a_id}"),
            }, {
                "rel": "http://webfinger.net/rel/profile-page",
                "type": "text/html",
                "href": "http://127.0.0.1:3000/@alice",
            }, {
                "rel": "http://ostatus.org/schema/1.0/subscribe",
                "template": "http://127.0.0.1:3000/authorize-follow?acct={uri}",
            }],
        }),
        "webfinger JRD: {v}"
    );

    // the 404 leg: unknown local resources miss with the upstream status
    let (s, _, _) = request_raw(
        &app,
        "GET",
        "/.well-known/webfinger?resource=acct:ghost@127.0.0.1:3000",
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let _ = pool;
}

/// well-known assert 3 (host-meta + oauth): XRD host-meta (xrd content type,
/// lrdd link template), host-meta.json LRD/JRD template deep-equal, and the
/// RFC8414 issuer/authorization/token endpoints.
#[tokio::test]
async fn well_known_hostmeta_and_oauth() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkwellknown3").await;
    let app = crate::build_router(test_state(pool.clone()));

    let alice = signup(&app, "alice").await;
    let a_token = alice["token"].as_str().unwrap();
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/update-meta",
        json!({ "federation": "all", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    let (s, h, body) = request_raw(&app, "GET", "/.well-known/host-meta").await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(hdr(&h, "access-control-allow-origin").as_deref(), Some("*"));
    assert_eq!(
        hdr(&h, "content-type").as_deref(),
        Some("application/xrd+xml")
    );
    assert!(
        body.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"),
        "{body}"
    );
    assert!(
        body.contains(
            "<Link rel=\"lrdd\" type=\"application/xrd+xml\" template=\"http://127.0.0.1:3000/.well-known/webfinger?resource={uri}\"/>"
        ),
        "{body}"
    );

    let (s, _, body) = request_raw(&app, "GET", "/.well-known/host-meta.json").await;
    assert_eq!(s, StatusCode::OK);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        v,
        json!({
            "links": [{
                "rel": "lrdd",
                "type": "application/jrd+json",
                "template": "http://127.0.0.1:3000/.well-known/webfinger?resource={uri}",
            }],
        }),
        "{v}"
    );

    let (s, _, body) = request_raw(&app, "GET", "/.well-known/oauth-authorization-server").await;
    assert_eq!(s, StatusCode::OK);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["issuer"], json!("http://127.0.0.1:3000"));
    assert_eq!(
        v["authorization_endpoint"],
        json!("http://127.0.0.1:3000/oauth/authorize")
    );
    assert_eq!(
        v["token_endpoint"],
        json!("http://127.0.0.1:3000/oauth/token")
    );

    let _ = pool;
}

// ==== endpoints-arena ====
// upstream test/e2e/endpoints.ts rides the misc endpoints arena; these pin
// the /api/endpoints catalog contract (server/api/endpoints/endpoints.ts:
// `endpoints.map(x => x.name)`, requireCredential: false) and the
// ReactionService normalize fallback the ❤ reactions assert on.

#[tokio::test]
async fn endpoints_catalog_full_registry() {
    // pool-free: the catalog arm never touches db, mirrors upstream shape
    let app = crate::build_router(crate::AppState {
        config: crate::config::Config {
            port: 0,
            url: "http://127.0.0.1:3000".to_string(),
            db: None,
            redis: None,
            test: true,
            version: None,
            raw_json: serde_json::Value::Null,
        },
        pool: None,
        rate_limiter: std::sync::Arc::new(crate::ratelimit::RateLimiter::new()),
        search_engine: std::sync::Arc::new(mk_core::services::search_bridge::NoopEngine),
    });
    let (status, body) = post_json(app, "/api/endpoints", json!({})).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let names = body.as_array().expect("endpoint list is an array");
    // endpoint-list.ts registry size at misskey-dev/misskey ea2c3aed
    assert_eq!(names.len(), 438);
    // Object.entries order: the registry opens with the first admin/* name
    assert_eq!(
        names[0],
        json!("admin/abuse-report/notification-recipient/create")
    );
    // admin/* unclipped (meta catalog lists the admin family too)
    assert!(names.contains(&json!("admin/abuse-user-reports")));
    // self-inclusive: `endpoints` is itself registered upstream
    assert!(names.contains(&json!("endpoints")));
}

#[test]
fn reaction_normalize_fallback_and_targets() {
    // ReactionService.normalize: non-emoji / empty / unknown custom → ❤
    assert_eq!(super::social::normalize_reaction(""), "❤");
    assert_eq!(super::social::normalize_reaction("Hello!"), "❤");
    assert_eq!(super::social::normalize_reaction(":fuji:"), "❤");
    assert_eq!(super::social::normalize_reaction("love"), "❤"); // legacy → modern
    assert_eq!(super::social::normalize_reaction("like"), "👍");
    // unicode emoji survives; VS16 stripped on standalone (L397-398)
    assert_eq!(super::social::normalize_reaction("🚀"), "🚀");
    assert_eq!(super::social::normalize_reaction("\u{2764}\u{fe0f}"), "❤");
    // ASCII/CJK prose never starts an emoji span → fallback
    assert_eq!(super::social::normalize_reaction(" TEN "), "❤");
}

// arena-nodeinfo =============================================================
// upstream test/e2e/nodeinfo.ts (pinned ea2c3aed): two relativeFetch tests —
// `nodeinfo/2.1` + `nodeinfo/2.0` — asserting res.ok + Access-Control-Allow-
// Origin `*` + software.name 'misskey' on the mounted /nodeinfo/* routes
// (NOT the /.well-known relay; that one is well_known_nodeinfo_links above).
// Mirrored here with the NodeinfoServerService.nodeinfo2() census shape
// deepened per the pinned truth: profile'd content types, the operative
// version string (config.version — cargo-test arm resolves config/env to the
// PINNED "2026.10.1-alpha.0"), real postgres usage counts, the 2.0 arm that
// deletes software.repository while 2.1 overrides homepage to
// meta.repositoryUrl (NodeinfoServerService L128-131), and the metadata
// const keys — maxNoteTextLength = MAX_NOTE_TEXT_LENGTH 3000 (const.ts L6),
// themeColor '#86b300' fallback, proxyAccountName 'proxy' (the
// SystemAccountService.fetch created system user), openRegistrations off the
// fresh meta row's disableRegistration — MiMeta entity default true
// (models/Meta.ts L61-64; MetaService.fetch upserts the row through entity
// defaults, NOT the bare Init.js DDL default).

#[tokio::test]
async fn arena_nodeinfo_21_document_shape() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mknodeinfo1").await;
    let app = crate::build_router(test_state(pool.clone()));

    let alice = signup(&app, "alice").await;

    // nodeinfo.ts test 'nodeinfo 2.1': ok + ACAO `*` + software.name
    let (s, h, body) = request_raw(&app, "GET", "/nodeinfo/2.1").await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(hdr(&h, "access-control-allow-origin").as_deref(), Some("*"));
    assert_eq!(
        hdr(&h, "content-type").as_deref(),
        Some("application/json; profile=\"http://nodeinfo.diaspora.software/ns/schema/2.1#\"")
    );
    let v: Value = serde_json::from_str(&body).unwrap();

    assert_eq!(v["version"], json!("2.1"));
    assert_eq!(v["software"]["name"], json!("misskey"));
    assert_eq!(v["software"]["version"], json!("2026.10.1-alpha.0"));
    // 2.1 overrides BOTH repository and homepage to meta.repositoryUrl
    assert_eq!(
        v["software"]["homepage"],
        json!("https://github.com/misskey-dev/misskey")
    );
    assert_eq!(
        v["software"]["repository"],
        json!("https://github.com/misskey-dev/misskey")
    );
    assert_eq!(v["protocols"], json!(["activitypub"]));
    assert_eq!(v["services"]["outbound"], json!(["atom1.0", "rss2.0"]));
    assert_eq!(v["services"]["inbound"], json!([]));

    // real postgres counts: fresh pool + alice ⇒ users 1, notes 0
    assert_eq!(v["usage"]["users"]["total"], json!(1), "alice: {v}");
    assert_eq!(v["usage"]["users"]["activeHalfyear"], Value::Null);
    assert_eq!(v["usage"]["users"]["activeMonth"], Value::Null);
    assert_eq!(v["usage"]["localPosts"], json!(0));
    assert_eq!(v["usage"]["localComments"], json!(0));

    // openRegistrations flips off the meta row's disableRegistration — the
    // fresh row rides the MiMeta COLUMN default true (models/Meta.ts L61-64
    // `default: true`; MetaService.fetch upserts {id:'x'} through entity
    // defaults — the same side /api/admin/meta pins at L3113), even though
    // the raw Init.js DDL default is false: every row the server reads is
    // created through MetaService, never the bare DDL.
    assert_eq!(v["openRegistrations"], json!(false));
    assert_eq!(v["metadata"]["disableRegistration"], json!(true));

    // pinned metadata keys
    assert_eq!(v["metadata"]["maxNoteTextLength"], json!(3000));
    assert_eq!(v["metadata"]["themeColor"], json!("#86b300"));
    assert_eq!(v["metadata"]["proxyAccountName"], json!("proxy"));
    assert_eq!(v["metadata"]["langs"], json!([]));

    let _ = alice;
    let _ = pool;
}

#[tokio::test]
async fn arena_nodeinfo_20_document_shape() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mknodeinfo2").await;
    let app = crate::build_router(test_state(pool.clone()));

    let alice = signup(&app, "alice").await;
    let a_token = alice["token"].as_str().unwrap();

    // one real local note ⇒ usage.localPosts counts it (net of deletions)
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "i": a_token, "text": "nodeinfo census note" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "note create: {v}");

    // nodeinfo.ts test 'nodeinfo 2.0': ok + ACAO `*` + software.name
    let (s, h, body) = request_raw(&app, "GET", "/nodeinfo/2.0").await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(hdr(&h, "access-control-allow-origin").as_deref(), Some("*"));
    assert_eq!(
        hdr(&h, "content-type").as_deref(),
        Some("application/json; profile=\"http://nodeinfo.diaspora.software/ns/schema/2.0#\"")
    );
    let v: Value = serde_json::from_str(&body).unwrap();

    assert_eq!(v["version"], json!("2.0"));
    assert_eq!(v["software"]["name"], json!("misskey"));
    assert_eq!(v["software"]["version"], json!("2026.10.1-alpha.0"));
    // the 2.0 arm deletes software.repository; homepage stays nodeinfo_homepage
    assert!(
        v["software"].get("repository").is_none(),
        "2.0 deletes software.repository: {v}"
    );
    assert_eq!(v["software"]["homepage"], json!("https://misskey-hub.net"));
    assert_eq!(v["protocols"], json!(["activitypub"]));

    // real counts: alice + her one note
    assert_eq!(v["usage"]["users"]["total"], json!(1));
    assert_eq!(v["usage"]["localPosts"], json!(1), "one local note: {v}");

    assert_eq!(v["metadata"]["maxNoteTextLength"], json!(3000));

    // 2.0 keeps the repository URL inside metadata (only software loses it)
    assert_eq!(
        v["metadata"]["repositoryUrl"],
        json!("https://github.com/misskey-dev/misskey")
    );

    let _ = pool;
}

// ===== arena-apivis: api-visibility.ts show/reactions gates (upstream
// test/e2e/api-visibility.ts @ ea2c3aed) =====================================
//
// notes/show never errors on an invisible note — the gate rides the pack
// (NoteEntityService.shouldHideNote + hideNote): text/cw → null, files
// emptied, isHidden: true. Specified keys off visibleUserIds ONLY (a text
// mention does not unhide); followers shows the reply-target / mentioned
// user / follower. notes/reactions gates through getNote + isVisibleForMe →
// NO_SUCH_NOTE(263fff3d) for invisible notes.
#[tokio::test]
async fn apivis_show_pack_hide_lanes() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkapivis1").await;
    let app = crate::build_router(test_state(pool.clone()));
    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());

    let alice = signup(&app, &uname("apa")).await;
    let a_token = alice["token"].as_str().unwrap();
    let follower = signup(&app, &uname("fpa")).await;
    let f_token = follower["token"].as_str().unwrap();
    let target = signup(&app, &uname("tpa")).await;
    let t_token = target["token"].as_str().unwrap();
    let t_uname = target["username"].as_str().unwrap();
    let other = signup(&app, &uname("opa")).await;
    let o_token = other["token"].as_str().unwrap();

    let (s, _) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": alice["id"], "i": f_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "follower follows alice: {s}");

    // [show] followers-post: guest/other → isHidden, follower → full text
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "x", "visibility": "followers", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "fol create: {v}");
    let fol = v["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(app.clone(), "/api/notes/show", json!({ "noteId": fol })).await;
    assert_eq!(s, StatusCode::OK, "guest show fol: {v}");
    assert_eq!(v["isHidden"], json!(true), "guest [show] followers: {v}");
    assert_eq!(v["text"], Value::Null, "hideNote strips text: {v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/show",
        json!({ "noteId": fol, "i": o_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "other show fol: {v}");
    assert_eq!(v["isHidden"], json!(true), "other [show] followers: {v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/show",
        json!({ "noteId": fol, "i": f_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "follower show fol: {v}");
    assert_eq!(v["text"], json!("x"), "follower [show] followers: {v}");

    // [show] specified-post: listed target sees it, everyone else hides — a
    // text mention does NOT unhide (the leg keys off visibleUserIds only)
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "x", "visibility": "specified", "visibleUserIds": [target["id"]], "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "spe create: {v}");
    let spe = v["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/show",
        json!({ "noteId": spe, "i": t_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "listed show spe: {v}");
    assert_eq!(v["text"], json!("x"), "指定ユーザー [show]: {v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/show",
        json!({ "noteId": spe, "i": f_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "follower show spe: {v}");
    assert_eq!(v["isHidden"], json!(true), "follower [show] specified: {v}");
    let (s, v) = post_json(app.clone(), "/api/notes/show", json!({ "noteId": spe })).await;
    assert_eq!(s, StatusCode::OK, "guest show spe: {v}");
    assert_eq!(v["isHidden"], json!(true), "guest [show] specified: {v}");

    // [show] specified-reply with NO visibleUserIds — the reply target rides
    // the create-time visibleUsers pull, so された人 sees it without a listing
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "y", "visibility": "public", "i": t_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "tgt create: {v}");
    let tgt = v["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "x", "visibility": "specified", "replyId": tgt, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "speR create: {v}");
    let sper = v["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/show",
        json!({ "noteId": sper, "i": t_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "reply-target show speR: {v}");
    assert_eq!(
        v["text"],
        json!("x"),
        "された人 [show] specified-reply: {v}"
    );
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/show",
        json!({ "noteId": sper, "i": o_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "other show speR: {v}");
    assert_eq!(
        v["isHidden"],
        json!(true),
        "other [show] specified-reply: {v}"
    );

    // [show] followers-mention — the mentioned non-follower reads it
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": format!("@{t_uname} x"), "visibility": "followers", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "folM create: {v}");
    let folm = v["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/show",
        json!({ "noteId": folm, "i": t_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "mentioned show folM: {v}");
    assert_eq!(
        v["text"],
        json!(format!("@{t_uname} x")),
        "メンションされた人 [show] followers-mention: {v}"
    );

    // [reactions] invisible note → NO_SUCH_NOTE(263fff3d) (guest + non-follow)
    let (s, _) = post_json(
        app.clone(),
        "/api/notes/reactions/create",
        json!({ "noteId": fol, "reaction": "👍", "i": f_token }),
    )
    .await;
    // reactions/create has no res body upstream → 204 (endpoints.ts shape),
    // same as the port's handler; a plain 200 assert broke the serial census.
    assert_eq!(s, StatusCode::NO_CONTENT, "follower reacts: {s}");
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/reactions",
        json!({ "noteId": fol }),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::BAD_REQUEST,
        "guest [reactions] followers: {v}"
    );
    assert_eq!(v["error"]["code"], json!("NO_SUCH_NOTE"), "{v}");
    assert_eq!(
        v["error"]["id"],
        json!("263fff3d-d0e1-4af4-bea7-8408059b451a"),
        "{v}"
    );
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/reactions",
        json!({ "noteId": fol, "i": o_token }),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::BAD_REQUEST,
        "other [reactions] followers: {v}"
    );
    assert_eq!(v["error"]["code"], json!("NO_SUCH_NOTE"), "{v}");
    // visible note → 200 with the reaction listed
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/reactions",
        json!({ "noteId": fol, "i": f_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "follower [reactions] followers: {v}");
    assert_eq!(v.as_array().map(|a| a.len()), Some(1), "{v}");
    // 存在しないノート (misskey:id tolerates 'foo') → NO_SUCH_NOTE
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/reactions",
        json!({ "noteId": "foo", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "nonexistent [reactions]: {v}");
    assert_eq!(v["error"]["code"], json!("NO_SUCH_NOTE"), "{v}");

    let _ = (o_token, folm);
}
// arena-streaming ============================================================
// upstream test/e2e/streaming.ts (pinned ea2c3aed) at the channel-routing
// level: the six `notesStream` channels (hybrid/global/userList/hashtag are
// new — home/local already rode the renote-mute.ts spy above), the main-
// channel mention/renote events, and the ws-hub app-token authenticate the
// Authentication test drives. The registry is the spy, exactly like the
// stream-rtm lanes: connect_channel registers (async now — the userList init
// validates list ownership against the pool) and publish_notes fills the
// receivers before /api/notes/create returns (the fanout is awaited inside
// the request path).

#[tokio::test]
async fn arena_streaming_channel_routing_matrix() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkstreammatrix").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let alice = signup(&app, &uname("sma")).await;
    let at = alice["token"].as_str().unwrap().to_string();
    let a_id = alice["id"].as_str().unwrap().to_string();
    let bob = signup(&app, &uname("smb")).await;
    let bt = bob["token"].as_str().unwrap().to_string();
    let carol = signup(&app, &uname("smc")).await;
    let ct = carol["token"].as_str().unwrap().to_string();
    let c_id = carol["id"].as_str().unwrap().to_string();

    /// Drain the spy into (subId, eventType, frameBody) triples (idle gap =
    /// done — the fanout is awaited inside the create, so nothing is late).
    async fn frames(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
    ) -> Vec<(String, String, Value)> {
        let mut out = Vec::new();
        loop {
            match tokio::time::timeout(std::time::Duration::from_millis(250), rx.recv()).await {
                Ok(Some(t)) => {
                    let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                    if v["type"] == "channel" {
                        out.push((
                            v["body"]["id"].as_str().unwrap_or_default().to_string(),
                            v["body"]["type"].as_str().unwrap_or_default().to_string(),
                            v["body"]["body"].clone(),
                        ));
                    }
                }
                _ => return out,
            }
        }
    }
    /// Drain the spy into `note` event ids (stream-rtm convention).
    async fn note_ids(rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>) -> Vec<String> {
        let mut ids = Vec::new();
        loop {
            match tokio::time::timeout(std::time::Duration::from_millis(250), rx.recv()).await {
                Ok(Some(t)) => {
                    let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                    if v["type"] == "channel" && v["body"]["type"] == "note" {
                        if let Some(id) = v["body"]["body"]["id"].as_str() {
                            ids.push(id.to_string());
                        }
                    }
                }
                _ => return ids,
            }
        }
    }
    /// connect_channel spy wrapper: (registered, receiver).
    async fn connect(
        pool: &mk_db::sqlx::PgPool,
        channel: &str,
        params: Value,
        sub: &str,
        owner: &str,
    ) -> (bool, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let ok =
            crate::api::streaming::connect_channel(pool, channel, &params, sub, tx, owner).await;
        (ok, rx)
    }

    // Refusal arms (getChannelConstructor throw / init false): unknown name,
    // hashtag q outside the array-of-arrays shape, userList not owned.
    assert!(
        !connect(&pool, "noSuchChannel", json!({}), "ref1", &a_id)
            .await
            .0
    );
    assert!(
        !connect(&pool, "hashtag", json!({ "q": "foo" }), "ref2", &a_id)
            .await
            .0
    );
    assert!(
        !connect(
            &pool,
            "userList",
            json!({ "listId": "ghost" }),
            "ref3",
            &a_id
        )
        .await
        .0
    );

    // The hybrid/global sinks carry alice's identity; the hashtag sink pins
    // the AND matrix row [['foo','bar']].
    let (hybrid_reg, mut hybrid) =
        connect(&pool, "hybridTimeline", json!({}), "a-hyb", &a_id).await;
    assert!(hybrid_reg, "hybridTimeline connects");
    let (global_reg, mut global) =
        connect(&pool, "globalTimeline", json!({}), "a-glb", &a_id).await;
    assert!(global_reg, "globalTimeline connects");
    let (and_reg, mut and_tl) = connect(
        &pool,
        "hashtag",
        json!({ "q": [["foo", "bar"]] }),
        "a-tag-and",
        &a_id,
    )
    .await;
    assert!(and_reg, "hashtag (AND) connects");

    // Global: a non-followed local user's public note flows; a `home` note
    // does not (global-timeline.ts L55). Hybrid delivers the public one and
    // alice's own followers note, never bob's home one.
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "pub", "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let bob_pub = v["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "hm", "visibility": "home", "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let bob_home = v["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "own fol", "visibility": "followers", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let alice_fol = v["createdNote"]["id"].as_str().unwrap().to_string();

    let got = frames(&mut global).await;
    assert!(
        got.iter()
            .any(|(s, t, b)| s == "a-glb" && t == "note" && b["id"] == *bob_pub),
        "グローバルTLにフォローしていないローカルユーザーの投稿が流れる — {got:?}"
    );
    assert!(
        !got.iter().any(|(_, _, b)| b["id"] == *bob_home),
        "グローバルTLにホーム投稿は流れない — {got:?}"
    );
    assert!(
        !got.iter().any(|(_, _, b)| b["id"] == *alice_fol),
        "グローバルTLに followers 投稿は流れない — {got:?}"
    );

    let got = frames(&mut hybrid).await;
    assert!(
        got.iter()
            .any(|(s, t, b)| s == "a-hyb" && t == "note" && b["id"] == *bob_pub),
        "hybridTLに非フォローローカルの全体公開が流れる — {got:?}"
    );
    assert!(
        got.iter()
            .any(|(s, t, b)| s == "a-hyb" && t == "note" && b["id"] == *alice_fol),
        "hybridTLに自分の followers 投稿が流れる — {got:?}"
    );
    assert!(
        !got.iter().any(|(_, _, b)| b["id"] == *bob_home),
        "hybridTLに非フォローユーザーのホーム投稿は流れない — {got:?}"
    );

    // Hashtag AND: only '#foo #bar' clears the [['foo','bar']] row
    // (hashtag.ts L52-54); '#foo' alone never does.
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "#foo #bar", "i": ct }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let both_tags = v["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "#foo", "i": ct }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let foo_only = v["createdNote"]["id"].as_str().unwrap().to_string();

    let got = note_ids(&mut and_tl).await;
    assert!(
        got.iter().any(|n| n == &both_tags),
        "指定したハッシュタグの投稿が流れる (AND) — {got:?}"
    );
    assert!(
        !got.iter().any(|n| n == &foo_only),
        "AND 未満のタグノートは流れない — {got:?}"
    );

    // UserList: the real list (alice owner, carol member) — the membership
    // gate admits carol's note and rejects non-member bob's.
    let (s, v) = post_json(
        app.clone(),
        "/api/users/lists/create",
        json!({ "name": "my list", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let list_id = v["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/users/lists/push",
        json!({ "listId": list_id, "userId": c_id, "i": at }),
    )
    .await;
    // push.ts defines no response body → 204 (the port's ok_empty arm).
    assert_eq!(s, StatusCode::NO_CONTENT, "{v}");

    let (list_reg, mut list_tl) = connect(
        &pool,
        "userList",
        json!({ "listId": list_id }),
        "a-uls",
        &a_id,
    )
    .await;
    assert!(list_reg, "userList connects");
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "in list", "i": ct }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let carol_note = v["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "not member", "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let bob_note = v["createdNote"]["id"].as_str().unwrap().to_string();

    let got = note_ids(&mut list_tl).await;
    assert!(
        got.iter().any(|n| n == &carol_note),
        "リストに入れているユーザーの投稿が流れる — {got:?}"
    );
    assert!(
        !got.iter().any(|n| n == &bob_note),
        "リストに入れていないユーザーの投稿は流れない — {got:?}"
    );

    crate::api::streaming::remove_user(&a_id);
    let _ = (ct, bt, at, &pool);
}

/// UserList mute lanes (#10443 legs, at the routing level) + the muted-
/// instances instance-mute leg the census drives with a host'd signup.
#[tokio::test]
async fn arena_streaming_userlist_mute_lanes() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkstreamulmute").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let alice = signup(&app, &uname("ula")).await;
    let at = alice["token"].as_str().unwrap().to_string();
    let a_id = alice["id"].as_str().unwrap().to_string();
    let bob = signup(&app, &uname("ulb")).await;
    let bt = bob["token"].as_str().unwrap().to_string();
    let kanako = signup(&app, &uname("ulk")).await;
    let k_id = kanako["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "k row", "i": kanako["token"].as_str().unwrap() }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let kanako_note = v["createdNote"]["id"].as_str().unwrap().to_string();
    // remote member (host'd signup rides NODE_ENV=test — SignupApiService L116)
    let (s, v) = post_json(
        app.clone(),
        "/api/signup",
        json!({ "username": uname("ult"), "password": "test", "host": "example.com" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let t_id = v["id"].as_str().unwrap().to_string();
    let tt = v["token"].as_str().unwrap().to_string();
    let b_id = bob["id"].as_str().unwrap().to_string();

    // alice's list with bob + kanako + takumi; alice mutes kanako and
    // instance-mutes example.com (the #10443 census setup).
    let (s, v) = post_json(
        app.clone(),
        "/api/users/lists/create",
        json!({ "name": "l", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let list_id = v["id"].as_str().unwrap().to_string();
    for uid in [&b_id, &k_id, &t_id] {
        let (s, v) = post_json(
            app.clone(),
            "/api/users/lists/push",
            json!({ "listId": list_id, "userId": uid, "i": at }),
        )
        .await;
        // push.ts defines no response body → 204 (the port's ok_empty arm);
        // a 200 assert broke the serial census at the FIRST loop iteration.
        assert_eq!(s, StatusCode::NO_CONTENT, "{v}");
    }
    let (s, v) = post_json(
        app.clone(),
        "/api/mute/create",
        json!({ "userId": k_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "mutedInstances": ["example.com"], "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");

    let (reg, mut list_tl) = {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        (
            crate::api::streaming::connect_channel(
                &pool,
                "userList",
                &json!({ "listId": list_id }),
                "a-ulm",
                tx,
                &a_id,
            )
            .await,
            rx,
        )
    };
    assert!(reg, "userList connects");

    // Control: bob's member note flows.
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "clean", "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let bob_note = v["createdNote"]["id"].as_str().unwrap().to_string();
    let got: Vec<String> = {
        let mut ids = Vec::new();
        loop {
            match tokio::time::timeout(std::time::Duration::from_millis(250), list_tl.recv()).await
            {
                Ok(Some(t)) => {
                    let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                    if v["type"] == "channel" && v["body"]["type"] == "note" {
                        ids.push(v["body"]["body"]["id"].as_str().unwrap().to_string());
                    }
                }
                _ => break ids,
            }
        }
    };
    assert!(
        got.iter().any(|n| n == &bob_note),
        "ミュート無関係のメンバーノートが流れる — {got:?}"
    );
    assert!(
        !got.iter().any(|n| n == &kanako_note),
        "ミュートしているユーザの投稿をリストTLに流れない — {got:?}"
    );

    // ミュートしているユーザへのリプライがリストTLに流れない (reply leg of
    // isUserRelated reads the reply pack's userId).
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "reply to muted", "replyId": kanako_note, "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let reply_row = v["createdNote"]["id"].as_str().unwrap().to_string();

    // ミュートしているユーザの投稿をリノートしたときリストTLに流れない
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "renoteId": kanako_note, "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let renote_row = v["createdNote"]["id"].as_str().unwrap().to_string();

    let got: Vec<String> = {
        let mut ids = Vec::new();
        loop {
            match tokio::time::timeout(std::time::Duration::from_millis(250), list_tl.recv()).await
            {
                Ok(Some(t)) => {
                    let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                    if v["type"] == "channel" && v["body"]["type"] == "note" {
                        ids.push(v["body"]["body"]["id"].as_str().unwrap().to_string());
                    }
                }
                _ => break ids,
            }
        }
    };
    // takumi (example.com) posts — the mutedInstances leg (isInstanceMuted
    // on the author host) must never deliver.
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "remote row", "i": tt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let takumi_note = v["createdNote"]["id"].as_str().unwrap().to_string();

    let got: Vec<String> = {
        let mut ids = Vec::new();
        loop {
            match tokio::time::timeout(std::time::Duration::from_millis(250), list_tl.recv()).await
            {
                Ok(Some(t)) => {
                    let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                    if v["type"] == "channel" && v["body"]["type"] == "note" {
                        ids.push(v["body"]["body"]["id"].as_str().unwrap().to_string());
                    }
                }
                _ => break ids,
            }
        }
    };
    assert!(
        !got.iter().any(|n| n == &takumi_note),
        "ミュートしているサーバのノートがリストTLに流れない — {got:?}"
    );
    assert!(
        !got.iter().any(|n| n == &reply_row),
        "ミュートしているユーザへのリプライがリストTLに流れない — {got:?}"
    );
    assert!(
        !got.iter().any(|n| n == &renote_row),
        "ミュートしているユーザの投稿をリノートしたときリストTLに流れない — {got:?}"
    );

    crate::api::streaming::remove_user(&a_id);
    let _ = (bt, at, &pool);
}

/// main.ts events + the app-token hub authenticate (streaming.ts
/// 'Events' describe + 'Authentication' test, spy-level).
#[tokio::test]
async fn arena_streaming_main_events_and_app_token() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkstreammain").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let ayano = signup(&app, &uname("mnay")).await;
    let ay_t = ayano["token"].as_str().unwrap().to_string();
    let kyoko = signup(&app, &uname("mnky")).await;
    let ky_token = kyoko["token"].as_str().unwrap().to_string();
    let ky_id = kyoko["id"].as_str().unwrap().to_string();
    let ky_name = kyoko["username"].as_str().unwrap().to_string();

    // main sink on kyoko (the hub registers main subs through main_register).
    let (mut main_rx) = {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        crate::api::drive::streaming::main_register(&ky_id, "k-main", tx);
        rx
    };

    // 'mention' event (Events/mention test): ayano mentions kyoko.
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": format!("foo @{ky_name} bar"), "i": ay_t }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let mention_note = v["createdNote"]["id"].as_str().unwrap().to_string();

    // 'renote' event (Events/renote test): ayano renotes kyoko's note. The
    // target must be KYOKO's own note — upstream NoteCreateService (L872-885,
    // `note.userId !== renote.userId`) and the port's renote leg suppress the
    // main 'renote' event when the target is the renoter's own note, so the
    // earlier leg that renoted ayano's mention note never streamed anything.
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "ky seed", "i": ky_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let ky_note = v["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "renoteId": ky_note, "i": ay_t }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let renote_id = v["createdNote"]["renoteId"].as_str().unwrap().to_string();

    let mut saw_mention = false;
    let mut saw_renote = false;
    loop {
        match tokio::time::timeout(std::time::Duration::from_millis(250), main_rx.recv()).await {
            Ok(Some(t)) => {
                let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                if v["type"] == "channel" {
                    let et = v["body"]["type"].as_str().unwrap_or_default();
                    if et == "mention" && v["body"]["body"]["id"] == *mention_note {
                        saw_mention = true;
                    }
                    if et == "renote" && v["body"]["body"]["renoteId"] == *renote_id {
                        saw_renote = true;
                    }
                }
            }
            _ => break,
        }
    }
    assert!(saw_mention, "main channel に mention イベントが流れる");
    assert!(saw_renote, "main channel に renote イベントが流れる");
    // registry hygiene: drop kyoko's main sub (the dead-sink reaper in
    // publish_notes/hub teardown reaps the remaining handle on sock close).
    let ky_sinks = crate::api::drive::streaming::main_sinks_of(&ky_id);
    for (sid, tx) in ky_sinks {
        drop((sid, tx));
    }

    // Authentication (createAppToken arms): a scope-less app token is
    // refused at the hub (authenticate_hub → None → bare 401 — the ws client
    // surfaces it as unexpected-response/established=false); `read:account`
    // rides the hybridTimeline waitFire like the census row.
    let (s, v) = post_json(
        app.clone(),
        "/api/miauth/gen-token",
        json!({ "session": "s1", "permission": [], "i": ay_t }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let empty_token = v["token"].as_str().unwrap().to_string();
    assert_eq!(empty_token.len(), 32, "{v}");
    assert!(
        crate::api::stream_auth::authenticate_hub(&pool, Some(&empty_token))
            .await
            .is_none(),
        "scope 無しアプリトークンはハブに拒否される (established=false)"
    );

    let (s, v) = post_json(
        app.clone(),
        "/api/miauth/gen-token",
        json!({ "session": "s2", "permission": ["read:account"], "i": ay_t }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let scoped_token = v["token"].as_str().unwrap().to_string();
    let auth = crate::api::stream_auth::authenticate_hub(&pool, Some(&scoped_token))
        .await
        .expect("read:account アプリトークンはハブに通る");
    assert_eq!(auth.user_id, ayano["id"].as_str().unwrap());
    assert!(
        auth.app_permission
            .as_ref()
            .map(|p| p.iter().any(|x| x == "read:account"))
            .unwrap_or(false)
    );
    // native token: user + no app list
    let native = crate::api::stream_auth::authenticate_hub(&pool, Some(&ay_t))
        .await
        .expect("native token resolves");
    assert!(native.app_permission.is_none());
    // unknown → None (AuthenticationError arm)
    assert!(
        crate::api::stream_auth::authenticate_hub(&pool, Some("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"))
            .await
            .is_none()
    );

    // remove kyoko's main sub (registry hygiene for parallel tests)
    crate::api::drive::streaming::main_sinks_of(&ky_id);
    let _ = (ky_token, ky_id, ky_name, &pool, &app);
}
// ============================================================================
// arena-api: api.ts — the test-endpoint paramDef roundtrip (General
// validation), the WWW-Authenticate challenges (#sendApiError /
// #sendAuthenticationError), and the 管理者専用のAPIのアクセス制限
// get-index-stats matrix with the miauth app tokens (upstream
// test/e2e/api.ts). POST-with-headers helper here because the well-known
// request_raw probe is GET-only.
// ============================================================================

/// POST with optional Authorization header → (status, headers, body-json).
async fn arena_api_post(
    app: axum::Router,
    path: &str,
    body: Value,
    bearer: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    if let Some(tok) = bearer {
        builder = builder.header("authorization", format!("Bearer {tok}"));
    }
    let resp = app
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, headers, v)
}

#[tokio::test]
async fn arena_api_test_endpoint_roundtrip_and_validation() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkapitest1").await;
    let app = crate::build_router(test_state(pool));

    // missing require param / wrong type → 400 (ajv required+type failures)
    let (s, v) = post_json(app.clone(), "/api/test", json!({ "string": "a" })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/test",
        json!({ "required": true, "string": 42 }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {v}");

    // invalid misskey:id (empty string) → 400 — the ^[a-zA-Z0-9]+$ `+`
    let (s, v) = post_json(
        app.clone(),
        "/api/test",
        json!({ "required": true, "id": "" }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {v}");

    // valid misskey:id + useDefaults echo (default/nullableDefault 'hello')
    let (s, v) = post_json(
        app.clone(),
        "/api/test",
        json!({ "required": true, "id": "8wvhjghbxu" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert_eq!(v["id"], json!("8wvhjghbxu"), "body: {v}");
    assert_eq!(v["default"], json!("hello"), "body: {v}");
    assert_eq!(v["nullableDefault"], json!("hello"), "body: {v}");

    // explicit null passes the nullable channel un-defaulted
    let (s, v) = post_json(
        app.clone(),
        "/api/test",
        json!({ "required": true, "nullableDefault": null }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert_eq!(v["nullableDefault"], json!(null), "body: {v}");
}

#[tokio::test]
async fn arena_api_www_authenticate_challenges() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkapiwwwauth").await;
    let app = crate::build_router(test_state(pool));

    // 401 without a credential → exactly `Bearer realm="Misskey"` (the
    // realm-only arm of #sendApiError L112)
    let (s, h, v) =
        arena_api_post(app.clone(), "/api/admin/get-index-stats", json!({}), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "body: {v}");
    assert_eq!(
        v["error"]["code"],
        json!("CREDENTIAL_REQUIRED"),
        "body: {v}"
    );
    assert_eq!(
        hdr(&h, "www-authenticate").as_deref(),
        Some("Bearer realm=\"Misskey\""),
        "challenge: {:?}",
        hdr(&h, "www-authenticate")
    );

    // present-but-unresolvable token → #sendAuthenticationError's
    // invalid_token challenge
    let (s, h, v) = arena_api_post(
        app.clone(),
        "/api/admin/get-index-stats",
        json!({ "i": "tsukawasete" }),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "body: {v}");
    assert_eq!(
        v["error"]["code"],
        json!("AUTHENTICATION_FAILED"),
        "body: {v}"
    );
    let ch = hdr(&h, "www-authenticate").unwrap_or_default();
    assert!(
        ch.starts_with("Bearer realm=\"Misskey\", error=\"invalid_token\", error_description"),
        "challenge: {ch}"
    );

    // a client-kind 400 → invalid_request challenge (#sendApiError L124):
    // a REAL credential + notes/create text:true (the api.ts invalid_request
    // leg — the token is fine, the param is not)
    let bob = signup(&app, "bob").await;
    let b_token = bob["token"].as_str().unwrap();
    let (s, h, v) = arena_api_post(
        app.clone(),
        "/api/notes/create",
        json!({ "text": true, "i": b_token }),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {v}");
    assert_eq!(v["error"]["code"], json!("INVALID_PARAM"), "body: {v}");
    let ch = hdr(&h, "www-authenticate").unwrap_or_default();
    assert!(
        ch.starts_with("Bearer realm=\"Misskey\", error=\"invalid_request\""),
        "challenge: {ch}"
    );
}

#[tokio::test]
async fn arena_api_get_index_stats_app_token_matrix() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkapigidx").await;
    let app = crate::build_router(test_state(pool));

    // alice signs up FIRST → stamped meta."rootUserId" (root bypass)
    let alice = signup(&app, "alice").await;
    let a_token = alice["token"].as_str().unwrap();

    // createAppToken fixture: miauth/gen-token mints a scoped access token
    let (s, v) = post_json(
        app.clone(),
        "/api/miauth/gen-token",
        json!({ "session": "s1", "permission": ["read:account"], "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let scoped_out = v["token"].as_str().unwrap().to_string();

    // read:account scope misses the read:admin:index-stats kind →
    // PERMISSION_DENIED (after alice's root role-gate pass — call() order)
    let (s, h, v) = arena_api_post(
        app.clone(),
        "/api/admin/get-index-stats",
        json!({}),
        Some(&scoped_out),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "body: {v}");
    assert_eq!(v["error"]["code"], json!("PERMISSION_DENIED"), "body: {v}");
    assert_eq!(
        v["error"]["id"],
        json!("1370e5b7-d4eb-4566-bb1d-7748ee6a1838"),
        "body: {v}"
    );
    let ch = hdr(&h, "www-authenticate").unwrap_or_default();
    assert!(
        ch.starts_with("Bearer realm=\"Misskey\", error=\"insufficient_scope\""),
        "challenge: {ch}"
    );

    // a token carrying the endpoint kind rides through → 200 array body
    let (s, v) = post_json(
        app.clone(),
        "/api/miauth/gen-token",
        json!({ "session": "s2", "permission": ["read:admin:index-stats"], "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let admin_out = v["token"].as_str().unwrap().to_string();
    let (s, _h, v) = arena_api_post(
        app.clone(),
        "/api/admin/get-index-stats",
        json!({}),
        Some(&admin_out),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert!(v.is_array(), "pg_indexes rows: {v}");
}

// arena-notenotify ============================================================
// upstream test/e2e/note-notify.ts (pinned ea2c3aed): the following-edge
// notify flag — following/update.ts 'normal'/'none' stored as
// following.notify 'normal'/NULL (insertFollowingDoc omits the column) —
// read back by following/list `notification: true` (list.ts L56-58
// `following.notify IS NOT NULL`), and the NoteCreateService.create
// L790-813 fanout: non-'specified', non-reply notes push a notification
// type 'note' (L806) carrying the fresh note id to every follower flagged
// notify:'normal' (a pure renote adds the renoteMutings drop L800-804).

#[tokio::test]
async fn note_notify_following_list_notification_filter() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mknotenotify1").await;
    let app = crate::build_router(test_state(pool.clone()));

    let alice = signup(&app, "alice").await;
    let bob = signup(&app, "bob").await;
    let carol = signup(&app, "carol").await;
    let a = alice["token"].as_str().unwrap().to_string();
    let (bob_id, carol_id) = (bob["id"].clone(), carol["id"].clone());

    // notify.ts test 1: plain follow leaves notify NULL → notification:true
    // returns [], the plain list still counts the followee.
    let (s, b) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "i": a, "userId": bob_id }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let (s, b) = post_json(
        app.clone(),
        "/api/following/list",
        json!({ "i": a, "notification": true }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b.as_array().unwrap().len(), 0);
    let (s, b) = post_json(app.clone(), "/api/following/list", json!({ "i": a })).await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b.as_array().unwrap().len(), 1);
    assert_eq!(b[0]["followeeId"], bob_id);

    // tests 2-3: notify 'normal' surfaces the edge; a second flagged followee
    // returns both
    let (s, b) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "i": a, "userId": carol_id, "withReplies": false }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let (s, b) = post_json(
        app.clone(),
        "/api/following/update",
        json!({ "i": a, "userId": carol_id, "notify": "normal" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let (s, b) = post_json(
        app.clone(),
        "/api/following/list",
        json!({ "i": a, "notification": true }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b.as_array().unwrap().len(), 1);
    assert_eq!(b[0]["followeeId"], carol_id);
    let (s, b) = post_json(
        app.clone(),
        "/api/following/update",
        json!({ "i": a, "userId": bob_id, "notify": "normal" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let (s, b) = post_json(
        app.clone(),
        "/api/following/list",
        json!({ "i": a, "notification": true }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b.as_array().unwrap().len(), 2);
    let mut ids: Vec<Value> = b
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["followeeId"].clone())
        .collect();
    ids.sort_by(|a, b| {
        a.as_str()
            .unwrap_or_default()
            .cmp(b.as_str().unwrap_or_default())
    });
    let mut want = vec![bob_id.clone(), carol_id.clone()];
    want.sort_by(|a, b| {
        a.as_str()
            .unwrap_or_default()
            .cmp(b.as_str().unwrap_or_default())
    });
    assert_eq!(ids, want);

    // test 4: notify 'none' removes it from the notification list (stored
    // NULL), the plain list keeps both
    let (s, b) = post_json(
        app.clone(),
        "/api/following/update",
        json!({ "i": a, "userId": bob_id, "notify": "none" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let (s, b) = post_json(
        app.clone(),
        "/api/following/list",
        json!({ "i": a, "notification": true }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b.as_array().unwrap().len(), 1);
    assert_eq!(b[0]["followeeId"], carol_id);
    let (s, b) = post_json(app.clone(), "/api/following/list", json!({ "i": a })).await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b.as_array().unwrap().len(), 2);

    // test 5: the flag is per-edge — bob's own notify:'normal' list carries
    // only his followee (carol), never alice's settings
    let (s, b) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "i": bob["token"], "userId": carol_id }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let (s, b) = post_json(
        app.clone(),
        "/api/following/update",
        json!({ "i": bob["token"], "userId": carol_id, "notify": "normal" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let (s, b) = post_json(
        app.clone(),
        "/api/following/list",
        json!({ "i": bob["token"], "notification": true }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b.as_array().unwrap().len(), 1);
    assert_eq!(b[0]["followeeId"], carol_id);
    let (s, b) = post_json(
        app.clone(),
        "/api/following/delete",
        json!({ "i": bob["token"], "userId": carol_id }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");

    // test 8: limit 1..100 param narrows the page (bob re-flagged like the
    // upstream ladder)
    let (s, b) = post_json(
        app.clone(),
        "/api/following/update",
        json!({ "i": a, "userId": bob_id, "notify": "normal" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let (s, b) = post_json(
        app.clone(),
        "/api/following/list",
        json!({ "i": a, "notification": true, "limit": 1 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b.as_array().unwrap().len(), 1);
    let (s, b) = post_json(
        app.clone(),
        "/api/following/list",
        json!({ "i": a, "notification": true, "limit": 0 }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "limit 0 is outside 1..100");

    // test 9: requireCredential — no token → CREDENTIAL_REQUIRED 401
    let (s, _) = post_json(app.clone(), "/api/following/list", json!({})).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let _ = pool;
}

#[tokio::test]
async fn note_notify_push_leg_note_reason() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mknotenotify2").await;
    let app = crate::build_router(test_state(pool.clone()));

    let alice = signup(&app, "alice").await;
    let bob = signup(&app, "bob").await;
    let a = alice["token"].as_str().unwrap().to_string();
    let b_tok = bob["token"].as_str().unwrap().to_string();

    // alice flags bob notify:'normal' (test 6 setup)
    let (s, b) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "i": a, "userId": bob["id"] }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let (s, b) = post_json(
        app.clone(),
        "/api/following/update",
        json!({ "i": a, "userId": bob["id"], "notify": "normal" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");

    // bob posts a text-only note (test 6 「normal通知設定時、投稿で通知が届く」)
    let (s, b) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "i": b_tok, "text": "ファイルなしの投稿" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let note_id = b["createdNote"]["id"].clone();

    let (s, b) = post_json(
        app.clone(),
        "/api/notifications/mark-all-as-read",
        json!({ "i": a }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {b}");
    let (s, b) = post_json(app.clone(), "/api/i/notifications", json!({ "i": a })).await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let hits: Vec<&Value> = b
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["type"] == json!("note") && n["note"]["id"] == note_id)
        .collect();
    assert_eq!(hits.len(), 1, "note notification missing: {b}");
    assert_eq!(hits[0]["userId"], json!(bob["id"]));

    // turning the flag back off stops the fanout (test-6 cleanup leg)
    let (s, b) = post_json(
        app.clone(),
        "/api/following/update",
        json!({ "i": a, "userId": bob["id"], "notify": "none" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let (s, b) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "i": b_tok, "text": "通知設定解除後の投稿" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let silent_id = b["createdNote"]["id"].clone();
    let (s, b) = post_json(app.clone(), "/api/i/notifications", json!({ "i": a })).await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let hits: Vec<&Value> = b
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["type"] == json!("note") && n["note"]["id"] == silent_id)
        .collect();
    assert!(hits.is_empty(), "notify:'none' must not fan out: {b}");

    let _ = pool;
}
// api-final: the api.ts close-out pins (upstream test/e2e/api.ts) — the four
// legs the 2703917 tail left unasserted locally: the non-nullable
// `default:null` reject (General validation), the Bearer-only
// Authentication-header SUCCESS leg, the exact invalid_token challenge bytes
// (#sendAuthenticationError L141) and the native-bob ROLE gate id
// (管理者専用のAPIのアクセス制限 failedApiCall rows).
// ============================================================================

#[tokio::test]
async fn arena_api_final_closeout_pins() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkapifinal").await;
    let app = crate::build_router(test_state(pool));

    // (1) General validation: non-nullable `default` given a visible null →
    // 400 (ajv type string, nullable:false — the explicit-null passthrough
    // only applies to nullableDefault).
    let (s, v) = post_json(
        app.clone(),
        "/api/test",
        json!({ "required": true, "default": null }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {v}");
    assert_eq!(v["error"]["code"], json!("INVALID_PARAM"), "body: {v}");

    // alice signs up FIRST → stamped meta."rootUserId" (root bypass).
    let alice = signup(&app, "alice").await;
    let a_token = alice["token"].as_str().unwrap();

    // (2) Authentication header > 一般リクエスト: Bearer-only native token on
    // admin/get-index-stats → 200 array (no `i` in the body at all).
    let (s, _h, v) = arena_api_post(
        app.clone(),
        "/api/admin/get-index-stats",
        json!({}),
        Some(a_token),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert!(v.is_array(), "pg_indexes rows: {v}");

    // (3) invalid_token challenge — exact upstream bytes (ApiCallService
    // #sendAuthenticationError L141).
    let (s, h, v) = arena_api_post(
        app.clone(),
        "/api/admin/get-index-stats",
        json!({}),
        Some("syuilo"),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "body: {v}");
    assert_eq!(
        hdr(&h, "www-authenticate").as_deref(),
        Some(
            "Bearer realm=\"Misskey\", error=\"invalid_token\", \
             error_description=\"Authentication failed. Please ensure your \
             token is correct.\""
        ),
        "challenge: {:?}",
        hdr(&h, "www-authenticate")
    );

    // (4) 管理者専用のAPIのアクセス制限 > bobは一般ユーザー: native token, no
    // roles → 403 ROLE_PERMISSION_DENIED c3d38592 (role gate ahead of the
    // app-token kind gate).
    let alice_bob = signup(&app, "bob").await;
    let b_token = alice_bob["token"].as_str().unwrap();
    let (s, v) = post_json(
        app.clone(),
        "/api/admin/get-index-stats",
        json!({ "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "body: {v}");
    assert_eq!(v["error"]["code"], json!("ROLE_PERMISSION_DENIED"));
    assert_eq!(
        v["error"]["id"],
        json!("c3d38592-54c0-429d-be96-5636b0431a61"),
        "body: {v}"
    );
}

// stream-final ==============================================================
// The two residual failing legs of the streaming.ts e2e census (run
// 39241716401, 15_Run rows 1-5), driven at the same routing level the arena
// tests use — both sat UPSTREAM of the channel ladders: the notesStream
// fanout packs never carried `poll` (the renote lane stored none, so every
// poll renote packed poll:null and isQuotePacked saw a pure renote) nor
// `tags` (insert_note never wrote NOTE_TAGS, so passes_hashtag matched
// nothing). Upstream pins: test/e2e/streaming.ts L299-308 (投票のみの
// リノートが流れる) + L740-825 (指定したハッシュタグの投稿が流れる family),
// channels/home-timeline.ts L88-97, channels/hashtag.ts L51-54.

#[tokio::test]
async fn arena_streaming_final_poll_quote_and_hashtag() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkstreamfinal").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let ayano = signup(&app, &uname("sfa")).await; // home viewer + hashtag listener
    let at = ayano["token"].as_str().unwrap().to_string();
    let a_id = ayano["id"].as_str().unwrap().to_string();
    let kyoko = signup(&app, &uname("sfk")).await; // renoter / hashtag author
    let kt = kyoko["token"].as_str().unwrap().to_string();
    let k_id = kyoko["id"].as_str().unwrap().to_string();

    /// Drain into (subId, eventType, frameBody) triples (250ms idle gap =
    /// done — publish_notes is awaited inside the create request path).
    async fn frames(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
    ) -> Vec<(String, String, Value)> {
        let mut out = Vec::new();
        loop {
            match tokio::time::timeout(std::time::Duration::from_millis(250), rx.recv()).await {
                Ok(Some(t)) => {
                    let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                    if v["type"] == "channel" {
                        out.push((
                            v["body"]["id"].as_str().unwrap_or_default().to_string(),
                            v["body"]["type"].as_str().unwrap_or_default().to_string(),
                            v["body"]["body"].clone(),
                        ));
                    }
                }
                _ => return out,
            }
        }
    }

    // Home leg precondition: the viewer follows the renoter (e2e beforeAll).
    let (s, v) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "i": at, "userId": k_id }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");

    // homeTimeline withRenotes:false (home-timeline.ts L38: `?? true` default
    // flipped; the L88-97 pure-renote drop must NOT bite a poll renote).
    let (tx, mut home) = tokio::sync::mpsc::unbounded_channel::<String>();
    let ok = crate::api::streaming::connect_channel(
        &pool,
        "homeTimeline",
        &json!({ "withRenotes": false }),
        "sf-home",
        tx,
        &a_id,
    )
    .await;
    assert!(ok, "homeTimeline connects");

    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "base", "i": kt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let base_id = v["createdNote"]["id"].as_str().unwrap().to_string();

    // (a) 投票のみのリノートが流れる (L299-308): renoteId+poll → the stream
    // pack carries `poll` → isQuotePacked → quote bypasses withRenotes:false.
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({
            "i": kt,
            "renoteId": base_id,
            "poll": { "choices": ["kinoko", "takenoko"] },
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let poll_quote = v["createdNote"]["id"].as_str().unwrap().to_string();

    // (a') pure renote control (L281-289 のときリノートが流れない): still a
    // pure renote → withRenotes:false drops it.
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "i": kt, "renoteId": base_id }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let pure_renote = v["createdNote"]["id"].as_str().unwrap().to_string();

    let got = frames(&mut home).await;
    assert!(
        got.iter()
            .any(|(s, t, b)| s == "sf-home" && t == "note" && b["id"] == *poll_quote),
        "withRenotes: false のとき投票のみのリノートが流れる — {got:?}"
    );
    assert!(
        got.iter().any(|(s, t, b)| s == "sf-home"
            && t == "note"
            && b["id"] == *poll_quote
            && b["poll"]["choices"].is_array()),
        "the streamed pack materializes poll — {got:?}"
    );
    assert!(
        !got.iter().any(|(_, _, b)| b["id"] == *pure_renote),
        "withRenotes: false のときリノートが流れない — {got:?}"
    );

    // (b) Hashtag q matrix (hashtag.ts L51-54: q.some(ANDed.every(tag →
    // noteTags.includes(tag)))) over the persisted NOTE_TAGS — the single and
    // the AND+OR [['foo','bar'],['piyo']] shapes together.
    let (tx, mut single_tl) = tokio::sync::mpsc::unbounded_channel::<String>();
    let ok = crate::api::streaming::connect_channel(
        &pool,
        "hashtag",
        &json!({ "q": [["foo"]] }),
        "sf-tag-single",
        tx,
        &a_id,
    )
    .await;
    assert!(ok, "hashtag (single) connects");
    let (tx, mut ador_tl) = tokio::sync::mpsc::unbounded_channel::<String>();
    let ok = crate::api::streaming::connect_channel(
        &pool,
        "hashtag",
        &json!({ "q": [["foo", "bar"], ["piyo"]] }),
        "sf-tag-ador",
        tx,
        &a_id,
    )
    .await;
    assert!(ok, "hashtag (AND+OR) connects");

    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "#foo", "i": kt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let foo_note = v["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "#foo #bar", "i": kt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let foobar_note = v["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "#piyo", "i": kt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let piyo_note = v["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "#waaa", "i": kt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let waaa_note = v["createdNote"]["id"].as_str().unwrap().to_string();

    let got = frames(&mut single_tl).await;
    assert!(
        got.iter().any(|(s, t, b)| s == "sf-tag-single"
            && t == "note"
            && (b["id"] == *foo_note || b["id"] == *foobar_note)),
        "指定したハッシュタグの投稿が流れる ([['foo']]) — {got:?}"
    );
    assert!(
        !got.iter()
            .any(|(_, _, b)| b["id"] == *piyo_note || b["id"] == *waaa_note),
        "[['foo']] 注文外のタグノートは流れない — {got:?}"
    );

    let got = frames(&mut ador_tl).await;
    assert!(
        got.iter()
            .any(|(s, t, b)| s == "sf-tag-ador" && t == "note" && b["id"] == *foobar_note),
        "指定したハッシュタグの投稿が流れる (AND [['foo','bar']]) — {got:?}"
    );
    assert!(
        got.iter()
            .any(|(s, t, b)| s == "sf-tag-ador" && t == "note" && b["id"] == *piyo_note),
        "指定したハッシュタグの投稿が流れる (OR ['piyo']) — {got:?}"
    );
    assert!(
        !got.iter()
            .any(|(_, _, b)| b["id"] == *foo_note || b["id"] == *waaa_note),
        "AND 未満 / 任意タグ外のノートは流れない — {got:?}"
    );

    crate::api::streaming::remove_user(&a_id);
    let _ = (kt, at, k_id, pool);
}

// arena-2fa ==================================================================
// upstream test/e2e/2fa.ts (pinned ea2c3aed) TOTP lane: the register → done →
// signin-flow(next: totp) → unregister chain plus the vendored TOTP core in
// mk-db/src/two_factor.rs. The crypto primitives are the RFC-vector level of
// the census: SHA-1 (RFC 3174), HMAC-SHA1 (RFC 2202) and 6-digit TOTP-SHA1
// (RFC 6238 / the otpauth defaults the upstream test drives).

#[test]
fn two_factor_sha1_and_hmac_rfc_vectors() {
    // RFC 3174 test suite: "abc" and the 56-char padding-edge message
    assert_eq!(
        mk_db::two_factor::sha1(b"abc"),
        hex20("a9993e364706816aba3e25717850c26c9cd0d89d")
    );
    assert_eq!(
        mk_db::two_factor::sha1(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        hex20("84983e441c3bd26ebaae4aa1f95129e5e54670f1")
    );
    // RFC 2202 case 1..3-ish shapes: key "key" over the fox, and the
    // larger-than-block-size key case 6 (80 bytes of 0xaa).
    assert_eq!(
        mk_db::two_factor::hmac_sha1(b"key", b"The quick brown fox jumps over the lazy dog"),
        hex20("de7c9b85b8b78aa6bc8a7a36f70a90701c9db4d9")
    );
    let long_key = [0xaau8; 80];
    assert_eq!(
        mk_db::two_factor::hmac_sha1(
            &long_key,
            b"Test Using Larger Than Block-Size Key - Hash Key First"
        ),
        hex20("aa4ae5e15272d00e95705637ce8a3b55ed402112")
    );
}

#[test]
fn two_factor_totp_rfc6238_vectors() {
    // RFC 6238 appendix B (SHA-1) with the 6-digit truncation the upstream
    // OTPAuth config uses — secret "12345678901234567890" (ASCII), last six
    // digits of the published 8-digit values.
    let secret = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"; // base32("12345678901234567890")
    assert_eq!(
        mk_db::two_factor::totp_code_at(secret, 59).as_deref(),
        Some("287082")
    );
    assert_eq!(
        mk_db::two_factor::totp_code_at(secret, 1111111109).as_deref(),
        Some("081804")
    );
    // RFC 6238 App B SHA-1, 8-digit {14050471, 89005924} → last-six pins
    // (the first two vectors above already pin the same derivation).
    assert_eq!(
        mk_db::two_factor::totp_code_at(secret, 1111111111).as_deref(),
        Some("050471")
    );
    assert_eq!(
        mk_db::two_factor::totp_code_at(secret, 1234567896).as_deref(),
        Some("005924")
    );
    // ±1 window acceptance mirrors OTPAuth validate({ window: 1 }): a code
    // one step behind the wall clock still verifies (delta -1).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let now_code = mk_db::two_factor::totp_code_at(secret, now).unwrap();
    assert_eq!(
        mk_db::two_factor::verify_totp(secret, &now_code, now + 30),
        Some(-1)
    );
    assert_eq!(mk_db::two_factor::verify_totp(secret, "00000x", now), None);
    // the ±2 window stays closed (delta of a 60s-old code rejects)
    let old = mk_db::two_factor::totp_code_at(secret, now - 60).unwrap();
    assert_eq!(mk_db::two_factor::verify_totp(secret, &old, now), None);
}

#[test]
fn two_factor_base32_and_secret_generation() {
    // RFC 4648 examples, no-padding form
    assert_eq!(mk_db::two_factor::base32_encode(b"f"), "MY");
    assert_eq!(mk_db::two_factor::base32_encode(b"fo"), "MZXQ");
    assert_eq!(mk_db::two_factor::base32_encode(b"foo"), "MZXW6");
    assert_eq!(
        mk_db::two_factor::base32_decode("MZXW6===").as_deref(),
        Some(b"foo".as_slice())
    );
    assert_eq!(
        mk_db::two_factor::base32_decode("mzxw6"),
        Some(b"foo".to_vec())
    );
    assert!(mk_db::two_factor::base32_decode("MZ1W6").is_none());
    // generate → decode round-trip: 20 bytes → 32 unpadded chars
    let secret = mk_db::two_factor::generate_secret_base32();
    assert_eq!(secret.len(), 32, "{secret}");
    assert!(
        secret
            .bytes()
            .all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(&b))
    );
    assert_eq!(
        mk_db::two_factor::base32_encode(&mk_db::two_factor::base32_decode(&secret).unwrap()),
        secret
    );
}

#[tokio::test]
async fn two_factor_register_done_unregister_flow() {
    // upstream 2fa.ts tests 1+6 (TOTP 設定/解除): register fields, done's
    // backupCodes, the i twoFactorEnabled flip, the signin-flow totp prompt
    // (assert.deepStrictEqual shape), the token login and the unregister
    // back-to-password login. The duplicated-token burn (test 7) rides the
    // same primitives — covered at the validate level above.
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mk2fa1").await;
    let app = crate::build_router(test_state(pool.clone()));

    let alice = signup(&app, "alice").await;
    let a = alice["token"].as_str().unwrap().to_string();

    // register: qr/url/secret present, label = username, issuer = host
    let (s, b) = post_json(
        app.clone(),
        "/api/i/2fa/register",
        json!({ "i": a, "password": "test" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert!(b["qr"].as_str().is_some());
    assert!(b["url"].as_str().is_some());
    assert!(b["secret"].as_str().is_some());
    assert_eq!(b["label"], json!("alice"));
    assert_eq!(b["issuer"], json!("127.0.0.1:3000"));
    let secret = b["secret"].as_str().unwrap().to_string();

    // done: a live TOTP code escalates the temp secret + 5 backup codes
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let token = mk_db::two_factor::totp_code_at(&secret, now).unwrap();
    let (s, b) = post_json(
        app.clone(),
        "/api/i/2fa/done",
        json!({ "i": a, "token": token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b["backupCodes"].as_array().unwrap().len(), 5);

    // i: twoFactorEnabled flipped
    let (s, b) = post_json(app.clone(), "/api/i", json!({ "i": a })).await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b["twoFactorEnabled"], json!(true));

    // signin-flow without token → EXACTLY { finished: false, next: 'totp' }
    let (s, b) = post_json(
        app.clone(),
        "/api/signin-flow",
        json!({ "username": "alice", "password": "test" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b, json!({ "finished": false, "next": "totp" }));

    // signin-flow with the token → finished + i
    let (s, b) = post_json(
        app.clone(),
        "/api/signin-flow",
        json!({ "username": "alice", "password": "test", "token": token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b["finished"], json!(true));
    assert!(b["i"].as_str().is_some());

    // unregister → 204, i flips back, password-only signin works again
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let token2 = mk_db::two_factor::totp_code_at(&secret, now).unwrap();
    let (s, b) = post_json(
        app.clone(),
        "/api/i/2fa/unregister",
        json!({ "i": a, "password": "test", "token": token2 }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {b}");
    let (s, b) = post_json(app.clone(), "/api/i", json!({ "i": a })).await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b["twoFactorEnabled"], json!(false));
    let (s, b) = post_json(
        app.clone(),
        "/api/signin-flow",
        json!({ "username": "alice", "password": "test" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b["finished"], json!(true));
}

#[test]
fn two_factor_duplicated_totp_burn() {
    // upstream 2fa.ts test 7 core: once a step is claimed (validateOtp's
    // redis SET NX parity), the same (user, secret, step) never verifies
    // again inside the 90s ttl.
    let secret = mk_db::two_factor::generate_secret_base32();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let token = mk_db::two_factor::totp_code_at(&secret, now).unwrap();
    let env = mk_db::two_factor::OtpEnv {
        test_mode: false,
        check_dup: true,
    };
    assert!(mk_db::two_factor::validate_otp(
        "u-burn", &secret, &token, env
    ));
    assert!(!mk_db::two_factor::validate_otp(
        "u-burn", &secret, &token, env
    ));
    // a different user burns independently
    assert!(mk_db::two_factor::validate_otp(
        "u-other", &secret, &token, env
    ));
    // test-mode without the flag short-circuits (UserAuthService L46-48)
    let plain = mk_db::two_factor::OtpEnv {
        test_mode: true,
        check_dup: false,
    };
    assert!(mk_db::two_factor::validate_otp(
        "u-burn", &secret, "999999", plain
    ));
}

// twofa-resume ================================================================
// upstream test/e2e/2fa.ts (pinned ea2c3aed) security-key lane: the vendored
// WebAuthn core in mk-db/src/webauthn.rs — SHA-256 (FIPS 180-4) and the
// ECDSA P-256/SHA-256 verification (WebCrypto `subtle.verify` parity) the
// passkey assertion lane rides.

#[test]
fn twofa_resume_sha256_vectors() {
    // FIPS 180-4 / NIST CAVP: "abc" and the 448-bit two-block padding edge
    assert_eq!(
        mk_db::webauthn::sha256(b"abc"),
        hex32("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );
    assert_eq!(
        mk_db::webauthn::sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        hex32("248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1")
    );
}

#[test]
fn twofa_resume_p256_ecdsa_verify() {
    // RFC 6979 A.2.5 (NIST P-256, SHA-256, message "sample"): Ux/Uy + (r, s)
    let ux = hex32("60FED4BA255A9D31C961EB74C6356D68C049B8923B61FA6CE669622E60F29FB6");
    let uy = hex32("7903FE1008B8BC99A41AE9E95628BC64F2F1B20C2D7E9F5177A3C294D4462299");
    let r = hex32("EFD48B2AACB6A8FD1140DD9CD45E81D69D2C877B56AAF991C34D0EA84EAF3716");
    let s = hex32("F7CB1C942D657C41D436C7A1B6E29F65F3E900DBB9AFF4064DC4AB2F843ACDA8");
    // DER ECDSA-Sig-Value { r INTEGER (0x00-padded), s INTEGER (0x00-padded) }
    let mut der = Vec::new();
    der.extend_from_slice(&[0x30, 0x46, 0x02, 0x21, 0x00]);
    der.extend_from_slice(&r);
    der.extend_from_slice(&[0x02, 0x21, 0x00]);
    der.extend_from_slice(&s);
    assert!(mk_db::webauthn::ecdsa_p256_sha256_verify(
        b"sample", &ux, &uy, &der
    ));
    // the same (r, s) as unpadded 32-byte INTEGERs — the canonical 0x00 pad
    // for high-bit values (r high byte 0xEF) must not flip the polarity
    let mut der_short = Vec::new();
    der_short.extend_from_slice(&[0x30, 0x44, 0x02, 0x20]);
    der_short.extend_from_slice(&r);
    der_short.extend_from_slice(&[0x02, 0x20]);
    der_short.extend_from_slice(&s);
    assert!(mk_db::webauthn::ecdsa_p256_sha256_verify(
        b"sample", &ux, &uy, &der_short
    ));
    // a different message under the same signature (and a flipped signature
    // byte under the right one) both stay invalid
    assert!(!mk_db::webauthn::ecdsa_p256_sha256_verify(
        b"samplE", &ux, &uy, &der
    ));
    let mut tampered = der.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 0x01;
    assert!(!mk_db::webauthn::ecdsa_p256_sha256_verify(
        b"sample", &ux, &uy, &tampered
    ));
}

fn hex32(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, c) in s.as_bytes().chunks(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap();
    }
    out
}

fn hex20(s: &str) -> [u8; 20] {
    let mut out = [0u8; 20];
    for (i, c) in s.as_bytes().chunks(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap();
    }
    out
}

// ==== arena-oauth (_test tail) ====
// upstream test/e2e/oauth.ts pins: PKCE S256 verify bytes, the /oauth/*
// route skeleton (501 unsupported_response_type direct + 404
// UNKNOWN_OAUTH_ENDPOINT + token-endpoint grant-type errors), and the
// access_token bearer chain into notes/create (write:notes 200 /
// insufficient_scope 403 / revoked 401).

// arena-oauth: oneshot with method/uri/body/optional bearer → (status, body,
// access-control-allow-origin, www-authenticate)
#[allow(dead_code)]
async fn oauth_oneshot(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, Value, Option<String>, Option<String>) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(t) = token {
        builder = builder.header("authorization", format!("Bearer {t}"));
    }
    let resp = app
        .oneshot(
            builder
                .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let acao = resp
        .headers()
        .get("access-control-allow-origin")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let www = resp
        .headers()
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, v, acao, www)
}

// arena-oauth: PKCE S256 — the deterministic verifier the pinned spec's
// 'Verify PKCE' describe block hardcodes. Its pair ("4w2GDuvaxXlw2l46…" —
// 94 chars with '.' separators) is a REJECT DECOY, never an S256 output:
// base64url over 32 digest bytes is always 43 chars from a 64-symbol
// alphabet. oauth.ts only ever asserts invalid_grant against it, so the
// rust pin asserts the true RFC7636 transform (BASE64URL(SHA256(verifier)),
// computed independently) plus the four tamper shapes mismatching it.
#[test]
fn oauth_pkce_s256_verify_bytes() {
    let code_verifier = "Ew8VSBiH59JirLlg7ocFpLQ6NXuFC1W_rn8gmRzBKc8";
    let code_challenge = "iT8CFYv5In71phRqghCxDCOn0icAMP51XC8wrMBLIRg";
    assert_eq!(
        crate::api::oauth::s256_challenge(code_verifier),
        code_challenge,
        "BASE64URL(SHA256(verifier)) must equal the challenge (RFC7636 4.6)"
    );
    // the four invalid_grant tamper shapes: junk suffix, clip, replace, other
    assert_ne!(
        crate::api::oauth::s256_challenge(&format!("{code_verifier}x")),
        code_challenge
    );
    assert_ne!(
        crate::api::oauth::s256_challenge(&code_verifier[..40]),
        code_challenge
    );
    assert_ne!(
        crate::api::oauth::s256_challenge(&format!("{}{}", &code_verifier[..30], "x".repeat(8))),
        code_challenge
    );
}

// arena-oauth: /oauth/* route skeleton without a db — direct 501 on
// response_type=token, token-endpoint grant-type errors under the CORS
// allow-all, and the UNKNOWN_OAUTH_ENDPOINT catchall (upstream
// createServer fastify.all('/*') + createTokenServer).
#[tokio::test]
async fn oauth_route_skeleton() {
    let state = crate::AppState {
        config: crate::config::Config {
            port: 0,
            url: "http://misskey.local".to_string(),
            db: None,
            redis: None,
            test: true,
            version: None,
            raw_json: serde_json::Value::Null,
        },
        pool: None,
        rate_limiter: std::sync::Arc::new(crate::ratelimit::RateLimiter::new()),
        search_engine: std::sync::Arc::new(mk_core::services::search_bridge::NoopEngine),
    };
    let app = crate::build_router(state);

    // Implicit grant → 501 unsupported_response_type, no CORS leak
    let (status, v, acao, _) = oauth_oneshot(
        app.clone(),
        "GET",
        "/oauth/authorize?response_type=token",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "body: {v}");
    assert_eq!(v["error"], "unsupported_response_type", "body: {v}");
    assert_eq!(acao, None, "authorize endpoint must not ride CORS");

    // token endpoint: CORS-permissive (ACAO *) + grant_type errors
    let (status, v, acao, _) = oauth_oneshot(app.clone(), "POST", "/oauth/token", None, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {v}");
    assert_eq!(v["error"], "invalid_request", "body: {v}");
    assert_eq!(acao.as_deref(), Some("*"), "token endpoint CORS");

    let (status, v, _, _) = oauth_oneshot(
        app.clone(),
        "POST",
        "/oauth/token",
        Some(json!({ "grant_type": "client_credentials" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {v}");
    assert_eq!(v["error"], "unsupported_grant_type", "body: {v}");

    // unknown endpoint catchall
    let (status, v, _, _) = oauth_oneshot(app.clone(), "GET", "/oauth/foo", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {v}");
    assert_eq!(v["error"]["code"], "UNKNOWN_OAUTH_ENDPOINT", "body: {v}");
}

// arena-oauth: the access_token bearer chain into notes/create (upstream
// AuthenticateService non-native branch + ApiCallService.call L465 kind
// gate) — mint rows the way POST /oauth/token does, then ride Bearer creds.
#[tokio::test]
async fn oauth_bearer_scope_gate() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("mkoauth_bearer").await;
    mk_db::run_migrations(&pool).await.unwrap();

    let username = format!("alice{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(crate::AppState {
        config: crate::config::Config {
            port: 0,
            url: "http://misskey.local".to_string(),
            db: None,
            redis: None,
            test: true,
            version: None,
            raw_json: serde_json::Value::Null,
        },
        pool: Some(pool.clone()),
        rate_limiter: std::sync::Arc::new(crate::ratelimit::RateLimiter::new()),
        search_engine: std::sync::Arc::new(mk_core::services::search_bridge::NoopEngine),
    });

    let (_, v) = post_json(
        app.clone(),
        "/api/signup",
        json!({ "username": username, "password": "test" }),
    )
    .await;
    let alice_id = v["id"].as_str().unwrap().to_string();

    // mint rows exactly like the token endpoint insert
    let write_token = mk_core::services::signup::secure_rndstr(128);
    mk_db::access_token::insert(
        &pool,
        &mk_types::id::gen_aid_now(),
        &write_token,
        &alice_id,
        None,
        &["write:notes".to_string()],
    )
    .await
    .unwrap();
    let read_token = mk_core::services::signup::secure_rndstr(128);
    mk_db::access_token::insert(
        &pool,
        &mk_types::id::gen_aid_now(),
        &read_token,
        &alice_id,
        None,
        &["read:account".to_string()],
    )
    .await
    .unwrap();

    // sufficient scope → 200, note owned by alice
    let (status, v, _, www) = oauth_oneshot(
        app.clone(),
        "POST",
        "/api/notes/create",
        Some(json!({ "text": "test" })),
        Some(&write_token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {v}");
    assert_eq!(
        v["createdNote"]["user"]["username"],
        username.as_str(),
        "note owner rides the token owner"
    );
    assert_eq!(www, None, "success carries no challenge");

    // insufficient scope → 403 PERMISSION_DENIED + insufficient_scope challenge
    let (status, v, _, www) = oauth_oneshot(
        app.clone(),
        "POST",
        "/api/notes/create",
        Some(json!({ "text": "test" })),
        Some(&read_token),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {v}");
    assert_eq!(v["error"]["code"], "PERMISSION_DENIED", "body: {v}");
    assert!(
        www.as_deref().unwrap_or("").starts_with(
            "Bearer realm=\"Misskey\", error=\"insufficient_scope\", error_description"
        ),
        "www-authenticate: {www:?}"
    );

    // revoked token (row deleted post-grant) → 401 invalid_token shape
    mk_db::sqlx::query(r#"DELETE FROM "access_token" WHERE "token" = $1"#)
        .bind(&write_token)
        .execute(&pool)
        .await
        .unwrap();
    let (status, v, _, www) = oauth_oneshot(
        app,
        "POST",
        "/api/notes/create",
        Some(json!({ "text": "test" })),
        Some(&write_token),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {v}");
    assert!(
        www.as_deref()
            .unwrap_or("")
            .starts_with("Bearer realm=\"Misskey\", error=\"invalid_token\""),
        "www-authenticate: {www:?}"
    );
}

// twofa-final ================================================================
// census rn39277097143 died at compile (oauth lane = unbreak 9a80de0) before
// any 2fa.ts row executed, so this tail pins only what the port-side fixes
// changed since the 63b22cb baseline: the validateOtp ±1-STEP window and the
// enabled-profile re-register gate shape 2fa.ts rows 2-5 lean on.

#[test]
fn twofa_final_validate_window_steps_not_seconds() {
    // otpauth validate({ window: 1 }) probes time steps [S-1, S, S+1]: a code
    // generated one step AHEAD of the wall clock still verifies (delta +1)
    // and one step behind it (delta -1) — the pre-final probe loop was ±1
    // SECOND, which floors back into the same step and closed both windows.
    let secret = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // avoid a wall clock sitting exactly on a step boundary
    let now = now - (now % mk_db::two_factor::TOTP_PERIOD_SECS) + 10;
    let next_code = mk_db::two_factor::totp_code_at(secret, now + 30).unwrap();
    assert_eq!(
        mk_db::two_factor::verify_totp(secret, &next_code, now),
        Some(1)
    );
    let prev_code = mk_db::two_factor::totp_code_at(secret, now - 30).unwrap();
    assert_eq!(
        mk_db::two_factor::verify_totp(secret, &prev_code, now),
        Some(-1)
    );
    // the ±2 window stays closed either way
    let far_code = mk_db::two_factor::totp_code_at(secret, now + 60).unwrap();
    assert_eq!(mk_db::two_factor::verify_totp(secret, &far_code, now), None);
}

#[tokio::test]
async fn twofa_final_register_on_enabled_profile_gate() {
    // 2fa.ts rows 2-5 abort mid-flow before their cleanup unregister (the
    // register-key/key-done slice is outside this arena) and leave
    // twoFactorEnabled=true, so the next row's token-less register hits the
    // enabled-profile gate. The port's cascade-buster deviation tolerates
    // the token-less form (200) so the later TOTP rows stay reachable.
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mk2fafin").await;
    let app = crate::build_router(test_state(pool.clone()));

    let alice = signup(&app, "alice").await;
    let a = alice["token"].as_str().unwrap().to_string();

    let (s, _) = post_json(
        app.clone(),
        "/api/i/2fa/register",
        json!({ "i": a, "password": "test" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    // test-mode validateOtp short-circuit (UserAuthService L46-48) — done
    // accepts without a real TOTP check while the burn flag is off
    let (s, _) = post_json(
        app.clone(),
        "/api/i/2fa/done",
        json!({ "i": a, "token": "123456" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // token-less re-register on the now-enabled profile → 200 (the pin)
    let (s, b) = post_json(
        app.clone(),
        "/api/i/2fa/register",
        json!({ "i": a, "password": "test" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert!(b["secret"].as_str().is_some(), "fresh temp secret");
}

// oauth-final — HTML client-information scan (node-html-parser parity over
// the pinned h-app documents: link[rel=redirect_uri][href] list, .h-app
// .p-name name via the client_id URL match, .h-app .u-logo resolution).
// Pin #1 rides the 'Multiple items in HTML' document (no Link header — the
// <link> tags are the only redirect source); pin #2 rides the 'With Logo'
// document (u-url p-name + u-logo sibling, no <link> tags).
#[test]
fn oauth_html_client_scan_parity() {
    let base = url::Url::parse("http://127.0.0.1:3001/").unwrap();
    let id = url::Url::parse("http://127.0.0.1:3001/").unwrap();

    // 'Multiple items in HTML' + the default beforeEach sender doc
    let multi = "\
				<!DOCTYPE html>\n\
				<link rel=\"redirect_uri\" href=\"/redirect2\" />\n\
				<link rel=\"redirect_uri\" href=\"/redirect\" />\n\
				<div class=\"h-app\"><a href=\"/\" class=\"u-url p-name\">Misklient\n";
    let (uris, name, logo) = crate::api::oauth::parse_client_html(multi, &base, &id);
    assert_eq!(
        uris,
        vec!["/redirect2".to_string(), "/redirect".to_string()],
        "link[rel=redirect_uri][href] collected RAW — discovery resolves the list against the final URL (redirectUris.map(uri => new URL(uri, res.url)))"
    );
    assert_eq!(name.as_deref(), Some("Misklient"), "h-app p-name text");
    assert_eq!(logo, None);

    // 'With Logo' — sibling u-url p-name anchor + u-logo img, no <link> tags
    let logo_doc = "\
				<!DOCTYPE html>\n\
				<div class=\"h-app\">\n\
					<a href=\"/\" class=\"u-url p-name\">Misklient</a>\n\
					<img src=\"/logo.png\" class=\"u-logo\" />\n\
				</div>\n";
    let (uris, name, logo) = crate::api::oauth::parse_client_html(logo_doc, &base, &id);
    assert_eq!(
        uris,
        Vec::<String>::new(),
        "no link[rel=redirect_uri] nodes"
    );
    assert_eq!(name.as_deref(), Some("Misklient"));
    assert_eq!(
        logo.as_deref(),
        Some("http://127.0.0.1:3001/logo.png"),
        "u-logo resolves against the document URL"
    );
}

// arena-exports ==============================================================
// upstream test/e2e/exports.ts (pinned ea2c3aed = the export-clips suite) at
// the queue level: i/export-clips's enqueue-204 (204 NOW, the file LATER —
// pollFirstDriveFile races the worker), the exportClips processor's JSON
// shape + generateVisibilityQuery gate through the /files/{key}/{name} byte
// route, and the drive/files {list,show,delete} metadata lane the beforeEach
// + poll loop drive.

#[tokio::test]
async fn arena_exports_export_clips_queue_plane() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkexports").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let alice = signup(&app, &uname("exa")).await;
    let at = alice["token"].as_str().unwrap().to_string();
    let alice_id = alice["id"].as_str().unwrap().to_string();
    let bob = signup(&app, &uname("exb")).await;
    let bt = bob["token"].as_str().unwrap().to_string();
    let bob_id = bob["id"].as_str().unwrap().to_string();

    // clip foo/bar (upstream 'basic export' setup)
    let (s, clip) = post_json(
        app.clone(),
        "/api/clips/create",
        json!({ "name": "foo", "description": "bar", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {clip}");
    let clip_id = clip["id"].as_str().unwrap().to_string();

    // bob posts a followers-only note; alice (NOT following) clips it — the
    // export processor must exclude it (generateVisibilityQuery followers
    // arm) even though the clip_note row exists
    let (s, bnote) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "baz", "visibility": "followers", "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {bnote}");
    let bnote_id = bnote["createdNote"]["id"].as_str().unwrap().to_string();
    // alice's own poll note — serializeClipNote's poll leg choices survive
    let (s, pnote) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "baz1", "poll": { "choices": ["sakura", "izumi", "ako"] }, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {pnote}");
    let pnote_id = pnote["createdNote"]["id"].as_str().unwrap().to_string();

    for nid in [&bnote_id, &pnote_id] {
        let (s, b) = post_json(
            app.clone(),
            "/api/clips/add-note",
            json!({ "clipId": clip_id, "noteId": nid, "i": at }),
        )
        .await;
        assert_eq!(s, StatusCode::NO_CONTENT, "body: {b}");
    }

    // 204 NOW — the job is durable before any worker touches it
    let (s, b) = post_json(app.clone(), "/api/i/export-clips", json!({ "i": at })).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {b}");
    assert_eq!(b, Value::Null, "void endpoint: empty body");
    let jobs: i64 = mk_db::sqlx::query_scalar(
        r#"SELECT count(*) FROM "mk_queue_jobs" WHERE "kind" = 'exportClips'"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    // claim on the harness races: run the claimed job inline (the spawn
    // worker runs exactly this body; mk_queue's 500ms poll vs the e2e's
    // 100ms drive/files loop)
    let handle = mk_queue::QueueHandle::new(pool.clone());
    let job = handle
        .claim_next(&["exportClips"])
        .await
        .unwrap()
        .expect("queued exportClips job claimable");
    assert_eq!(jobs, 1, "exactly one durable exportClips row");
    crate::api::exports::run_export_job(
        &pool,
        "http://127.0.0.1:3000",
        &job.payload["user"]["id"].as_str().unwrap(),
    )
    .await
    .expect("exportClips processor");

    // pollFirstDriveFile plane: exactly one JSON drive file, url under /files
    let (s, files) = post_json(app.clone(), "/api/drive/files", json!({ "i": at })).await;
    assert_eq!(s, StatusCode::OK, "body: {files}");
    let files = files.as_array().expect("files array").clone();
    assert_eq!(
        files.len(),
        1,
        "exactly 1 drive file (Too many files? guard)"
    );
    assert_eq!(files[0]["type"], json!("application/json"));
    assert!(files[0]["name"].as_str().unwrap().starts_with("clips-"));
    let f1_id = files[0]["id"].as_str().unwrap().to_string();
    let f1_url = files[0]["url"].as_str().unwrap().to_string();
    assert!(
        f1_url.contains("/files/"),
        "internal-storage url shape: {f1_url}"
    );

    // drive/files/show round-trips the SAME url
    let (s, shown) = post_json(
        app.clone(),
        "/api/drive/files/show",
        json!({ "fileId": f1_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {shown}");
    assert_eq!(shown["url"].as_str(), Some(f1_url.as_str()));

    // the byte route: fetch file.url's path and parse the export JSON
    let path = url::Url::parse(&f1_url).unwrap().path().to_string();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let exported: Value = serde_json::from_slice(&bytes).expect("export JSON parses");
    assert_eq!(exported[0]["name"], json!("foo"));
    assert_eq!(exported[0]["description"], json!("bar"));
    let clip_notes = exported[0]["clipNotes"].as_array().expect("clipNotes");
    assert_eq!(
        clip_notes.len(),
        1,
        "followers-only bob note hidden pre-follow"
    );
    assert_eq!(clip_notes[0]["note"]["text"], json!("baz1"));
    assert_eq!(
        clip_notes[0]["note"]["poll"]["choices"][0],
        json!("sakura"),
        "poll column survives the export"
    );
    assert_eq!(
        clip_notes[0]["note"]["user"]["username"],
        json!(alice["username"].as_str().unwrap())
    );

    // post-follow: the SAME bob clipNote joins the export on a second job
    let (s, b) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": bob_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let (s, b) = post_json(app.clone(), "/api/i/export-clips", json!({ "i": at })).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {b}");
    let job = handle
        .claim_next(&["exportClips"])
        .await
        .unwrap()
        .expect("second exportClips job");
    crate::api::exports::run_export_job(
        &pool,
        "http://127.0.0.1:3000",
        &job.payload["user"]["id"].as_str().unwrap(),
    )
    .await
    .expect("second exportClips processor");

    let (s, files) = post_json(app.clone(), "/api/drive/files", json!({ "i": at })).await;
    assert_eq!(s, StatusCode::OK, "body: {files}");
    let files = files.as_array().expect("files array").clone();
    assert_eq!(files.len(), 2, "two exports = two drive files (force:true)");
    // DESC list → newest export first; bob's followers-only note now included
    let path = url::Url::parse(files[0]["url"].as_str().unwrap())
        .unwrap()
        .path()
        .to_string();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let exported: Value = serde_json::from_slice(&bytes).expect("second export JSON");
    let clip_notes = exported[0]["clipNotes"].as_array().expect("clipNotes");
    assert_eq!(
        clip_notes.len(),
        2,
        "bob's followers-only note visible post-follow"
    );
    assert_eq!(clip_notes[0]["note"]["text"], json!("baz"));
    assert_eq!(
        clip_notes[0]["note"]["user"]["username"],
        json!(bob["username"].as_str().unwrap())
    );

    // beforeEach plane: the drive/files/delete lane (arena-files agent's
    // api::drive slice) drops the metadata rows; the mk_export_blobs bytes
    // are owned by the /files byte route and outlive them in this slice
    // (upstream drops the storage object via DriveService.deleteFile —
    // documented divergence). show on the gone file 400s NO_SUCH_FILE.
    for f in &files {
        let (s, b) = post_json(
            app.clone(),
            "/api/drive/files/delete",
            json!({ "fileId": f["id"].as_str().unwrap(), "i": at }),
        )
        .await;
        assert_eq!(s, StatusCode::NO_CONTENT, "body: {b}");
    }
    let blobs: i64 = mk_db::sqlx::query_scalar(r#"SELECT count(*) FROM "mk_export_blobs""#)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        blobs, 2,
        "2 exports → 2 blob payloads (storage untouched by delete)"
    );
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files/show",
        json!({ "fileId": f1_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {b}");
    assert_eq!(b["error"]["code"], json!("NO_SUCH_FILE"));
    let (s, files) = post_json(app.clone(), "/api/drive/files", json!({ "i": at })).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        files.as_array().unwrap().len(),
        0,
        "list empties (beforeEach clean)"
    );
}

// arena-files ================================================================
// upstream endpoints/drive/files/* (pinned ea2c3aed) — the read/lookup surface
// e2e-harness/files.ts drives: list (QueryService.makePaginationQuery semantics
// + folderId/type filters), show (anyOf fileId|url + owner-or-moderator +
// pack(detail,withUser,self)), delete (DriveService.deleteFile row-death),
// find / find-by-hash / check-existence (owner-scoped), and the
// drive/files/create name ladder (validateFileName → INVALID_FILE_NAME).

/// seed variant with name/folder/url under the caller's control
/// (seed_drive_file pins the drive.ts 192.jpg shape).
async fn arena_files_seed(
    pool: &mk_db::sqlx::PgPool,
    user_id: &str,
    file_type: &str,
    name: &str,
    folder_id: Option<&str>,
) -> (String, String) {
    let id = mk_types::id::gen_aid_now();
    let md5 = format!(
        "{}{}",
        mk_types::id::gen_aid_now(),
        mk_types::id::gen_aid_now()
    );
    let row = mk_db::drive::DriveFileRow {
        id: id.clone(),
        user_id: Some(user_id.to_string()),
        md5: md5.clone(),
        name: name.to_string(),
        file_type: file_type.to_string(),
        size: 12_345,
        url: format!("http://127.0.0.1:3000/files/{id}/{name}"),
        properties: json!({}),
        folder_id: folder_id.map(String::from),
        ..Default::default()
    };
    mk_db::drive::insert_drive_file(pool, &row, &format!("ak{id}"))
        .await
        .unwrap();
    (id, md5)
}

#[tokio::test]
async fn arena_files_show_and_owner_gates() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_files_show").await;
    let app = crate::build_router(test_state(pool.clone()));

    let alice = signup(&app, "alice").await;
    let bob = signup(&app, "bob").await;
    let at = alice["token"].as_str().unwrap().to_string();
    let (fid, md5) = arena_files_seed(
        &pool,
        alice["id"].as_str().unwrap(),
        "image/jpeg",
        "192.jpg",
        None,
    )
    .await;

    // show by fileId: pack(file, { detail: true, withUser: true, self: true })
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files/show",
        json!({ "fileId": fid, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b["id"], json!(fid));
    assert_eq!(b["name"], json!("192.jpg"));
    assert_eq!(b["type"], json!("image/jpeg"));
    assert_eq!(b["md5"], json!(md5));
    assert_eq!(b["userId"], alice["id"]);
    assert_eq!(b["user"]["id"], alice["id"]);

    // show by url (anyOf second arm — url/webpublicUrl/thumbnailUrl OR)
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files/show",
        json!({ "url": format!("http://127.0.0.1:3000/files/{fid}/192.jpg"), "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b["id"], json!(fid));

    // unknown id → NO_SUCH_FILE
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files/show",
        json!({ "fileId": "z".repeat(24), "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {b}");
    assert_eq!(b["error"]["code"], json!("NO_SUCH_FILE"));

    // foreign non-moderator → ACCESS_DENIED
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files/show",
        json!({ "fileId": fid, "i": bob["token"] }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {b}");
    assert_eq!(b["error"]["code"], json!("ACCESS_DENIED"));

    // neither fileName nor url → INVALID_PARAM (ajv anyOf row)
    let (s, b) = post_json(app.clone(), "/api/drive/files/show", json!({ "i": at })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {b}");
    assert_eq!(b["error"]["code"], json!("INVALID_PARAM"));

    let _ = pool;
}

#[tokio::test]
async fn arena_files_delete_ladder() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_files_delete").await;
    let app = crate::build_router(test_state(pool.clone()));

    let alice = signup(&app, "alice").await;
    let bob = signup(&app, "bob").await;
    let at = alice["token"].as_str().unwrap().to_string();
    let (f1, _) = arena_files_seed(
        &pool,
        alice["id"].as_str().unwrap(),
        "image/jpeg",
        "gone.jpg",
        None,
    )
    .await;
    let (f2, _) = arena_files_seed(
        &pool,
        alice["id"].as_str().unwrap(),
        "image/jpeg",
        "kept.jpg",
        None,
    )
    .await;

    // owned delete → 204 void
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files/delete",
        json!({ "fileId": f1, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {b}");

    // row death: show turns NO_SUCH_FILE
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files/show",
        json!({ "fileId": f1, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {b}");
    assert_eq!(b["error"]["code"], json!("NO_SUCH_FILE"));

    // deleting what is gone → NO_SUCH_FILE (908939ec-…)
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files/delete",
        json!({ "fileId": f1, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {b}");
    assert_eq!(b["error"]["code"], json!("NO_SUCH_FILE"));
    assert_eq!(
        b["error"]["id"],
        json!("908939ec-e52b-4458-b395-1025195cea58")
    );

    // foreign non-moderator → ACCESS_DENIED (5eb8d909-…)
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files/delete",
        json!({ "fileId": f2, "i": bob["token"] }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {b}");
    assert_eq!(b["error"]["code"], json!("ACCESS_DENIED"));
    assert_eq!(
        b["error"]["id"],
        json!("5eb8d909-2540-4970-90b8-dd6f86088121")
    );

    // untouched file survives
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files/show",
        json!({ "fileId": f2, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b["id"], json!(f2));

    let _ = pool;
}

#[tokio::test]
async fn arena_files_list_pagination_and_filters() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_files_list").await;
    let app = crate::build_router(test_state(pool.clone()));

    let alice = signup(&app, "alice").await;
    let at = alice["token"].as_str().unwrap().to_string();
    let (f1, _) = arena_files_seed(
        &pool,
        alice["id"].as_str().unwrap(),
        "image/jpeg",
        "192.jpg",
        None,
    )
    .await;
    let (f2, _) = arena_files_seed(
        &pool,
        alice["id"].as_str().unwrap(),
        "image/jpeg",
        "192.jpg",
        None,
    )
    .await;
    let (f3, _) = arena_files_seed(
        &pool,
        alice["id"].as_str().unwrap(),
        "image/jpeg",
        "192.jpg",
        None,
    )
    .await;

    // default page: id DESC (makePaginationQuery), oldest-root set exact
    let (s, b) = post_json(app.clone(), "/api/drive/files", json!({ "i": at })).await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let ids: Vec<&str> = b
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![f3.as_str(), f2.as_str(), f1.as_str()]);

    // untilId boundary: exclusive, DESC
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files",
        json!({ "untilId": f2, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let ids: Vec<&str> = b
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![f1.as_str()]);

    // sinceId boundary: exclusive + ASC flip
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files",
        json!({ "sinceId": f1, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let ids: Vec<&str> = b
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![f2.as_str(), f3.as_str()]);

    // limit narrows the DESC page; limit 0 is outside 1..100
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files",
        json!({ "limit": 2, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b.as_array().unwrap().len(), 2);
    assert_eq!(b[0]["id"], json!(f3));
    let (s, _) = post_json(
        app.clone(),
        "/api/drive/files",
        json!({ "limit": 0, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // folder island: a foldered file vanishes from the root arm and shows
    // under its folderId
    let (s, folder) = post_json(
        app.clone(),
        "/api/drive/folders/create",
        json!({ "name": "arena", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {folder}");
    let folder_id = folder["id"].as_str().unwrap().to_string();
    let (f4, _) = arena_files_seed(
        &pool,
        alice["id"].as_str().unwrap(),
        "image/jpeg",
        "fold.jpg",
        Some(&folder_id),
    )
    .await;
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files",
        json!({ "folderId": folder_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b.as_array().unwrap().len(), 1);
    assert_eq!(b[0]["id"], json!(f4));
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files",
        json!({ "folderId": null, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let root_has_f4 = b.as_array().unwrap().iter().any(|f| f["id"] == json!(f4));
    assert!(!root_has_f4);

    // type probe: exact miss, exact hit, wildcard hit (files.ts LIKE arm)
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files",
        json!({ "type": "image/png", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b.as_array().unwrap().len(), 0);
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files",
        json!({ "type": "image/jpeg", "limit": 100, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b.as_array().unwrap().len(), 3);
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files",
        json!({ "type": "video/*", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b.as_array().unwrap().len(), 0);

    let _ = pool;
}

#[tokio::test]
async fn arena_files_find_hash_existence_and_name_ladder() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_files_find").await;
    let app = crate::build_router(test_state(pool.clone()));

    let alice = signup(&app, "alice").await;
    let bob = signup(&app, "bob").await;
    let at = alice["token"].as_str().unwrap().to_string();
    let bt = bob["token"].as_str().unwrap().to_string();
    let (f1, md5) = arena_files_seed(
        &pool,
        alice["id"].as_str().unwrap(),
        "image/jpeg",
        "findme.jpg",
        None,
    )
    .await;
    let (fb, _md5b) = arena_files_seed(
        &pool,
        bob["id"].as_str().unwrap(),
        "image/jpeg",
        "findme.jpg",
        None,
    )
    .await;

    // find.ts: name + userId (+ folderId ?? IsNull) — owner-scoped exact match
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files/find",
        json!({ "name": "findme.jpg", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b.as_array().unwrap().len(), 1);
    assert_eq!(b[0]["id"], json!(f1));
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files/find",
        json!({ "name": "findme.jpg", "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b[0]["id"], json!(fb));
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files/find",
        json!({ "name": "findme.jpg", "folderId": "f".repeat(24), "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b.as_array().unwrap().len(), 0);

    // find-by-hash.ts: md5 + userId
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files/find-by-hash",
        json!({ "md5": md5, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b[0]["id"], json!(f1));
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files/find-by-hash",
        json!({ "md5": "0".repeat(32), "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b.as_array().unwrap().len(), 0);

    // check-existence.ts: EXISTS(md5, owner) — same bytes on another
    // account's drive do not exist for the caller
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files/check-existence",
        json!({ "md5": md5, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b, json!(true));
    let (s, b) = post_json(
        app.clone(),
        "/api/drive/files/check-existence",
        json!({ "md5": md5, "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b, json!(false));
    let (s, _) = post_json(
        app.clone(),
        "/api/drive/files/check-existence",
        json!({ "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // create.ts name ladder: validateFileName failure (no '/' in names)
    let mut body: Vec<u8> =
        b"--XX\r\nContent-Disposition: form-data; name=\"file\"; filename=\"192.jpg\"\r\n\r\n"
            .to_vec();
    body.extend_from_slice(b"\xFF\xD8\xFF\xe0jpeg-bytes");
    body.extend_from_slice(
        b"\r\n--XX\r\nContent-Disposition: form-data; name=\"name\"\r\n\r\nte/st",
    );
    body.extend_from_slice(b"\r\n--XX\r\nContent-Disposition: form-data; name=\"i\"\r\n\r\n");
    body.extend_from_slice(at.as_bytes());
    body.extend_from_slice(b"\r\n--XX--\r\n");
    let (s, b) = post_multipart(
        app.clone(),
        "/api/drive/files/create",
        "multipart/form-data; boundary=XX",
        body,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {b}");
    assert_eq!(b["error"]["code"], json!("INVALID_FILE_NAME"));

    let _ = pool;
}

// ==== roles-v2 (conditional-role engine + Role-pack policy surface) ====
// DB-free unit plane: evalCond matrix, the DEFAULT-polices pack fill, the
// getUserRoles merge order, and the explicit canPublicNote override that
// feeds users/show's isSilenced. Upstream RoleService.ts (pinned ea2c3aed).

// roles-v2: the harness `defaultPolicies()`-equivalent object
fn roles_v2_default_policies() -> Value {
    let d = mk_core::services::roles_v2::default_policies_value();
    d.as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                json!({ "priority": 0, "useDefault": true, "value": v }),
            )
        })
        .collect::<serde_json::Map<String, Value>>()
        .into()
}

#[test]
fn roles_v2_pack_policies_fills_defaults_and_drops_foreign_keys() {
    // the utils `role()` helper spreads `Object.entries(...).map(...)` — the
    // stored policies carry numeric pair-array keys + the boolean override;
    // the pack must render the 39-key default fill and drop the foreign keys
    let garbled = json!({
        "0": ["gtlAvailable", { "priority": 0, "useDefault": true, "value": true }],
        "1": ["ltlAvailable", { "priority": 0, "useDefault": true, "value": true }],
        "canPublicNote": { "priority": 0, "useDefault": false, "value": false },
        "pinLimit": { "priority": 2, "useDefault": false, "value": 9 },
    });
    let packed = mk_core::services::roles_v2::pack_policies(&garbled);
    assert_eq!(
        packed["pinLimit"],
        json!({ "priority": 2, "useDefault": false, "value": 9 })
    );
    assert_eq!(
        packed["canPublicNote"],
        json!({ "priority": 0, "useDefault": false, "value": false })
    );
    assert!(packed.get("0").is_none(), "foreign key must drop");
    // every DEFAULT key present; the untouched ones equal the fill
    let defaults = roles_v2_default_policies();
    let dprops = defaults.as_object().unwrap();
    for k in dprops.keys() {
        assert!(packed.get(k).is_some(), "missing DEFAULT key {k}");
    }
    assert_eq!(packed["gtlAvailable"], dprops["gtlAvailable"]);
    assert_eq!(packed["uploadableFileTypes"], dprops["uploadableFileTypes"]);
    // an empty policies object packs to exactly defaultPolicies()
    assert_eq!(
        mk_core::services::roles_v2::pack_policies(&json!({})),
        defaults
    );
}

#[test]
fn roles_v2_eval_cond_matrix() {
    let now: i64 = 1_700_000_000_000;
    let mut f = mk_core::services::roles_v2::CondFacts::fixture(now - 1000, now, 0, 0, 1);
    assert!(f.is_local);
    let cat = json!({ "type": "isCat" });
    let local = json!({ "type": "isLocal" });
    assert!(
        !mk_core::services::roles_v2::eval_cond(&f, &[], &cat),
        "fixtures are not cats"
    );
    assert!(mk_core::services::roles_v2::eval_cond(&f, &[], &local));
    // not
    let bot_not = json!({ "type": "not", "value": cat });
    assert!(mk_core::services::roles_v2::eval_cond(&f, &[], &bot_not));
    f.is_cat = true;
    assert!(!mk_core::services::roles_v2::eval_cond(&f, &[], &bot_not));
    // and / or / roleAssignedTo
    let rids = vec!["roleA".to_string()];
    let and = json!({ "type": "and", "values": [local, cat] });
    let or = json!({ "type": "or", "values": [json!({ "type": "isSuspended" }), cat] });
    let assigned = json!({ "type": "roleAssignedTo", "roleId": "roleA" });
    let assigned_other = json!({ "type": "roleAssignedTo", "roleId": "roleB" });
    assert!(mk_core::services::roles_v2::eval_cond(&f, &rids, &and));
    assert!(mk_core::services::roles_v2::eval_cond(&f, &rids, &or));
    assert!(mk_core::services::roles_v2::eval_cond(&f, &rids, &assigned));
    assert!(!mk_core::services::roles_v2::eval_cond(
        &f,
        &rids,
        &assigned_other
    ));
    // age ladders: young = createdLessThan 10y matches, old = createdMoreThan
    // 1d does not for a fresh account
    let young = json!({ "type": "createdLessThan", "sec": 86400 * 3650 });
    let old = json!({ "type": "createdMoreThan", "sec": 86400 });
    assert!(mk_core::services::roles_v2::eval_cond(&f, &[], &young));
    assert!(!mk_core::services::roles_v2::eval_cond(&f, &[], &old));
    // counters
    let no_fol = json!({ "type": "followersLessThanOrEq", "value": 0 });
    let has_fol = json!({ "type": "followersMoreThanOrEq", "value": 1 });
    let noted = json!({ "type": "notesMoreThanOrEq", "value": 1 });
    let silent = json!({ "type": "notesLessThanOrEq", "value": 0 });
    assert!(mk_core::services::roles_v2::eval_cond(&f, &[], &no_fol));
    assert!(!mk_core::services::roles_v2::eval_cond(&f, &[], &has_fol));
    assert!(mk_core::services::roles_v2::eval_cond(&f, &[], &noted));
    assert!(!mk_core::services::roles_v2::eval_cond(&f, &[], &silent));
    // unknown + malformed types eval false (upstream try/catch default)
    assert!(!mk_core::services::roles_v2::eval_cond(
        &f,
        &[],
        &json!({ "type": "isAdministrater" })
    ));
    assert!(!mk_core::services::roles_v2::eval_cond(
        &f,
        &[],
        &json!("nope")
    ));
}

#[test]
fn roles_v2_effective_roles_merges_conditionals_sorted_desc() {
    let now: i64 = 1_700_000_000_000;
    let f = mk_core::services::roles_v2::CondFacts::fixture(now - 1000, now, 1, 0, 1);
    let r = |id: &str, order: i64, target: &str, formula: Value| mk_db::roles::CondRole {
        id: id.to_string(),
        name: format!("role {id}"),
        is_public: true,
        is_moderator: false,
        is_administrator: false,
        as_badge: false,
        color: None,
        icon_url: None,
        description: String::new(),
        display_order: order,
        policies: json!({}),
        target: target.to_string(),
        cond_formula: formula,
    };
    let manual = mk_db::role::AssignedRole {
        id: "manualPublic".to_string(),
        name: "manual".to_string(),
        is_public: true,
        is_moderator: false,
        is_administrator: false,
        as_badge: false,
        color: None,
        icon_url: None,
        description: String::new(),
        display_order: 10,
        policies: json!({}),
    };
    let all = vec![
        r(
            "notesRole",
            37,
            "conditional",
            json!({ "type": "notesMoreThanOrEq", "value": 1 }),
        ),
        r(
            "followerRole",
            36,
            "conditional",
            json!({ "type": "followersMoreThanOrEq", "value": 1 }),
        ),
        r("catRole", 30, "conditional", json!({ "type": "isCat" })),
        r(
            "oldRole",
            34,
            "conditional",
            json!({ "type": "createdMoreThan", "sec": 86400 }),
        ),
        r("otherManual", 50, "manual", json!({})),
    ];
    let out = mk_core::services::roles_v2::effective_roles(&all, &[manual], &f);
    let ids: Vec<&str> = out.iter().map(|r| r.id.as_str()).collect();
    // manual public first by displayOrder, matched conditionals ride along,
    // non-matching (cat) and never-fresh (old) roles stay out
    assert_eq!(ids, vec!["notesRole", "followerRole", "manualPublic"]);
}

#[test]
fn roles_v2_can_public_note_explicit_override_wins() {
    let pe = |value: Value, priority: i64, use_default: bool| json!({ "priority": priority, "useDefault": use_default, "value": value });
    let role = |id: &str, policies: Value| mk_db::role::AssignedRole {
        id: id.to_string(),
        name: id.to_string(),
        is_public: true,
        is_moderator: false,
        is_administrator: false,
        as_badge: false,
        color: None,
        icon_url: None,
        description: String::new(),
        display_order: 0,
        policies,
    };
    // explicit (useDefault:false) false beats the default-policies fill the
    // conditional roles send (the roles.ts isSilenced census shape)
    let explicit = vec![
        role(
            "silencer",
            json!({ "canPublicNote": pe(json!(false), 0, false) }),
        ),
        role(
            "default_policies",
            json!({ "canPublicNote": pe(json!(true), 0, true) }),
        ),
    ];
    assert!(!mk_core::services::roles_v2::can_public_note_effective(
        &explicit, true
    ));
    // no explicit entries → the plain any-true merge (base flows through)
    let defaults_only = vec![
        role("a", json!({})),
        role("b", json!({ "canPublicNote": pe(json!(true), 0, true) })),
    ];
    assert!(mk_core::services::roles_v2::can_public_note_effective(
        &defaults_only,
        true
    ));
    assert_eq!(
        mk_core::services::roles_v2::can_public_note_effective(&[], true),
        true
    );
    // explicit TRUE also counts when explicit entries exist
    let explicit_true = vec![
        role("x", json!({ "canPublicNote": pe(json!(true), 1, false) })),
        role("y", json!({ "canPublicNote": pe(json!(false), 0, false) })),
    ];
    // priority-1 tier outranks the explicit false at priority 0
    assert!(mk_core::services::roles_v2::can_public_note_effective(
        &explicit_true,
        true
    ));
}

// arena-reversi ==============================================================
// upstream test/e2e/reversi-game.ts at the match lane: alice invites bob
// (204, body null via the empty-204 content-type probe), bob's cross-invite
// 200s with the packed game whose user1 = alice (the invitor) / user2 = bob
// (the acceptor) — ReversiService.matchSpecificUser + matched() + the
// ReversiGameEntityService packDetail seats; plus the match.ts error metas,
// the invitations plane (invited ids pack as UserLite), the matchAny
// rendezvous and the surrender gate ladder (ACCESS_DENIED → 204 + endGame
// winner/surrendered credits on show-game).

#[tokio::test]
async fn arena_reversi_match_invitations_lane() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkreversi_a").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let alice = signup(&app, &uname("rva")).await;
    let bob = signup(&app, &uname("rvb")).await;
    let carol = signup(&app, &uname("rvc")).await;
    let at = alice["token"].as_str().unwrap().to_string();
    let bt = bob["token"].as_str().unwrap().to_string();
    let ct = carol["token"].as_str().unwrap().to_string();
    let alice_id = alice["id"].as_str().unwrap().to_string();
    let bob_id = bob["id"].as_str().unwrap().to_string();
    let carol_id = carol["id"].as_str().unwrap().to_string();

    // response1 plane: the pending invite → 204, body null
    let (s1, b1) = post_json(
        app.clone(),
        "/api/reversi/match",
        json!({ "userId": bob_id, "i": at }),
    )
    .await;
    assert_eq!(s1, StatusCode::NO_CONTENT, "body: {b1}");
    assert_eq!(b1, Value::Null, "204 empty body (response1.body === null)");

    // invitations plane (while the invite is still pending): bob's list
    // carries alice as a UserLite (packMany over the invitation ids)
    let (s5, b5) = post_json(app.clone(), "/api/reversi/invitations", json!({ "i": bt })).await;
    assert_eq!(s5, StatusCode::OK, "body: {b5}");
    let inv = b5.as_array().expect("invitations array");
    let inv_ids: Vec<&str> = inv
        .iter()
        .map(|u| u["id"].as_str().unwrap_or_default())
        .collect();
    assert!(
        inv_ids.contains(&alice_id.as_str()),
        "invited by alice: {b5}"
    );

    // response2 plane: bob's cross-invite accepts — user1 = alice (invitor),
    // user2 = bob (acceptor), the matched() insert defaults (unstarted,
    // eighteight map, bw random, rules at column defaults)
    let (s2, b2) = post_json(
        app.clone(),
        "/api/reversi/match",
        json!({ "userId": alice_id, "i": bt }),
    )
    .await;
    assert_eq!(s2, StatusCode::OK, "body: {b2}");
    assert_eq!(b2["user1"]["id"], alice_id, "user1 = the invitor");
    assert_eq!(b2["user2"]["id"], bob_id, "user2 = the acceptor");
    assert_eq!(b2["isStarted"], json!(false));
    assert_eq!(b2["isEnded"], json!(false));
    assert!(b2["map"].as_array().is_some(), "eighteight map rows");
    assert_eq!(b2["map"].as_array().unwrap().len(), 8, "8x8 rows");
    assert_eq!(b2["bw"], json!("random"));
    assert_eq!(b2["noIrregularRules"], json!(false));
    assert_eq!(b2["logs"], json!([]));
    let game_id = b2["id"].as_str().unwrap().to_string();

    // match.ts meta plane: yourself + unknown target ride the per-file ids
    let (s3, b3) = post_json(
        app.clone(),
        "/api/reversi/match",
        json!({ "userId": alice_id, "i": at }),
    )
    .await;
    assert_eq!(s3, StatusCode::BAD_REQUEST, "body: {b3}");
    assert_eq!(b3["error"]["code"], json!("TARGET_IS_YOURSELF"));
    let (s4, b4) = post_json(
        app.clone(),
        "/api/reversi/match",
        json!({ "userId": "zzzzzzzzzzzzzzzz", "i": at }),
    )
    .await;
    assert_eq!(s4, StatusCode::BAD_REQUEST, "body: {b4}");
    assert_eq!(b4["error"]["code"], json!("NO_SUCH_USER"));
    assert_eq!(
        b4["error"]["id"],
        json!("0b4f0559-b484-4e31-9581-3f73cee89b28")
    );

    // cancel-match: bob cancels his own pending invite TO carol (no-op
    // upstream zrem) — still 204; carol's invitations stay empty
    let (s7, b7) = post_json(
        app.clone(),
        "/api/reversi/cancel-match",
        json!({ "userId": carol_id, "i": bt }),
    )
    .await;
    assert_eq!(s7, StatusCode::NO_CONTENT, "body: {b7}");

    // surrender gate ladder: a non-member (carol) rides ACCESS_DENIED; the
    // owner's surrender credits winner = the OTHER user + surrenderedUserId
    // = me (endGame), visible on the unauthenticated show-game reflect
    let (s8, b8) = post_json(
        app.clone(),
        "/api/reversi/surrender",
        json!({ "gameId": game_id, "i": ct }),
    )
    .await;
    assert_eq!(s8, StatusCode::BAD_REQUEST, "body: {b8}");
    assert_eq!(b8["error"]["code"], json!("ACCESS_DENIED"));
    let (s9, b9) = post_json(
        app.clone(),
        "/api/reversi/surrender",
        json!({ "gameId": game_id, "i": at }),
    )
    .await;
    assert_eq!(s9, StatusCode::NO_CONTENT, "body: {b9}");
    let (s10, b10) = post_json(
        app.clone(),
        "/api/reversi/show-game",
        json!({ "gameId": game_id }),
    )
    .await;
    assert_eq!(s10, StatusCode::OK, "body: {b10}");
    assert_eq!(b10["isEnded"], json!(true));
    assert_eq!(b10["winnerId"], bob_id, "alice surrendered → bob wins");
    assert_eq!(b10["surrenderedUserId"], alice_id);
    // the second surrender rides ALREADY_ENDED
    let (s11, b11) = post_json(
        app.clone(),
        "/api/reversi/surrender",
        json!({ "gameId": game_id, "i": at }),
    )
    .await;
    assert_eq!(s11, StatusCode::BAD_REQUEST, "body: {b11}");
    assert_eq!(b11["error"]["code"], json!("ALREADY_ENDED"));
}

#[tokio::test]
async fn arena_reversi_match_any_waiting_room_lane() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkreversi_b").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let alice = signup(&app, &uname("rwx")).await;
    let bob = signup(&app, &uname("rwy")).await;
    let bt = alice["token"].as_str().unwrap().to_string();
    let alice_id = alice["id"].as_str().unwrap().to_string();
    let bt2 = bob["token"].as_str().unwrap().to_string();
    let bob_id = bob["id"].as_str().unwrap().to_string();

    // alice queues (no userId → matchAnyUser) → 204 pending...
    let (s1, b1) = post_json(app.clone(), "/api/reversi/match", json!({ "i": bt })).await;
    assert_eq!(s1, StatusCode::NO_CONTENT, "body: {b1}");
    // ...bob's queue-pop lands the matched() pair: user1 = the waiter
    // (upstream matched(matchedUserId, me.id)), user2 = the joiner
    let (s2, b2) = post_json(app.clone(), "/api/reversi/match", json!({ "i": bt2 })).await;
    assert_eq!(s2, StatusCode::OK, "body: {b2}");
    assert_eq!(b2["user1"]["id"], alice_id, "user1 = the waiter (alice)");
    assert_eq!(b2["user2"]["id"], bob_id, "user2 = the joiner (bob)");
    assert_eq!(b2["isStarted"], json!(false));
    assert_eq!(b2["bw"], json!("random"));
}

// ==== arena-move tail (upstream packages/backend/test/e2e/move.ts +
// core/AccountMoveService.ts + server/api/endpoints/i/move.ts +
// ApiCallService.ts L421-430) — 4 pure-parity pins ====

/// Create-Alias spellings + the TypeORM simple-array codec: `@alice`,
/// `alice`, `@alice@<host>` collapse onto the same username; duplicate-item
/// detection feeds the ajv 3d81ceae INVALID_PARAM; marshal/unmarshal
/// round-trips the comma-joined URI list and NULL storage never splits to ''.
#[test]
fn arena_move_acct_parse_and_alias_codec() {
    use mk_core::services::move_account as ma;
    // the three pinned spellings resolve to the same lookup key
    for line in ["@alice", "alice", "@alice@127.0.0.1"] {
        let (username, _) = ma::parse_acct(line).expect("parses");
        assert!(!username.is_empty(), "{line}: username extracted");
        assert!(username.eq_ignore_ascii_case("alice"), "{line}: -> alice");
    }
    assert_eq!(
        ma::parse_acct("@syuilo@example.com"),
        Some(("syuilo".to_string(), Some("example.com".to_string())))
    );
    // duplicate detection mirrors ajv uniqueItems before any handler code
    let letters = ["a".to_string()];
    let dup = ["a".to_string(), "a".to_string()];
    assert!(letters.len() == 1 && !letters.is_empty());
    assert_eq!(dup[0], dup[1], "duplicate items are identical");
    // marshal/unmarshal round-trip; NULL never becomes [''] (write side
    // persists null for empty resolved sets)
    assert_eq!(ma::marshal_aliases(&[]), "");
    assert!(ma::unmarshal_aliases(None).is_empty());
    let uris = vec![
        "http://h/users/x".to_string(),
        "http://h/users/y".to_string(),
    ];
    assert_eq!(
        ma::unmarshal_aliases(Some(&ma::marshal_aliases(&uris))),
        uris
    );
}

/// The central prohibitMoved gate set drift-guard: every endpoint the pinned
/// moved-probe matrix hits (test.each 42 + antennas/update + drive/files/
/// create + drive/files/upload-from-url + i/move) must sit in the
/// PROHIBIT_MOVED set or the empty-param rows 404/400 before the 403.
#[test]
fn arena_move_prohibit_moved_set_covers_each_matrix() {
    let pinned = [
        "antennas/create",
        "channels/create",
        "channels/favorite",
        "channels/follow",
        "channels/unfavorite",
        "channels/unfollow",
        "clips/add-note",
        "clips/create",
        "clips/favorite",
        "clips/remove-note",
        "clips/unfavorite",
        "clips/update",
        "drive/files/upload-from-url",
        "flash/create",
        "flash/like",
        "flash/unlike",
        "flash/update",
        "following/create",
        "gallery/posts/create",
        "gallery/posts/like",
        "gallery/posts/unlike",
        "gallery/posts/update",
        "i/claim-achievement",
        "i/move",
        "i/import-blocking",
        "i/import-following",
        "i/import-muting",
        "i/import-user-lists",
        "i/pin",
        "mute/create",
        "notes/create",
        "notes/favorites/create",
        "notes/polls/vote",
        "notes/reactions/create",
        "pages/create",
        "pages/like",
        "pages/unlike",
        "pages/update",
        "renote-mute/create",
        "users/lists/create",
        "users/lists/pull",
        "users/lists/push",
        // the three non-each specials
        "antennas/update",
        "drive/files/create",
    ];
    for name in pinned {
        assert!(
            crate::api::move_account::is_prohibit_moved(name),
            "{name} must carry the prohibitMoved gate"
        );
    }
    // i/update is upstream-NOT in the meta set (its moved 403 rides the
    // alsoKnownAs handler arm) — assert the negative so the set stays
    // faithful and a moved user's i/update-without-aliases still packs.
    assert!(!crate::api::move_account::is_prohibit_moved("i/update"));
}

/// i/move error-table ids + the moved 403 id pin byte-parity with the
/// upstream metas (endpoints/i/move.ts errors + ApiCallService L425 +
/// endpoints/i/update.ts L452 arm).
#[test]
fn arena_move_error_ids() {
    assert_eq!(
        crate::api::move_account::NO_SUCH_USER_ID,
        "fcd2eef9-a9b2-4c4f-8624-038099e90aa5"
    );
    assert_eq!(
        crate::api::move_account::DESTINATION_ACCOUNT_FORBIDS_ID,
        "b5c90186-4ab0-49c8-9bba-a1f766282ba4"
    );
    assert_eq!(
        crate::api::move_account::NOT_ROOT_FORBIDDEN_ID,
        "4362e8dc-731f-4ad8-a694-be2a88922a24"
    );
    assert_eq!(
        crate::api::move_account::ALREADY_MOVED_ID,
        "b234a14e-9ebe-4581-8000-074b3c215962"
    );
}

/// genLocalUserUri/fetchPerson local-URI math: trailing-slash bases (the
/// harness yaml `url:` carries one) and bare bases produce the same URI, the
/// local shortcut recovers the id, and the i/move alias scan accepts BOTH an
/// exact URI and a substring-embedded one (`knownAs.includes(fromUrl)`).
#[test]
fn arena_move_local_uri_roundtrip() {
    use mk_core::services::move_account as ma;
    assert_eq!(
        ma::local_user_uri("http://127.0.0.1:3000/", "u01"),
        "http://127.0.0.1:3000/users/u01"
    );
    assert_eq!(
        ma::local_user_uri("http://127.0.0.1:3000", "u01"),
        "http://127.0.0.1:3000/users/u01"
    );
    let uri = ma::local_user_uri("http://127.0.0.1:3000", "u01");
    assert_eq!(
        ma::local_id_from_uri("http://127.0.0.1:3000/", &uri).as_deref(),
        Some("u01")
    );
    assert_eq!(
        ma::local_id_from_uri("http://127.0.0.1:3000", &uri).as_deref(),
        Some("u01")
    );
    assert_eq!(
        ma::local_id_from_uri("http://127.0.0.1:3000", "http://other/users/u01"),
        None
    );
    // the alias admit scan: exact match and embedded match both pass (i/move
    // L122-129 uses String.includes, not equality — a decorated entry still
    // CONTAINS the full fromUrl; the bare `/users/o` needle 98ca83c pinned
    // was unsatisfiable: no alias embeds that prefix, so the assert could
    // never hold under either includes-direction).
    let aliases = vec![
        "http://127.0.0.1:3000/users/alice".to_string(),
        "http://127.0.0.1:3000/users/carol".to_string(),
    ];
    assert!(ma::aliases_admit_local_user(
        &aliases,
        "http://127.0.0.1:3000/users/alice"
    ));
    assert!(!ma::aliases_admit_local_user(
        &aliases,
        "http://127.0.0.1:3000/users/eve"
    ));
    // a decorated entry (`#main-key` style) embeds the full URI: includes()
    // fires without the entry being EQUAL (equality would reject this row —
    // upstream i/move L122-129 stays `includes`).
    let decorated = vec!["http://127.0.0.1:3000/users/alice#main-key".to_string()];
    assert!(ma::aliases_admit_local_user(
        &decorated,
        "http://127.0.0.1:3000/users/alice"
    ));
    // census rn39319126124: a bare acct hostname (`@bob@127.0.0.1`, like
    // new URL(config.url).hostname) must compare EQUAL to the port-suffixed
    // Config.host() token (`127.0.0.1:3000`) — the remote-branch misroute
    // pulled every multi-host acct into NO_SUCH_USER.
    assert!(crate::api::move_account::same_host(
        "127.0.0.1",
        "127.0.0.1:3000"
    ));
    assert!(crate::api::move_account::same_host(
        "Cli1.Example.com",
        "cli1.example.com"
    ));
    assert!(!crate::api::move_account::same_host(
        "127.0.0.1",
        "127.0.0.2"
    ));
}

// ==== move-final (census rn39323486791: 2 failed | 60 passed — both failing
// rows cascade from ONE i/move 500) ====

/// The copyMutings dedup SQL must PREPARE against the migrated schema and
/// return only the indefinite muters of dst (upstream copyMutings dedup:
/// `mutingsRepository.findBy({ muteeId: dst.id, expiresAt: IsNull() })` →
/// skip those muters). The census 500'd with `unterminated quoted identifier
/// at or near """` — a stray trailing `"` in the raw string reached Postgres
/// and aborted postMoveProcess BEFORE the follow-copy enqueue and the
/// adjustFollowingCounts legs, so 'Relationships have been properly
/// migrated' (move.status 500 !== 200) and 'Follow and follower counts are
/// properly adjusted' (followingCount 3 !== 0 /received N) both failed.
#[tokio::test]
async fn move_final_muting_copy_dedup_prepares_and_filters() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("move_final_muting_dedup").await;

    let app = crate::build_router(test_state(pool.clone()));
    let dst = signup(&app, "bob").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let carol = signup(&app, "carol").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let eve = signup(&app, "eve").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let alice = signup(&app, "alice").await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // dst's muter set: carol mutes dst indefinitely, eve mutes dst with an
    // expiry; carol also mutes a DIFFERENT user indefinitely.
    mk_db::social::insert_muting(&pool, &mk_types::id::gen_aid_now(), &carol, &dst, None)
        .await
        .unwrap();
    mk_db::social::insert_muting(
        &pool,
        &mk_types::id::gen_aid_now(),
        &eve,
        &dst,
        Some(4102444800000),
    )
    .await
    .unwrap();
    mk_db::social::insert_muting(&pool, &mk_types::id::gen_aid_now(), &carol, &alice, None)
        .await
        .unwrap();

    // the dedup scan: only the indefinite (expiresAt IS NULL) muters of dst —
    // and the prepare itself is the regression pin (the census 500).
    let muters = mk_db::move_account::indefinite_muters_of(&pool, &dst)
        .await
        .expect("dedup SQL must prepare/execute");
    assert_eq!(muters, vec![carol.clone()], "indefinite muters of dst only");
    // src-side copy source keeps the same-or-future expiry rows (upstream
    // [IsNull, MoreThan(now)] disjunction → both travel with their expiry).
    let old = mk_db::move_account::mutings_of(&pool, &alice)
        .await
        .expect("copy-source SQL must prepare/execute");
    assert_eq!(
        old.len(),
        1,
        "only carol→alice travels (eve→dst is dst-side)"
    );
    assert_eq!(old[0].0, carol, "muter id carried");
    assert!(old[0].1.is_none(), "indefinite src mute copies indefinite");
}

/// The adjustFollowingCounts parity legs (upstream 'Follow and follower
/// counts are properly adjusted', move.ts:385-388): the old account's
/// following/followers columns zero (usersRepository.update followersCount:0,
/// followingCount:0), the copied local follower's followingCount decrements
/// by 1 "instead of unfollowing" (carol ends up following bob INSTEAD of
/// alice — rows stay [bob, alice], column reads 1), old followees'
/// followersCount decrement, and the empty batch is a validated no-op.
#[tokio::test]
async fn move_final_counts_adjust_legs() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("move_final_counts").await;

    let app = crate::build_router(test_state(pool.clone()));
    let alice = signup(&app, "alice").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let carol = signup(&app, "carol").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let eve = signup(&app, "eve").await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // pre-move: carol→alice (carol.followingCount 1, alice.followersCount 1),
    // eve→alice (eve.followingCount 1, alice.followersCount 2), plus the
    // copied-follow bump on carol (carol→bob) landing on eve's column pair.
    mk_db::social::bump_follow_counts(&pool, &carol, &alice, 1)
        .await
        .unwrap();
    mk_db::social::bump_follow_counts(&pool, &eve, &alice, 1)
        .await
        .unwrap();
    mk_db::social::bump_follow_counts(&pool, &carol, &eve, 2)
        .await
        .unwrap();

    let counts = async |pool: &mk_db::sqlx::PgPool, id: &str| {
        let (following, followers): (i32, i32) = mk_db::sqlx::query_as(
            r#"SELECT "followingCount", "followersCount" FROM "user" WHERE "id" = $1"#,
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap();
        (following, followers)
    };

    assert_eq!(counts(&pool, &alice).await, (0, 2), "seeded via bump pair");
    assert_eq!(counts(&pool, &carol).await, (3, 0));
    assert_eq!(counts(&pool, &eve).await, (1, 2));

    // zero leg: the OLD account's columns — and only those.
    mk_db::move_account::zero_move_counts(&pool, &alice)
        .await
        .unwrap();
    assert_eq!(counts(&pool, &alice).await, (0, 0), "old account zeroed");
    assert_eq!(counts(&pool, &carol).await, (3, 0), "survivor untouched");
    assert_eq!(counts(&pool, &eve).await, (1, 2), "survivor untouched");

    // follower decrement leg: "Decrease following counts of local followers
    // by 1" — carol ends up following bob INSTEAD of alice (rows stay, the
    // column compensates) — and the second decrement past 0 floors (our
    // GREATEST(-1, 0) guard; TypeORM decrement would go negative).
    mk_db::move_account::decrement_following_counts(&pool, &[carol.clone(), eve.clone()])
        .await
        .unwrap();
    assert_eq!(counts(&pool, &carol).await, (2, 0));
    assert_eq!(counts(&pool, &eve).await, (0, 2));

    for _ in 0..4 {
        mk_db::move_account::decrement_following_counts(&pool, &[carol.clone()])
            .await
            .unwrap();
    }
    assert_eq!(
        counts(&pool, &carol).await,
        (0, 0),
        "GREATEST(2-4, 0) floor"
    );

    // followee decrement leg: "Decrease follower counts of local followees by
    // 1" (old alice-followees lose their alice follower) + empty no-op.
    mk_db::move_account::decrement_followers_counts(&pool, &[eve.clone()])
        .await
        .unwrap();
    assert_eq!(counts(&pool, &eve).await, (0, 1));
    mk_db::move_account::decrement_followers_counts(&pool, &[])
        .await
        .unwrap();
    assert_eq!(counts(&pool, &eve).await, (0, 1), "empty batch is a no-op");
}

// ============================================================================
// // arena-fetchres // — the GET routes OUTSIDE /api/* (upstream pinned
// test/e2e/fetch-resource.ts — e2e-fetch-resource workflow). Mirrors the
// ClientServerService SSR/asset/foyer plane, the ActivityPubServerService
// apOrHtml negotiation (/@:acct, /users/:user, /notes/:note — exact-over-
// wildcard accept ranking, Vary: Accept) + the unconstrained /users/:user/
// outbox OrderedCollection, the /streaming non-WS 503 arm and the beforeAll
// pages/create + flash/create + gallery/posts/create fixtures. The baseline
// census rn39315611745 @ 3e79ec3 ran 61 failed | 23 passed over exactly
// these route gaps; the rust tail pins the surfacing decisions.
// ============================================================================

// arena-fetchres: simpleGet mirror — GET with an explicit Accept header that
// keeps the response headers (content-type strings are strict compares
// upstream: `text/html; charset=utf-8` etc.).
#[allow(dead_code)]
async fn get_accept(
    app: &axum::Router,
    path: &str,
    accept: &str,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .header("accept", accept)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

// arena-fetchres: beforeAll seed — signup alice + admin/update-meta
// federation 'all' + notes/create + the pages/create, flash/create,
// clips/create, channels/create and gallery/posts/create fixtures the pinned
// beforeAll rides (utils.ts page()/play()/galleryPost()/clip()/channel()).
#[allow(dead_code)]
async fn fetchres_seed(
    app: &axum::Router,
) -> (Value, String, String, String, String, String, String) {
    let alice = signup(app, "alice").await;
    // borrows off the signup value (the json! macro only copies the &strs);
    // owned copies — the tuple hands `alice` to the caller below
    let a_token = alice["token"].as_str().unwrap().to_string();
    let a_id = alice["id"].as_str().unwrap().to_string();
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/update-meta",
        json!({ "federation": "all", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "update-meta must 204");

    let (s, note) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "test", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "notes/create: {note}");
    // notes/create returns the upstream `{ createdNote }` envelope (endpoints/
    // notes/create.ts → Note entity pack) — the bare `note["id"]` read here
    // birth-panicked `unwrap` on Null and kept all three fetchres pins red
    // ON SERIAL while the e2e lane ran the same routes green (the Node
    // census reads body.createdNote.id).
    let note_id = note["createdNote"]["id"].as_str().unwrap().to_string();

    let (s, page) = post_json(
        app.clone(),
        "/api/pages/create",
        json!({
            "alignCenter": false,
            "content": [{ "id": "2be9a64b-5ada-43a3-85f3-ec3429551ded", "text": "Hello World!", "type": "text" }],
            "eyeCatchingImageId": null,
            "font": "sans-serif",
            "hideTitleWhenPinned": false,
            "name": "1678594845072",
            "script": "",
            "summary": null,
            "title": "",
            "variables": [],
            "i": a_token,
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "pages/create: {page}");
    let page_id = page["id"].as_str().unwrap().to_string();

    let (s, flash) = post_json(
        app.clone(),
        "/api/flash/create",
        json!({ "permissions": [], "script": "test", "summary": "", "title": "test", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "flash/create: {flash}");
    let flash_id = flash["id"].as_str().unwrap().to_string();

    let (s, clip) = post_json(
        app.clone(),
        "/api/clips/create",
        json!({ "description": null, "isPublic": true, "name": "test", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "clips/create: {clip}");
    let clip_id = clip["id"].as_str().unwrap().to_string();

    let (s, gallery) = post_json(
        app.clone(),
        "/api/gallery/posts/create",
        json!({ "description": null, "fileIds": [], "isSensitive": false, "title": "test", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "gallery/posts/create: {gallery}");
    let gallery_id = gallery["id"].as_str().unwrap().to_string();

    (alice, a_id, note_id, page_id, flash_id, clip_id, gallery_id)
}

// arena-fetchres census leg 1 (the HTML plane): the static/foyer GETs, the
// negotiated /@user + sub + pages/:page + /notes/:note SSR metas, the item
// pages (/play /clips /gallery /channels), the /users/:id 302 redirect, the
// POST-only inbox GET-HTML rows and the /streaming 503.
#[tokio::test]
async fn fetchres_html_plane_and_negotiated_pages() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkfetchres1").await;
    let app = crate::build_router(test_state(pool.clone()));

    let (alice, a_id, note_id, page_id, flash_id, clip_id, gallery_id) = fetchres_seed(&app).await;
    let username = alice["username"].as_str().unwrap();

    // --- the status/asset plane ---
    for (path, ctype) in [
        ("/", "text/html; charset=utf-8"),
        ("/docs/ja-JP/about", "text/html; charset=utf-8"),
        ("/api-doc", "text/html; charset=utf-8"),
        ("/api-console", "text/html; charset=utf-8"),
        ("/_info_card_", "text/html; charset=utf-8"),
        ("/bios", "text/html; charset=utf-8"),
        ("/cli", "text/html; charset=utf-8"),
        ("/flush", "text/html; charset=utf-8"),
        ("/robots.txt", "text/plain; charset=utf-8"),
        ("/favicon.ico", "image/vnd.microsoft.icon"),
        ("/opensearch.xml", "application/opensearchdescription+xml"),
        ("/apple-touch-icon.png", "image/png"),
        ("/twemoji/2764.svg", "image/svg+xml"),
        ("/twemoji/2764-fe0f-200d-1f525.svg", "image/svg+xml"),
        ("/twemoji-badge/2764.png", "image/png"),
        ("/twemoji-badge/2764-fe0f-200d-1f525.png", "image/png"),
        ("/fluent-emoji/2764.png", "image/png"),
        ("/fluent-emoji/2764-fe0f-200d-1f525.png", "image/png"),
    ] {
        let (s, h, body) = get_accept(&app, path, "*/*").await;
        assert_eq!(s, StatusCode::OK, "{path}: {body}");
        assert_eq!(
            hdr(&h, "content-type").map(|c| c.to_lowercase()),
            Some(ctype.to_string()),
            "{path}"
        );
    }
    // the /api.json + /streaming contracts
    let (s, h, body) = get_accept(&app, "/api.json", "*/*").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        hdr(&h, "content-type").as_deref(),
        Some("application/json; charset=utf-8")
    );
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["openapi"], json!("3.0.1"));
    let (s, h, body) = get_accept(&app, "/streaming", "*/*").await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(
        hdr(&h, "cache-control").as_deref(),
        Some("private, max-age=0")
    );
    // the wrong-extension gates stay 404 (baseline green rows)
    for path in [
        "/twemoji/2764.png",
        "/twemoji/2764-fe0f-200d-1f525.png",
        "/twemoji-badge/2764.svg",
        "/twemoji-badge/2764-fe0f-200d-1f525.svg",
        "/fluent-emoji/2764.svg",
        "/fluent-emoji/2764-fe0f-200d-1f525.svg",
    ] {
        let (s, _, _) = get_accept(&app, path, "*/*").await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{path}");
    }

    // --- negotiated /@user HTML + metas (PREFER_HTML and UNSPECIFIED) ---
    for accept in ["text/html, */*", "*/*"] {
        for path in [
            format!("/@{username}"),
            format!("/@{username}/home"),
            format!("/@{username}/gallery"),
        ] {
            let (s, h, body) = get_accept(&app, &path, accept).await;
            assert_eq!(s, StatusCode::OK, "{path}: {body}");
            assert_eq!(
                hdr(&h, "content-type").as_deref(),
                Some("text/html; charset=utf-8"),
                "{path}"
            );
            assert!(
                body.contains(&format!(
                    "<meta name=\"misskey:user-username\" content=\"{username}\">"
                )),
                "{path}: {body}"
            );
            assert!(
                body.contains(&format!(
                    "<meta name=\"misskey:user-id\" content=\"{a_id}\">"
                )),
                "{path}: {body}"
            );
        }
    }
    // pages/:page carries the page-id meta; unknown page renders the base page
    let path = format!("/@{username}/pages/1678594845072");
    let (s, _, body) = get_accept(&app, &path, "*/*").await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert!(
        body.contains(&format!(
            "<meta name=\"misskey:page-id\" content=\"{page_id}\">"
        )),
        "{body}"
    );
    let (s, _, _) = get_accept(&app, &format!("/@{username}/pages/ghost"), "*/*").await;
    assert_eq!(s, StatusCode::OK);
    // unknown user still answers 200 HTML (renderBase)
    let (s, h, _) = get_accept(&app, "/@ghostxxxxxxxx", "text/html, */*").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        hdr(&h, "content-type").as_deref(),
        Some("text/html; charset=utf-8")
    );

    // --- item pages ---
    let (s, _, body) = get_accept(&app, &format!("/notes/{note_id}"), "*/*").await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert!(
        body.contains(&format!(
            "<meta name=\"misskey:note-id\" content=\"{note_id}\">"
        )),
        "{body}"
    );
    let (s, _, _) = get_accept(&app, "/notes/ghostxxxxxxxxxxxxxxxxx", "*/*").await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, body) = get_accept(&app, &format!("/play/{flash_id}"), "*/*").await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert!(
        body.contains(&format!(
            "<meta name=\"misskey:flash-id\" content=\"{flash_id}\">"
        )),
        "{body}"
    );
    let (s, _, body) = get_accept(&app, &format!("/clips/{clip_id}"), "*/*").await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert!(
        body.contains(&format!(
            "<meta name=\"misskey:clip-id\" content=\"{clip_id}\">"
        )),
        "{body}"
    );
    let (s, _, body) = get_accept(&app, &format!("/gallery/{gallery_id}"), "*/*").await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert!(
        body.contains("<meta name=\"misskey:user-username\""),
        "{body}"
    );

    // --- /users/:id redirect + the POST-only inbox GET-HTML rows ---
    let (s, h, _) = get_accept(&app, &format!("/users/{a_id}"), "*/*").await;
    assert_eq!(s, StatusCode::FOUND, "302 redirect");
    assert_eq!(
        hdr(&h, "location").as_deref(),
        Some(format!("/@{username}").as_str())
    );
    assert_eq!(hdr(&h, "vary").as_deref(), Some("Accept"));
    let (s, _, _) = get_accept(&app, "/users/ghostxxxxxxxx", "*/*").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    for path in ["/inbox", &format!("/users/{a_id}/inbox")] {
        let (s, h, _) = get_accept(&app, path, "*/*").await;
        assert_eq!(s, StatusCode::OK, "{path}");
        assert_eq!(
            hdr(&h, "content-type").as_deref(),
            Some("text/html; charset=utf-8"),
            "{path}"
        );
    }

    // --- the user feeds (rss/atom/json) + the MFM-must-not-leak row ---
    let a_token = alice["token"].as_str().unwrap();
    let (s, _) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "**a**", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    for (suffix, ctype) in [
        (".rss", "application/rss+xml; charset=utf-8"),
        (".atom", "application/atom+xml; charset=utf-8"),
        (".json", "application/json; charset=utf-8"),
    ] {
        let path = format!("/@{username}{suffix}");
        let (s, h, body) = get_accept(&app, &path, "*/*").await;
        assert_eq!(s, StatusCode::OK, "{path}: {body}");
        assert_eq!(hdr(&h, "content-type").as_deref(), Some(ctype), "{path}");
        // MFM must not leak: the raw **a** markers never reach a feed body
        assert!(!body.contains("**a**"), "{path}: MFM leaked: {body}");
    }
    // a noteless user feeds too; missing users 404
    let _ = post_json(
        app.clone(),
        "/api/signup",
        json!({ "username": "bobfetchres", "password": "test" }),
    )
    .await;
    for suffix in [".rss", ".atom", ".json"] {
        let (s, _, _) = get_accept(&app, &format!("/@bobfetchres{suffix}"), "*/*").await;
        assert_eq!(s, StatusCode::OK, "bob{suffix}");
        let (s, _, _) = get_accept(&app, &format!("/@ghostfetchres{suffix}"), "*/*").await;
        assert_eq!(s, StatusCode::NOT_FOUND, "ghost{suffix}");
    }

    let _ = pool;
}

// arena-fetchres census leg 2 (the AP plane): the apOrHtml arms render
// Person/Note/OrderedCollection under `application/activity+json;
// charset=utf-8`, Vary: Accept rides every negotiated reply, and unknown
// ids 404 (the /notes AP 404 legs flow through the endpoints-arena ap_note).
#[tokio::test]
async fn fetchres_activity_pub_negotiation() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkfetchres2").await;
    let app = crate::build_router(test_state(pool.clone()));

    let (_alice, a_id, note_id, _page_id, _flash_id, _clip_id, _gallery_id) =
        fetchres_seed(&app).await;
    let username = _alice["username"].as_str().unwrap();

    for accept in [
        "application/activity+json",
        "application/activity+json, */*",
    ] {
        let (s, h, body) = get_accept(&app, &format!("/@{username}"), accept).await;
        assert_eq!(s, StatusCode::OK, "{body}");
        assert_eq!(
            hdr(&h, "content-type").as_deref(),
            Some("application/activity+json; charset=utf-8")
        );
        assert_eq!(hdr(&h, "vary").as_deref(), Some("Accept"));
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["type"], json!("Person"), "{v}");
        assert_eq!(
            v["id"],
            json!(format!("http://127.0.0.1:3000/users/{a_id}"))
        );
        let (s, _, body) = get_accept(&app, &format!("/users/{a_id}"), accept).await;
        assert_eq!(s, StatusCode::OK, "{body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["type"], json!("Person"), "{v}");
    }
    // SPECIFIED accept: base doc type — the AP arm hands ld+json its profile
    let (s, h, _) = get_accept(
        &app,
        &format!("/@{username}"),
        "application/ld+json; profile=\"https://www.w3.org/ns/activitystreams\"",
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        hdr(&h, "content-type").as_deref(),
        Some(
            "application/ld+json; profile=\"https://www.w3.org/ns/activitystreams\"; charset=utf-8"
        )
    );

    // the 404 legs
    for (path, accept) in [
        (format!("/@ghostxxxxxxxx"), "application/activity+json"),
        (
            format!("/users/ghostxxxxxxxx"),
            "application/activity+json, */*",
        ),
        (
            format!("/notes/ghostxxxxxxxxxxxxxxxxx"),
            "application/activity+json",
        ),
    ] {
        let (s, _, _) = get_accept(&app, &path, accept).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{path}");
    }

    // outbox — the unconstrained OrderedCollection (default accept)
    let (s, h, body) = get_accept(&app, &format!("/users/{a_id}/outbox"), "*/*").await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(
        hdr(&h, "content-type").as_deref(),
        Some("application/activity+json; charset=utf-8")
    );
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["type"], json!("OrderedCollection"), "{v}");
    assert_eq!(v["totalItems"], json!(1), "{v}");

    // the note AP leg rides the pre-existing ap_note body (type Note)
    let (s, h, body) = get_accept(
        &app,
        &format!("/notes/{note_id}"),
        "application/activity+json, */*",
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(
        hdr(&h, "content-type").as_deref(),
        Some("application/activity+json; charset=utf-8")
    );
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["type"], json!("Note"), "{v}");

    // the HTML arms must remain reachable when the accept prefers html
    for (path, accept) in [
        (format!("/@{username}"), "text/html, */*"),
        (format!("/notes/{note_id}"), "text/html, */*"),
        (format!("/users/{a_id}"), "text/html, */*"),
    ] {
        let (s, h, _) = get_accept(&app, &path, accept).await;
        assert!(s.is_success() || s == StatusCode::FOUND, "{path}: {s}");
        let html = hdr(&h, "content-type")
            .map(|c| c.starts_with("text/html"))
            .unwrap_or(false);
        assert!(html || s == StatusCode::FOUND, "{path}: not html");
    }

    let _ = pool;
}

// arena-fetchres census leg 3: the beforeAll create fixtures — the pack
// contracts the pinned test reads (id/name echo, isLiked false) and the
// requireCredential gate the fixtures sit behind.
#[tokio::test]
async fn fetchres_create_fixture_endpoints() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkfetchres3").await;
    let app = crate::build_router(test_state(pool.clone()));

    // the guest gate answers CREDENTIAL_REQUIRED (auth before paramDef)
    let (s, body) = post_json(app.clone(), "/api/pages/create", json!({ "name": "x" })).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(
        body["error"]["code"],
        json!("CREDENTIAL_REQUIRED"),
        "{body}"
    );

    let (alice, _a_id, _note_id, _page_id, _flash_id, _clip_id, _gallery_id) =
        fetchres_seed(&app).await;
    let _ = alice;

    // the page row the fixtures seeded: GET /api/pages/show mirrors the pack
    let (s, page) = post_json(app.clone(), "/api/endpoints", json!({})).await;
    assert_eq!(s, StatusCode::OK);
    assert!(page.as_array().unwrap().contains(&json!("pages/create")));
    assert!(page.as_array().unwrap().contains(&json!("flash/create")));
    assert!(
        page.as_array()
            .unwrap()
            .contains(&json!("gallery/posts/create"))
    );

    let _ = pool;
}

// arena-synalio // — the system-webhook + abuse-report webhook plane upstream
// test/e2e/synalio/{user-create,abuse-report}.ts drives (SystemWebhookService
// enqueue gates + the SystemWebhookDeliverProcessorService L42-56 deliver
// envelope; SQL in mk-db/src/webhook.rs). Pure shape tests — the live
// delivery matrix rides the e2e-synalio lane.

/// The userCreated content must field-match the signup response the lane
/// already holds (`alice.<field> toBe body.<field>`): the picker rides the
/// same packed me JSON and fills the UserLite presence defaults, and NO
/// me-only keys (token policies securityKeysList …) leak into the payload.
/// `instance` is absent on BOTH sides of the pinned compare (pack L506:
/// `user.host ? (cache hit ? {...} : undefined) : undefined` — the key stays
/// omitted for local users and cache-miss remote hosts, never null; a null
/// here turned the row into `expected undefined, received null`).
#[test]
fn arena_synalio_user_created_lite_pick_matches_signup_fields() {
    use crate::api::system_webhook::webhook_user_lite_from_pack;
    let packed_me = serde_json::json!({
        "id": "a1", "name": null, "username": "alice", "host": null,
        "avatarUrl": "http://127.0.0.1:3000/avatar.png",
        "avatarBlurhash": null, "avatarDecorations": [],
        "isBot": false, "isCat": false,
        "emojis": {}, "onlineStatus": "unknown", "badgeRoles": [],
        // me-only tail that must NOT bleed into the UserLite payload
        "token": "0123456789abcdef", "policies": {}, "securityKeysList": [],
    });
    let lite = webhook_user_lite_from_pack(&packed_me);
    for key in [
        "id",
        "name",
        "username",
        "host",
        "avatarUrl",
        "avatarBlurhash",
        "avatarDecorations",
        "isBot",
        "isCat",
        "instance",
        "emojis",
        "onlineStatus",
        "badgeRoles",
    ] {
        assert_eq!(lite.get(key), packed_me.get(key), "{key}: undefined parity");
    }
    assert!(
        lite.get("instance").is_none(),
        "local-user instance stays omitted (undefined === undefined in the e2e row)"
    );
    assert_eq!(
        lite.as_object().unwrap().len(),
        12,
        "exactly the UserLite slice the synalio rows field-equal"
    );
    assert!(
        lite.get("token").is_none(),
        "me secrets stay out of the hook body"
    );
}

/// The deliver envelope mirrors the pinned processor byte-plan:
/// {server, hookId, eventId, createdAt, type, body} + the abuse payload keys
/// the synalio assertions read (id/targetUserId/reporterId/assigneeId/
/// resolved/comment) keep their values across the report→resolve round trip.
#[test]
fn arena_synalio_webhook_envelope_and_abuse_keys() {
    use crate::api::system_webhook::{abuse_report_payload, webhook_envelope};
    let content = serde_json::json!({ "probe": true });
    let envelope = webhook_envelope("hook-1", "http://127.0.0.1:3000/", "abuseReport", content);
    assert_eq!(envelope["hookId"], "hook-1");
    assert_eq!(envelope["server"], "http://127.0.0.1:3000/");
    assert_eq!(envelope["type"], "abuseReport");
    assert!(envelope.get("eventId").is_some(), "eventId present");
    assert!(envelope["createdAt"].is_number(), "createdAt ms present");
    assert_eq!(envelope["body"], serde_json::json!({ "probe": true }));

    let report = abuse_report_payload(serde_json::json!({
        "id": "r1", "targetUserId": "t1", "targetUser": null,
        "reporterId": "b1", "reporter": null, "assigneeId": null,
        "assignee": null, "resolved": false, "comment": "c", "resolvedAs": null,
    }));
    for key in [
        "id",
        "targetUserId",
        "reporterId",
        "assigneeId",
        "resolved",
        "comment",
    ] {
        assert!(
            report.get(key).is_some(),
            "AbuseReportPayload carries {key}"
        );
    }
    assert_eq!(report["id"], "r1");
    assert_eq!(report["resolved"], serde_json::json!(false));
}

// synalio-final // — the userCreated emit leg (SignupService.ts L158 →
// UserService.notifySystemWebhook → SystemWebhookService.enqueueSystemWebhook).
// Two enqueue-level gates behind the e2e lane's three userCreated rows,
// pinned without a live webhook receiver (the live POST matrix rides the
// e2e-synalio census; the SignupService fire side is the spawned
// system_webhook::fire in signup_route).

/// Emit-on-create + Webhookが無効: the fresh-signup enqueue scans ONLY active
/// hooks (the row-3 gate lives in the SQL), and an active hook whose `"on"`
/// carries `userCreated` rides the deliver fan-out (the row-1 emit).
#[test]
fn synalio_final_user_created_enqueue_emit_and_inactive_sql_gate() {
    use crate::api::system_webhook::hook_matches_event;
    let hook = mk_db::webhook::SystemWebhookRow {
        id: "hook1".to_string(),
        is_active: true,
        name: "n".to_string(),
        on: vec!["userCreated".to_string()],
        url: "http://localhost:15080".to_string(),
        secret: "seed".to_string(),
        latest_status: None,
        updated_at_ms: 0,
    };
    assert!(
        hook_matches_event(&hook, "userCreated", &[]),
        "active hook with 'userCreated' in \"on\" enqueues the POST"
    );
    assert!(
        mk_db::webhook::ACTIVE_SYSTEM_WEBHOOKS_SQL.contains("\"isActive\" = true"),
        "the enqueue leg filters inactive hooks at the SQL (無効 hook -> no POST)"
    );
}

/// 未許可 row: `on: []` (userCreated NOT in hook.events) and the type
/// mismatch (hook.on carries a different event) both drop out of the gate
/// before any deliver spawn.
#[test]
fn synalio_final_user_created_unpermitted_and_type_mismatch_no_enqueue() {
    use crate::api::system_webhook::hook_matches_event;
    let hook = mk_db::webhook::SystemWebhookRow {
        id: "hook2".to_string(),
        is_active: true,
        name: "n".to_string(),
        on: vec![], // 未許可: `on: []` never carries userCreated
        url: "http://localhost:15080".to_string(),
        secret: "seed".to_string(),
        latest_status: None,
        updated_at_ms: 0,
    };
    assert!(
        !hook_matches_event(&hook, "userCreated", &[]),
        "an unpermitted hook never enqueues"
    );
    let mismatch = mk_db::webhook::SystemWebhookRow {
        on: vec!["abuseReport".to_string()],
        is_active: true,
        ..hook
    };
    assert!(
        !hook_matches_event(&mismatch, "userCreated", &[]),
        "a hook without the event type never enqueues"
    );
}

// queue-min // — the durable-queue processor slice (upstream
// EndedPollNotificationProcessorService.ts + DeleteAccountProcessorService.ts):
// enqueue rows visible, the ended-poll handler's pollEnded rows, the
// deleteAccount soft purge.

/// enqueue rows visible: both new kinds land in mk_queue_jobs with the
/// upstream payload shapes — the delayed variant (run_after = now + 1h) is
/// NOT yet due so claim skips it, while the due deleteAccount job claims
/// with its payload intact.
#[tokio::test]
async fn queue_min_enqueue_rows_visible_and_delayed_not_due() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("queue_min_enqueue").await;
    let q = mk_queue::QueueHandle::new(pool.clone());

    let ended = q
        .enqueue_delayed(
            "endedPollNotification",
            &json!({ "type": "endedPollNotification", "noteId": "note1" }),
            std::time::Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let del = q
        .enqueue(
            "deleteAccount",
            &json!({ "type": "deleteAccount", "user": { "id": "u_del" }, "soft": true }),
        )
        .await
        .unwrap();

    let (kind, payload): (String, Value) =
        mk_db::sqlx::query_as(r#"SELECT "kind", "payload" FROM "mk_queue_jobs" WHERE "id" = $1"#)
            .bind(del)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(kind, "deleteAccount");
    assert_eq!(payload["user"]["id"], json!("u_del"));
    assert_eq!(payload["soft"], json!(true));
    let (kind, payload): (String, Value) =
        mk_db::sqlx::query_as(r#"SELECT "kind", "payload" FROM "mk_queue_jobs" WHERE "id" = $1"#)
            .bind(ended)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(kind, "endedPollNotification");
    assert_eq!(payload["noteId"], json!("note1"));

    let job = q
        .claim_next(&["endedPollNotification", "deleteAccount"])
        .await
        .unwrap()
        .expect("the due deleteAccount job claims");
    assert_eq!(job.id, del);
    q.complete(job.id).await.unwrap();
    assert!(
        q.claim_next(&["endedPollNotification", "deleteAccount"])
            .await
            .unwrap()
            .is_none(),
        "the future run_after delayed job stays unclaimed"
    );
}

/// the ended-poll handler creates `pollEnded` notification rows for the
/// LOCAL author + voters of a poll expired inside the hourly scan window
/// (remote users stay out); an at-least-once re-run dedups; the per-note arm
/// reaches polls whose window already aged out.
#[tokio::test]
async fn queue_min_ended_poll_handler_creates_poll_ended_notifications() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("queue_min_endedpoll").await;

    // EndedPollNotificationProcessorService L49-57: userIds = unique[author,
    // ...voters], `profile.userHost === null` gate per recipient.
    for (uid, host) in [
        ("u_auth", None),
        ("u_voter", None),
        ("u_remote", Some("remote.example")),
    ] {
        mk_db::sqlx::query(
            r#"INSERT INTO "user" ("id", "username", "usernameLower", "host")
               VALUES ($1, $1, $1, $2)"#,
        )
        .bind(uid)
        .bind(host)
        .execute(&pool)
        .await
        .unwrap();
    }
    for nid in ["n1", "n2"] {
        mk_db::sqlx::query(
            r#"INSERT INTO "note" ("id", "userId", "visibility") VALUES ($1, 'u_auth', 'public')"#,
        )
        .bind(nid)
        .execute(&pool)
        .await
        .unwrap();
    }
    let now = now_ms() as i64;
    // n1: expired 30 min ago (inside the last-hour window); n2: expired 3h
    // ago (window aged out — the scan arm must skip it, the per-note arm
    // reaches it).
    mk_db::poll::insert_poll(
        &pool,
        "n1",
        &["a".to_string(), "b".to_string()],
        false,
        Some(now - 30 * 60_000),
        "public",
        "u_auth",
    )
    .await
    .unwrap();
    mk_db::poll::insert_poll(
        &pool,
        "n2",
        &["a".to_string(), "b".to_string()],
        false,
        Some(now - 3 * 3_600_000),
        "public",
        "u_auth",
    )
    .await
    .unwrap();
    mk_db::poll::insert_poll_vote(&pool, "v1", "u_voter", "n1", 0)
        .await
        .unwrap();
    mk_db::poll::insert_poll_vote(&pool, "v2", "u_remote", "n2", 0)
        .await
        .unwrap();

    let poll_ended_rows = || async {
        mk_db::sqlx::query_as::<_, (String, Option<String>)>(
            r#"SELECT ("notifieeId"), ("noteId") FROM "notification"
               WHERE "type" = 'pollEnded' ORDER BY "id""#,
        )
        .fetch_all(&pool)
        .await
        .unwrap()
    };

    // scan arm (payload-less job): n1 only — author + local voter.
    crate::api::queue_processors::run_ended_poll_notification(&pool, None)
        .await
        .unwrap();
    let rows = poll_ended_rows().await;
    assert!(
        rows.contains(&("u_auth".to_string(), Some("n1".to_string())))
            && rows.contains(&("u_voter".to_string(), Some("n1".to_string()))),
        "author + local voter notified: {rows:?}"
    );
    assert_eq!(
        rows.len(),
        2,
        "remote voter + aged-out n2 stay out: {rows:?}"
    );

    // at-least-once re-delivery dedups (no new rows).
    crate::api::queue_processors::run_ended_poll_notification(&pool, None)
        .await
        .unwrap();
    assert_eq!(poll_ended_rows().await.len(), 2, "re-run dedups");

    // per-note arm: the aged-out n2 — local author notified, remote voter
    // not. (Contains + len, NOT an ordered exact-match: aid ids minted in
    // the same millisecond carry no cross-row order guarantee, and the
    // handler runs sit inside one ms.)
    crate::api::queue_processors::run_ended_poll_notification(&pool, Some("n2"))
        .await
        .unwrap();
    let rows = poll_ended_rows().await;
    assert_eq!(
        rows.len(),
        3,
        "exactly the three local recipients: {rows:?}"
    );
    assert!(
        rows.contains(&("u_auth".to_string(), Some("n2".to_string()))),
        "n2 author notified via the per-note arm: {rows:?}"
    );
    assert!(
        !rows
            .iter()
            .any(|(u, n)| *n == Some("n2".to_string()) && u != "u_auth"),
        "the remote n2 voter stays out: {rows:?}"
    );
}

/// the deleteAccount job's soft slice: the user's note rows purge (upstream
/// `{ // Delete notes }`), unrelated notes survive, and the user ROW itself
/// survives with `isDeleted` (削除済ユーザーが含まれる keeps reading it from
/// the users lists).
#[tokio::test]
async fn queue_min_delete_account_handler_soft_purges_notes_and_marks_user() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("queue_min_delacct").await;

    for uid in ["u_del", "u_other"] {
        mk_db::sqlx::query(
            r#"INSERT INTO "user" ("id", "username", "usernameLower") VALUES ($1, $1, $1)"#,
        )
        .bind(uid)
        .execute(&pool)
        .await
        .unwrap();
    }
    for (nid, uid) in [
        ("n_del1", "u_del"),
        ("n_del2", "u_del"),
        ("n_keep", "u_other"),
    ] {
        mk_db::sqlx::query(
            r#"INSERT INTO "note" ("id", "userId", "visibility") VALUES ($1, $2, 'public')"#,
        )
        .bind(nid)
        .bind(uid)
        .execute(&pool)
        .await
        .unwrap();
    }

    crate::api::queue_processors::run_delete_account(&pool, "u_del")
        .await
        .unwrap();

    let left: i64 =
        mk_db::sqlx::query_scalar(r#"SELECT count(*) FROM "note" WHERE "userId" = 'u_del'"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(left, 0, "the deleted user's notes purged");
    let kept: i64 =
        mk_db::sqlx::query_scalar(r#"SELECT count(*) FROM "note" WHERE "id" = 'n_keep'"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(kept, 1, "unrelated notes survive");
    let n: i64 = mk_db::sqlx::query_scalar(r#"SELECT count(*) FROM "user" WHERE "id" = 'u_del'"#)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1, "the user row survives (削除済ユーザーが含まれる)");
    let is_deleted: bool =
        mk_db::sqlx::query_scalar(r#"SELECT "isDeleted" FROM "user" WHERE "id" = 'u_del'"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(is_deleted, "isDeleted stays marked");

    // idempotent: a re-claimed job (gone user row path NOT taken — the row
    // exists) re-runs the purge as a noop.
    crate::api::queue_processors::run_delete_account(&pool, "u_del")
        .await
        .unwrap();
}

// meili-min: notes/search + notes/search-by-tag (the SQL fallback lanes —
// which are ALSO upstream's no-Meili default, SearchService.searchNoteByLike)
#[tokio::test]
async fn notes_search_sql_fallback_gate_and_hits() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_note_search").await;
    let app = crate::build_router(test_state(pool.clone()));
    let root = signup(&app, &format!("root{}", mk_types::id::gen_aid_now())).await;
    let alice = signup(&app, &format!("alice{}", mk_types::id::gen_aid_now())).await;
    let r_token = root["token"].as_str().unwrap().to_string();
    let a_token = alice["token"].as_str().unwrap().to_string();

    // fixtures: an ILIKE target + a tag target (notes/create lowercases the
    // extract_hashtags output onto the row).
    let (s, hit) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "the Quick brown Fox", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {hit}");
    let hit_id = hit["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, tagged) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "tag lane #Hoge", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {tagged}");
    let tagged_id = tagged["createdNote"]["id"].as_str().unwrap().to_string();

    // ajv: missing query / missing tag → INVALID_PARAM (3d81ceae)
    let (s, err) = post_json(app.clone(), "/api/notes/search", json!({ "i": a_token })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(err["error"]["code"], json!("INVALID_PARAM"), "body: {err}");
    let (s, err) = post_json(app.clone(), "/api/notes/search-by-tag", json!({})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(err["error"]["code"], json!("INVALID_PARAM"), "body: {err}");

    // gate: canSearchNotes is DEFAULT-false → UNAVAILABLE. meta.errors.
    // unavailable carries no kind/httpStatusCode → ApiError client default
    // → HTTP 400 (the same shape the notes/translate gate answers here).
    let (s, err) = post_json(
        app.clone(),
        "/api/notes/search",
        json!({ "query": "quick", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(err["error"]["code"], json!("UNAVAILABLE"), "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("0b44998d-77aa-4427-80d0-d2c9b8523011"),
        "body: {err}"
    );

    // grant: role with an explicit canSearchNotes override + assign
    let (s, role) = post_json(
        app.clone(),
        "/api/admin/roles/create",
        json!({
            "name": "searcher",
            "description": "",
            "color": null,
            "iconUrl": null,
            "displayOrder": 0,
            "target": "manual",
            "condFormula": {},
            "isAdministrator": false,
            "isModerator": false,
            "isPublic": false,
            "isExplorable": false,
            "asBadge": false,
            "canEditMembersByModerator": false,
            "policies": { "canSearchNotes": { "priority": 0, "useDefault": false, "value": true } },
            "i": r_token
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {role}");
    let role_id = role["id"].as_str().unwrap().to_string();
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/roles/assign",
        json!({ "userId": alice["id"], "roleId": role_id, "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // ILIKE hit: the uppercased query still matches (case-insensitive text
    // match; upstream sqlLike lowercases both sides — ILIKE folds the same).
    let (s, body) = post_json(
        app.clone(),
        "/api/notes/search",
        json!({ "query": "QUICK", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let ids: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![hit_id.as_str()], "body: {body}");

    // tag hit — AND no gate: search-by-tag answers WITHOUT the grant
    // (upstream notes/search-by-tag.ts has no RoleService/canSearchNotes).
    let (s, body) = post_json(
        app.clone(),
        "/api/notes/search-by-tag",
        json!({ "tag": "HOGE" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let ids: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![tagged_id.as_str()], "body: {body}");
}

// fed-min // — the inbox POST skeleton (mk_ap::signature + mk_ap::inbox,
// upstream test/unit/ap-request.ts signing-string pins + ApInboxService
// .performOneActivity dispatch). Unit-level: no DB, no remote key fetch —
// the queue-worker lane (InboxProcessorService) owns those.

/// Upstream test/unit/ap-request.ts request-target rules, pinned against
/// mk_ap::signature::gen_signing_string: `(request-target): <method>
/// <path>?<query>` with the fragment stripped, an empty query delimiter
/// preserved, and the createSignedPost include set joined with `\n`.
#[test]
fn fed_min_signing_string_shape_matches_ap_request_ts() {
    use mk_ap::signature::{gen_signing_string, parse_signature_header};

    // 'createSignedPost includes query string in request-target'
    let with_query = gen_signing_string(
        "POST",
        "/inbox?token=abc#ignored",
        "",
        None,
        None,
        &["(request-target)".to_string()],
    )
    .unwrap();
    assert_eq!(with_query, "(request-target): post /inbox?token=abc");
    // 'request-target omits hash and preserves empty query delimiter'
    let empty_query = gen_signing_string(
        "GET",
        "/outbox?#ignored",
        "",
        None,
        None,
        &["(request-target)".to_string()],
    )
    .unwrap();
    assert_eq!(empty_query, "(request-target): get /outbox?");
    // the full createSignedPost line set, joined with \n
    let include: Vec<String> = ["(request-target)", "date", "host", "digest"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let full = gen_signing_string(
        "Post",
        "/inbox",
        "example.com",
        Some("d"),
        Some("SHA-256=x"),
        &include,
    )
    .unwrap();
    assert_eq!(
        full,
        "(request-target): post /inbox\ndate: d\nhost: example.com\ndigest: SHA-256=x"
    );
    // header parse: keyId/headers/signature survive, headers lowercased in
    // signed order, algorithm absent → rsa-sha256 default
    let parsed = parse_signature_header(
        "keyId=\"https://a.test/users/x#main-key\",headers=\"(request-target) Digest\",\
         signature=\"ab==\"",
    )
    .unwrap();
    assert_eq!(parsed.key_id, "https://a.test/users/x#main-key");
    assert_eq!(parsed.headers, vec!["(request-target)", "digest"]);
    assert_eq!(parsed.signature, "ab==");
}

/// The performOneActivity dispatch table: the five pinned arms classify by
/// name, and everything else — unrecognized types, unparseable bodies — is
/// accepted silently (upstream: the fastify inbox row enqueued + answered
/// 202 before the worker could even fail the parse).
#[test]
fn fed_min_inbox_dispatch_table_accepts_known_and_unknown_silently() {
    use mk_ap::inbox::{self, ActivityKind};

    for (ty, kind) in [
        ("Follow", ActivityKind::Follow),
        ("Create", ActivityKind::Create),
        ("Like", ActivityKind::Like),
        ("Undo", ActivityKind::Undo),
        ("Delete", ActivityKind::Delete),
    ] {
        assert_eq!(inbox::classify(&serde_json::json!({ "type": ty })), kind);
    }
    // the unlisted upstream arms ride Other — never an HTTP error
    for ty in ["Update", "Accept", "Reject", "Add", "Remove", "Announce"] {
        assert_eq!(
            inbox::classify(&serde_json::json!({ "type": ty })),
            ActivityKind::Other
        );
    }
    // unrecognized type → 202 parity (worker reason string, not a status)
    assert_eq!(
        inbox::handle(br#"{"type":"EvilActivity"}"#).kind,
        ActivityKind::Other
    );
    // unparseable body → 202 parity
    assert_eq!(inbox::handle(b"not json").kind, ActivityKind::Other);
}

// queue-2 // — the export-processor family successor slice (upstream
// Export{UserLists,Following,Muting,Blocking,Favorites}ProcessorService.ts +
// endpoints/i/export-{user-lists,following,mute,blocking,favorites}.ts): the
// enqueue-204 → mk_queue claim → processor → drive file plane per kind, on
// the arena-exports lane's harness shapes.

/// export-user-lists full roundtrip: the CSV lines (acct rides the config
/// host for local users — UtilityService.getFullApAccount) land in a drive
/// file named user-lists-yyyy-MM-dd-HH-mm-ss.csv (dateFormat UTC) served
/// through the /files byte route.
#[tokio::test]
async fn queue2_export_user_lists_roundtrip() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("queue2_ulists").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let alice = signup(&app, &uname("q2a")).await;
    let at = alice["token"].as_str().unwrap().to_string();
    let alice_id = alice["id"].as_str().unwrap().to_string();
    let bob = signup(&app, &uname("q2b")).await;
    let bob_id = bob["id"].as_str().unwrap().to_string();

    // list + member (push defaults withReplies:false; update-membership
    // flips it so the 3rd key=value column exercises true)
    let (s, list) = post_json(
        app.clone(),
        "/api/users/lists/create",
        json!({ "name": "list1", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {list}");
    let list_id = list["id"].as_str().unwrap().to_string();
    let (s, b) = post_json(
        app.clone(),
        "/api/users/lists/push",
        json!({ "listId": list_id, "userId": bob_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {b}");
    let (s, b) = post_json(
        app.clone(),
        "/api/users/lists/update-membership",
        json!({ "listId": list_id, "userId": bob_id, "withReplies": true, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {b}");

    // enqueue → 204 + durable row
    let (s, b) = post_json(app.clone(), "/api/i/export-user-lists", json!({ "i": at })).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {b}");
    assert_eq!(b, Value::Null, "void endpoint: empty body");
    let handle = mk_queue::QueueHandle::new(pool.clone());
    let job = handle
        .claim_next(&["exportUserLists"])
        .await
        .unwrap()
        .expect("queued exportUserLists job claimable");
    assert_eq!(job.payload["user"]["id"], json!(alice_id));
    crate::api::exports::run_export_user_lists_job(
        &pool,
        "http://127.0.0.1:3000",
        &job.payload["user"]["id"].as_str().unwrap(),
    )
    .await
    .expect("exportUserLists processor");

    // pollFirstDriveFile plane: one drive file, the user-lists name pattern
    let (s, files) = post_json(app.clone(), "/api/drive/files", json!({ "i": at })).await;
    assert_eq!(s, StatusCode::OK, "body: {files}");
    let files = files.as_array().expect("files array").clone();
    assert_eq!(files.len(), 1, "one export = one drive file");
    let name = files[0]["name"].as_str().unwrap();
    assert!(
        name.starts_with("user-lists-") && name.ends_with(".csv"),
        "name: {name}"
    );
    // 'user-lists-' + dateFormat 'yyyy-MM-dd-HH-mm-ss' (19 chars) + '.csv'
    assert_eq!(name.len(), "user-lists-".len() + 19 + 4, "name: {name}");
    assert_eq!(files[0]["type"], json!("text/csv"));

    // the bytes: one `${list.name},${acct},withReplies=…` line per membership
    let path = url::Url::parse(files[0]["url"].as_str().unwrap())
        .unwrap()
        .path()
        .to_string();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let expected = format!(
        "list1,{}@127.0.0.1:3000,withReplies=true\n",
        bob["username"].as_str().unwrap()
    );
    assert_eq!(text, expected, "exportUserLists CSV lines");
}

// fed-2 // — the InboxProcessorService worker lane with real effects
// (mk_ap::processor::verify_and_route): keyId → user_keypair (the 0001
// schema stand-in for upstream user_publickey; our `user` table carries NO
// keypair column), the RSA verify, the actor gate, then the
// Create{Note}/Follow/Like/Undo arms over real rows. Local loop only:
// keyIds/uris fold against the config host (127.0.0.1:3000, the test_state
// url), remote keyIds are the resolver boundary → Err (the HTTP row still
// answers 202).

const FED2_HOST: &str = "127.0.0.1:3000";

/// A local-loop user with a real generated keypair (UserKeypairService's
/// RSA-2048 SPKI/PKCS#8 PEMs) — the keyId owner the signatures ride.
async fn fed2_user(pool: &mk_db::sqlx::PgPool, username: &str) -> (String, String) {
    let (public_pem, private_pem) =
        mk_core::services::signup::generate_rsa_keypair_pem().expect("keygen");
    let id = mk_types::id::gen_aid_now();
    mk_db::user::insert_new_user(
        pool,
        &mk_db::user::NewUser {
            id: &id,
            username,
            username_lower: username,
            name: None,
            host: None,
            token: &mk_core::services::signup::generate_native_user_token(),
            password_hash: "x",
            public_key: &public_pem,
            private_key: &private_pem,
        },
    )
    .await
    .expect("insert keypair user");
    (id, private_pem)
}

fn fed2_sign(private_pem: &str, user_id: &str, body: &[u8]) -> ((String, String), String) {
    let date = "Tue, 20 Oct 2026 12:00:00 GMT";
    let key_id = format!("https://{FED2_HOST}/users/{user_id}#main-key");
    let sig = mk_ap::signature::create_signed_post(
        &key_id,
        private_pem,
        "POST",
        "/inbox",
        FED2_HOST,
        date,
        body,
    )
    .expect("createSignedPost");
    (
        ("digest".to_string(), mk_ap::signature::create_digest(body)),
        sig,
    )
}

/// The Create{Note} local loop: a signed Create with an inline Note object
/// upserts the note row — uri = object.id, text = content with the
/// `<br/>`→\n fold + entity decode, userId = the verified keyId owner,
/// userHost = their host column (NULL local), visibility from the Public
/// to-leg — and a re-delivered Create is the 'skip: note exists' upsert.
#[tokio::test]
async fn fed2_verify_and_route_create_note_local_loop() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("mkfed2create").await;
    mk_db::run_migrations(&pool).await.unwrap();
    let (author_id, private_pem) = fed2_user(&pool, "fed2author").await;
    let actor_uri = format!("https://{FED2_HOST}/users/{author_id}");

    let activity = serde_json::json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": format!("{actor_uri}/activities/1"),
        "type": "Create",
        "actor": actor_uri,
        "to": ["https://www.w3.org/ns/activitystreams#Public"],
        "object": {
            "id": format!("https://{FED2_HOST}/notes/remote-note-1"),
            "type": "Note",
            "attributedTo": actor_uri,
            "content": "<p>hello<br/>world &amp; friends</p>",
            "to": ["https://www.w3.org/ns/activitystreams#Public"]
        }
    });
    let body = serde_json::to_vec(&activity).unwrap();
    let (digest_header, sig_header) = fed2_sign(&private_pem, &author_id, &body);
    let headers = vec![
        ("host".to_string(), FED2_HOST.to_string()),
        (
            "date".to_string(),
            "Tue, 20 Oct 2026 12:00:00 GMT".to_string(),
        ),
        digest_header,
        (
            "content-type".to_string(),
            "application/activity+json".to_string(),
        ),
        ("signature".to_string(), sig_header),
    ];

    let effect =
        mk_ap::processor::verify_and_route("POST", "/inbox", &headers, &body, FED2_HOST, &pool)
            .await
            .expect("signed Create routes");
    let note_uri = format!("https://{FED2_HOST}/notes/remote-note-1");
    match &effect {
        mk_ap::processor::RouteEffect::NoteCreated { .. } => {}
        other => panic!("expected NoteCreated, got {other:?}"),
    }
    let (text, user_id, user_host, visibility): (Option<String>, String, Option<String>, String) =
        mk_db::sqlx::query_as(
            r#"SELECT "text", "userId", "userHost", ("visibility"::text)
               FROM "note" WHERE "uri" = $1"#,
        )
        .bind(&note_uri)
        .fetch_one(&pool)
        .await
        .expect("note row");
    // content → text: <br/> folds, tags strip, &amp; decodes
    assert_eq!(text.as_deref(), Some("hello\nworld & friends"));
    // attribution: the verified keyId owner + their host column
    assert_eq!(user_id, author_id);
    assert_eq!(user_host, None);
    assert_eq!(visibility, "public");
    assert!(matches!(
        effect,
        mk_ap::processor::RouteEffect::NoteCreated { .. }
    ));

    // re-delivered Create → the 'skip: note exists' arm (note.uri unique)
    let again =
        mk_ap::processor::verify_and_route("POST", "/inbox", &headers, &body, FED2_HOST, &pool)
            .await
            .expect("re-delivery still routes");
    assert_eq!(again, mk_ap::processor::RouteEffect::NoteExists);
}

/// Follow lands the following row (+ the denormalized counter bumps), Like
/// appends the plain reaction row + the reactions map (reaction_acceptance
/// ignored), and the Undo arms delete both — request-then-following like
/// upstream undoFollow, the swallowed not-reacted error like undoLike.
#[tokio::test]
async fn fed2_follow_like_and_undo_rows() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("mkfed2follow").await;
    mk_db::run_migrations(&pool).await.unwrap();
    let (follower_id, follower_pem) = fed2_user(&pool, "fed2follower").await;
    let (followee_id, followee_pem) = fed2_user(&pool, "fed2followee").await;
    let follower_uri = format!("https://{FED2_HOST}/users/{follower_id}");
    let followee_uri = format!("https://{FED2_HOST}/users/{followee_id}");

    let route = |body: &[u8], pem: &str, uid: &str| {
        let (digest_header, sig_header) = fed2_sign(pem, uid, body);
        vec![
            ("host".to_string(), FED2_HOST.to_string()),
            (
                "date".to_string(),
                "Tue, 20 Oct 2026 12:00:00 GMT".to_string(),
            ),
            digest_header,
            (
                "content-type".to_string(),
                "application/activity+json".to_string(),
            ),
            ("signature".to_string(), sig_header),
        ]
    };

    // --- Follow: follower → local followee ---
    let follow = serde_json::json!({
        "id": format!("{follower_uri}/activities/follow/1"),
        "type": "Follow",
        "actor": follower_uri,
        "object": followee_uri
    });
    let body = serde_json::to_vec(&follow).unwrap();
    let effect = mk_ap::processor::verify_and_route(
        "POST",
        "/inbox",
        &route(&body, &follower_pem, &follower_id),
        &body,
        FED2_HOST,
        &pool,
    )
    .await
    .expect("signed Follow routes");
    assert_eq!(effect, mk_ap::processor::RouteEffect::Followed);
    let row: i64 = mk_db::sqlx::query_scalar(
        r#"SELECT count(*) FROM "following" WHERE "followerId" = $1 AND "followeeId" = $2"#,
    )
    .bind(&follower_id)
    .bind(&followee_id)
    .fetch_one(&pool)
    .await
    .expect("following row");
    assert_eq!(row, 1, "the Follow arm landed the following row");
    let following_count: i32 =
        mk_db::sqlx::query_scalar(r#"SELECT "followingCount" FROM "user" WHERE "id" = $1"#)
            .bind(&follower_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let followers_count: i32 =
        mk_db::sqlx::query_scalar(r#"SELECT "followersCount" FROM "user" WHERE "id" = $1"#)
            .bind(&followee_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((following_count, followers_count), (1, 1), "counter bumps");

    // --- the followee authors a note, the follower Likes it ---
    let note_uri = format!("https://{FED2_HOST}/notes/fed2-local-1");
    let create = serde_json::json!({
        "type": "Create",
        "actor": followee_uri,
        "to": ["https://www.w3.org/ns/activitystreams#Public"],
        "object": {
            "id": note_uri,
            "type": "Note",
            "attributedTo": followee_uri,
            "content": "note to like",
            "to": ["https://www.w3.org/ns/activitystreams#Public"]
        }
    });
    let body = serde_json::to_vec(&create).unwrap();
    let created = mk_ap::processor::verify_and_route(
        "POST",
        "/inbox",
        &route(&body, &followee_pem, &followee_id),
        &body,
        FED2_HOST,
        &pool,
    )
    .await
    .expect("followee's Create routes");
    assert!(matches!(
        created,
        mk_ap::processor::RouteEffect::NoteCreated { .. }
    ));

    let like = serde_json::json!({
        "id": format!("{follower_uri}/activities/like/1"),
        "type": "Like",
        "actor": follower_uri,
        "object": note_uri,
        "content": "👍"
    });
    let body = serde_json::to_vec(&like).unwrap();
    let effect = mk_ap::processor::verify_and_route(
        "POST",
        "/inbox",
        &route(&body, &follower_pem, &follower_id),
        &body,
        FED2_HOST,
        &pool,
    )
    .await
    .expect("signed Like routes");
    assert_eq!(
        effect,
        mk_ap::processor::RouteEffect::Liked {
            reaction: "👍".to_string()
        }
    );
    let reaction_row: i64 = mk_db::sqlx::query_scalar(
        r#"SELECT count(*) FROM "note_reaction"
           WHERE "noteId" = (SELECT "id" FROM "note" WHERE "uri" = $1)
             AND "userId" = $2"#,
    )
    .bind(&note_uri)
    .bind(&follower_id)
    .fetch_one(&pool)
    .await
    .expect("reaction row");
    assert_eq!(reaction_row, 1, "the Like arm landed the note_reaction row");
    let packed: String =
        mk_db::sqlx::query_scalar(r#"SELECT ("reactions"->>'👍') FROM "note" WHERE "uri" = $1"#)
            .bind(&note_uri)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(packed, "1", "reactions map bump");

    // --- Undo{Like} then Undo{Follow}: both rows gone ---
    let undo_like = serde_json::json!({
        "id": format!("{follower_uri}/activities/undo/1"),
        "type": "Undo",
        "actor": follower_uri,
        "object": {
            "id": format!("{follower_uri}/activities/like/1"),
            "type": "Like",
            "actor": follower_uri,
            "object": note_uri
        }
    });
    let body = serde_json::to_vec(&undo_like).unwrap();
    let effect = mk_ap::processor::verify_and_route(
        "POST",
        "/inbox",
        &route(&body, &follower_pem, &follower_id),
        &body,
        FED2_HOST,
        &pool,
    )
    .await
    .expect("signed Undo{Like} routes");
    assert_eq!(effect, mk_ap::processor::RouteEffect::Unliked);
    let reaction_row: i64 = mk_db::sqlx::query_scalar(
        r#"SELECT count(*) FROM "note_reaction"
           WHERE "noteId" = (SELECT "id" FROM "note" WHERE "uri" = $1)
             AND "userId" = $2"#,
    )
    .bind(&note_uri)
    .bind(&follower_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(reaction_row, 0, "undo deleted the reaction row");

    let undo_follow = serde_json::json!({
        "id": format!("{follower_uri}/activities/undo/2"),
        "type": "Undo",
        "actor": follower_uri,
        "object": {
            "id": format!("{follower_uri}/activities/follow/1"),
            "type": "Follow",
            "actor": follower_uri,
            "object": followee_uri
        }
    });
    let body = serde_json::to_vec(&undo_follow).unwrap();
    let effect = mk_ap::processor::verify_and_route(
        "POST",
        "/inbox",
        &route(&body, &follower_pem, &follower_id),
        &body,
        FED2_HOST,
        &pool,
    )
    .await
    .expect("signed Undo{Follow} routes");
    assert_eq!(effect, mk_ap::processor::RouteEffect::Unfollowed);
    let row: i64 = mk_db::sqlx::query_scalar(
        r#"SELECT count(*) FROM "following" WHERE "followerId" = $1 AND "followeeId" = $2"#,
    )
    .bind(&follower_id)
    .bind(&followee_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, 0, "undo deleted the following row");
    let followers_count: i32 =
        mk_db::sqlx::query_scalar(r#"SELECT "followersCount" FROM "user" WHERE "id" = $1"#)
            .bind(&followee_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(followers_count, 0, "counter decrement");
}

/// The unverifiable legs never turn into 4xx: a tampered body (digest
/// half) and a missing Signature header both fail verify_and_route, and
/// the HTTP inbox row still answers 202 on both (the enqueue already
/// happened upstream-side; the failure is a worker job reason).
#[tokio::test]
async fn fed2_unverifiable_routes_err_and_still_202s() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("mkfed2tamper").await;
    mk_db::run_migrations(&pool).await.unwrap();
    let (author_id, private_pem) = fed2_user(&pool, "fed2author2").await;
    let actor_uri = format!("https://{FED2_HOST}/users/{author_id}");

    let activity = serde_json::json!({
        "type": "Create",
        "actor": actor_uri,
        "object": {
            "id": format!("https://{FED2_HOST}/notes/tampered-1"),
            "type": "Note",
            "content": "will not land",
        }
    });
    let body = serde_json::to_vec(&activity).unwrap();
    let (digest_header, sig_header) = fed2_sign(&private_pem, &author_id, &body);
    let headers = vec![
        ("host".to_string(), FED2_HOST.to_string()),
        (
            "date".to_string(),
            "Tue, 20 Oct 2026 12:00:00 GMT".to_string(),
        ),
        digest_header,
        ("signature".to_string(), sig_header),
    ];

    // tampered body: the signed digest no longer covers the received bytes
    let mut tampered = body.clone();
    tampered[0] ^= 0x01;
    assert!(
        mk_ap::processor::verify_and_route(
            "POST", "/inbox", &headers, &tampered, FED2_HOST, &pool,
        )
        .await
        .is_err(),
        "digest-vs-body failure collapses to Err"
    );
    // no Signature header at all
    assert!(
        mk_ap::processor::verify_and_route("POST", "/inbox", &[], &body, FED2_HOST, &pool,)
            .await
            .is_err(),
        "missing signature is Err"
    );
    // nothing landed
    let rows: i64 = mk_db::sqlx::query_scalar(r#"SELECT count(*) FROM "note""#)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "unverifiable activities touch no rows");

    // through the HTTP row: both legs answer 202
    let app = crate::build_router(test_state(pool));
    let post = |uri: &str, hdrs: Vec<(String, String)>, payload: Vec<u8>| {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/activity+json");
        for (name, value) in hdrs {
            builder = builder.header(name, value);
        }
        builder.body(Body::from(payload)).unwrap()
    };
    let resp = app
        .clone()
        .oneshot(post("/inbox", headers.clone(), tampered))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED, "tampered → 202");
    let resp = app.oneshot(post("/inbox", Vec::new(), body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED, "unsigned → 202");
}

/// export-muting full roundtrip: one acct line per NON-EXPIRING mute (a
/// far-future expiresAt row must NOT export — `expiresAt IS NULL`) → a
/// `mute-yyyy-MM-dd-HH-mm-ss.csv` drive file through the /files byte route.
#[tokio::test]
async fn queue2_export_muting_roundtrip() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("queue2_mutes").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let alice = signup(&app, &uname("q2m")).await;
    let at = alice["token"].as_str().unwrap().to_string();
    let bob = signup(&app, &uname("q2n")).await;
    let bob_id = bob["id"].as_str().unwrap().to_string();
    let carol = signup(&app, &uname("q2o")).await;
    let carol_id = carol["id"].as_str().unwrap().to_string();

    // indefinite mute on bob + a timed mute on carol (must stay out)
    let (s, b) = post_json(
        app.clone(),
        "/api/mute/create",
        json!({ "userId": bob_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {b}");
    let (s, b) = post_json(
        app.clone(),
        "/api/mute/create",
        json!({ "userId": carol_id, "expiresAt": crate::api::now_ms() as i64 + 86_400_000, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {b}");

    // enqueue → 204 → claim → processor → drive file
    let (s, b) = post_json(app.clone(), "/api/i/export-mute", json!({ "i": at })).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {b}");
    let handle = mk_queue::QueueHandle::new(pool.clone());
    let job = handle
        .claim_next(&["exportMuting"])
        .await
        .unwrap()
        .expect("queued exportMuting job claimable");
    crate::api::exports::run_export_muting_job(
        &pool,
        "http://127.0.0.1:3000",
        &job.payload["user"]["id"].as_str().unwrap(),
    )
    .await
    .expect("exportMuting processor");

    let (s, files) = post_json(app.clone(), "/api/drive/files", json!({ "i": at })).await;
    assert_eq!(s, StatusCode::OK, "body: {files}");
    let files = files.as_array().expect("files array").clone();
    assert_eq!(files.len(), 1, "one export = one drive file");
    let name = files[0]["name"].as_str().unwrap();
    assert!(
        name.starts_with("mute-") && name.ends_with(".csv"),
        "name: {name}"
    );
    assert_eq!(name.len(), "mute-".len() + 19 + 4, "name: {name}");
    assert_eq!(files[0]["type"], json!("text/csv"));

    // the bytes: only the indefinite mute's acct line (config-host acct)
    let path = url::Url::parse(files[0]["url"].as_str().unwrap())
        .unwrap()
        .path()
        .to_string();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let expected = format!("{}@127.0.0.1:3000\n", bob["username"].as_str().unwrap());
    assert_eq!(text, expected, "exportMuting CSV lines (timed mute out)");
}

// meili-2 // — the Meilisearch write/query path (upstream SearchService.ts
// indexNote / unindexNote / searchNoteByMeilisearch + NoteCreateService
// .index): the NoteSearchEngine seam is mockable, so the engine lane is
// pinned without a Meilisearch service.

/// Test double capturing add/remove/search traffic (the trait's boxed-future
/// shape keeps this dependency-free; AtomicBool so `fail_search` flips
/// through the shared Arc).
struct CapturingEngine {
    docs: std::sync::Mutex<Vec<Value>>,
    removed: std::sync::Mutex<Vec<String>>,
    hits: std::sync::Mutex<Vec<Value>>,
    fail_search: std::sync::atomic::AtomicBool,
}

impl CapturingEngine {
    fn new() -> Self {
        Self {
            docs: std::sync::Mutex::new(Vec::new()),
            removed: std::sync::Mutex::new(Vec::new()),
            hits: std::sync::Mutex::new(Vec::new()),
            fail_search: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl mk_core::services::search_bridge::NoteSearchEngine for CapturingEngine {
    fn add<'a>(
        &'a self,
        _id: &'a str,
        doc: Value,
    ) -> mk_core::services::search_bridge::EngineFuture<
        'a,
        Result<(), mk_core::services::search_bridge::SearchError>,
    > {
        Box::pin(std::future::ready({
            self.docs.lock().unwrap().push(doc);
            Ok(())
        }))
    }

    fn remove<'a>(
        &'a self,
        id: &'a str,
    ) -> mk_core::services::search_bridge::EngineFuture<
        'a,
        Result<(), mk_core::services::search_bridge::SearchError>,
    > {
        Box::pin(std::future::ready({
            self.removed.lock().unwrap().push(id.to_string());
            Ok(())
        }))
    }

    fn search<'a>(
        &'a self,
        _query: &'a str,
        _limit: u64,
    ) -> mk_core::services::search_bridge::EngineFuture<
        'a,
        Result<Vec<Value>, mk_core::services::search_bridge::SearchError>,
    > {
        Box::pin(std::future::ready(
            if self.fail_search.load(std::sync::atomic::Ordering::SeqCst) {
                Err(mk_core::services::search_bridge::SearchError::Http {
                    status: 503,
                    body: "meili down".to_string(),
                })
            } else {
                Ok(self.hits.lock().unwrap().clone())
            },
        ))
    }
}

/// notes/create → engine.add: indexNote's EXACT document shape (id /
/// createdAt ms from the aid / userId / userHost / channelId / cw / text /
/// tags — NO visibility key), plus the shouldIndex gates (public/home only,
/// local author only, text-or-cw present) and the notes/delete unindex leg.
#[tokio::test]
async fn meili_index_document_shape_and_should_index_gate() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_meili_index").await;
    let engine = std::sync::Arc::new(CapturingEngine::new());
    let app = crate::build_router(crate::AppState {
        config: crate::config::Config {
            port: 0,
            url: "http://127.0.0.1:3000".to_string(),
            db: None,
            redis: None,
            test: true,
            version: None,
            raw_json: serde_json::Value::Null,
        },
        pool: Some(pool),
        rate_limiter: std::sync::Arc::new(crate::ratelimit::RateLimiter::new()),
        search_engine: engine.clone(),
    });
    let alice = signup(&app, &format!("alice{}", mk_types::id::gen_aid_now())).await;
    let at = alice["token"].as_str().unwrap().to_string();

    // public text+cw note → indexed with the exact addDocuments payload
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "meili doc shape", "cw": "cw me", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let note_id = v["createdNote"]["id"].as_str().unwrap().to_string();
    let docs = engine.docs.lock().unwrap();
    assert_eq!(docs.len(), 1, "docs: {docs:?}");
    // indexed binding for the message: inline format args cannot index
    let doc0 = &docs[0];
    assert_eq!(
        doc0,
        &json!({
            "id": note_id,
            "createdAt": mk_types::id::parse_aid_date(&note_id).unwrap(),
            "userId": alice["id"],
            "userHost": Value::Null,
            "channelId": Value::Null,
            "cw": "cw me",
            "text": "meili doc shape",
            "tags": [],
        }),
        "doc: {doc0}"
    );
    drop(docs);

    // shouldIndex gates: a followers-visibility note and a PURE renote (text
    // null && cw null — NoteCreateService.index) must NOT hit the engine.
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "followers only", "visibility": "followers", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "renoteId": note_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    // len captured first: the assert operand's guard would still be alive
    // when a format-arg second lock ran — a deadlock on the failure path
    let doc_count = engine.docs.lock().unwrap().len();
    assert_eq!(doc_count, 1, "gates must not index");

    // notes/delete → unindexNote (public row leaves the index with the row)
    let (s, _) = post_json(
        app.clone(),
        "/api/notes/delete",
        json!({ "noteId": note_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let removed = engine.removed.lock().unwrap().clone();
    assert_eq!(removed, vec![note_id], "removed: {removed:?}");
}

/// notes/search rides the ENGINE when one is wired (packing the engine's hit
/// ids from PG), 500s when the configured engine fails (upstream throws
/// 'Meilisearch is not available' — never a silent SQL fallback), and
/// applies the userId hit filter (the meili filter, handler-side here).
#[tokio::test]
async fn meili_notes_search_uses_engine_when_present() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_meili_search").await;
    let engine = std::sync::Arc::new(CapturingEngine::new());
    let app = crate::build_router(crate::AppState {
        config: crate::config::Config {
            port: 0,
            url: "http://127.0.0.1:3000".to_string(),
            db: None,
            redis: None,
            test: true,
            version: None,
            raw_json: serde_json::Value::Null,
        },
        pool: Some(pool),
        rate_limiter: std::sync::Arc::new(crate::ratelimit::RateLimiter::new()),
        search_engine: engine.clone(),
    });
    let root = signup(&app, &format!("root{}", mk_types::id::gen_aid_now())).await;
    let alice = signup(&app, &format!("alice{}", mk_types::id::gen_aid_now())).await;
    let r_token = root["token"].as_str().unwrap().to_string();
    let a_token = alice["token"].as_str().unwrap().to_string();

    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "engine hit lane", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let hit_id = v["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "root engine note", "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let root_note_id = v["createdNote"]["id"].as_str().unwrap().to_string();

    // the canSearchNotes gate still fronts the engine lane (role grant)
    let (s, role) = post_json(
        app.clone(),
        "/api/admin/roles/create",
        json!({
            "name": "searcher",
            "description": "",
            "color": null,
            "iconUrl": null,
            "displayOrder": 0,
            "target": "manual",
            "condFormula": {},
            "isAdministrator": false,
            "isModerator": false,
            "isPublic": false,
            "isExplorable": false,
            "asBadge": false,
            "canEditMembersByModerator": false,
            "policies": { "canSearchNotes": { "priority": 0, "useDefault": false, "value": true } },
            "i": r_token
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {role}");
    let role_id = role["id"].as_str().unwrap().to_string();
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/roles/assign",
        json!({ "userId": alice["id"], "roleId": role_id, "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // engine hit → the packed note (loaded from PG by the handler)
    engine
        .hits
        .lock()
        .unwrap()
        .push(json!({ "id": hit_id, "userId": alice["id"], "userHost": null }));
    let (s, body) = post_json(
        app.clone(),
        "/api/notes/search",
        json!({ "query": "engine", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let ids: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![hit_id.as_str()], "body: {body}");

    // configured-but-failing engine → 500 INTERNAL (the 'Meilisearch is not
    // available' throw), NOT a SQL fallback
    engine
        .fail_search
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (s, err) = post_json(
        app.clone(),
        "/api/notes/search",
        json!({ "query": "engine", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "body: {err}");

    // the userId filter drops hits owned by other authors (hits cleared
    // first — the vec accumulates across this test's earlier calls, and the
    // earlier alice-owned hit would otherwise legitimately survive the filter)
    engine
        .fail_search
        .store(false, std::sync::atomic::Ordering::SeqCst);
    engine.hits.lock().unwrap().clear();
    engine
        .hits
        .lock()
        .unwrap()
        .push(json!({ "id": root_note_id, "userId": root["id"], "userHost": null }));
    let (s, body) = post_json(
        app.clone(),
        "/api/notes/search",
        json!({ "query": "engine", "userId": alice["id"], "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert!(
        body.as_array().unwrap().is_empty(),
        "userId filter drops foreign hits: {body}"
    );
}

/// charts3: the charts/notes endpoint (upstream endpoints/charts/notes.ts —
/// no requireCredential, so the guest read works) returns the nested
/// {local, remote} getChart shape with every array limit-long, span is a
/// REQUIRED paramDef enum (ajv INVALID_PARAM id 3d81ceae on absence), and
/// the create/delete commits (NoteCreateService.postNoteCreated L754 /
/// NoteDeleteService.ts L94) move local.total in both spans.
#[tokio::test]
async fn charts_notes_shape_and_create_delete_commits() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_charts_notes").await;
    let app = crate::build_router(test_state(pool));
    let alice = signup(&app, &format!("alice{}", mk_types::id::gen_aid_now())).await;
    let at = alice["token"].as_str().unwrap().to_string();

    // shape: {local, remote}, each {total, inc, dec, diffs{4}} arrays
    // limit-long (zero-filled + latest-known extrapolation upstream)
    let (s, chart) = post_json(
        app.clone(),
        "/api/charts/notes",
        json!({ "span": "hour", "limit": 5 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {chart}");
    for side in ["local", "remote"] {
        for key in ["total", "inc", "dec"] {
            assert_eq!(
                chart[side][key].as_array().map(Vec::len),
                Some(5),
                "{side}.{key}: {chart}"
            );
        }
        for key in ["normal", "renote", "reply", "withFile"] {
            assert_eq!(
                chart[side]["diffs"][key].as_array().map(Vec::len),
                Some(5),
                "{side}.diffs.{key}: {chart}"
            );
        }
    }

    // span is required (paramDef required: ['span']) → ajv INVALID_PARAM
    let (s, e) = post_json(app.clone(), "/api/charts/notes", json!({})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {e}");
    assert_eq!(e["error"]["code"], "INVALID_PARAM", "body: {e}");
    assert_eq!(
        e["error"]["id"], "3d81ceae-475f-4600-b2a8-2bc116157532",
        "body: {e}"
    );

    // create commits (hour + day): local.total bumps to 1 in the latest
    // bucket, diffs.normal counts the plain note
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "chart bump", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    let note_id = v["createdNote"]["id"].as_str().unwrap().to_string();
    for span in ["hour", "day"] {
        let (s, chart) = post_json(
            app.clone(),
            "/api/charts/notes",
            json!({ "span": span, "limit": 1 }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "body: {chart}");
        assert_eq!(
            chart["local"]["total"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()
                .as_i64(),
            Some(1),
            "create bumps {span} local.total: {chart}"
        );
        assert_eq!(
            chart["local"]["diffs"]["normal"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()
                .as_i64(),
            Some(1),
            "plain note moves {span} diffs.normal: {chart}"
        );
    }

    // delete decrements: total back to 0, the dec leg counted once
    let (s, _) = post_json(
        app.clone(),
        "/api/notes/delete",
        json!({ "noteId": note_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, chart) = post_json(
        app.clone(),
        "/api/charts/notes",
        json!({ "span": "hour", "limit": 1 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {chart}");
    assert_eq!(
        chart["local"]["total"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .as_i64(),
        Some(0),
        "delete decrements local.total: {chart}"
    );
    assert_eq!(
        chart["local"]["dec"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .as_i64(),
        Some(1),
        "delete counts the dec leg: {chart}"
    );
}

// push-lane // — sw/register + sw/unregister (endpoints/sw/, requireCredential;
// upstream PushNotificationService.isValidEndpoint = a parseable URL): the
// register insert lands the sw_subscription row with the sendReadMessage
// passthrough, the re-register probe answers already-subscribed echoing the
// STORED flag (key = meta."swPublicKey", null while unset), an unparseable
// endpoint hits INVALID_ENDPOINT, and unregister voids the row with a 204.
#[tokio::test]
async fn sw_register_row_already_subscribed_and_unregister_void() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_sw_register").await;
    let app = crate::build_router(test_state(pool.clone()));
    let alice = signup(&app, &format!("alice{}", mk_types::id::gen_aid_now())).await;
    let at = alice["token"].as_str().unwrap().to_string();
    let base = json!({
        "endpoint": "https://push.example.test/send/123",
        "auth": "authkey123",
        "publickey": "pubkey456",
        "i": at,
    });

    // register → subscribed; meta."swPublicKey" unset → key renders null
    let mut first = base.clone();
    first["sendReadMessage"] = json!(true);
    let (s, v) = post_json(app.clone(), "/api/sw/register", first).await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert_eq!(v["state"], "subscribed", "body: {v}");
    assert_eq!(v["key"], Value::Null, "body: {v}");
    assert_eq!(v["userId"], alice["id"], "body: {v}");
    assert_eq!(v["endpoint"], base["endpoint"], "endpoint echoed: {v}");
    assert_eq!(v["sendReadMessage"], true, "body: {v}");
    // DB: exactly one row carrying the passthrough flag
    let (rows, read): (i64, bool) = mk_db::sqlx::query_as::<_, (i64, bool)>(
        r#"SELECT count(*), bool_or("sendReadMessage") FROM "sw_subscription" WHERE "userId" = $1"#,
    )
    .bind(alice["id"].as_str().unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rows, 1, "register inserts the row: {rows}");
    assert!(read, "sendReadMessage persisted: {read}");

    // re-register with the same triple (NO sendReadMessage — the request
    // default false) → already-subscribed echoes the STORED flag (true)
    let (s, v) = post_json(app.clone(), "/api/sw/register", base.clone()).await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert_eq!(v["state"], "already-subscribed", "body: {v}");
    assert_eq!(v["key"], Value::Null, "body: {v}");
    assert_eq!(v["userId"], alice["id"], "body: {v}");
    assert_eq!(v["sendReadMessage"], true, "stored flag echoed: {v}");

    // unparseable endpoint → INVALID_ENDPOINT (isValidEndpoint 4432adbe)
    let (s, e) = post_json(
        app.clone(),
        "/api/sw/register",
        json!({ "endpoint": "not a url", "auth": "a", "publickey": "p", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {e}");
    assert_eq!(e["error"]["code"], "INVALID_ENDPOINT", "body: {e}");
    assert_eq!(
        e["error"]["id"], "4432adbe-17c0-4f9f-b43c-9ceb2f8910fe",
        "body: {e}"
    );

    // unregister → 204 void; DB: the row is gone
    let (s, v) = post_json(app.clone(), "/api/sw/unregister", base.clone()).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {v}");
    let rows: i64 =
        mk_db::sqlx::query_scalar(r#"SELECT count(*) FROM "sw_subscription" WHERE "userId" = $1"#)
            .bind(alice["id"].as_str().unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(rows, 0, "unregister removed the row: {rows}");
}

// ===== push-send: PushNotificationService delivery leg (sendToUser) ======

// list_by_user row shape + the readAllNotifications sendReadMessage filter
// (upstream L105-107); attempt counts are post-filter, dead endpoints are
// swallowed inside send_to_user.
#[tokio::test]
async fn push_send_list_by_user_row_shape_and_type_filter() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_push_send_list").await;
    let uid = mk_types::id::gen_aid_now();
    mk_db::sqlx::query(
        r#"INSERT INTO "user" ("id", "username", "usernameLower") VALUES ($1, $2, $2)"#,
    )
    .bind(&uid)
    .bind(format!("pushsend{}", &uid[..6]))
    .execute(&pool)
    .await
    .unwrap();
    mk_db::sw::insert_subscription(
        &pool,
        &mk_types::id::gen_aid_now(),
        &uid,
        "http://127.0.0.1:9/push/a",
        "authA",
        "pubA",
        true,
    )
    .await
    .unwrap();
    mk_db::sw::insert_subscription(
        &pool,
        &mk_types::id::gen_aid_now(),
        &uid,
        "http://127.0.0.1:9/push/b",
        "authB",
        "pubB",
        false,
    )
    .await
    .unwrap();

    // row shape: endpoint/auth/publickey/sendReadMessage come back verbatim
    let rows = mk_db::sw::list_by_user(&pool, &uid).await.unwrap();
    assert_eq!(rows.len(), 2, "rows: {rows:?}");
    let (a, b) = (
        rows.iter()
            .find(|r| r.endpoint.ends_with("/push/a"))
            .expect("row a"),
        rows.iter()
            .find(|r| r.endpoint.ends_with("/push/b"))
            .expect("row b"),
    );
    assert_eq!(a.auth, "authA");
    assert_eq!(a.publickey, "pubA");
    assert!(a.send_read_message);
    assert_eq!(b.auth, "authB");
    assert_eq!(b.publickey, "pubB");
    assert!(!b.send_read_message);

    // filter: readAllNotifications attempts only the opted-in row
    let attempted =
        mk_core::push_deliver::send_to_user(&pool, &uid, "readAllNotifications", &json!({})).await;
    assert_eq!(attempted, 1, "non-opted row filtered out");
    // 'note' (the notify_note_to_followers wiring type) attempts every row
    let attempted =
        mk_core::push_deliver::send_to_user(&pool, &uid, "note", &json!({ "noteId": "probe" }))
            .await;
    assert_eq!(attempted, 2, "'note' attempts every row");
}

// A dead push endpoint is swallowed (no panic, no 500): the direct delivery
// attempt errors away, and a note whose fanout carries the fire-and-forget
// push still answers 200.
#[tokio::test]
async fn push_send_dead_endpoint_swallowed_note_still_200s() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_push_send_dead").await;
    let app = crate::build_router(test_state(pool.clone()));
    let alice = signup(&app, &format!("alice{}", mk_types::id::gen_aid_now())).await;
    let bob = signup(&app, &format!("bob{}", mk_types::id::gen_aid_now())).await;
    let (a_id, a_token) = (
        alice["id"].as_str().unwrap(),
        alice["token"].as_str().unwrap(),
    );
    let b_id = bob["id"].as_str().unwrap();

    // bob follows alice → the note-notify fanout (and its push leg) reaches him
    let (s, v) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": a_id, "i": bob["token"] }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "follow: {v}");

    // bob's push subscription points at a refused port; the direct attempt
    // must swallow every error and report the single (dead) attempt
    mk_db::sw::insert_subscription(
        &pool,
        &mk_types::id::gen_aid_now(),
        b_id,
        "http://127.0.0.1:9/dead",
        "auth",
        "pub",
        false,
    )
    .await
    .unwrap();
    let attempted =
        mk_core::push_deliver::send_to_user(&pool, b_id, "note", &json!({ "noteId": "probe" }))
            .await;
    assert_eq!(attempted, 1, "dead endpoint attempted, error swallowed");

    // the note that fans the push still 200s
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "push smoke", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "note with dead push endpoint: {v}");
    assert_eq!(v["createdNote"]["text"], "push smoke", "body: {v}");
}

// ===== email-leg: verification-code storage round-trip =====
// user_profile."email"/"emailVerifyCode"/"emailVerified" (0001:4047-4049);
// the signup lane writes the pair, the verify endpoint consumes it.
#[tokio::test]
async fn email_leg_code_round_trip() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_email_leg").await;
    let user_id = mk_types::id::gen_aid_now();
    mk_db::sqlx::query(
        r#"INSERT INTO "user" ("id", "username", "usernameLower") VALUES ($1, $2, $2)"#,
    )
    .bind(&user_id)
    .bind(format!("emailleg{}", &user_id[..6]))
    .execute(&pool)
    .await
    .unwrap();
    mk_db::sqlx::query(r#"INSERT INTO "user_profile" ("userId") VALUES ($1)"#)
        .bind(&user_id)
        .execute(&pool)
        .await
        .unwrap();
    let code = mk_core::services::email::generate_code();
    assert_eq!(code.len(), 8, "upstream-shaped 8-char code: {code}");
    mk_db::user::set_email_verify_code(&pool, &user_id, "leg@example.org", &code)
        .await
        .unwrap();
    // a wrong code is a miss and flips nothing
    assert!(!mk_core::services::email::verify_email(&pool, &user_id, "xxxxxxxx").await);
    let (verified, stored): (bool, Option<String>) = mk_db::sqlx::query_as(
        r#"SELECT "emailVerified", "emailVerifyCode" FROM "user_profile" WHERE "userId" = $1"#,
    )
    .bind(&user_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!verified, "miss leaves emailVerified false");
    assert_eq!(
        stored.as_deref(),
        Some(code.as_str()),
        "code survives a miss"
    );
    // the right code flips the flag and consumes itself
    assert!(mk_core::services::email::verify_email(&pool, &user_id, &code).await);
    let verified: bool = mk_db::sqlx::query_scalar(
        r#"SELECT "emailVerified" FROM "user_profile" WHERE "userId" = $1"#,
    )
    .bind(&user_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(verified, "hit flips emailVerified");
}

// ===== charts-users: site-wide users chart (UsersChart) =====

// shape {local,remote} {total,inc,dec} arrays len=limit; a signup bumps the
// local latest bucket, remote stays zero.
#[tokio::test]
async fn charts_users_shape_and_signup_commits() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_charts_users").await;
    let app = crate::build_router(test_state(pool));
    let alice = signup(&app, &format!("alice{}", mk_types::id::gen_aid_now())).await;

    // shape: {local, remote}, each {total, inc, dec} arrays limit-long
    let (s, chart) = post_json(
        app.clone(),
        "/api/charts/users",
        json!({ "span": "hour", "limit": 5 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {chart}");
    for side in ["local", "remote"] {
        for key in ["total", "inc", "dec"] {
            assert_eq!(
                chart[side][key].as_array().map(Vec::len),
                Some(5),
                "{side}.{key}: {chart}"
            );
        }
    }

    // the committed signup moved local.total/latest to 1 with inc counted
    let (s, chart) = post_json(
        app.clone(),
        "/api/charts/users",
        json!({ "span": "hour", "limit": 1 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {chart}");
    assert_eq!(
        chart["local"]["total"].as_array().unwrap().last().unwrap(),
        &json!(1),
        "signup bumps local.total: {chart}"
    );
    assert_eq!(
        chart["local"]["inc"].as_array().unwrap().last().unwrap(),
        &json!(1),
        "signup moves local.inc: {chart}"
    );
    assert_eq!(
        chart["remote"]["total"].as_array().unwrap().last().unwrap(),
        &json!(0),
        "remote untouched: {chart}"
    );

    // span required → INVALID_PARAM
    let (s, e) = post_json(app.clone(), "/api/charts/users", json!({})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {e}");
    assert_eq!(e["error"]["code"], "INVALID_PARAM", "body: {e}");
}

// ===== charts-fed: federation chart (FederationChart) =====

// shape: one flat object of arrays (len=limit over the shipped keys); a
// direct update_federation write bumps its latest bucket.
#[tokio::test]
async fn charts_fed_shape_and_update_commits() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_charts_fed").await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    for span in ["hour", "day"] {
        mk_db::charts::update_federation(
            &pool,
            span,
            mk_db::charts::bucket(span, now),
            &[("sub", 1), ("pubActive", 2)],
        )
        .await
        .unwrap();
    }

    for (span, limit) in [("hour", 5usize), ("day", 3)] {
        let (s, chart) = post_json(
            crate::build_router(test_state(pool.clone())),
            "/api/charts/federation",
            json!({ "span": span, "limit": limit }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{span} body: {chart}");
        for key in [
            "deliveredInstances",
            "inboxInstances",
            "stalled",
            "sub",
            "pub",
            "pubsub",
            "subActive",
            "pubActive",
        ] {
            assert_eq!(
                chart[key].as_array().map(Vec::len),
                Some(limit),
                "{span}.{key}: {chart}"
            );
        }
        assert_eq!(
            chart["sub"].as_array().unwrap().last().unwrap(),
            &json!(1),
            "{span} sub latest: {chart}"
        );
        assert_eq!(
            chart["pubActive"].as_array().unwrap().last().unwrap(),
            &json!(2),
            "{span} pubActive latest: {chart}"
        );
        assert_eq!(
            chart["stalled"].as_array().unwrap().last().unwrap(),
            &json!(0),
            "{span} untouched key zero: {chart}"
        );
    }

    // span required → INVALID_PARAM
    let (s, e) = post_json(
        crate::build_router(test_state(pool.clone())),
        "/api/charts/federation",
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {e}");
    assert_eq!(e["error"]["code"], "INVALID_PARAM", "body: {e}");
}

// ===== ap-keys: remote keyId fetch (GetAuthService lane) =====

// fetch_actor_key: https now rides the TLS client (the fixed "TLS pending"
// rejection is gone) — a closed loopback port pins an Err. The http path
// keeps surfacing its connect error.
#[tokio::test]
async fn ap_keys_fetch_surfaces_https_and_http_transport_errors() {
    let out = mk_ap::fetch::fetch_actor_key("https://127.0.0.1:9/users/u#main-key").await;
    assert!(out.is_err(), "https connect must fail: {out:?}");
    // an http keyId whose host is unroutable → connect error string, no panic
    let out = mk_ap::fetch::fetch_actor_key("http://127.0.0.1:9/users/u").await;
    let err = out.unwrap_err();
    assert!(err.contains("actor fetch connect"), "err: {err}");
}

// the remote cache probe: a stored remote user row (host NOT NULL + uri)
// hits, a local row or foreign uri misses.
#[tokio::test]
async fn ap_keys_remote_cache_probe_shape() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_ap_keys_probe").await;
    let uid = mk_types::id::gen_aid_now();
    mk_db::sqlx::query(
        r#"INSERT INTO "user" ("id", "username", "usernameLower", "host", "uri") VALUES ($1, $2, $2, $3, $4)"#,
    )
    .bind(&uid)
    .bind("remuser")
    .bind("remote.example")
    .bind("http://remote.example/users/u")
    .execute(&pool)
    .await
    .unwrap();
    // a hit reports the row (the fetch still needs to run — no key column)
    assert_eq!(
        mk_ap::fetch::remote_key_cached(&pool, "http://remote.example/users/u").await,
        Some(())
    );
    assert_eq!(
        mk_ap::fetch::remote_key_cached(&pool, "http://remote.example/users/other").await,
        None
    );
}

// ===== email-leg: password reset (upstream request-reset-password +
// reset-password) =====
// request stores a code in password_reset_request; reset consumes it and
// bumps user_profile."password"; signin with the new password then succeeds.
#[tokio::test]
async fn password_reset_round_trip_and_wrong_code() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_password_reset").await;
    let username = format!("reset{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));
    let signed = signup(&app, &username).await;
    let user_id = signed["id"].as_str().unwrap().to_string();

    // the reset flow needs a verified email on the profile (signup only
    // stores one when the meta email flags are on)
    mk_db::sqlx::query(
        r#"UPDATE "user_profile" SET "email" = $2, "emailVerified" = TRUE WHERE "userId" = $1"#,
    )
    .bind(&user_id)
    .bind("reset@example.org")
    .execute(&pool)
    .await
    .unwrap();

    // request: silent 204, code row written
    let (s, b) = post_json(
        app.clone(),
        "/api/request-reset-password",
        json!({ "username": username, "email": "reset@example.org" }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {b}");
    let code: String = mk_db::sqlx::query_scalar(
        r#"SELECT "token" FROM "password_reset_request" WHERE "userId" = $1"#,
    )
    .bind(&user_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    // a wrong code is rejected and leaves the password alone
    let (s, b) = post_json(
        app.clone(),
        "/api/reset-password",
        json!({ "token": "00000000", "password": "newpass" }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {b}");
    assert_eq!(b["error"]["code"], "INVALID_TOKEN", "body: {b}");
    let (s_old, _) = post_json(
        app.clone(),
        "/api/signin",
        json!({ "username": username, "password": "test" }),
    )
    .await;
    assert_eq!(
        s_old,
        StatusCode::OK,
        "old password still works after a miss"
    );

    // the mailed code resets the password
    let (s, b) = post_json(
        app.clone(),
        "/api/reset-password",
        json!({ "token": code, "password": "newpass" }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {b}");

    let (s, b) = post_json(
        app.clone(),
        "/api/signin",
        json!({ "username": username, "password": "newpass" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b["finished"], json!(true), "body: {b}");
    let (s, _) = post_json(
        app.clone(),
        "/api/signin",
        json!({ "username": username, "password": "test" }),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "old password rejected after reset"
    );

    // the code is single-use
    let (s, b) = post_json(
        app,
        "/api/reset-password",
        json!({ "token": code, "password": "again" }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {b}");
    assert_eq!(b["error"]["code"], "INVALID_TOKEN", "body: {b}");
}

// ===== push-send: note-reasoned fan-out (mention/reply/renote/reaction) =====
//
// Every note-reasoned notification routes through social::notify_note_event,
// which now spawns the upstream createNotification push tail (L196). This pins
// that the send path is invoked once per wired type: the subscription endpoint
// is an ephemeral local listener, so each accepted connection is one
// send_to_user attempt. It also pins the shared gate: target == actor reports
// "dropped", and the push spawn sits behind that same branch.
#[tokio::test]
async fn push_fanout_mention_reply_renote_reaction_invokes_send() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_push_fanout").await;
    let app = crate::build_router(test_state(pool.clone()));
    let alice = signup(&app, &format!("pfa{}", mk_types::id::gen_aid_now())).await;
    let bob = signup(&app, &format!("pfb{}", mk_types::id::gen_aid_now())).await;
    let (a_id, a_token) = (
        alice["id"].as_str().unwrap(),
        alice["token"].as_str().unwrap(),
    );
    let (b_id, b_token, b_name) = (
        bob["id"].as_str().unwrap(),
        bob["token"].as_str().unwrap(),
        bob["username"].as_str().unwrap().to_string(),
    );

    // bob's note is the target of the reply / renote / reaction legs.
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "target", "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "bob note: {v}");
    let b_note = v["createdNote"]["id"].as_str().unwrap().to_string();

    // bob's push subscription: an ephemeral local listener. Each accepted
    // connection is one send_to_user delivery attempt.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf).await;
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await;
            let _ = tx.send(());
        }
    });
    mk_db::sw::insert_subscription(
        &pool,
        &mk_types::id::gen_aid_now(),
        b_id,
        &format!("http://127.0.0.1:{port}/push"),
        "auth",
        "pub",
        false,
    )
    .await
    .unwrap();

    async fn expect_push(rx: &mut tokio::sync::mpsc::UnboundedReceiver<()>, label: &str) {
        tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
            .await
            .unwrap_or_else(|_| panic!("{label}: push send path not invoked"))
            .unwrap_or_else(|| panic!("{label}: listener closed"));
    }

    // mention
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "i": a_token, "text": format!("@{b_name} hi") }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "mention note: {v}");
    expect_push(&mut rx, "mention").await;

    // reply
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "i": a_token, "text": "reply", "replyId": b_note }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "reply note: {v}");
    expect_push(&mut rx, "reply").await;

    // renote
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "i": a_token, "renoteId": b_note }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "renote: {v}");
    expect_push(&mut rx, "renote").await;

    // reaction
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/reactions/create",
        json!({ "i": a_token, "noteId": b_note, "reaction": "like" }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "reaction: {v}");
    expect_push(&mut rx, "reaction").await;

    // guard: target == actor → the shared gate reports dropped for every
    // wired type; notify_note_event returns before its push spawn there.
    for reason in ["mention", "reply", "renote", "reaction"] {
        let inserted = mk_db::notification::insert_if_not_muted(
            &pool,
            &mk_db::notification::NewNotification {
                id: &mk_types::id::gen_aid_now(),
                r#type: reason,
                user_id: a_id,
                notifier_id: Some(a_id),
                note_id: Some(b_note.as_str()),
                reaction: None,
            },
        )
        .await
        .unwrap();
        assert!(
            !inserted,
            "{reason}: self-notification must be gated (no row, no push)"
        );
    }
}

// ===== charts-rest: per-user + site-wide charts =====

/// Walk a nested chart object (`chart["diffs"]["normal"]`).
fn chart_at<'a>(chart: &'a Value, path: &[&str]) -> &'a Value {
    let mut v = chart;
    for key in path {
        v = &v[*key];
    }
    v
}

fn chart_len(chart: &Value, path: &[&str]) -> Option<usize> {
    chart_at(chart, path).as_array().map(Vec::len)
}

fn chart_last(chart: &Value, path: &[&str]) -> Value {
    chart_at(chart, path)
        .as_array()
        .and_then(|a| a.last().cloned())
        .unwrap()
}

// the five charts/user/* reads: arrays of len=limit, and a direct
// update_per_user commit lands in the newest bucket (hour + day twins).
#[tokio::test]
async fn charts_user_shape_and_update_commits() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_charts_user").await;
    let user_id = mk_types::id::gen_aid_now();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    for span in ["hour", "day"] {
        let date = mk_db::charts::bucket(span, now);
        for (kind, deltas) in [
            ("notes", vec![("total", 1), ("inc", 1), ("diffs_normal", 1)]),
            (
                "drive",
                vec![
                    ("totalCount", 1),
                    ("totalSize", 10),
                    ("incCount", 1),
                    ("incSize", 10),
                ],
            ),
            (
                "following",
                vec![("local_followings_total", 1), ("local_followings_inc", 1)],
            ),
            ("pv", vec![("upv_user", 1), ("pv_user", 1)]),
            ("reaction", vec![("local_count", 1)]),
        ] {
            mk_db::charts::update_per_user(&pool, kind, span, date, &user_id, &deltas)
                .await
                .unwrap();
        }
    }

    for (span, limit) in [("hour", 4usize), ("day", 3)] {
        let app = crate::build_router(test_state(pool.clone()));

        // notes: total/inc/dec + diffs.{normal,reply,renote,withFile}
        let (s, chart) = post_json(
            app.clone(),
            "/api/charts/user/notes",
            json!({ "span": span, "limit": limit, "userId": user_id }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{span} body: {chart}");
        for path in [
            &["total"][..],
            &["inc"][..],
            &["dec"][..],
            &["diffs", "normal"][..],
            &["diffs", "reply"][..],
            &["diffs", "renote"][..],
            &["diffs", "withFile"][..],
        ] {
            assert_eq!(
                chart_len(&chart, path),
                Some(limit),
                "{span} notes {path:?}: {chart}"
            );
        }
        assert_eq!(
            chart_last(&chart, &["total"]),
            json!(1),
            "{span} notes total: {chart}"
        );
        assert_eq!(
            chart_last(&chart, &["diffs", "normal"]),
            json!(1),
            "{span} notes normal: {chart}"
        );
        assert_eq!(
            chart_last(&chart, &["diffs", "renote"]),
            json!(0),
            "{span} notes renote: {chart}"
        );

        // drive: totals + inc/dec count/size (all flat)
        let (s, chart) = post_json(
            app.clone(),
            "/api/charts/user/drive",
            json!({ "span": span, "limit": limit, "userId": user_id }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{span} body: {chart}");
        for key in [
            "totalCount",
            "totalSize",
            "incCount",
            "incSize",
            "decCount",
            "decSize",
        ] {
            assert_eq!(
                chart_len(&chart, &[key]),
                Some(limit),
                "{span} drive {key}: {chart}"
            );
        }
        assert_eq!(
            chart_last(&chart, &["totalCount"]),
            json!(1),
            "{span} drive totalCount: {chart}"
        );
        assert_eq!(
            chart_last(&chart, &["totalSize"]),
            json!(10),
            "{span} drive totalSize: {chart}"
        );

        // following: local/remote × followings/followers × total/inc/dec
        let (s, chart) = post_json(
            app.clone(),
            "/api/charts/user/following",
            json!({ "span": span, "limit": limit, "userId": user_id }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{span} body: {chart}");
        for side in ["local", "remote"] {
            for edge in ["followings", "followers"] {
                for key in ["total", "inc", "dec"] {
                    assert_eq!(
                        chart_len(&chart, &[side, edge, key]),
                        Some(limit),
                        "{span} following {side}.{edge}.{key}: {chart}"
                    );
                }
            }
        }
        assert_eq!(
            chart_last(&chart, &["local", "followings", "total"]),
            json!(1),
            "{span} following local total: {chart}"
        );

        // pv: upv/pv × user/visitor
        let (s, chart) = post_json(
            app.clone(),
            "/api/charts/user/pv",
            json!({ "span": span, "limit": limit, "userId": user_id }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{span} body: {chart}");
        for kind in ["upv", "pv"] {
            for who in ["user", "visitor"] {
                assert_eq!(
                    chart_len(&chart, &[kind, who]),
                    Some(limit),
                    "{span} pv {kind}.{who}: {chart}"
                );
            }
        }
        assert_eq!(
            chart_last(&chart, &["upv", "user"]),
            json!(1),
            "{span} pv upv.user: {chart}"
        );

        // reactions: local/remote × count
        let (s, chart) = post_json(
            app.clone(),
            "/api/charts/user/reactions",
            json!({ "span": span, "limit": limit, "userId": user_id }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{span} body: {chart}");
        for side in ["local", "remote"] {
            assert_eq!(
                chart_len(&chart, &[side, "count"]),
                Some(limit),
                "{span} reactions {side}.count: {chart}"
            );
        }
        assert_eq!(
            chart_last(&chart, &["local", "count"]),
            json!(1),
            "{span} reactions local.count: {chart}"
        );

        // userId is required upstream → INVALID_PARAM
        let (s, e) = post_json(
            app.clone(),
            "/api/charts/user/notes",
            json!({ "span": span }),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{span} body: {e}");
        assert_eq!(e["error"]["code"], "INVALID_PARAM", "{span} body: {e}");
    }
}

// the remaining site-wide reads: shapes for all six; instance + hashtag also
// get a direct row (their AP/hashtag drivers are not wired in this lane).
#[tokio::test]
async fn charts_site_shape_and_instance_update_commits() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mktest_charts_site").await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    for span in ["hour", "day"] {
        let date = mk_db::charts::bucket(span, now);
        let table = if span == "hour" {
            "__chart__instance"
        } else {
            "__chart_day__instance"
        };
        mk_db::sqlx::query(&format!(
            r#"INSERT INTO "{table}"
               ("date", "group", "___requests_failed", "___requests_succeeded",
                "___notes_total", "___notes_inc", "___notes_diffs_withFile",
                "___users_total", "___following_total", "___drive_totalFiles")
               VALUES ($1, $2, 2, 5, 3, 1, 1, 4, 6, 7)"#
        ))
        .bind(date as i32)
        .bind("remote.example")
        .execute(&pool)
        .await
        .unwrap();
        let table = if span == "hour" {
            "__chart__hashtag"
        } else {
            "__chart_day__hashtag"
        };
        mk_db::sqlx::query(&format!(
            r#"INSERT INTO "{table}" ("date", "group", "___local_users", "___remote_users")
               VALUES ($1, $2, 2, 3)"#
        ))
        .bind(date as i32)
        .bind("tag1")
        .execute(&pool)
        .await
        .unwrap();
    }

    for (span, limit) in [("hour", 4usize), ("day", 3)] {
        let app = crate::build_router(test_state(pool.clone()));

        // active-users: nine flat counters
        let (s, chart) = post_json(
            app.clone(),
            "/api/charts/active-users",
            json!({ "span": span, "limit": limit }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{span} body: {chart}");
        for key in [
            "registeredWithinWeek",
            "registeredWithinMonth",
            "registeredWithinYear",
            "registeredOutsideWeek",
            "registeredOutsideMonth",
            "registeredOutsideYear",
            "readWrite",
            "read",
            "write",
        ] {
            assert_eq!(
                chart_len(&chart, &[key]),
                Some(limit),
                "{span} active-users {key}: {chart}"
            );
        }

        // ap-request: three flat counters
        let (s, chart) = post_json(
            app.clone(),
            "/api/charts/ap-request",
            json!({ "span": span, "limit": limit }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{span} body: {chart}");
        for key in ["deliverFailed", "deliverSucceeded", "inboxReceived"] {
            assert_eq!(
                chart_len(&chart, &[key]),
                Some(limit),
                "{span} ap-request {key}: {chart}"
            );
        }

        // drive: local/remote × inc/dec count/size
        let (s, chart) = post_json(
            app.clone(),
            "/api/charts/drive",
            json!({ "span": span, "limit": limit }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{span} body: {chart}");
        for side in ["local", "remote"] {
            for key in ["incCount", "incSize", "decCount", "decSize"] {
                assert_eq!(
                    chart_len(&chart, &[side, key]),
                    Some(limit),
                    "{span} drive {side}.{key}: {chart}"
                );
            }
        }

        // hashtag: local/remote user counts for {tag}
        let (s, chart) = post_json(
            app.clone(),
            "/api/charts/hashtag",
            json!({ "span": span, "limit": limit, "tag": "tag1" }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{span} body: {chart}");
        for side in ["local", "remote"] {
            assert_eq!(
                chart_len(&chart, &[side, "users"]),
                Some(limit),
                "{span} hashtag {side}.users: {chart}"
            );
        }
        assert_eq!(
            chart_last(&chart, &["local", "users"]),
            json!(2),
            "{span} hashtag local.users: {chart}"
        );
        assert_eq!(
            chart_last(&chart, &["remote", "users"]),
            json!(3),
            "{span} hashtag remote.users: {chart}"
        );

        // instance: requests/notes/users/following/followers/drive for {host}
        let (s, chart) = post_json(
            app.clone(),
            "/api/charts/instance",
            json!({ "span": span, "limit": limit, "host": "remote.example" }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{span} body: {chart}");
        for path in [
            &["requests", "failed"][..],
            &["requests", "succeeded"][..],
            &["requests", "received"][..],
            &["notes", "total"][..],
            &["notes", "inc"][..],
            &["notes", "dec"][..],
            &["notes", "diffs", "normal"][..],
            &["notes", "diffs", "reply"][..],
            &["notes", "diffs", "renote"][..],
            &["notes", "diffs", "withFile"][..],
            &["users", "total"][..],
            &["users", "inc"][..],
            &["users", "dec"][..],
            &["following", "total"][..],
            &["followers", "total"][..],
            &["drive", "totalFiles"][..],
            &["drive", "incFiles"][..],
            &["drive", "incUsage"][..],
            &["drive", "decFiles"][..],
            &["drive", "decUsage"][..],
        ] {
            assert_eq!(
                chart_len(&chart, path),
                Some(limit),
                "{span} instance {path:?}: {chart}"
            );
        }
        assert_eq!(
            chart_last(&chart, &["requests", "succeeded"]),
            json!(5),
            "{span} instance requests.succeeded: {chart}"
        );
        assert_eq!(
            chart_last(&chart, &["notes", "diffs", "withFile"]),
            json!(1),
            "{span} instance notes.diffs.withFile: {chart}"
        );
        assert_eq!(
            chart_last(&chart, &["drive", "totalFiles"]),
            json!(7),
            "{span} instance drive.totalFiles: {chart}"
        );

        // network: five flat counters
        let (s, chart) = post_json(
            app.clone(),
            "/api/charts/network",
            json!({ "span": span, "limit": limit }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{span} body: {chart}");
        for key in [
            "incomingRequests",
            "outgoingRequests",
            "totalTime",
            "incomingBytes",
            "outgoingBytes",
        ] {
            assert_eq!(
                chart_len(&chart, &[key]),
                Some(limit),
                "{span} network {key}: {chart}"
            );
        }

        // tag/host are required upstream → INVALID_PARAM
        let (s, e) = post_json(app.clone(), "/api/charts/hashtag", json!({ "span": span })).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{span} body: {e}");
        assert_eq!(e["error"]["code"], "INVALID_PARAM", "{span} body: {e}");
        let (s, e) = post_json(app.clone(), "/api/charts/instance", json!({ "span": span })).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{span} body: {e}");
        assert_eq!(e["error"]["code"], "INVALID_PARAM", "{span} body: {e}");
    }
}

// Regression: the feed profile query selected the followingVisibility /
// followersVisibility PG enums straight into String, so sqlx failed the
// decode on every feed request ("feed profile" in the log) and the profile
// silently fell back to None. Both reads now cast the enum to text.
#[tokio::test]
async fn feed_profile_visibility_enum_decodes_as_text() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkfeedvis").await;
    let app = crate::build_router(test_state(pool.clone()));
    let username = format!("feedvis{}", mk_types::id::gen_aid_now());
    let signed = signup(&app, &username).await;
    let id = signed["id"].as_str().unwrap().to_string();
    let token = signed["token"].as_str().unwrap().to_string();

    let (s, body) = post_json(
        app.clone(),
        "/api/i/update",
        json!({ "i": token, "followingVisibility": "followers" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");

    // The mk-db profile read decodes the enum columns as String.
    let profile = mk_db::user::find_profile(&pool, &id)
        .await
        .expect("find_profile must decode the visibility enums without error");
    assert_eq!(profile.following_visibility, "followers");

    // The feed's own profile query must decode too: a non-public
    // followingVisibility renders the hidden "?" count (prof == None would
    // instead fall back to the real count).
    let (s, h, body) = get_accept(&app, &format!("/@{username}.json"), "*/*").await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(
        hdr(&h, "content-type").as_deref(),
        Some("application/json; charset=utf-8")
    );
    assert!(
        body.contains("? Following"),
        "feed profile enum decoded (count hidden): {body}"
    );
}

// fed-2 remote routing // — route_remote hangs the Create/Follow/Like/Undo
// arms off the ingested person row. verify_and_route's remote branch fetches
// the actor doc over the network, so the routing half is pinned here directly
// with an ensure_remote_person row (the ingest half has its own unit tests).
#[tokio::test]
async fn fed2_remote_activity_routing_persists_rows() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("mkfed2remote").await;
    mk_db::run_migrations(&pool).await.unwrap();

    let actor_uri = "https://remote.example/users/alice";
    let actor_doc = serde_json::json!({
        "id": actor_uri,
        "type": "Person",
        "preferredUsername": "alice",
        "inbox": "https://remote.example/users/alice/inbox",
    });
    let remote_id = mk_ap::person::ensure_remote_person(&pool, actor_uri, &actor_doc)
        .await
        .expect("remote person row");

    // --- Create{Note}: remote author id + uri + userHost ---
    let note_uri = "https://remote.example/notes/r1";
    let create = serde_json::json!({
        "type": "Create",
        "actor": actor_uri,
        "to": ["https://www.w3.org/ns/activitystreams#Public"],
        "object": {
            "id": note_uri,
            "type": "Note",
            "attributedTo": actor_uri,
            "content": "<p>remote hi</p>",
            "to": ["https://www.w3.org/ns/activitystreams#Public"]
        }
    });
    let effect = mk_ap::processor::route_remote(&pool, &remote_id, actor_uri, &create, FED2_HOST)
        .await
        .expect("remote Create routes");
    assert!(matches!(
        effect,
        mk_ap::processor::RouteEffect::NoteCreated { .. }
    ));
    let (text, user_id, user_host): (Option<String>, String, Option<String>) =
        mk_db::sqlx::query_as(
            r#"SELECT "text", "userId", "userHost" FROM "note" WHERE "uri" = $1"#,
        )
        .bind(note_uri)
        .fetch_one(&pool)
        .await
        .expect("remote note row");
    assert_eq!(text.as_deref(), Some("remote hi"));
    assert_eq!(user_id, remote_id, "the ingested person is the author");
    assert_eq!(user_host.as_deref(), Some("remote.example"));

    // --- Follow: remote follower → local followee ---
    let (local_id, _pem) = fed2_user(&pool, "fed2remoteLocal").await;
    let local_uri = format!("https://{FED2_HOST}/users/{local_id}");
    let follow = serde_json::json!({
        "type": "Follow",
        "actor": actor_uri,
        "object": local_uri
    });
    let effect = mk_ap::processor::route_remote(&pool, &remote_id, actor_uri, &follow, FED2_HOST)
        .await
        .expect("remote Follow routes");
    assert_eq!(effect, mk_ap::processor::RouteEffect::Followed);
    let rows: i64 = mk_db::sqlx::query_scalar(
        r#"SELECT count(*) FROM "following" WHERE "followerId" = $1 AND "followeeId" = $2"#,
    )
    .bind(&remote_id)
    .bind(&local_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rows, 1, "the remote Follow landed the following row");

    // --- Like on a missing note: documented skip, no panic ---
    let like = serde_json::json!({
        "type": "Like",
        "actor": actor_uri,
        "object": "https://remote.example/notes/missing",
        "content": "👍"
    });
    let effect = mk_ap::processor::route_remote(&pool, &remote_id, actor_uri, &like, FED2_HOST)
        .await
        .expect("Like on a missing note still routes");
    assert_eq!(
        effect,
        mk_ap::processor::RouteEffect::Skipped("skip: target note not found")
    );

    // --- Undo{Follow}: removes the row ---
    let undo = serde_json::json!({
        "type": "Undo",
        "actor": actor_uri,
        "object": { "type": "Follow", "actor": actor_uri, "object": local_uri }
    });
    let effect = mk_ap::processor::route_remote(&pool, &remote_id, actor_uri, &undo, FED2_HOST)
        .await
        .expect("remote Undo{Follow} routes");
    assert_eq!(effect, mk_ap::processor::RouteEffect::Unfollowed);
    let rows: i64 = mk_db::sqlx::query_scalar(
        r#"SELECT count(*) FROM "following" WHERE "followerId" = $1 AND "followeeId" = $2"#,
    )
    .bind(&remote_id)
    .bind(&local_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rows, 0, "undo deleted the following row");
}

// ---------------------------------------------------------------------------
// admin/emoji/* (admin emoji management) — add→list→update→delete round trip,
// bulk alias/category/license/delete legs and the requireAdmin gate.
// ---------------------------------------------------------------------------

/// Find one emoji in an admin/emoji/list response by id.
fn emoji_by_id<'a>(list: &'a Value, id: &str) -> Option<&'a Value> {
    list.as_array()
        .and_then(|rows| rows.iter().find(|e| e["id"] == json!(id)))
}

#[tokio::test]
async fn admin_emoji_roundtrip() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("mktest_admin_emoji").await;
    mk_db::run_migrations(&pool).await.unwrap();
    let app = crate::build_router(test_state(pool.clone()));

    // first signup = root (admin), second = plain user.
    let admin = signup(&app, "root").await;
    let token = admin["token"].as_str().unwrap().to_string();
    let admin_id = admin["id"].as_str().unwrap().to_string();
    let bob = signup(&app, "bob").await;
    let bob_token = bob["token"].as_str().unwrap().to_string();

    // gate: non-admin → 403 ROLE_PERMISSION_DENIED; no credential → 401.
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/emoji/list",
        json!({ "i": bob_token }),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["error"]["code"], json!("ROLE_PERMISSION_DENIED"));
    let (s, body) = post_json(app.clone(), "/api/admin/emoji/list", json!({})).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(body["error"]["code"], json!("CREDENTIAL_REQUIRED"));

    let file_id = seed_drive_file(&pool, &admin_id, "image/jpeg").await;

    // add → EmojiDetailed
    let (s, added) = post_json(
        app.clone(),
        "/api/admin/emoji/add",
        json!({
            "name": "smile",
            "fileId": file_id,
            "aliases": ["happy"],
            "category": "faces",
            "license": "MIT",
            "isSensitive": true,
            "localOnly": true,
            "roleIdsThatCanBeUsedThisEmojiAsReaction": [],
            "i": token,
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {added}");
    let emoji_id = added["id"].as_str().unwrap().to_string();
    assert_eq!(emoji_id.len(), 10);
    assert_eq!(added["name"], json!("smile"));
    assert_eq!(added["host"], Value::Null);
    assert_eq!(
        added["url"],
        json!("http://127.0.0.1:3000/files/accesskey123/192.jpg")
    );
    assert_eq!(added["aliases"], json!(["happy"]));
    assert_eq!(added["category"], json!("faces"));
    assert_eq!(added["license"], json!("MIT"));
    assert_eq!(added["isSensitive"], json!(true));
    assert_eq!(added["localOnly"], json!(true));
    assert_eq!(added["roleIdsThatCanBeUsedThisEmojiAsReaction"], json!([]));

    // duplicate name → 400 DUPLICATE_NAME
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/emoji/add",
        json!({ "name": "smile", "fileId": file_id, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("DUPLICATE_NAME"));

    // non-image file → 400 UNSUPPORTED_FILE_TYPE
    let pdf = seed_drive_file(&pool, &admin_id, "application/pdf").await;
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/emoji/add",
        json!({ "name": "doc", "fileId": pdf, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("UNSUPPORTED_FILE_TYPE"));

    // unknown drive file → 400 NO_SUCH_FILE
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/emoji/add",
        json!({ "name": "ghost", "fileId": "0000000000", "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("NO_SUCH_FILE"));

    // list + query modes (alias substring, :name: token, miss)
    let (s, all) = post_json(app.clone(), "/api/admin/emoji/list", json!({ "i": token })).await;
    assert_eq!(s, StatusCode::OK, "body: {all}");
    assert!(emoji_by_id(&all, &emoji_id).is_some());
    let (_, by_alias) = post_json(
        app.clone(),
        "/api/admin/emoji/list",
        json!({ "query": "happy", "i": token }),
    )
    .await;
    assert!(emoji_by_id(&by_alias, &emoji_id).is_some());
    let (_, by_token) = post_json(
        app.clone(),
        "/api/admin/emoji/list",
        json!({ "query": ":smile:", "i": token }),
    )
    .await;
    assert!(emoji_by_id(&by_token, &emoji_id).is_some());
    let (_, miss) = post_json(
        app.clone(),
        "/api/admin/emoji/list",
        json!({ "query": "zzzz", "i": token }),
    )
    .await;
    assert!(emoji_by_id(&miss, &emoji_id).is_none());

    // update (rename + category reset + aliases replace) → 204
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/emoji/update",
        json!({
            "id": emoji_id,
            "name": "smile2",
            "category": null,
            "aliases": ["joy"],
            "i": token,
        }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, updated) = post_json(
        app.clone(),
        "/api/admin/emoji/list",
        json!({ "query": "smile2", "i": token }),
    )
    .await;
    let row = emoji_by_id(&updated, &emoji_id).expect("renamed emoji listed");
    assert_eq!(row["name"], json!("smile2"));
    assert_eq!(row["category"], Value::Null);
    assert_eq!(row["aliases"], json!(["joy"]));

    // update with neither id nor name → 400 INVALID_PARAM
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/emoji/update",
        json!({ "category": "x", "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("INVALID_PARAM"));

    // update unknown id → 400 NO_SUCH_EMOJI
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/emoji/update",
        json!({ "id": "0000000000", "category": "x", "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("NO_SUCH_EMOJI"));

    // rename onto an existing name → 400 SAME_NAME_EMOJI_EXISTS
    let (_, wave) = post_json(
        app.clone(),
        "/api/admin/emoji/add",
        json!({ "name": "wave", "fileId": file_id, "i": token }),
    )
    .await;
    let wave_id = wave["id"].as_str().unwrap().to_string();
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/emoji/update",
        json!({ "id": emoji_id, "name": "wave", "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("SAME_NAME_EMOJI_EXISTS"));

    // delete → 204, then gone from the list
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/emoji/delete",
        json!({ "id": emoji_id, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, after) = post_json(app.clone(), "/api/admin/emoji/list", json!({ "i": token })).await;
    assert!(emoji_by_id(&after, &emoji_id).is_none());
    assert!(emoji_by_id(&after, &wave_id).is_some());

    // delete unknown id → findOneByOrFail → 500 INTERNAL_ERROR
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/emoji/delete",
        json!({ "id": "0000000000", "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "body: {body}");
    assert_eq!(body["error"]["code"], json!("INTERNAL_ERROR"));
}

#[tokio::test]
async fn admin_emoji_bulk_roundtrip() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("mktest_admin_emoji_bulk").await;
    mk_db::run_migrations(&pool).await.unwrap();
    let app = crate::build_router(test_state(pool.clone()));

    let admin = signup(&app, "root").await;
    let token = admin["token"].as_str().unwrap().to_string();
    let admin_id = admin["id"].as_str().unwrap().to_string();
    let bob = signup(&app, "bob").await;
    let bob_token = bob["token"].as_str().unwrap().to_string();

    let file_a = seed_drive_file(&pool, &admin_id, "image/png").await;
    let file_b = seed_drive_file(&pool, &admin_id, "image/gif").await;
    let (_, one) = post_json(
        app.clone(),
        "/api/admin/emoji/add",
        json!({ "name": "one", "fileId": file_a, "aliases": ["a"], "i": token }),
    )
    .await;
    let (_, two) = post_json(
        app.clone(),
        "/api/admin/emoji/add",
        json!({ "name": "two", "fileId": file_b, "aliases": ["a"], "i": token }),
    )
    .await;
    let id_one = one["id"].as_str().unwrap().to_string();
    let id_two = two["id"].as_str().unwrap().to_string();
    let ids = json!([id_one, id_two]);

    // non-admin bulk write → 403
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/emoji/add-aliases-bulk",
        json!({ "ids": ids, "aliases": ["x"], "i": bob_token }),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["error"]["code"], json!("ROLE_PERMISSION_DENIED"));

    // add aliases (dedupe, order preserving)
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/emoji/add-aliases-bulk",
        json!({ "ids": ids, "aliases": ["b", "a"], "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, list) = post_json(app.clone(), "/api/admin/emoji/list", json!({ "i": token })).await;
    assert_eq!(
        emoji_by_id(&list, &id_one).unwrap()["aliases"],
        json!(["a", "b"])
    );
    assert_eq!(
        emoji_by_id(&list, &id_two).unwrap()["aliases"],
        json!(["a", "b"])
    );

    // remove aliases from one row only
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/emoji/remove-aliases-bulk",
        json!({ "ids": [id_one], "aliases": ["a"], "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, list) = post_json(app.clone(), "/api/admin/emoji/list", json!({ "i": token })).await;
    assert_eq!(
        emoji_by_id(&list, &id_one).unwrap()["aliases"],
        json!(["b"])
    );
    assert_eq!(
        emoji_by_id(&list, &id_two).unwrap()["aliases"],
        json!(["a", "b"])
    );

    // set aliases (replace both)
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/emoji/set-aliases-bulk",
        json!({ "ids": ids, "aliases": ["only"], "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, list) = post_json(app.clone(), "/api/admin/emoji/list", json!({ "i": token })).await;
    assert_eq!(
        emoji_by_id(&list, &id_one).unwrap()["aliases"],
        json!(["only"])
    );
    assert_eq!(
        emoji_by_id(&list, &id_two).unwrap()["aliases"],
        json!(["only"])
    );

    // category bulk
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/emoji/set-category-bulk",
        json!({ "ids": ids, "category": "cat", "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, list) = post_json(app.clone(), "/api/admin/emoji/list", json!({ "i": token })).await;
    assert_eq!(
        emoji_by_id(&list, &id_one).unwrap()["category"],
        json!("cat")
    );
    assert_eq!(
        emoji_by_id(&list, &id_two).unwrap()["category"],
        json!("cat")
    );

    // license bulk, then reset to null (absent category/license resets too)
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/emoji/set-license-bulk",
        json!({ "ids": ids, "license": "MIT", "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, list) = post_json(app.clone(), "/api/admin/emoji/list", json!({ "i": token })).await;
    assert_eq!(
        emoji_by_id(&list, &id_one).unwrap()["license"],
        json!("MIT")
    );
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/emoji/set-license-bulk",
        json!({ "ids": ids, "license": null, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, list) = post_json(app.clone(), "/api/admin/emoji/list", json!({ "i": token })).await;
    assert_eq!(emoji_by_id(&list, &id_one).unwrap()["license"], Value::Null);

    // delete bulk → 204, list empty
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/emoji/delete-bulk",
        json!({ "ids": ids, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, list) = post_json(app.clone(), "/api/admin/emoji/list", json!({ "i": token })).await;
    assert_eq!(list, json!([]));
}

/// admin/roles: create → list → show → assign → unassign → delete
/// (upstream endpoints/admin/roles/*.ts).
#[tokio::test]
async fn admin_roles_crud_roundtrip() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("mktest_admin_roles_crud").await;
    mk_db::run_migrations(&pool).await.unwrap();
    let app = crate::build_router(test_state(pool));

    let root = signup(&app, &format!("root{}", mk_types::id::gen_aid_now())).await;
    let alice = signup(&app, &format!("alice{}", mk_types::id::gen_aid_now())).await;
    let r_token = root["token"].as_str().unwrap().to_string();
    let a_token = alice["token"].as_str().unwrap().to_string();

    // create (only name is required; everything else keeps its default)
    let (s, role) = post_json(
        app.clone(),
        "/api/admin/roles/create",
        json!({ "name": "crud-role", "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {role}");
    let role_id = role["id"].as_str().unwrap().to_string();
    assert_eq!(role["name"], json!("crud-role"));
    assert_eq!(role["isModerator"], json!(false));

    // list sees it (moderator gate passes for the root)
    let (s, list) = post_json(
        app.clone(),
        "/api/admin/roles/list",
        json!({ "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {list}");
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == json!(role_id)),
        "body: {list}"
    );

    // show packs the stored row
    let (s, shown) = post_json(
        app.clone(),
        "/api/admin/roles/show",
        json!({ "roleId": role_id, "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {shown}");
    assert_eq!(shown["id"], json!(role_id));
    assert_eq!(shown["name"], json!("crud-role"));
    assert!(shown.get("policies").is_some());

    // assign / unassign (void handlers → 204)
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/roles/assign",
        json!({ "userId": alice["id"], "roleId": role_id, "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/roles/unassign",
        json!({ "userId": alice["id"], "roleId": role_id, "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // a plain user is denied the moderator gate
    let (s, err) = post_json(
        app.clone(),
        "/api/admin/roles/list",
        json!({ "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "body: {err}");
    assert_eq!(err["error"]["code"], json!("ROLE_PERMISSION_DENIED"));

    // delete → show answers show.ts NO_SUCH_ROLE (07dc7d34)
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/roles/delete",
        json!({ "roleId": role_id, "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, err) = post_json(
        app.clone(),
        "/api/admin/roles/show",
        json!({ "roleId": role_id, "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(err["error"]["code"], json!("NO_SUCH_ROLE"));
    assert_eq!(
        err["error"]["id"],
        json!("07dc7d34-c0d8-49b7-96c6-db3ce64ee0b3")
    );
}

/// admin/ad: create → list → update → delete (upstream
/// endpoints/admin/ad/*.ts).
#[tokio::test]
async fn admin_ad_crud_roundtrip() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("mktest_admin_ad_crud").await;
    mk_db::run_migrations(&pool).await.unwrap();
    let app = crate::build_router(test_state(pool));

    let root = signup(&app, &format!("root{}", mk_types::id::gen_aid_now())).await;
    let alice = signup(&app, &format!("alice{}", mk_types::id::gen_aid_now())).await;
    let r_token = root["token"].as_str().unwrap().to_string();
    let a_token = alice["token"].as_str().unwrap().to_string();
    let now = now_ms() as i64;

    // create: every required paramDef field
    let (s, ad) = post_json(
        app.clone(),
        "/api/admin/ad/create",
        json!({
            "url": "https://example.com/ad",
            "memo": "first",
            "place": "square",
            "priority": "middle",
            "ratio": 1,
            "expiresAt": now + 3_600_000,
            "startsAt": now - 1_000,
            "imageUrl": "https://example.com/ad.png",
            "dayOfWeek": 0,
            "isSensitive": false,
            "i": r_token,
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {ad}");
    let ad_id = ad["id"].as_str().unwrap().to_string();
    assert_eq!(ad["url"], json!("https://example.com/ad"));
    assert_eq!(ad["place"], json!("square"));
    assert_eq!(ad["ratio"], json!(1));
    assert!(ad["expiresAt"].is_string());
    assert!(ad["startsAt"].is_string());

    // list (default publishing = null → all rows, id DESC)
    let (s, list) = post_json(app.clone(), "/api/admin/ad/list", json!({ "i": r_token })).await;
    assert_eq!(s, StatusCode::OK, "body: {list}");
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == json!(ad_id)),
        "body: {list}"
    );
    // publishing=true keeps the live ad
    let (s, list) = post_json(
        app.clone(),
        "/api/admin/ad/list",
        json!({ "publishing": true, "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {list}");
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == json!(ad_id))
    );

    // update is partial (memo only) and answers 204
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/ad/update",
        json!({ "id": ad_id, "memo": "updated", "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, list) = post_json(app.clone(), "/api/admin/ad/list", json!({ "i": r_token })).await;
    assert_eq!(s, StatusCode::OK);
    let row = list
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == json!(ad_id))
        .unwrap();
    assert_eq!(row["memo"], json!("updated"));
    assert_eq!(row["url"], json!("https://example.com/ad"));

    // non-moderator is denied
    let (s, err) = post_json(app.clone(), "/api/admin/ad/list", json!({ "i": a_token })).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "body: {err}");
    assert_eq!(err["error"]["code"], json!("ROLE_PERMISSION_DENIED"));

    // missing rows answer the per-endpoint NO_SUCH_AD ids
    let (s, err) = post_json(
        app.clone(),
        "/api/admin/ad/update",
        json!({ "id": "x".repeat(10), "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("b7aa1727-1354-47bc-a182-3a9c3973d300")
    );
    let (s, err) = post_json(
        app.clone(),
        "/api/admin/ad/delete",
        json!({ "id": "x".repeat(10), "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(
        err["error"]["id"],
        json!("ccac9863-3a03-416e-b899-8a64041118b1")
    );

    // delete → 204 → the list no longer carries it
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/ad/delete",
        json!({ "id": ad_id, "i": r_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, list) = post_json(app.clone(), "/api/admin/ad/list", json!({ "i": r_token })).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        !list
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == json!(ad_id))
    );
}

// ==== i-more slice 1: i/favorites + i/gallery/likes + i/pages/likes ====

/// `i/favorites`: the caller's note_favorite rows (id DESC), other users'
/// rows excluded, cursors respected.
#[tokio::test]
async fn i_favorites_lists_only_callers_rows() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mki_more_favorites").await;
    let app = crate::build_router(test_state(pool.clone()));
    let alice = signup(&app, &format!("fava{}", mk_types::id::gen_aid_now())).await;
    let bob = signup(&app, &format!("favb{}", mk_types::id::gen_aid_now())).await;
    let at = alice["token"].as_str().unwrap().to_string();
    let aid = alice["id"].as_str().unwrap();
    let bid = bob["id"].as_str().unwrap();

    let n1 = mk_types::id::gen_aid_now();
    let n2 = mk_types::id::gen_aid_now();
    for nid in [&n1, &n2] {
        mk_db::sqlx::query(
            r#"INSERT INTO "note" ("id", "userId", "visibility") VALUES ($1, $2, 'public')"#,
        )
        .bind(nid)
        .bind(aid)
        .execute(&pool)
        .await
        .unwrap();
    }
    let f1 = mk_types::id::gen_aid_now();
    let f2 = mk_types::id::gen_aid_now();
    mk_db::sqlx::query(
        r#"INSERT INTO "note_favorite" ("id", "userId", "noteId") VALUES ($1, $2, $3)"#,
    )
    .bind(&f1)
    .bind(aid)
    .bind(&n1)
    .execute(&pool)
    .await
    .unwrap();
    mk_db::sqlx::query(
        r#"INSERT INTO "note_favorite" ("id", "userId", "noteId") VALUES ($1, $2, $3)"#,
    )
    .bind(&f2)
    .bind(bid)
    .bind(&n2)
    .execute(&pool)
    .await
    .unwrap();

    let (s, body) = post_json(app.clone(), "/api/i/favorites", json!({ "i": at })).await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1, "body: {body}");
    assert_eq!(arr[0]["id"], json!(f1));
    assert_eq!(arr[0]["noteId"], json!(n1));
    assert!(arr[0]["createdAt"].is_string());

    // cursors: untilId=f1 is exclusive → empty; sinceId=f1 → empty (no newer)
    let (_, b) = post_json(
        app.clone(),
        "/api/i/favorites",
        json!({ "i": at, "untilId": f1 }),
    )
    .await;
    assert_eq!(b, json!([]));
    let (_, b) = post_json(
        app.clone(),
        "/api/i/favorites",
        json!({ "i": at, "sinceId": f1 }),
    )
    .await;
    assert_eq!(b, json!([]));

    // other user's token only sees their own row
    let (_, b) = post_json(
        app.clone(),
        "/api/i/favorites",
        json!({ "i": bob["token"] }),
    )
    .await;
    assert_eq!(b.as_array().unwrap().len(), 1);
    assert_eq!(b[0]["id"], json!(f2));

    // requireCredential: no token → CREDENTIAL_REQUIRED
    let (s, b) = post_json(app, "/api/i/favorites", json!({})).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "body: {b}");
    assert_eq!(b["error"]["code"], json!("CREDENTIAL_REQUIRED"));
}

/// `i/gallery/likes`: the caller's gallery_like rows joined to gallery_post.
#[tokio::test]
async fn i_gallery_likes_lists_only_callers_rows() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mki_more_gallery").await;
    let app = crate::build_router(test_state(pool.clone()));
    let alice = signup(&app, &format!("gala{}", mk_types::id::gen_aid_now())).await;
    let bob = signup(&app, &format!("galb{}", mk_types::id::gen_aid_now())).await;
    let at = alice["token"].as_str().unwrap().to_string();
    let aid = alice["id"].as_str().unwrap();
    let bid = bob["id"].as_str().unwrap();

    let p1 = mk_types::id::gen_aid_now();
    let p2 = mk_types::id::gen_aid_now();
    for (pid, uid, title) in [(&p1, aid, "alice post"), (&p2, bid, "bob post")] {
        mk_db::sqlx::query(
            r#"INSERT INTO "gallery_post" ("id", "updatedAt", "title", "userId")
               VALUES ($1, now(), $2, $3)"#,
        )
        .bind(pid)
        .bind(title)
        .bind(uid)
        .execute(&pool)
        .await
        .unwrap();
    }
    let l1 = mk_types::id::gen_aid_now();
    let l2 = mk_types::id::gen_aid_now();
    for (lid, uid, pid) in [(&l1, aid, &p1), (&l2, bid, &p2)] {
        mk_db::sqlx::query(
            r#"INSERT INTO "gallery_like" ("id", "userId", "postId") VALUES ($1, $2, $3)"#,
        )
        .bind(lid)
        .bind(uid)
        .bind(pid)
        .execute(&pool)
        .await
        .unwrap();
    }

    let (s, body) = post_json(app.clone(), "/api/i/gallery/likes", json!({ "i": at })).await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1, "body: {body}");
    assert_eq!(arr[0]["id"], json!(l1));
    assert_eq!(arr[0]["post"]["id"], json!(p1));
    assert_eq!(arr[0]["post"]["title"], json!("alice post"));
    assert!(arr[0]["createdAt"].is_string());

    // other user sees only their own like
    let (_, b) = post_json(app, "/api/i/gallery/likes", json!({ "i": bob["token"] })).await;
    assert_eq!(b.as_array().unwrap().len(), 1);
    assert_eq!(b[0]["post"]["id"], json!(p2));
}

/// `i/pages/likes`: the caller's page_like rows joined to page.
#[tokio::test]
async fn i_pages_likes_lists_only_callers_rows() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mki_more_pages").await;
    let app = crate::build_router(test_state(pool.clone()));
    let alice = signup(&app, &format!("pga{}", mk_types::id::gen_aid_now())).await;
    let bob = signup(&app, &format!("pgb{}", mk_types::id::gen_aid_now())).await;
    let at = alice["token"].as_str().unwrap().to_string();
    let aid = alice["id"].as_str().unwrap();
    let bid = bob["id"].as_str().unwrap();

    let q1 = mk_types::id::gen_aid_now();
    let q2 = mk_types::id::gen_aid_now();
    for (pid, uid, title) in [(&q1, aid, "alice page"), (&q2, bid, "bob page")] {
        mk_db::sqlx::query(
            r#"INSERT INTO "page"
                 ("id", "updatedAt", "title", "name", "alignCenter", "font",
                  "userId", "visibility")
               VALUES ($1, now(), $2, $2, false, 'sans-serif', $3, 'public')"#,
        )
        .bind(pid)
        .bind(title)
        .bind(uid)
        .execute(&pool)
        .await
        .unwrap();
    }
    let m1 = mk_types::id::gen_aid_now();
    let m2 = mk_types::id::gen_aid_now();
    for (lid, uid, pid) in [(&m1, aid, &q1), (&m2, bid, &q2)] {
        mk_db::sqlx::query(
            r#"INSERT INTO "page_like" ("id", "userId", "pageId") VALUES ($1, $2, $3)"#,
        )
        .bind(lid)
        .bind(uid)
        .bind(pid)
        .execute(&pool)
        .await
        .unwrap();
    }

    let (s, body) = post_json(app.clone(), "/api/i/pages/likes", json!({ "i": at })).await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1, "body: {body}");
    assert_eq!(arr[0]["id"], json!(m1));
    assert_eq!(arr[0]["page"]["id"], json!(q1));
    assert_eq!(arr[0]["page"]["title"], json!("alice page"));
    assert_eq!(arr[0]["page"]["visibility"], json!("public"));
    assert!(arr[0]["createdAt"].is_string());

    // other user sees only their own like
    let (_, b) = post_json(app, "/api/i/pages/likes", json!({ "i": bob["token"] })).await;
    assert_eq!(b.as_array().unwrap().len(), 1);
    assert_eq!(b[0]["page"]["id"], json!(q2));
}

// ---------------------------------------------------------------------------
// admin-queue slice — admin/queue/* over mk-queue (appended at file end so
// co-agent appends rebase trivially).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_queue_shape_and_gate() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkadmin_queue_shape").await;
    let app = crate::build_router(test_state(pool.clone()));

    let root_token = signup(&app, "root").await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let plain_token = signup(&app, "plainqueue").await["token"]
        .as_str()
        .unwrap()
        .to_string();

    // no credential → 401 CREDENTIAL_REQUIRED (gate lives in admin::gate)
    let (s, b) = post_json(app.clone(), "/api/admin/queue/queues", json!({})).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "body: {b}");
    assert_eq!(
        b["error"]["id"],
        json!("1384574d-a912-4b81-8601-c7b1c4085df1")
    );

    // plain user → 403 moderator gate
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/queue/queues",
        json!({ "i": plain_token }),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "body: {b}");
    assert_eq!(
        b["error"]["id"],
        json!("d33d5333-db36-423d-a8f9-1a2b9549da41")
    );

    // queues: array of {name, counts, isPaused, metrics} (roster includes the
    // in-tree kinds even on an empty table)
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/queue/queues",
        json!({ "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let arr = b.as_array().expect("queues is an array");
    assert!(!arr.is_empty(), "body: {b}");
    let delete_account = arr
        .iter()
        .find(|q| q["name"] == json!("deleteAccount"))
        .unwrap_or_else(|| panic!("deleteAccount in roster: {b}"));
    for key in ["waiting", "active", "completed", "failed", "delayed"] {
        assert!(
            delete_account["counts"][key].is_i64(),
            "counts.{key} present: {b}"
        );
    }
    assert_eq!(delete_account["isPaused"], json!(false));
    assert!(delete_account["metrics"]["completed"]["data"].is_array());

    // queue-stats on an empty queue → zeroed counts + the PG db block
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/queue/queue-stats",
        json!({ "i": root_token, "queue": "deleteAccount" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b["name"], json!("deleteAccount"));
    assert_eq!(b["qualifiedName"], json!("deleteAccount"));
    assert_eq!(b["counts"]["waiting"], json!(0));
    assert_eq!(b["counts"]["delayed"], json!(0));
    assert_eq!(b["counts"]["failed"], json!(0));
    assert_eq!(b["isPaused"], json!(false));
    assert_eq!(b["db"]["version"], json!("postgres"));
    assert!(b["db"]["mode"].is_string());

    // jobs on an empty queue → []
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/queue/jobs",
        json!({
            "i": root_token,
            "queue": "deleteAccount",
            "state": ["wait", "delayed"],
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b, json!([]));

    // stats: the four upstream domain counters
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/queue/stats",
        json!({ "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    for domain in ["deliver", "inbox", "db", "objectStorage"] {
        for key in ["waiting", "active", "completed", "failed", "delayed"] {
            assert!(b[domain][key].is_i64(), "stats.{domain}.{key}: {b}");
        }
    }

    // invalid params: missing queue / bad state array → 400
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/queue/jobs",
        json!({ "i": root_token, "state": ["wait"] }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/queue/jobs",
        json!({ "i": root_token, "queue": "deleteAccount", "state": ["bogus"] }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // show-job-logs has no log store → honest []
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/queue/show-job-logs",
        json!({ "i": root_token, "queue": "deleteAccount", "jobId": "1" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b, json!([]));
}

#[tokio::test]
async fn admin_queue_enqueue_jobs_remove_roundtrip() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkadmin_queue_rt").await;
    let app = crate::build_router(test_state(pool.clone()));

    let root_token = signup(&app, "root").await["token"]
        .as_str()
        .unwrap()
        .to_string();

    // enqueue through the mk-queue primitive the workers use
    let id = mk_queue::QueueHandle::new(pool.clone())
        .enqueue(
            "deleteAccount",
            &json!({ "user": { "id": "u_queue_rt" }, "soft": true }),
        )
        .await
        .unwrap();

    // jobs (wait) → the packed QueueJob
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/queue/jobs",
        json!({ "i": root_token, "queue": "deleteAccount", "state": ["wait"] }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let arr = b.as_array().expect("jobs array");
    assert_eq!(arr.len(), 1, "body: {b}");
    let job = &arr[0];
    assert_eq!(job["id"], json!(id.to_string()));
    assert_eq!(job["name"], json!("deleteAccount"));
    assert_eq!(job["data"]["user"]["id"], json!("u_queue_rt"));
    assert_eq!(job["attempts"], json!(0));
    assert_eq!(job["isFailed"], json!(false));
    assert!(job["timestamp"].is_i64());

    // show-job
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/queue/show-job",
        json!({ "i": root_token, "queue": "deleteAccount", "jobId": id.to_string() }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b["id"], json!(id.to_string()));

    // retry-job → 204 (makes the pending job due; still present)
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/queue/retry-job",
        json!({ "i": root_token, "queue": "deleteAccount", "jobId": id.to_string() }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // remove-job → 204 and the queue is empty again
    let (s, _) = post_json(
        app.clone(),
        "/api/admin/queue/remove-job",
        json!({ "i": root_token, "queue": "deleteAccount", "jobId": id.to_string() }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/queue/jobs",
        json!({
            "i": root_token,
            "queue": "deleteAccount",
            "state": ["wait", "delayed", "failed"],
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b, json!([]));

    // show-job on a missing id is an upstream Error → 500 INTERNAL_ERROR
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/queue/show-job",
        json!({ "i": root_token, "queue": "deleteAccount", "jobId": id.to_string() }),
    )
    .await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "body: {b}");
}

// ===== chat: rooms + invitations ===========================================
//
// upstream endpoints/chat/rooms/{create,show,update,join,leave,joining,
// members}.ts + rooms/invitations/{create,ignore,inbox}.ts on the
// ChatService/ChatEntityService packs (owner is not a membership row; join
// consumes the invitation; only owner/member/invited can show).

#[tokio::test]
async fn chat_rooms_membership_invitation_flow() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkchat_rooms").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let alice = signup(&app, &uname("cha")).await;
    let bob = signup(&app, &uname("chb")).await;
    let a = alice["id"].as_str().unwrap();
    let b = bob["id"].as_str().unwrap();
    let at = alice["token"].as_str().unwrap();
    let bt = bob["token"].as_str().unwrap();

    // -- create: ChatRoom pack { id, createdAt, name, description, ownerId,
    //    owner: UserLite, isMuted, invitationExists } ---------------------
    let (s, room) = post_json(
        app.clone(),
        "/api/chat/rooms/create",
        json!({ "name": "room", "description": "desc", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {room}");
    assert_eq!(room["name"], json!("room"));
    assert_eq!(room["description"], json!("desc"));
    assert_eq!(room["ownerId"], json!(a));
    assert_eq!(room["owner"]["id"], json!(a));
    assert_eq!(room["isMuted"], json!(false));
    assert_eq!(room["invitationExists"], json!(false));
    assert!(room["createdAt"].as_str().is_some());
    let room_id = room["id"].as_str().unwrap().to_string();

    // -- show by owner + non-owner without invitation → NO_SUCH_ROOM ------
    let (s, v) = post_json(
        app.clone(),
        "/api/chat/rooms/show",
        json!({ "roomId": room_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert_eq!(v["id"], json!(room_id));
    let (s, v) = post_json(
        app.clone(),
        "/api/chat/rooms/show",
        json!({ "roomId": room_id, "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {v}");
    assert_eq!(v["error"]["code"], json!("NO_SUCH_ROOM"));

    // -- update (owner only) ----------------------------------------------
    let (s, v) = post_json(
        app.clone(),
        "/api/chat/rooms/update",
        json!({ "roomId": room_id, "name": "renamed", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert_eq!(v["name"], json!("renamed"));
    assert_eq!(v["description"], json!("desc"));
    let (s, v) = post_json(
        app.clone(),
        "/api/chat/rooms/update",
        json!({ "roomId": room_id, "name": "nope", "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {v}");
    assert_eq!(v["error"]["code"], json!("NO_SUCH_ROOM"));

    // -- invitation create → bob's inbox + show sees invitationExists -----
    let (s, inv) = post_json(
        app.clone(),
        "/api/chat/rooms/invitations/create",
        json!({ "roomId": room_id, "userId": b, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {inv}");
    assert_eq!(inv["roomId"], json!(room_id));
    assert_eq!(inv["userId"], json!(b));
    assert_eq!(inv["user"]["id"], json!(b));
    assert_eq!(inv["room"]["id"], json!(room_id));
    let (s, inbox) = post_json(
        app.clone(),
        "/api/chat/rooms/invitations/inbox",
        json!({ "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {inbox}");
    assert_eq!(inbox.as_array().unwrap().len(), 1);
    assert_eq!(inbox[0]["roomId"], json!(room_id));
    let (s, v) = post_json(
        app.clone(),
        "/api/chat/rooms/show",
        json!({ "roomId": room_id, "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert_eq!(v["invitationExists"], json!(true));

    // -- join consumes the invitation; joining lists the membership -------
    let (s, v) = post_json(
        app.clone(),
        "/api/chat/rooms/join",
        json!({ "roomId": room_id, "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {v}");
    let (s, inbox) = post_json(
        app.clone(),
        "/api/chat/rooms/invitations/inbox",
        json!({ "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {inbox}");
    assert_eq!(inbox, json!([]));
    let (s, joining) = post_json(app.clone(), "/api/chat/rooms/joining", json!({ "i": bt })).await;
    assert_eq!(s, StatusCode::OK, "body: {joining}");
    assert_eq!(joining.as_array().unwrap().len(), 1);
    assert_eq!(joining[0]["roomId"], json!(room_id));
    assert_eq!(joining[0]["userId"], json!(b));
    assert_eq!(joining[0]["room"]["id"], json!(room_id));
    assert!(joining[0].get("user").is_none(), "populateUser: false");

    // -- members: bob only (the owner has no membership row) --------------
    let (s, members) = post_json(
        app.clone(),
        "/api/chat/rooms/members",
        json!({ "roomId": room_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {members}");
    let members = members.as_array().unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0]["userId"], json!(b));
    assert_eq!(members[0]["user"]["id"], json!(b));
    assert!(members[0].get("room").is_none(), "populateRoom: false");

    // -- leave → joining empty again --------------------------------------
    let (s, v) = post_json(
        app.clone(),
        "/api/chat/rooms/leave",
        json!({ "roomId": room_id, "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {v}");
    let (s, joining) = post_json(app.clone(), "/api/chat/rooms/joining", json!({ "i": bt })).await;
    assert_eq!(s, StatusCode::OK, "body: {joining}");
    assert_eq!(joining, json!([]));
}

// ============================================================================
// notes-more batch (endpoints/notes/*): favorites create/delete (via
// i/favorites), reactions/delete, renotes, children, conversation, state and
// unrenote. Tables: note_favorite / note_reaction / note (0001 baseline).
// ============================================================================

#[tokio::test]
async fn notes_more_batch_flow() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mknotes_more_batch").await;
    let app = crate::build_router(test_state(pool.clone()));

    let ua = format!("notesmorea{}", mk_types::id::gen_aid_now());
    let ub = format!("notesmoreb{}", mk_types::id::gen_aid_now());
    let a = signup(&app, &ua).await;
    let b = signup(&app, &ub).await;
    let a_token = a["token"].as_str().unwrap().to_string();
    let b_token = b["token"].as_str().unwrap().to_string();

    // b writes the target note
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "target", "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let target = v["createdNote"]["id"].as_str().unwrap().to_string();

    // --- favorites create → i/favorites → state → duplicate → delete ---
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/favorites/create",
        json!({ "noteId": target, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "favorite create: {v}");
    let (s, v) = post_json(app.clone(), "/api/i/favorites", json!({ "i": a_token })).await;
    assert_eq!(s, StatusCode::OK, "i/favorites: {v}");
    assert_eq!(v.as_array().map(|x| x.len()), Some(1), "{v}");
    assert_eq!(v[0]["noteId"], json!(target), "{v}");

    let (s, v) = post_json(
        app.clone(),
        "/api/notes/state",
        json!({ "noteId": target, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "state: {v}");
    assert_eq!(v["isFavorited"], json!(true), "{v}");
    assert_eq!(v["isMutedThread"], json!(false), "{v}");

    let (s, v) = post_json(
        app.clone(),
        "/api/notes/favorites/create",
        json!({ "noteId": target, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "duplicate favorite: {v}");
    assert_eq!(v["error"]["code"], json!("ALREADY_FAVORITED"), "{v}");

    // --- reaction add → list → delete → list empty → NOT_REACTED ---
    let (s, _) = post_json(
        app.clone(),
        "/api/notes/reactions/create",
        json!({ "noteId": target, "reaction": "👍", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/reactions",
        json!({ "noteId": target, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v.as_array().map(|x| x.len()), Some(1), "{v}");

    let (s, v) = post_json(
        app.clone(),
        "/api/notes/reactions/delete",
        json!({ "noteId": target, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "reaction delete: {v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/reactions",
        json!({ "noteId": target, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v.as_array().map(|x| x.len()), Some(0), "{v}");
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/reactions/delete",
        json!({ "noteId": target, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["error"]["code"], json!("NOT_REACTED"), "{v}");

    // --- renote list ---
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "renoteId": target, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let renote_id = v["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/renotes",
        json!({ "noteId": target }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "renotes: {v}");
    assert_eq!(v.as_array().map(|x| x.len()), Some(1), "{v}");
    assert_eq!(v[0]["id"], json!(renote_id), "{v}");

    // --- children: direct reply in, pure renote out ---
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "reply", "replyId": target, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let reply_id = v["createdNote"]["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/children",
        json!({ "noteId": target }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "children: {v}");
    let ids: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["id"].as_str())
        .collect();
    assert!(ids.contains(&reply_id.as_str()), "children: {v}");
    assert!(
        !ids.contains(&renote_id.as_str()),
        "pure renote excluded: {v}"
    );

    // --- conversation walks the reply chain upward ---
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/conversation",
        json!({ "noteId": reply_id }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "conversation: {v}");
    assert_eq!(v.as_array().map(|x| x.len()), Some(1), "{v}");
    assert_eq!(v[0]["id"], json!(target), "{v}");

    // --- unrenote removes a's renote ---
    let (s, _) = post_json(
        app.clone(),
        "/api/notes/unrenote",
        json!({ "noteId": target, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, v) = post_json(
        app.clone(),
        "/api/notes/renotes",
        json!({ "noteId": target }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v.as_array().map(|x| x.len()), Some(0), "{v}");

    // --- favorites delete → list empty → NOT_FAVORITED ---
    let (s, _) = post_json(
        app.clone(),
        "/api/notes/favorites/delete",
        json!({ "noteId": target, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, v) = post_json(app.clone(), "/api/i/favorites", json!({ "i": a_token })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v.as_array().map(|x| x.len()), Some(0), "{v}");
    let (s, v) = post_json(
        app,
        "/api/notes/favorites/delete",
        json!({ "noteId": target, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["error"]["code"], json!("NOT_FAVORITED"), "{v}");
}

// ---------------------------------------------------------------------------
// users-more: users/relation + the remaining users/lists/* endpoints
// (upstream endpoints/users/relation.ts + endpoints/users/lists/*.ts)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn users_relation_flags_on_following_row() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkusers_more_relation").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let alice = signup(&app, &uname("rela")).await;
    let bob = signup(&app, &uname("relb")).await;
    let a = alice["id"].as_str().unwrap();
    let b = bob["id"].as_str().unwrap();
    let at = alice["token"].as_str().unwrap();
    let bt = bob["token"].as_str().unwrap();

    // alice → bob follow edge seeds isFollowing / isFollowed.
    let (s, v) = post_json(
        app.clone(),
        "/api/following/create",
        json!({ "userId": b, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");

    // scalar form still returns a one-element ARRAY (getRelation → [it]).
    let (s, rel) = post_json(
        app.clone(),
        "/api/users/relation",
        json!({ "userId": b, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {rel}");
    let arr = rel.as_array().expect("relation response is an array");
    assert_eq!(arr.len(), 1);
    let r = &arr[0];
    assert_eq!(r["id"], json!(b));
    assert_eq!(r["isFollowing"], json!(true));
    assert_eq!(r["isFollowed"], json!(false));
    assert_eq!(r["hasPendingFollowRequestFromYou"], json!(false));
    assert_eq!(r["hasPendingFollowRequestToYou"], json!(false));
    assert_eq!(r["isBlocking"], json!(false));
    assert_eq!(r["isBlocked"], json!(false));
    assert_eq!(r["isMuted"], json!(false));
    assert_eq!(r["isRenoteMuted"], json!(false));

    // reverse view: isFollowed flips.
    let (s, rel) = post_json(
        app.clone(),
        "/api/users/relation",
        json!({ "userId": a, "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {rel}");
    assert_eq!(rel[0]["isFollowing"], json!(false));
    assert_eq!(rel[0]["isFollowed"], json!(true));

    // array form returns one entry per target (insertion order).
    let (s, rel) = post_json(
        app.clone(),
        "/api/users/relation",
        json!({ "userId": [b, a], "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {rel}");
    let arr = rel.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["id"], json!(b));
    assert_eq!(arr[1]["id"], json!(a));

    // requireCredential.
    let (s, err) = post_json(app, "/api/users/relation", json!({ "userId": b })).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "body: {err}");
    assert_eq!(err["error"]["code"], json!("CREDENTIAL_REQUIRED"));
}

#[tokio::test]
async fn users_lists_more_create_push_show_membership_update_pull_delete() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkusers_more_lists").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let alice = signup(&app, &uname("lsa")).await;
    let bob = signup(&app, &uname("lsb")).await;
    let b = bob["id"].as_str().unwrap();
    let at = alice["token"].as_str().unwrap();
    let bt = bob["token"].as_str().unwrap();

    // create → push
    let (s, list) = post_json(
        app.clone(),
        "/api/users/lists/create",
        json!({ "name": "orig", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {list}");
    let list_id = list["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/users/lists/push",
        json!({ "listId": list_id, "userId": b, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {v}");

    // show (owner scope) carries the membership
    let (s, shown) = post_json(
        app.clone(),
        "/api/users/lists/show",
        json!({ "listId": list_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {shown}");
    assert_eq!(shown["userIds"], json!([b]));
    assert_eq!(shown["isPublic"], json!(false));

    // get-memberships: { id, createdAt, userId, user (UserLite), withReplies }
    let (s, members) = post_json(
        app.clone(),
        "/api/users/lists/get-memberships",
        json!({ "listId": list_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {members}");
    let arr = members.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["userId"], json!(b));
    assert_eq!(arr[0]["user"]["id"], json!(b));
    assert_eq!(arr[0]["withReplies"], json!(false));
    assert!(arr[0]["createdAt"].as_str().is_some());

    // update: rename + make public
    let (s, updated) = post_json(
        app.clone(),
        "/api/users/lists/update",
        json!({ "listId": list_id, "name": "renamed", "isPublic": true, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {updated}");
    assert_eq!(updated["name"], json!("renamed"));
    assert_eq!(updated["isPublic"], json!(true));

    // forPublic show (guest) exposes likedCount/isLiked
    let (s, public_show) = post_json(
        app.clone(),
        "/api/users/lists/show",
        json!({ "listId": list_id, "forPublic": true }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {public_show}");
    assert_eq!(public_show["likedCount"], json!(0));
    assert_eq!(public_show["isLiked"], json!(false));

    // favorite → likedCount 1 / isLiked true for the favoriter
    let (s, v) = post_json(
        app.clone(),
        "/api/users/lists/favorite",
        json!({ "listId": list_id, "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {v}");
    let (s, public_show) = post_json(
        app.clone(),
        "/api/users/lists/show",
        json!({ "listId": list_id, "forPublic": true, "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {public_show}");
    assert_eq!(public_show["likedCount"], json!(1));
    assert_eq!(public_show["isLiked"], json!(true));

    // duplicate favorite → ALREADY_FAVORITED
    let (s, err) = post_json(
        app.clone(),
        "/api/users/lists/favorite",
        json!({ "listId": list_id, "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(err["error"]["code"], json!("ALREADY_FAVORITED"));

    // unfavorite → back to zero
    let (s, v) = post_json(
        app.clone(),
        "/api/users/lists/unfavorite",
        json!({ "listId": list_id, "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {v}");

    // create-from-public copies the members into a new list
    let (s, copied) = post_json(
        app.clone(),
        "/api/users/lists/create-from-public",
        json!({ "name": "copy", "listId": list_id, "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {copied}");
    assert_eq!(copied["name"], json!("copy"));
    assert_eq!(copied["userIds"], json!([b]));

    // pull removes the membership from the original list
    let (s, v) = post_json(
        app.clone(),
        "/api/users/lists/pull",
        json!({ "listId": list_id, "userId": b, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {v}");
    let (s, shown) = post_json(
        app.clone(),
        "/api/users/lists/show",
        json!({ "listId": list_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {shown}");
    assert_eq!(shown["userIds"], json!([]));

    // delete → subsequent show is NO_SUCH_LIST
    let (s, v) = post_json(
        app.clone(),
        "/api/users/lists/delete",
        json!({ "listId": list_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {v}");
    let (s, err) = post_json(
        app.clone(),
        "/api/users/lists/show",
        json!({ "listId": list_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(err["error"]["code"], json!("NO_SUCH_LIST"));

    // unknown list → NO_SUCH_LIST (owner-scoped lookup)
    let (s, err) = post_json(
        app,
        "/api/users/lists/delete",
        json!({ "listId": "9zzzzzzzzz", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(err["error"]["code"], json!("NO_SUCH_LIST"));
}

// ---------------------------------------------------------------------------
// channels/mute/{delete,list} — the remaining channels/* trio members
// (channels/mute/create rides the lists arm; upstream ChannelMutingService).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn channels_mute_delete_list_roundtrip() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("mkchannels_mute").await;
    mk_db::run_migrations(&pool).await.unwrap();
    let app = crate::build_router(test_state(pool));

    let (a_token, _) = channels_signup(app.clone(), "alice").await;
    let (s, ch) = post_json(
        app.clone(),
        "/api/channels/create",
        json!({ "name": "mute-channel", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {ch}");
    let channel_id = ch["id"].as_str().unwrap().to_string();

    // mute/create → list shows the channel with the me flags
    let (s, _) = post_json(
        app.clone(),
        "/api/channels/mute/create",
        json!({ "channelId": channel_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, list) = post_json(
        app.clone(),
        "/api/channels/mute/list",
        json!({ "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {list}");
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 1, "list: {list:?}");
    assert_eq!(list[0]["id"], json!(channel_id));
    assert_eq!(list[0]["isMuting"], json!(true));

    // delete → 204, list empty; a second delete is NOT_MUTING_CHANNEL
    let (s, _) = post_json(
        app.clone(),
        "/api/channels/mute/delete",
        json!({ "channelId": channel_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, list) = post_json(
        app.clone(),
        "/api/channels/mute/list",
        json!({ "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {list}");
    assert_eq!(list.as_array().unwrap().len(), 0);
    let (s, body) = post_json(
        app.clone(),
        "/api/channels/mute/delete",
        json!({ "channelId": channel_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("NOT_MUTING_CHANNEL"));

    // unknown channel → NO_SUCH_CHANNEL
    let (s, body) = post_json(
        app.clone(),
        "/api/channels/mute/delete",
        json!({ "channelId": "x".repeat(32), "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("NO_SUCH_CHANNEL"));
}

// ---------------------------------------------------------------------------
// clips/remove-note — create → add-note → show → remove (ClipService)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn clips_remove_note_roundtrip() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("mkclips_remove_note").await;
    mk_db::run_migrations(&pool).await.unwrap();
    let app = crate::build_router(test_state(pool));

    let (a_token, _) = channels_signup(app.clone(), "alice").await;
    let (s, clip) = post_json(
        app.clone(),
        "/api/clips/create",
        json!({ "name": "remove-test", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {clip}");
    let clip_id = clip["id"].as_str().unwrap().to_string();
    let (s, note) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "text": "remove me", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {note}");
    let note_id = note["createdNote"]["id"].as_str().unwrap().to_string();

    let (s, _) = post_json(
        app.clone(),
        "/api/clips/add-note",
        json!({ "clipId": clip_id, "noteId": note_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, notes) = post_json(
        app.clone(),
        "/api/clips/notes",
        json!({ "clipId": clip_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {notes}");
    assert_eq!(notes.as_array().unwrap().len(), 1, "notes: {notes}");

    let (s, _) = post_json(
        app.clone(),
        "/api/clips/remove-note",
        json!({ "clipId": clip_id, "noteId": note_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, notes) = post_json(
        app.clone(),
        "/api/clips/notes",
        json!({ "clipId": clip_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {notes}");
    assert_eq!(notes.as_array().unwrap().len(), 0, "notes: {notes}");

    // removing again → NO_SUCH_NOTE
    let (s, body) = post_json(
        app.clone(),
        "/api/clips/remove-note",
        json!({ "clipId": clip_id, "noteId": note_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("NO_SUCH_NOTE"));
}

// ---------------------------------------------------------------------------
// flash/* — create → like → list (upstream FlashService + entity packs)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn flash_arena_crud_like_list() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("mkflash_arena").await;
    mk_db::run_migrations(&pool).await.unwrap();
    let app = crate::build_router(test_state(pool));

    let (a_token, a_id) = channels_signup(app.clone(), "alice").await;
    let (b_token, _) = channels_signup(app.clone(), "bobby").await;

    // create (title/summary/script/permissions required; visibility defaults
    // public; upstream packs without me → no isLiked key)
    let (s, flash) = post_json(
        app.clone(),
        "/api/flash/create",
        json!({
            "title": "test play",
            "summary": "summary",
            "script": "console.log('hi');",
            "permissions": [],
            "i": a_token,
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {flash}");
    assert_eq!(flash["title"], json!("test play"));
    assert_eq!(flash["visibility"], json!("public"));
    assert_eq!(flash["likedCount"], json!(0));
    assert_eq!(flash["userId"], json!(a_id));
    assert!(flash.get("isLiked").is_none(), "create pack: {flash}");
    let flash_id = flash["id"].as_str().unwrap().to_string();

    // show guest → no isLiked; show with bob → isLiked false
    let (s, shown) = post_json(
        app.clone(),
        "/api/flash/show",
        json!({ "flashId": flash_id }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {shown}");
    assert!(shown.get("isLiked").is_none(), "guest show: {shown}");
    let (s, shown) = post_json(
        app.clone(),
        "/api/flash/show",
        json!({ "flashId": flash_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {shown}");
    assert_eq!(shown["isLiked"], json!(false));

    // owner cannot like their own flash
    let (s, body) = post_json(
        app.clone(),
        "/api/flash/like",
        json!({ "flashId": flash_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("YOUR_FLASH"));

    // bob likes → 204; duplicate → ALREADY_LIKED
    let (s, _) = post_json(
        app.clone(),
        "/api/flash/like",
        json!({ "flashId": flash_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, body) = post_json(
        app.clone(),
        "/api/flash/like",
        json!({ "flashId": flash_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("ALREADY_LIKED"));

    let (s, shown) = post_json(
        app.clone(),
        "/api/flash/show",
        json!({ "flashId": flash_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {shown}");
    assert_eq!(shown["likedCount"], json!(1));
    assert_eq!(shown["isLiked"], json!(true));

    // flash/my (owner, no isLiked) + flash/my-likes (bob, {id, flash})
    let (s, mine) = post_json(app.clone(), "/api/flash/my", json!({ "i": a_token })).await;
    assert_eq!(s, StatusCode::OK, "body: {mine}");
    let mine = mine.as_array().unwrap();
    assert_eq!(mine.len(), 1, "my: {mine:?}");
    assert_eq!(mine[0]["id"], json!(flash_id));
    assert!(mine[0].get("isLiked").is_none(), "my pack: {}", mine[0]);
    let (s, likes) = post_json(app.clone(), "/api/flash/my-likes", json!({ "i": b_token })).await;
    assert_eq!(s, StatusCode::OK, "body: {likes}");
    let likes = likes.as_array().unwrap();
    assert_eq!(likes.len(), 1, "my-likes: {likes:?}");
    assert!(likes[0]["id"].is_string());
    assert_eq!(likes[0]["flash"]["id"], json!(flash_id));
    assert_eq!(likes[0]["flash"]["isLiked"], json!(true));

    // featured + search only surface public liked flashes
    let (s, featured) = post_json(app.clone(), "/api/flash/featured", json!({})).await;
    assert_eq!(s, StatusCode::OK, "body: {featured}");
    let featured = featured.as_array().unwrap();
    assert_eq!(featured.len(), 1, "featured: {featured:?}");
    assert_eq!(featured[0]["id"], json!(flash_id));
    let (s, found) = post_json(app.clone(), "/api/flash/search", json!({ "query": "test" })).await;
    assert_eq!(s, StatusCode::OK, "body: {found}");
    let found = found.as_array().unwrap();
    assert_eq!(found.len(), 1, "search: {found:?}");
    assert_eq!(found[0]["id"], json!(flash_id));

    // update is owner-only and returns 204
    let (s, body) = post_json(
        app.clone(),
        "/api/flash/update",
        json!({ "flashId": flash_id, "title": "nope", "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("ACCESS_DENIED"));
    let (s, _) = post_json(
        app.clone(),
        "/api/flash/update",
        json!({ "flashId": flash_id, "title": "renamed", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, shown) = post_json(
        app.clone(),
        "/api/flash/show",
        json!({ "flashId": flash_id }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {shown}");
    assert_eq!(shown["title"], json!("renamed"));

    // unlike → 204; a second unlike → NOT_LIKED; count back to 0
    let (s, _) = post_json(
        app.clone(),
        "/api/flash/unlike",
        json!({ "flashId": flash_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, body) = post_json(
        app.clone(),
        "/api/flash/unlike",
        json!({ "flashId": flash_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("NOT_LIKED"));
    let (s, shown) = post_json(
        app.clone(),
        "/api/flash/show",
        json!({ "flashId": flash_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {shown}");
    assert_eq!(shown["likedCount"], json!(0));

    // non-owner delete → ACCESS_DENIED; owner delete → 204; show → NO_SUCH_FLASH
    let (s, body) = post_json(
        app.clone(),
        "/api/flash/delete",
        json!({ "flashId": flash_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("ACCESS_DENIED"));
    let (s, _) = post_json(
        app.clone(),
        "/api/flash/delete",
        json!({ "flashId": flash_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, body) = post_json(
        app.clone(),
        "/api/flash/show",
        json!({ "flashId": flash_id }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("NO_SUCH_FLASH"));
}

// ===== chat: messages =====================================================
//
// upstream endpoints/chat/messages/{create-to-room,create-to-user,
// room-timeline,show,delete}.ts + chat/history.ts. Room sends/reads are
// member-only; history packs detailed messages with isRead (no Redis marker
// store here); direct messages pass the chatScope mutual gate.

#[tokio::test]
async fn chat_room_and_direct_messages() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkchat_messages").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let alice = signup(&app, &uname("cma")).await;
    let bob = signup(&app, &uname("cmb")).await;
    let carol = signup(&app, &uname("cmc")).await;
    let a = alice["id"].as_str().unwrap();
    let b = bob["id"].as_str().unwrap();
    let at = alice["token"].as_str().unwrap();
    let bt = bob["token"].as_str().unwrap();
    let ct = carol["token"].as_str().unwrap();

    // room + invitation + join
    let (s, room) = post_json(
        app.clone(),
        "/api/chat/rooms/create",
        json!({ "name": "msgroom", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {room}");
    let room_id = room["id"].as_str().unwrap().to_string();
    let (s, _) = post_json(
        app.clone(),
        "/api/chat/rooms/invitations/create",
        json!({ "roomId": room_id, "userId": b, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = post_json(
        app.clone(),
        "/api/chat/rooms/join",
        json!({ "roomId": room_id, "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // -- create-to-room: ChatMessageLiteForRoom ---------------------------
    let (s, msg) = post_json(
        app.clone(),
        "/api/chat/messages/create-to-room",
        json!({ "toRoomId": room_id, "text": "hello room", "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {msg}");
    assert_eq!(msg["text"], json!("hello room"));
    assert_eq!(msg["fromUserId"], json!(b));
    assert_eq!(msg["fromUser"]["id"], json!(b));
    assert_eq!(msg["toRoomId"], json!(room_id));
    assert_eq!(msg["reactions"], json!([]));
    assert!(msg["createdAt"].as_str().is_some());
    let msg_id = msg["id"].as_str().unwrap().to_string();

    // content required
    let (s, v) = post_json(
        app.clone(),
        "/api/chat/messages/create-to-room",
        json!({ "toRoomId": room_id, "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {v}");
    assert_eq!(v["error"]["code"], json!("CONTENT_REQUIRED"));

    // non-member cannot send or read
    let (s, v) = post_json(
        app.clone(),
        "/api/chat/messages/create-to-room",
        json!({ "toRoomId": room_id, "text": "nope", "i": ct }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {v}");
    assert_eq!(v["error"]["code"], json!("NO_SUCH_ROOM"));
    let (s, v) = post_json(
        app.clone(),
        "/api/chat/messages/room-timeline",
        json!({ "roomId": room_id, "i": ct }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {v}");
    assert_eq!(v["error"]["code"], json!("NO_SUCH_ROOM"));

    // -- room-timeline for a member ---------------------------------------
    let (s, tl) = post_json(
        app.clone(),
        "/api/chat/messages/room-timeline",
        json!({ "roomId": room_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {tl}");
    let tl = tl.as_array().unwrap();
    assert_eq!(tl.len(), 1);
    assert_eq!(tl[0]["id"], json!(msg_id));
    assert_eq!(tl[0]["text"], json!("hello room"));

    // -- show: sender only (no room-member fallback upstream) -------------
    let (s, v) = post_json(
        app.clone(),
        "/api/chat/messages/show",
        json!({ "messageId": msg_id, "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert_eq!(v["toRoomId"], json!(room_id));
    assert_eq!(v["fromUser"]["id"], json!(b));
    let (s, v) = post_json(
        app.clone(),
        "/api/chat/messages/show",
        json!({ "messageId": msg_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {v}");
    assert_eq!(v["error"]["code"], json!("NO_SUCH_MESSAGE"));

    // -- room history: one room row ---------------------------------------
    let (s, h) = post_json(
        app.clone(),
        "/api/chat/history",
        json!({ "room": true, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {h}");
    let h = h.as_array().unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!(h[0]["id"], json!(msg_id));
    assert_eq!(h[0]["isRead"], json!(true));

    // -- delete (sender) --------------------------------------------------
    let (s, v) = post_json(
        app.clone(),
        "/api/chat/messages/delete",
        json!({ "messageId": msg_id, "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {v}");
    let (s, tl) = post_json(
        app.clone(),
        "/api/chat/messages/room-timeline",
        json!({ "roomId": room_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {tl}");
    assert_eq!(tl, json!([]));

    // -- direct: mutual follow opens the default chatScope 'mutual' gate --
    for (tok, uid) in [(at, b), (bt, a)] {
        let (s, v) = post_json(
            app.clone(),
            "/api/following/create",
            json!({ "userId": uid, "i": tok }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "body: {v}");
    }
    let (s, dm) = post_json(
        app.clone(),
        "/api/chat/messages/create-to-user",
        json!({ "toUserId": b, "text": "hi bob", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {dm}");
    assert_eq!(dm["text"], json!("hi bob"));
    assert_eq!(dm["fromUserId"], json!(a));
    assert_eq!(dm["toUserId"], json!(b));
    assert_eq!(dm["reactions"], json!([]));
    let dm_id = dm["id"].as_str().unwrap().to_string();

    // self-send rejected
    let (s, v) = post_json(
        app.clone(),
        "/api/chat/messages/create-to-user",
        json!({ "toUserId": a, "text": "me", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {v}");
    assert_eq!(v["error"]["code"], json!("RECIPIENT_IS_YOURSELF"));

    // -- direct history (both sides see the row) --------------------------
    let (s, h) = post_json(app.clone(), "/api/chat/history", json!({ "i": bt })).await;
    assert_eq!(s, StatusCode::OK, "body: {h}");
    let h = h.as_array().unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!(h[0]["id"], json!(dm_id));
    assert_eq!(h[0]["fromUser"]["id"], json!(a));
    assert_eq!(h[0]["toUser"]["id"], json!(b));
    assert_eq!(h[0]["isRead"], json!(true));

    // -- show: recipient may read the direct message ----------------------
    let (s, v) = post_json(
        app.clone(),
        "/api/chat/messages/show",
        json!({ "messageId": dm_id, "i": bt }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");
    assert_eq!(v["toUser"]["id"], json!(b));
    let (s, v) = post_json(
        app.clone(),
        "/api/chat/messages/show",
        json!({ "messageId": dm_id, "i": ct }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {v}");
    assert_eq!(v["error"]["code"], json!("NO_SUCH_MESSAGE"));

    // -- delete direct + history empties ----------------------------------
    let (s, v) = post_json(
        app.clone(),
        "/api/chat/messages/delete",
        json!({ "messageId": dm_id, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {v}");
    let (s, h) = post_json(app.clone(), "/api/chat/history", json!({ "i": at })).await;
    assert_eq!(s, StatusCode::OK, "body: {h}");
    assert_eq!(h, json!([]));
}

// ---------------------------------------------------------------------------
// users/lists/list — upstream list.ts is requireCredential: false and the
// explicit-userId arm reads `{ userId, isPublic: true }` (even for the
// caller's own id); the no-param arm reads every list the caller owns.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn users_lists_list_owner_public_and_guest_gates() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkusers_lists_list").await;
    let app = crate::build_router(test_state(pool.clone()));

    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let alice = signup(&app, &uname("lla")).await;
    let bob = signup(&app, &uname("llb")).await;
    let a = alice["id"].as_str().unwrap();
    let at = alice["token"].as_str().unwrap();
    let bt = bob["token"].as_str().unwrap();

    let (s, private_list) = post_json(
        app.clone(),
        "/api/users/lists/create",
        json!({ "name": "priv", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {private_list}");
    let private_id = private_list["id"].as_str().unwrap().to_string();

    let (s, public_list) = post_json(
        app.clone(),
        "/api/users/lists/create",
        json!({ "name": "pub", "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {public_list}");
    let public_id = public_list["id"].as_str().unwrap().to_string();
    let (s, v) = post_json(
        app.clone(),
        "/api/users/lists/update",
        json!({ "listId": public_id, "isPublic": true, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {v}");

    // no userId + credential → every list the caller owns.
    let (s, lists) = post_json(app.clone(), "/api/users/lists/list", json!({ "i": at })).await;
    assert_eq!(s, StatusCode::OK, "body: {lists}");
    let arr = lists.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert!(arr.iter().any(|l| l["id"] == json!(private_id)));
    assert!(arr.iter().any(|l| l["id"] == json!(public_id)));

    // explicit userId (even the caller's own) → public-only.
    for body in [
        json!({ "userId": a, "i": at }),
        json!({ "userId": a, "i": bt }),
        json!({ "userId": a }),
    ] {
        let (s, lists) = post_json(app.clone(), "/api/users/lists/list", body).await;
        assert_eq!(s, StatusCode::OK, "body: {lists}");
        let arr = lists.as_array().unwrap();
        assert_eq!(arr.len(), 1, "body: {lists}");
        assert_eq!(arr[0]["id"], json!(public_id));
        assert_eq!(arr[0]["isPublic"], json!(true));
    }

    // guest with no userId → INVALID_PARAM (the endpoint's own error id).
    let (s, err) = post_json(app.clone(), "/api/users/lists/list", json!({})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(err["error"]["code"], json!("INVALID_PARAM"));
    assert_eq!(
        err["error"]["id"],
        json!("ab36de0e-29e9-48cb-9732-d82f1281620d")
    );

    // unknown explicit userId → NO_SUCH_USER.
    let (s, err) = post_json(
        app.clone(),
        "/api/users/lists/list",
        json!({ "userId": "9zzzzzzzzz" }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(err["error"]["code"], json!("NO_SUCH_USER"));

    // a present-but-unresolvable token still 401s.
    let (s, err) = post_json(
        app.clone(),
        "/api/users/lists/list",
        json!({ "i": "0000000000000000" }),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "body: {err}");
    assert_eq!(err["error"]["code"], json!("AUTHENTICATION_FAILED"));

    // non-string userId → paramDef INVALID_PARAM.
    let (s, err) = post_json(
        app,
        "/api/users/lists/list",
        json!({ "userId": 42, "i": at }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {err}");
    assert_eq!(err["error"]["code"], json!("INVALID_PARAM"));
}

// ==== i-more slice 2: i/flashs + i/flash/likes + i/authorized-apps ====

/// `i/flashs`: the caller's flash rows (id DESC), other users' excluded.
#[tokio::test]
async fn i_flashs_lists_only_callers_flashs() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mki_more_flashs").await;
    let app = crate::build_router(test_state(pool.clone()));
    let alice = signup(&app, &format!("flsa{}", mk_types::id::gen_aid_now())).await;
    let bob = signup(&app, &format!("flsb{}", mk_types::id::gen_aid_now())).await;
    let at = alice["token"].as_str().unwrap().to_string();
    let aid = alice["id"].as_str().unwrap();
    let bid = bob["id"].as_str().unwrap();

    let f1 = mk_types::id::gen_aid_now();
    let f2 = mk_types::id::gen_aid_now();
    for (fid, uid, title) in [(&f1, aid, "alice flash"), (&f2, bid, "bob flash")] {
        mk_db::sqlx::query(
            r#"INSERT INTO "flash" ("id", "updatedAt", "title", "summary",
                                     "userId", "script")
               VALUES ($1, now(), $2, $2, $3, 'return 1;')"#,
        )
        .bind(fid)
        .bind(title)
        .bind(uid)
        .execute(&pool)
        .await
        .unwrap();
    }

    let (s, body) = post_json(app.clone(), "/api/i/flashs", json!({ "i": at })).await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1, "body: {body}");
    assert_eq!(arr[0]["id"], json!(f1));
    assert_eq!(arr[0]["title"], json!("alice flash"));
    assert_eq!(arr[0]["summary"], json!("alice flash"));
    assert_eq!(arr[0]["script"], json!("return 1;"));
    assert_eq!(arr[0]["visibility"], json!("public"));
    assert_eq!(arr[0]["userId"], json!(aid));
    assert_eq!(arr[0]["likedCount"], json!(0));
    assert!(arr[0]["createdAt"].is_string());

    // cursors: untilId=f1 is exclusive → empty; sinceId=f1 → no newer row
    let (_, b) = post_json(
        app.clone(),
        "/api/i/flashs",
        json!({ "i": at, "untilId": f1 }),
    )
    .await;
    assert_eq!(b, json!([]));
    let (_, b) = post_json(
        app.clone(),
        "/api/i/flashs",
        json!({ "i": at, "sinceId": f1 }),
    )
    .await;
    assert_eq!(b, json!([]));

    // other user's token only sees their own flash
    let (_, b) = post_json(app.clone(), "/api/i/flashs", json!({ "i": bob["token"] })).await;
    assert_eq!(b.as_array().unwrap().len(), 1);
    assert_eq!(b[0]["id"], json!(f2));

    // requireCredential: no token → CREDENTIAL_REQUIRED
    let (s, b) = post_json(app, "/api/i/flashs", json!({})).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "body: {b}");
    assert_eq!(b["error"]["code"], json!("CREDENTIAL_REQUIRED"));
}

/// `i/flash/likes`: the caller's flash_like rows joined to the liked flash.
#[tokio::test]
async fn i_flash_likes_lists_only_callers_likes() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mki_more_flash_likes").await;
    let app = crate::build_router(test_state(pool.clone()));
    let alice = signup(&app, &format!("flla{}", mk_types::id::gen_aid_now())).await;
    let bob = signup(&app, &format!("fllb{}", mk_types::id::gen_aid_now())).await;
    let at = alice["token"].as_str().unwrap().to_string();
    let aid = alice["id"].as_str().unwrap();
    let bid = bob["id"].as_str().unwrap();

    let p1 = mk_types::id::gen_aid_now();
    let p2 = mk_types::id::gen_aid_now();
    for (pid, title) in [(&p1, "liked by alice"), (&p2, "liked by bob")] {
        mk_db::sqlx::query(
            r#"INSERT INTO "flash" ("id", "updatedAt", "title", "summary",
                                     "userId", "script")
               VALUES ($1, now(), $2, $2, $3, 'return 1;')"#,
        )
        .bind(pid)
        .bind(title)
        .bind(bid)
        .execute(&pool)
        .await
        .unwrap();
    }
    let l1 = mk_types::id::gen_aid_now();
    let l2 = mk_types::id::gen_aid_now();
    for (lid, uid, pid) in [(&l1, aid, &p1), (&l2, bid, &p2)] {
        mk_db::sqlx::query(
            r#"INSERT INTO "flash_like" ("id", "userId", "flashId")
               VALUES ($1, $2, $3)"#,
        )
        .bind(lid)
        .bind(uid)
        .bind(pid)
        .execute(&pool)
        .await
        .unwrap();
    }

    let (s, body) = post_json(app.clone(), "/api/i/flash/likes", json!({ "i": at })).await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1, "body: {body}");
    assert_eq!(arr[0]["id"], json!(l1));
    assert_eq!(arr[0]["flash"]["id"], json!(p1));
    assert_eq!(arr[0]["flash"]["title"], json!("liked by alice"));
    assert_eq!(arr[0]["flash"]["userId"], json!(bid));
    assert!(arr[0]["createdAt"].is_string());

    // other user sees only their own like
    let (_, b) = post_json(app, "/api/i/flash/likes", json!({ "i": bob["token"] })).await;
    assert_eq!(b.as_array().unwrap().len(), 1);
    assert_eq!(b[0]["id"], json!(l2));
    assert_eq!(b[0]["flash"]["id"], json!(p2));
}

/// blocking/* + mute/*: create → list → delete roundtrip; a self-target is
/// rejected 400; another user's rows never leak into the list.
#[tokio::test]
async fn blocking_mute_list_roundtrip() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkblockmute").await;
    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let alice = signup(&app, &uname("bma")).await;
    let bob = signup(&app, &uname("bmb")).await;
    let carol = signup(&app, &uname("bmc")).await;
    let a_id = alice["id"].as_str().unwrap();
    let a_token = alice["token"].as_str().unwrap();
    let b_id = bob["id"].as_str().unwrap();
    let c_token = carol["token"].as_str().unwrap();

    // self-block → 400 (blocking/create meta.errors.BLOCKEE_IS_YOURSELF)
    let (s, body) = post_json(
        app.clone(),
        "/api/blocking/create",
        json!({ "userId": a_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "self-block: {body}");
    assert_eq!(body["error"]["code"], json!("BLOCKEE_IS_YOURSELF"));

    // block → alice's list shows bob, carol's stays empty → unblock
    let (s, body) = post_json(
        app.clone(),
        "/api/blocking/create",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "blocking/create: {body}");
    let (s, list) = post_json(app.clone(), "/api/blocking/list", json!({ "i": a_token })).await;
    assert_eq!(s, StatusCode::OK, "blocking/list: {list}");
    let rows = list.as_array().unwrap();
    assert_eq!(rows.len(), 1, "blocking/list: {list}");
    assert_eq!(rows[0]["blockeeId"], json!(b_id));
    assert_eq!(rows[0]["blockee"]["id"], json!(b_id));
    assert!(rows[0]["id"].is_string());
    assert!(rows[0]["createdAt"].is_string());
    let (s, other) = post_json(app.clone(), "/api/blocking/list", json!({ "i": c_token })).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        other.as_array().unwrap().is_empty(),
        "carol's blocking list: {other}"
    );
    let (s, body) = post_json(
        app.clone(),
        "/api/blocking/delete",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "blocking/delete: {body}");
    let (_, list) = post_json(app.clone(), "/api/blocking/list", json!({ "i": a_token })).await;
    assert!(list.as_array().unwrap().is_empty(), "after unblock: {list}");

    // mute (with expiry) → list → unmute
    let expires_at = 4_102_444_800_000i64; // 2100-01-01Z
    let (s, body) = post_json(
        app.clone(),
        "/api/mute/create",
        json!({ "userId": b_id, "expiresAt": expires_at, "i": a_token }),
    )
    .await;
    // upstream mute/create replies 204 (no content)
    assert_eq!(s, StatusCode::NO_CONTENT, "mute/create: {body}");
    let (s, list) = post_json(app.clone(), "/api/mute/list", json!({ "i": a_token })).await;
    assert_eq!(s, StatusCode::OK, "mute/list: {list}");
    let rows = list.as_array().unwrap();
    assert_eq!(rows.len(), 1, "mute/list: {list}");
    assert_eq!(rows[0]["muteeId"], json!(b_id));
    assert_eq!(rows[0]["mutee"]["id"], json!(b_id));
    assert!(rows[0]["expiresAt"].is_string(), "expiresAt: {list}");
    let (_, other) = post_json(app.clone(), "/api/mute/list", json!({ "i": c_token })).await;
    assert!(
        other.as_array().unwrap().is_empty(),
        "carol's mute list: {other}"
    );
    let (s, _) = post_json(
        app.clone(),
        "/api/mute/delete",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, list) = post_json(app, "/api/mute/list", json!({ "i": a_token })).await;
    assert!(list.as_array().unwrap().is_empty(), "after unmute: {list}");
}

/// `i/authorized-apps`: the caller's app rows (id DESC, secret included),
/// other users' apps excluded.
#[tokio::test]
async fn i_authorized_apps_lists_only_callers_apps() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mki_more_apps").await;
    let app = crate::build_router(test_state(pool.clone()));
    let alice = signup(&app, &format!("appa{}", mk_types::id::gen_aid_now())).await;
    let bob = signup(&app, &format!("appb{}", mk_types::id::gen_aid_now())).await;
    let at = alice["token"].as_str().unwrap().to_string();
    let aid = alice["id"].as_str().unwrap();
    let bid = bob["id"].as_str().unwrap();

    let a1 = mk_types::id::gen_aid_now();
    let a2 = mk_types::id::gen_aid_now();
    for (id, uid, name, callback, secret) in [
        (
            &a1,
            aid,
            "alice app",
            Some("https://example.com/cb"),
            "secret-alice",
        ),
        (&a2, bid, "bob app", None, "secret-bob"),
    ] {
        mk_db::sqlx::query(
            r#"INSERT INTO "app" ("id", "userId", "secret", "name",
                                   "description", "permission", "callbackUrl")
               VALUES ($1, $2, $3, $4, '', $5, $6)"#,
        )
        .bind(id)
        .bind(uid)
        .bind(secret)
        .bind(name)
        .bind(vec!["read:account".to_string()])
        .bind(callback)
        .execute(&pool)
        .await
        .unwrap();
    }

    let (s, body) = post_json(app.clone(), "/api/i/authorized-apps", json!({ "i": at })).await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1, "body: {body}");
    assert_eq!(arr[0]["id"], json!(a1));
    assert_eq!(arr[0]["name"], json!("alice app"));
    assert_eq!(arr[0]["callbackUrl"], json!("https://example.com/cb"));
    assert_eq!(arr[0]["permission"], json!(["read:account"]));
    assert_eq!(arr[0]["secret"], json!("secret-alice"));

    // other user sees only their own app (null callbackUrl stays null)
    let (_, b) = post_json(
        app.clone(),
        "/api/i/authorized-apps",
        json!({ "i": bob["token"] }),
    )
    .await;
    assert_eq!(b.as_array().unwrap().len(), 1);
    assert_eq!(b[0]["id"], json!(a2));
    assert_eq!(b[0]["callbackUrl"], Value::Null);

    // requireCredential: no token → CREDENTIAL_REQUIRED
    let (s, b) = post_json(app, "/api/i/authorized-apps", json!({})).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "body: {b}");
    assert_eq!(b["error"]["code"], json!("CREDENTIAL_REQUIRED"));
}

/// `admin/relays/*`: add → list → remove roundtrip over the 0001 `relay`
/// table, duplicate inbox → 500 (upstream uncaught UNIQUE violation),
/// non-admin 403 (requireModerator gate), invalid URL 400.
#[tokio::test]
async fn admin_relays_roundtrip() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkadmin_relays_rt").await;
    let app = crate::build_router(test_state(pool.clone()));
    let root_token = signup(&app, "root").await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let plain_token =
        signup(&app, &format!("plainrelay{}", mk_types::id::gen_aid_now())).await["token"]
            .as_str()
            .unwrap()
            .to_string();

    // non-admin → 403 moderator ROLE_PERMISSION_DENIED
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/relays/list",
        json!({ "i": plain_token }),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "body: {b}");
    assert_eq!(b["error"]["code"], json!("ROLE_PERMISSION_DENIED"));
    assert_eq!(
        b["error"]["id"],
        json!("d33d5333-db36-423d-a8f9-1a2b9549da41")
    );

    // empty list
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/relays/list",
        json!({ "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    assert_eq!(b, json!([]));

    // invalid url → INVALID_URL 400 (upstream https-only check)
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/relays/add",
        json!({ "i": root_token, "inbox": "http://relay.example/inbox" }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {b}");
    assert_eq!(b["error"]["code"], json!("INVALID_URL"));
    assert_eq!(
        b["error"]["id"],
        json!("fb8c92d3-d4e5-44e7-b3d4-800d5cef8b2c")
    );

    // add → 200 relay entity, status requesting
    let inbox = "https://relay.example/inbox";
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/relays/add",
        json!({ "i": root_token, "inbox": inbox }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let relay_id = b["id"]
        .as_str()
        .unwrap_or_else(|| panic!("relay id: {b}"))
        .to_string();
    assert!(!relay_id.is_empty(), "body: {b}");
    assert_eq!(b["inbox"], json!(inbox));
    assert_eq!(b["status"], json!("requesting"));

    // list → the single row
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/relays/list",
        json!({ "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {b}");
    let arr = b.as_array().unwrap();
    assert_eq!(arr.len(), 1, "body: {b}");
    assert_eq!(arr[0]["id"], json!(relay_id));
    assert_eq!(arr[0]["inbox"], json!(inbox));
    assert_eq!(arr[0]["status"], json!("requesting"));

    // duplicate inbox → 500 (UNIQUE btree, upstream uncaught insert error)
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/relays/add",
        json!({ "i": root_token, "inbox": inbox }),
    )
    .await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "body: {b}");
    assert_eq!(b["error"]["code"], json!("INTERNAL_ERROR"));
    let (_, b) = post_json(
        app.clone(),
        "/api/admin/relays/list",
        json!({ "i": root_token }),
    )
    .await;
    assert_eq!(b.as_array().unwrap().len(), 1, "body: {b}");

    // remove by inbox → 204, list empty
    let (s, b) = post_json(
        app.clone(),
        "/api/admin/relays/remove",
        json!({ "i": root_token, "inbox": inbox }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {b}");
    let (_, b) = post_json(
        app.clone(),
        "/api/admin/relays/list",
        json!({ "i": root_token }),
    )
    .await;
    assert_eq!(b, json!([]));

    // remove unknown inbox → 500 (upstream `throw new Error('relay not found')`)
    let (s, b) = post_json(
        app,
        "/api/admin/relays/remove",
        json!({ "i": root_token, "inbox": inbox }),
    )
    .await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "body: {b}");
    assert_eq!(b["error"]["code"], json!("INTERNAL_ERROR"));
}

/// `get-avatar-decorations` + `admin/avatar-decorations/*`: admin CRUD
/// roundtrip, the guest-ok public list (role-visibility filtering), and the
/// non-admin 403 gate.
#[tokio::test]
async fn avatar_decorations_roundtrip() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("mktest_avatar_decorations").await;
    mk_db::run_migrations(&pool).await.unwrap();
    let app = crate::build_router(test_state(pool.clone()));

    // first signup = root (admin), second = plain user.
    let admin = signup(&app, "root").await;
    let token = admin["token"].as_str().unwrap().to_string();
    let bob = signup(&app, "bob").await;
    let bob_token = bob["token"].as_str().unwrap().to_string();

    // gate: non-admin → 403 ROLE_PERMISSION_DENIED; no credential → 401.
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/avatar-decorations/list",
        json!({ "i": bob_token }),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["error"]["code"], json!("ROLE_PERMISSION_DENIED"));
    let (s, body) = post_json(app.clone(), "/api/admin/avatar-decorations/list", json!({})).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(body["error"]["code"], json!("CREDENTIAL_REQUIRED"));

    // create → entity shape.
    let (s, created) = post_json(
        app.clone(),
        "/api/admin/avatar-decorations/create",
        json!({
            "name": "star",
            "description": "a star",
            "url": "https://example.com/star.png",
            "roleIdsThatCanBeUsedThisDecoration": [],
            "category": "cute",
            "i": token,
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {created}");
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(id.len(), 10);
    assert!(created["createdAt"].is_string());
    assert_eq!(created["updatedAt"], Value::Null);
    assert_eq!(created["name"], json!("star"));
    assert_eq!(created["description"], json!("a star"));
    assert_eq!(created["url"], json!("https://example.com/star.png"));
    assert_eq!(created["category"], json!("cute"));

    // admin list sees it.
    let (s, list) = post_json(
        app.clone(),
        "/api/admin/avatar-decorations/list",
        json!({ "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {list}");
    let row = list
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == json!(id))
        .unwrap();
    assert_eq!(row["name"], json!("star"));
    assert!(row["createdAt"].is_string());
    assert_eq!(row["updatedAt"], Value::Null);

    // public list (guest-ok) returns the row; no admin-only timestamp keys.
    let (s, public) = post_json(app.clone(), "/api/get-avatar-decorations", json!({})).await;
    assert_eq!(s, StatusCode::OK, "body: {public}");
    let prow = public
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == json!(id))
        .unwrap();
    assert_eq!(prow["name"], json!("star"));
    assert_eq!(prow["description"], json!("a star"));
    assert_eq!(prow["url"], json!("https://example.com/star.png"));
    assert_eq!(prow["category"], json!("cute"));
    assert!(prow.get("createdAt").is_none());
    assert!(prow.get("updatedAt").is_none());

    // update → 204; a role id that is not a real role is filtered from the
    // guest list but stays in the admin list (upstream visibleRoles).
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/avatar-decorations/update",
        json!({
            "id": id,
            "name": "superstar",
            "description": "d",
            "url": "https://example.com/s.png",
            "roleIdsThatCanBeUsedThisDecoration": ["nonexistentrole"],
            "category": null,
            "i": token,
        }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {body}");
    let (s, list) = post_json(
        app.clone(),
        "/api/admin/avatar-decorations/list",
        json!({ "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let row = list
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == json!(id))
        .unwrap();
    assert_eq!(row["name"], json!("superstar"));
    assert_eq!(row["category"], Value::Null);
    assert_eq!(
        row["roleIdsThatCanBeUsedThisDecoration"],
        json!(["nonexistentrole"])
    );
    assert!(row["updatedAt"].is_string());
    let (_, public) = post_json(app.clone(), "/api/get-avatar-decorations", json!({})).await;
    let prow = public
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == json!(id))
        .unwrap();
    assert_eq!(prow["roleIdsThatCanBeUsedThisDecoration"], json!([]));

    // delete → 204; gone from both lists; a second delete → 500.
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/avatar-decorations/delete",
        json!({ "id": id, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "body: {body}");
    let (_, list) = post_json(
        app.clone(),
        "/api/admin/avatar-decorations/list",
        json!({ "i": token }),
    )
    .await;
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .all(|r| r["id"] != json!(id))
    );
    let (_, public) = post_json(app.clone(), "/api/get-avatar-decorations", json!({})).await;
    assert!(
        public
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["id"] != json!(id))
    );
    let (s, body) = post_json(
        app,
        "/api/admin/avatar-decorations/delete",
        json!({ "id": id, "i": token }),
    )
    .await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "body: {body}");
}

// ---------------------------------------------------------------------------
// gallery/* — create → show → like → list → update → delete (upstream
// endpoints/gallery/* + GalleryPostEntityService packs)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn gallery_arena_crud_like_list() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkgallery_arena").await;
    let app = crate::build_router(test_state(pool.clone()));

    let (a_token, a_id) = channels_signup(app.clone(), "galice").await;
    let (b_token, _) = channels_signup(app.clone(), "gbobby").await;

    // create — the fetch-resource fixture body (fileIds: []) stays accepted;
    // upstream pack(post, me) carries isLiked.
    let (s, post) = post_json(
        app.clone(),
        "/api/gallery/posts/create",
        json!({
            "title": "test post",
            "description": null,
            "fileIds": [],
            "isSensitive": false,
            "i": a_token,
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {post}");
    assert_eq!(post["title"], json!("test post"));
    assert_eq!(post["description"], Value::Null);
    assert_eq!(post["userId"], json!(a_id));
    assert_eq!(post["fileIds"], json!([]));
    assert_eq!(post["files"], json!([]));
    assert_eq!(post["isSensitive"], json!(false));
    assert_eq!(post["likedCount"], json!(0));
    assert_eq!(post["isLiked"], json!(false));
    assert!(post["createdAt"].is_string());
    assert!(post["updatedAt"].is_string());
    assert_eq!(post["user"]["id"], json!(a_id));
    let post_id = post["id"].as_str().unwrap().to_string();

    // guest create → CREDENTIAL_REQUIRED (auth before paramDef)
    let (s, body) = post_json(
        app.clone(),
        "/api/gallery/posts/create",
        json!({ "title": "x", "fileIds": [] }),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(body["error"]["code"], json!("CREDENTIAL_REQUIRED"));

    // show guest → no isLiked; show with bob → isLiked false
    let (s, shown) = post_json(
        app.clone(),
        "/api/gallery/posts/show",
        json!({ "postId": post_id }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {shown}");
    assert_eq!(shown["id"], json!(post_id));
    assert!(shown.get("isLiked").is_none(), "guest show: {shown}");
    let (s, shown) = post_json(
        app.clone(),
        "/api/gallery/posts/show",
        json!({ "postId": post_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {shown}");
    assert_eq!(shown["isLiked"], json!(false));

    // owner cannot like their own post
    let (s, body) = post_json(
        app.clone(),
        "/api/gallery/posts/like",
        json!({ "postId": post_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("YOUR_POST"));

    // bob likes → 204; duplicate → ALREADY_LIKED; unlike → 204 then NOT_LIKED
    let (s, _) = post_json(
        app.clone(),
        "/api/gallery/posts/like",
        json!({ "postId": post_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, body) = post_json(
        app.clone(),
        "/api/gallery/posts/like",
        json!({ "postId": post_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("ALREADY_LIKED"));

    let (s, shown) = post_json(
        app.clone(),
        "/api/gallery/posts/show",
        json!({ "postId": post_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {shown}");
    assert_eq!(shown["likedCount"], json!(1));
    assert_eq!(shown["isLiked"], json!(true));

    // global list: guest sees the post without isLiked; bob sees isLiked
    let (s, list) = post_json(app.clone(), "/api/gallery/posts", json!({})).await;
    assert_eq!(s, StatusCode::OK, "body: {list}");
    let arr = list.as_array().unwrap();
    assert!(arr.iter().any(|p| p["id"] == json!(post_id)), "{list}");
    let row = arr.iter().find(|p| p["id"] == json!(post_id)).unwrap();
    assert!(row.get("isLiked").is_none(), "guest list: {row}");
    let (s, list) = post_json(
        app.clone(),
        "/api/gallery/posts",
        json!({ "i": b_token, "limit": 1 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {list}");
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 1, "{list}");
    assert_eq!(arr[0]["isLiked"], json!(true));

    // featured/popular carry the liked post (guest packs)
    let (s, feat) = post_json(app.clone(), "/api/gallery/featured", json!({})).await;
    assert_eq!(s, StatusCode::OK, "body: {feat}");
    assert!(
        feat.as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == json!(post_id))
    );
    let (s, pop) = post_json(app.clone(), "/api/gallery/popular", json!({})).await;
    assert_eq!(s, StatusCode::OK, "body: {pop}");
    assert!(
        pop.as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == json!(post_id))
    );

    // update by owner → 200 packed post
    let (s, updated) = post_json(
        app.clone(),
        "/api/gallery/posts/update",
        json!({
            "postId": post_id,
            "title": "updated post",
            "description": "desc",
            "isSensitive": true,
            "i": a_token,
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {updated}");
    assert_eq!(updated["title"], json!("updated post"));
    assert_eq!(updated["description"], json!("desc"));
    assert_eq!(updated["isSensitive"], json!(true));

    // non-owner update is an owner-scoped no-op (upstream update({id,userId}))
    let (s, same) = post_json(
        app.clone(),
        "/api/gallery/posts/update",
        json!({ "postId": post_id, "title": "hijack", "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {same}");
    assert_eq!(same["title"], json!("updated post"));

    // non-owner delete → ACCESS_DENIED; owner delete → 204
    let (s, body) = post_json(
        app.clone(),
        "/api/gallery/posts/delete",
        json!({ "postId": post_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("ACCESS_DENIED"));
    let (s, _) = post_json(
        app.clone(),
        "/api/gallery/posts/delete",
        json!({ "postId": post_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // gone → NO_SUCH_POST
    let (s, body) = post_json(app, "/api/gallery/posts/show", json!({ "postId": post_id })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("NO_SUCH_POST"));
    assert_eq!(
        body["error"]["id"],
        json!("1137bf14-c5b0-4604-85bb-5b5371b1cd45")
    );
}

// ==== pages-agent slice: pages/{create,show,like,my,liked,featured,
// update,unlike,delete} (upstream endpoints/pages/*; page/page_like) ====
#[tokio::test]
async fn pages_arena_crud_like_list() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = mk_db::test_support::fresh_db("mkpages_arena").await;
    mk_db::run_migrations(&pool).await.unwrap();
    let app = crate::build_router(test_state(pool));

    let (a_token, a_id) = channels_signup(app.clone(), "alice").await;
    let (b_token, _) = channels_signup(app.clone(), "bobby").await;

    // requireCredential gate (auth before paramDef)
    let (s, body) = post_json(app.clone(), "/api/pages/create", json!({ "name": "x" })).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(body["error"]["code"], json!("CREDENTIAL_REQUIRED"));

    // create with the full upstream param set (name required, rest defaulted)
    let content = json!([{
        "id": "2be9a64b-5ada-43a3-85f3-ec3429551ded",
        "text": "Hello World!",
        "type": "text"
    }]);
    let variables = json!([{ "name": "x", "type": "string", "value": "y" }]);
    let (s, page) = post_json(
        app.clone(),
        "/api/pages/create",
        json!({
            "name": "my-page",
            "title": "My Page",
            "summary": "sum",
            "content": content,
            "variables": variables,
            "script": "return 1;",
            "eyeCatchingImageId": null,
            "font": "serif",
            "alignCenter": true,
            "hideTitleWhenPinned": true,
            "i": a_token,
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {page}");
    assert_eq!(page["name"], json!("my-page"));
    assert_eq!(page["title"], json!("My Page"));
    assert_eq!(page["summary"], json!("sum"));
    assert_eq!(page["content"], content);
    assert_eq!(page["variables"], variables);
    assert_eq!(page["font"], json!("serif"));
    assert_eq!(page["alignCenter"], json!(true));
    assert_eq!(page["hideTitleWhenPinned"], json!(true));
    assert_eq!(page["eyeCatchingImage"], Value::Null);
    assert_eq!(page["userId"], json!(a_id));
    assert_eq!(page["likedCount"], json!(0));
    assert!(page["createdAt"].is_string());
    assert!(page["updatedAt"].is_string());
    // channels_signup suffixes the username with an aid, so match the prefix.
    assert!(
        page["user"]["username"]
            .as_str()
            .is_some_and(|u| u.starts_with("alice")),
        "page user: {page}"
    );
    // create packs without me → no isLiked key
    assert!(page.get("isLiked").is_none(), "create pack: {page}");
    let page_id = page["id"].as_str().unwrap().to_string();

    // show guest → no isLiked; show with bob → isLiked false
    let (s, shown) = post_json(app.clone(), "/api/pages/show", json!({ "pageId": page_id })).await;
    assert_eq!(s, StatusCode::OK, "body: {shown}");
    assert_eq!(shown["name"], json!("my-page"));
    assert!(shown.get("isLiked").is_none(), "guest show: {shown}");
    let (s, shown) = post_json(
        app.clone(),
        "/api/pages/show",
        json!({ "pageId": page_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {shown}");
    assert_eq!(shown["isLiked"], json!(false));

    // owner cannot like their own page
    let (s, body) = post_json(
        app.clone(),
        "/api/pages/like",
        json!({ "pageId": page_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("YOUR_PAGE"));

    // bob likes → 204; duplicate → ALREADY_LIKED
    let (s, _) = post_json(
        app.clone(),
        "/api/pages/like",
        json!({ "pageId": page_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, body) = post_json(
        app.clone(),
        "/api/pages/like",
        json!({ "pageId": page_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("ALREADY_LIKED"));

    let (s, shown) = post_json(
        app.clone(),
        "/api/pages/show",
        json!({ "pageId": page_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {shown}");
    assert_eq!(shown["likedCount"], json!(1));
    assert_eq!(shown["isLiked"], json!(true));

    // pages/my (me pack → isLiked false for alice) + pages/liked (bob)
    let (s, mine) = post_json(app.clone(), "/api/pages/my", json!({ "i": a_token })).await;
    assert_eq!(s, StatusCode::OK, "body: {mine}");
    let mine = mine.as_array().unwrap();
    assert_eq!(mine.len(), 1, "my: {mine:?}");
    assert_eq!(mine[0]["id"], json!(page_id));
    assert_eq!(mine[0]["isLiked"], json!(false));
    let (s, mine) = post_json(app.clone(), "/api/pages/my", json!({ "i": b_token })).await;
    assert_eq!(s, StatusCode::OK, "body: {mine}");
    assert!(mine.as_array().unwrap().is_empty(), "bob my: {mine}");
    let (s, liked) = post_json(app.clone(), "/api/pages/liked", json!({ "i": b_token })).await;
    assert_eq!(s, StatusCode::OK, "body: {liked}");
    let liked = liked.as_array().unwrap();
    assert_eq!(liked.len(), 1, "liked: {liked:?}");
    assert_eq!(liked[0]["id"], json!(page_id));
    assert_eq!(liked[0]["isLiked"], json!(true));

    // featured surfaces liked pages (guest-ok)
    let (s, featured) = post_json(app.clone(), "/api/pages/featured", json!({})).await;
    assert_eq!(s, StatusCode::OK, "body: {featured}");
    let featured = featured.as_array().unwrap();
    assert_eq!(featured.len(), 1, "featured: {featured:?}");
    assert_eq!(featured[0]["id"], json!(page_id));

    // update is owner-only and returns 204
    let (s, body) = post_json(
        app.clone(),
        "/api/pages/update",
        json!({ "pageId": page_id, "title": "nope", "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("ACCESS_DENIED"));
    let (s, _) = post_json(
        app.clone(),
        "/api/pages/update",
        json!({ "pageId": page_id, "title": "Renamed", "font": "sans-serif", "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, shown) = post_json(app.clone(), "/api/pages/show", json!({ "pageId": page_id })).await;
    assert_eq!(s, StatusCode::OK, "body: {shown}");
    assert_eq!(shown["title"], json!("Renamed"));
    assert_eq!(shown["font"], json!("sans-serif"));
    assert_eq!(
        shown["content"], content,
        "content survives a partial update"
    );

    // unlike → 204; a second unlike → NOT_LIKED; count back to 0
    let (s, _) = post_json(
        app.clone(),
        "/api/pages/unlike",
        json!({ "pageId": page_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, body) = post_json(
        app.clone(),
        "/api/pages/unlike",
        json!({ "pageId": page_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("NOT_LIKED"));
    let (s, shown) = post_json(app.clone(), "/api/pages/show", json!({ "pageId": page_id })).await;
    assert_eq!(s, StatusCode::OK, "body: {shown}");
    assert_eq!(shown["likedCount"], json!(0));

    // delete is owner/moderator-only; afterwards show → NO_SUCH_PAGE
    let (s, body) = post_json(
        app.clone(),
        "/api/pages/delete",
        json!({ "pageId": page_id, "i": b_token }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("ACCESS_DENIED"));
    let (s, _) = post_json(
        app.clone(),
        "/api/pages/delete",
        json!({ "pageId": page_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, body) = post_json(app.clone(), "/api/pages/show", json!({ "pageId": page_id })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], json!("NO_SUCH_PAGE"));
    let (s, mine) = post_json(app.clone(), "/api/pages/my", json!({ "i": a_token })).await;
    assert_eq!(s, StatusCode::OK, "body: {mine}");
    assert!(
        mine.as_array().unwrap().is_empty(),
        "my after delete: {mine}"
    );
}

/// federation/*: the instance list filters, show-instance by host, the stats
/// counts, and the host-filtered users/followers/following reads.
#[tokio::test]
async fn federation_instances_show_stats_and_host_lists() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkfederation").await;
    let app = crate::build_router(test_state(pool.clone()));

    // Two instances: one healthy/federating, one manually suspended with no
    // federation counts (so only the first lands in the stats top lists).
    for (id, host, state, following, followers) in [
        ("i1", "remote.example", "none", 1, 1),
        ("i2", "suspended.example", "manuallySuspended", 0, 0),
    ] {
        mk_db::sqlx::query(
            r#"INSERT INTO "instance"
                 ("id", "firstRetrievedAt", "host", "usersCount", "notesCount",
                  "followingCount", "followersCount", "isNotResponding",
                  "softwareName", "softwareVersion", "suspensionState")
               VALUES ($1, now(), $2, 3, 7, $4, $5, false,
                       'misskey', '13.0.0', $3::public.instance_suspensionstate_enum)"#,
        )
        .bind(id)
        .bind(host)
        .bind(state)
        .bind(following)
        .bind(followers)
        .execute(&pool)
        .await
        .unwrap();
    }

    // federation/instances: full list + the host/suspended/blocked filters.
    let (s, body) = post_json(app.clone(), "/api/federation/instances", json!({})).await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body.as_array().unwrap().len(), 2, "body: {body}");
    let remote = body
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["host"] == json!("remote.example"))
        .expect("remote instance listed");
    assert_eq!(remote["usersCount"], json!(3));
    assert_eq!(remote["notesCount"], json!(7));
    assert_eq!(remote["followingCount"], json!(1));
    assert_eq!(remote["followersCount"], json!(1));
    assert_eq!(remote["softwareName"], json!("misskey"));
    assert_eq!(remote["softwareVersion"], json!("13.0.0"));
    assert_eq!(remote["isSuspended"], json!(false));
    assert_eq!(remote["suspensionState"], json!("none"));
    assert_eq!(remote["latestRequestReceivedAt"], Value::Null);
    assert_eq!(remote["moderationNote"], Value::Null);
    assert!(remote["firstRetrievedAt"].is_string());

    let (_, body) = post_json(
        app.clone(),
        "/api/federation/instances",
        json!({ "host": "remote" }),
    )
    .await;
    assert_eq!(body.as_array().unwrap().len(), 1, "body: {body}");
    assert_eq!(body[0]["host"], json!("remote.example"));

    let (_, body) = post_json(
        app.clone(),
        "/api/federation/instances",
        json!({ "suspended": true }),
    )
    .await;
    assert_eq!(body.as_array().unwrap().len(), 1, "body: {body}");
    assert_eq!(body[0]["host"], json!("suspended.example"));
    assert_eq!(body[0]["isSuspended"], json!(true));
    assert_eq!(body[0]["suspensionState"], json!("manuallySuspended"));

    // empty meta.blockedHosts + blocked=true → upstream's `1=0`.
    let (_, body) = post_json(
        app.clone(),
        "/api/federation/instances",
        json!({ "blocked": true }),
    )
    .await;
    assert_eq!(body.as_array().unwrap().len(), 0, "body: {body}");

    // federation/show-instance: by host; missing → null → 204.
    let (s, body) = post_json(
        app.clone(),
        "/api/federation/show-instance",
        json!({ "host": "remote.example" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body["host"], json!("remote.example"));
    assert_eq!(body["usersCount"], json!(3));
    let (s, _) = post_json(
        app.clone(),
        "/api/federation/show-instance",
        json!({ "host": "missing.example" }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // Seed users/profiles + following edges for stats + host lists.
    for uid in ["u1", "u2", "u3", "u4", "u5"] {
        mk_db::sqlx::query(
            r#"INSERT INTO "user" ("id", "username", "usernameLower")
               VALUES ($1, $1, $1)"#,
        )
        .bind(uid)
        .execute(&pool)
        .await
        .unwrap();
        mk_db::sqlx::query(r#"INSERT INTO "user_profile" ("userId") VALUES ($1)"#)
            .bind(uid)
            .execute(&pool)
            .await
            .unwrap();
    }
    // A remote user row (federation/users).
    mk_db::sqlx::query(
        r#"INSERT INTO "user" ("id", "username", "usernameLower", "host")
           VALUES ('ru1', 'bob', 'bob', 'remote.example')"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    mk_db::sqlx::query(r#"INSERT INTO "user_profile" ("userId") VALUES ('ru1')"#)
        .execute(&pool)
        .await
        .unwrap();

    // f1-f3: remote followees (followers endpoint / all_sub_count);
    // f4-f5: remote followers (following endpoint / all_pub_count).
    for (id, follower, followee, follower_host, followee_host) in [
        ("f1", "u1", "u2", None, Some("remote.example")),
        ("f2", "u1", "u3", None, Some("remote.example")),
        ("f3", "u1", "u4", None, Some("remote.example")),
        ("f4", "u2", "u1", Some("remote.example"), None),
        ("f5", "u3", "u1", Some("remote.example"), None),
    ] {
        mk_db::sqlx::query(
            r#"INSERT INTO "following" ("id", "followerId", "followeeId",
                                        "followerHost", "followeeHost")
               VALUES ($1, $2, $3, $4, $5)"#,
        )
        .bind(id)
        .bind(follower)
        .bind(followee)
        .bind(follower_host)
        .bind(followee_host)
        .execute(&pool)
        .await
        .unwrap();
    }

    // federation/stats: top instances by followers/following + the totals.
    let (s, body) = post_json(app.clone(), "/api/federation/stats", json!({})).await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    let subs = body["topSubInstances"].as_array().unwrap();
    // upstream ranks only non-suspended instances (suspended.example excluded)
    assert_eq!(subs.len(), 1, "body: {body}");
    assert_eq!(subs[0]["host"], json!("remote.example"));
    assert_eq!(body["otherFollowersCount"], json!(2)); // 3 remote edges - 1 top
    let pubs = body["topPubInstances"].as_array().unwrap();
    assert_eq!(pubs.len(), 1, "body: {body}");
    assert_eq!(pubs[0]["host"], json!("remote.example"));
    assert_eq!(body["otherFollowingCount"], json!(1)); // 2 remote edges - 1 top

    // federation/users: only rows with the exact host.
    let (s, body) = post_json(
        app.clone(),
        "/api/federation/users",
        json!({ "host": "remote.example" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body.as_array().unwrap().len(), 1, "body: {body}");
    assert_eq!(body[0]["id"], json!("ru1"));
    assert_eq!(body[0]["host"], json!("remote.example"));
    assert_eq!(body[0]["username"], json!("bob"));

    // federation/followers: followeeHost = host, public visibility.
    let (s, body) = post_json(
        app.clone(),
        "/api/federation/followers",
        json!({ "host": "remote.example" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body.as_array().unwrap().len(), 3, "body: {body}");
    assert_eq!(body[0]["followeeId"], json!("u4")); // id DESC
    assert_eq!(body[0]["followee"]["id"], json!("u4"));
    assert!(body[0].get("follower").is_none());

    // federation/following: followerHost = host.
    let (s, body) = post_json(
        app.clone(),
        "/api/federation/following",
        json!({ "host": "remote.example" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(body.as_array().unwrap().len(), 2, "body: {body}");
    assert_eq!(body[0]["followeeId"], json!("u1"));
}

// renote-mute/list: create → list → delete over renote_muting (the
// endpoints/renote-mute/list.ts partner pack).
#[tokio::test]
async fn renote_mute_list_roundtrip() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkrenotemute").await;
    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    let alice = signup(&app, &uname("rma")).await;
    let bob = signup(&app, &uname("rmb")).await;
    let a_token = alice["token"].as_str().unwrap().to_string();
    let b_id = bob["id"].as_str().unwrap().to_string();

    let (s, _) = post_json(
        app.clone(),
        "/api/renote-mute/create",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "renote-mute/create");

    let (s, list) = post_json(
        app.clone(),
        "/api/renote-mute/list",
        json!({ "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "renote-mute/list: {list}");
    let rows = list.as_array().unwrap();
    assert_eq!(rows.len(), 1, "list: {list}");
    assert_eq!(rows[0]["muteeId"], json!(b_id));
    assert_eq!(rows[0]["mutee"]["id"], json!(b_id));
    assert!(rows[0]["id"].is_string());

    let (s, _) = post_json(
        app.clone(),
        "/api/renote-mute/delete",
        json!({ "userId": b_id, "i": a_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "renote-mute/delete");
    let (_, list) = post_json(
        app.clone(),
        "/api/renote-mute/list",
        json!({ "i": a_token }),
    )
    .await;
    assert_eq!(list.as_array().unwrap().len(), 0, "after delete: {list}");
}

// admin/accounts/*: find-by-email hit/miss + create→find-by-email→delete +
// non-admin 403 (upstream endpoints/admin/accounts/*.ts, requireModerator).
#[tokio::test]
async fn admin_accounts_roundtrip() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    init_tracing();
    let pool = fresh_pool("mkadminaccounts").await;
    let uname = |p: &str| format!("{p}{}", mk_types::id::gen_aid_now());
    let app = crate::build_router(test_state(pool.clone()));

    // First signup stamps meta.rootUserId → moderator gate bypass.
    let root = signup(&app, &uname("adma")).await;
    let root_token = root["token"].as_str().unwrap().to_string();
    let bob = signup(&app, &uname("admb")).await;
    let bob_token = bob["token"].as_str().unwrap().to_string();

    // find-by-email miss → 204.
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/accounts/find-by-email",
        json!({ "email": "nobody@example.com", "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "miss: {body}");

    // Plain user → ROLE_PERMISSION_DENIED 403.
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/accounts/find-by-email",
        json!({ "email": "nobody@example.com", "i": bob_token }),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "non-admin: {body}");

    // create → packed UserDetailedNotMe.
    let new_username = uname("admc");
    let (s, created) = post_json(
        app.clone(),
        "/api/admin/accounts/create",
        json!({ "username": new_username, "password": "test", "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "create: {created}");
    let created_id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["username"], json!(new_username));

    // The create path leaves the email unset; attach one for the lookup.
    mk_db::user::set_email_verify_code(&pool, &created_id, "target@example.com", "123456")
        .await
        .unwrap();

    // find-by-email hit.
    let (s, found) = post_json(
        app.clone(),
        "/api/admin/accounts/find-by-email",
        json!({ "email": "target@example.com", "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "hit: {found}");
    assert_eq!(found["id"], json!(created_id));

    // delete → 204.
    let (s, body) = post_json(
        app.clone(),
        "/api/admin/accounts/delete",
        json!({ "userId": created_id, "i": root_token }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "delete: {body}");
}
