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

    // e2e asserts exact field sets — optional-absent fields must be missing
    let got = note
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    for absent in [
        "deletedAt",
        "cw",
        "replyId",
        "renoteId",
        "reply",
        "renote",
        "poll",
        "channelId",
        "myReaction",
        "clippedCount",
    ] {
        assert!(!got.contains(absent), "{absent} must be absent");
    }
    for present in [
        "mentions",
        "fileIds",
        "files",
        "tags",
        "emojis",
        "reactions",
        "reactionEmojis",
        "localOnly",
        "reactionCount",
        "renoteCount",
        "visibleUserIds",
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
    assert_eq!(b["notesPerOneAd"], json!(20));
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

    // emergent block gate: dave blocks alice → excluded from her list even
    // though this server still accepted the reply at creation time;
    // mallory (blocked BY alice) stays included
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
    for target in [&d1, &m1] {
        let (s, body) = post_json(
            app.clone(),
            "/api/notes/create",
            json!({ "text": "re2", "replyId": target, "i": a_token }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "body: {body}");
    }
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
