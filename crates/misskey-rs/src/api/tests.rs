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
        },
        pool: Some(pool),
        rate_limiter: std::sync::Arc::new(crate::ratelimit::RateLimiter::new()),
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

    // persisted edge + counters (insertFollowingDoc + increments)
    let (notify, with_replies): (String, bool) = mk_db::sqlx::query_as(
        r#"SELECT "notify", "withReplies" FROM "following" WHERE "followerId" = $1 AND "followeeId" = $2"#,
    )
    .bind(a_id)
    .bind(b_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(notify, "none");
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
    };
    // same ioredis keyPrefix shape as ftt2 — the exact name the pinned e2e
    // `redisForTimelines.exists/lrange` computes for the followee's HTL.
    let prefix = format!("{}:", cfg.host());
    let hkey = |uid: &str| format!("{prefix}list:homeTimeline:{uid}");
    let app = crate::build_router(crate::AppState {
        config: cfg,
        pool: Some(pool.clone()),
        rate_limiter: std::sync::Arc::new(crate::ratelimit::RateLimiter::new()),
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

    // Registry spy: alice's localTimeline + homeTimeline subscriptions.
    let sink = |ch: &'static str, sub: &'static str| {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        assert!(crate::api::streaming::connect_channel(
            ch,
            &json!({}),
            sub,
            tx,
            &a_id
        ));
        rx
    };
    let mut ltl = sink("localTimeline", "a-ltl");
    let mut htl = sink("homeTimeline", "a-htl");

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
        },
        pool: None,
        rate_limiter: std::sync::Arc::new(crate::ratelimit::RateLimiter::new()),
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
