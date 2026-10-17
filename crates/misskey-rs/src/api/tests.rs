//! M1 signup round-trip against the real router + PostgreSQL (CI services;
//! skipped locally without DATABASE_URL).

use super::*;
use axum::body::Body;
use axum::http::Request;
use serde_json::{json, Value};
use tower::ServiceExt;

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

async fn fresh_pool(db: &str) -> sqlx::PgPool {
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
    let (s3, b3) = post_json(app, "/api/users/show", json!({ "username": username })).await;
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
