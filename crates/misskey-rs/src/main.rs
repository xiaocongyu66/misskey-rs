// Large `json!` literals (endpoint response bodies) blow past the default 128.
#![recursion_limit = "512"]

mod api;
mod config;
mod error;
mod ratelimit;

use std::net::SocketAddr;

use axum::Router;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::routing::{any, get};
use tower_http::cors::CorsLayer;

pub use config::Config;

#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub pool: Option<mk_db::sqlx::PgPool>,
    pub rate_limiter: std::sync::Arc<ratelimit::RateLimiter>,
}

pub fn build_router(state: AppState) -> Router {
    // CORS is scoped to /api only: the well-known webfinger OPTIONS preflight
    // (upstream fastify.options(allPath) → 204 with `Access-Control-Allow-
    // Headers: Accept`) must reach the WellKnownServerService dispatcher — a
    // router-wide permissive layer would answer it first with `*`.
    let api_cors = CorsLayer::permissive();
    let api = Router::new()
        .route("/api/{*name}", any(api::dispatch))
        .layer(api_cors);
    // ff-arena compile triage (CI run 39208798436, E0277): the (State, Request)
    // `any` handler only resolves its `Handler` bound on a chain in the same
    // shape as the /api dispatcher route (api::dispatch compiles there since
    // M1) — on the outer pre-state chain rustc 1.93 / axum 0.8.9 fails to
    // prove the bound for well_known_dispatch. Sibling state-later sub-router,
    // CORS-free on purpose: the OPTIONS 204 + hook headers are served inside
    // the dispatcher itself (a permissive layer would answer `*` first).
    let well_known = Router::new()
        .route(
            "/.well-known/{*rest}",
            any(api::well_known::well_known_dispatch),
        )
        .route("/nodeinfo/2.0", get(api::well_known::nodeinfo_20))
        .route("/nodeinfo/2.1", get(api::well_known::nodeinfo_21));
    // arena-oauth: OAuth2ProviderService /oauth/* provider routes sit OUTSIDE
    // /api like the upstream fastify instances (ServerModule.ts registers
    // OAuth2ProviderService.createServer under the /oauth prefix; CORS is
    // handled per-handler — only the token server is @fastify/cors-wrapped).
    let oauth = Router::new().route("/oauth/{*rest}", any(api::oauth::dispatch));
    Router::new()
        .merge(api)
        .merge(well_known)
        .merge(oauth)
        // streaming websocket hub (drive-agent2: publishMainStream /
        // urlUploadFinished — upstream StreamingApiServerService entry).
        .route("/streaming", get(api::drive::streaming::entry))
        // ff-arena: the AP collections the ff-visibility.ts AP probes hit
        // (upstream ActivityPubServerService.ts /users/:user/{following,followers}).
        .route("/users/{user}/following", get(api::ff_vis::ap_following))
        .route("/users/{user}/followers", get(api::ff_vis::ap_followers))
        // endpoints-arena: the AP note Document the drive/files/create
        // getWebpublicType legs probe (ActivityPubServerService /notes/:id).
        .route("/notes/{id}", get(api::ap_note))
        // endpoints-arena: the /url preview route (ClientServerService.ts
        // L450 fastify.get '/url' → UrlPreviewService.handle; sits outside
        // /api like the upstream web server).
        .route("/url", get(api::url_preview))
        // arena-exports: the internal-storage byte route the exported drive
        // files are fetched through (upstream FileServerService /files/*).
        .route("/files/{*rest}", get(api::exports::files_serve))
        .route("/healthz", get(healthz))
        .layer(middleware::from_fn(json_utf8_charset))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .with_state(state)
}

/// Upstream fastify emits `application/json; charset=utf-8`; axum's Json
/// emits bare `application/json`. The e2e client strictly string-compares
/// the header, so normalize it (only the bare form touches this layer).
async fn json_utf8_charset(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let mut res = next.run(req).await;
    if res
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .map(|v| v == "application/json")
        .unwrap_or(false)
    {
        res.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json; charset=utf-8"),
        );
    }
    res
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config.yaml".to_string());
    let config = config::load(&config_path)?;

    let pool = match &config.db {
        Some(db) => {
            let pool = mk_db::connect(&db.url()).await?;
            mk_db::run_migrations(&pool).await?;
            // arena-exports: the exportClips consumer (upstream's db-queue
            // BullMQ worker lane) — PG-brokered via mk_queue. config.url is
            // the base the exported drive files' /files urls embed.
            api::exports::spawn_export_worker(pool.clone(), config.url.clone());
            // arena-move: the relationship worker — follow/block copy jobs +
            // the delayed (test: 10s / prod: 24h) unfollow of the moved
            // account's own followings (AccountMoveService + queueService).
            api::move_account::spawn_move_worker(pool.clone());
            tracing::info!("postgres connected, migrations applied");
            Some(pool)
        }
        None => {
            tracing::warn!("no db configured; running without postgres");
            None
        }
    };

    let port = config.port;
    let url = config.url.clone();
    let state = AppState {
        config,
        pool,
        rate_limiter: std::sync::Arc::new(ratelimit::RateLimiter::new()),
    };

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("serving at {} (listening on {})", url, addr);
    axum::serve(listener, build_router(state)).await?;
    Ok(())
}

async fn healthz(State(state): State<AppState>) -> (StatusCode, &'static str) {
    match &state.pool {
        None => (StatusCode::OK, "ok"),
        Some(pool) => match mk_db::sqlx::query("SELECT 1").execute(pool).await {
            Ok(_) => (StatusCode::OK, "ok"),
            Err(err) => {
                tracing::error!(%err, "healthz db check failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "db unavailable")
            }
        },
    }
}
