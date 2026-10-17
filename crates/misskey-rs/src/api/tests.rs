//! M1 signup round-trip against the real router + PostgreSQL (CI services;
//! skipped locally without DATABASE_URL).

use super::*;
use axum::body::Body;
use axum::http::Request;
use serde_json::{json, Value};
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

    // A posts the mention text. M1 notes/create has no mention parsing, so
    // the notification row is inserted through the truthful mk-db path
    // (the same INSERT upstream NotificationService lands in).
    let (s, nb) = post_json(
        app.clone(),
        "/api/notes/create",
        json!({ "i": token_a, "text": format!("@{} hello", username_b) }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {nb}");
    let note_id = nb["createdNote"]["id"].as_str().unwrap().to_string();

    let mention_id = mk_types::id::gen_aid_now();
    mk_db::notification::insert_notification(
        &pool,
        &mk_db::notification::NewNotification {
            id: &mention_id,
            r#type: "mention",
            user_id: &b_id,
            notifier_id: Some(a["id"].as_str().unwrap()),
            note_id: Some(&note_id),
            reaction: None,
        },
    )
    .await
    .unwrap();

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
    assert_eq!(n["id"], mention_id.as_str());
    assert_eq!(n["type"], "mention");
    assert!(n["createdAt"].as_str().is_some());
    assert_eq!(n["userId"], a["id"]);
    assert_eq!(n["user"]["username"], username_a.as_str());
    assert!(n["user"]["avatarUrl"]
        .as_str()
        .unwrap()
        .contains("/identicon/"));
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
    let (s, list4) = post_json(app.clone(), "/api/notifications", json!({ "i": token_b })).await;
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
        json!({ "i": token_b, "untilId": renote_id }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {list5}");
    let arr5 = list5.as_array().unwrap();
    assert_eq!(arr5.len(), 1, "body: {list5}");
    assert_eq!(arr5[0]["type"], "mention");

    // sinceId-only flips to oldest-first ASC: mention comes after renote.
    let (s, list6) = post_json(
        app.clone(),
        "/api/notifications",
        json!({ "i": token_b, "sinceId": mention_id }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {list6}");
    let arr6 = list6.as_array().unwrap();
    assert_eq!(arr6.len(), 2, "body: {list6}");
    assert_eq!(arr6[0]["type"], "renote");
    assert_eq!(arr6[1]["type"], "follow");

    // markAsRead:false leaves rows unread.
    let (s, list7) = post_json(
        app.clone(),
        "/api/notifications",
        json!({ "i": token_b, "markAsRead": false, "unreadOnly": true, "includeTypes": ["renote", "follow"] }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "body: {list7}");
    let arr7 = list7.as_array().unwrap();
    assert_eq!(arr7.len(), 2, "body: {list7}");

    // Then a default call (markAsRead=true) clears them.
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
