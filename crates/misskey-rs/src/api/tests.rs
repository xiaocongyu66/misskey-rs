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
    for absent in ["isBot", "instance", "isFollowing"] {
        assert!(!obj.contains_key(absent), "{absent} must be absent");
    }
    assert!(body["avatarUrl"].as_str().unwrap().contains("/identicon/"));
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
