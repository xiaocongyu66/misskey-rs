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
use axum::routing::{any, get, post};
use tower_http::cors::CorsLayer;

pub use config::Config;

#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub pool: Option<mk_db::sqlx::PgPool>,
    pub rate_limiter: std::sync::Arc<ratelimit::RateLimiter>,
    // meili-idx: the NoteSearchEngine seam (mk-core services/search_bridge —
    // upstream DI.meilisearch + SearchService). NoopEngine when the config
    // carries no meilisearch.host: index writes drop, queries keep the
    // endpoints' SQL fallbacks.
    pub search_engine: std::sync::Arc<dyn mk_core::services::search_bridge::NoteSearchEngine>,
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
    // arena-fetchres: the GET web surfaces OUTSIDE /api the pinned
    // test/e2e/fetch-resource.ts drives (upstream ClientServerService.ts +
    // ActivityPubServerService.ts). /notes/{id} swaps its plain-AP handler
    // for the apOrHtml negotiation (HTML arm renders the note page; the AP
    // arm still rides api::ap_note); /streaming swaps to a non-upgrade 503
    // arm (upstream fastify.get '/streaming') that passes WS upgrades through.
    let fetch_resource = Router::new()
        .route("/@{*acct}", get(api::fetch_resource::at_dispatch))
        .route("/users/{user}", get(api::fetch_resource::users_get))
        .route("/users/{user}/outbox", get(api::fetch_resource::ap_outbox))
        .route("/play/{id}", get(api::fetch_resource::play_get))
        .route("/clips/{clip}", get(api::fetch_resource::clips_get))
        .route("/gallery/{post}", get(api::fetch_resource::gallery_get))
        .route("/channels/{channel}", get(api::fetch_resource::channel_get))
        .route("/robots.txt", get(api::fetch_resource::robots_txt))
        .route("/opensearch.xml", get(api::fetch_resource::opensearch))
        .route("/api.json", get(api::fetch_resource::api_json))
        .route("/favicon.ico", get(api::fetch_resource::favicon))
        .route(
            "/apple-touch-icon.png",
            get(api::fetch_resource::apple_touch_icon),
        )
        .route("/twemoji/{*path}", get(api::fetch_resource::twemoji))
        .route(
            "/twemoji-badge/{*path}",
            get(api::fetch_resource::twemoji_badge),
        )
        .route(
            "/fluent-emoji/{*path}",
            get(api::fetch_resource::fluent_emoji),
        )
        .route("/bios", get(api::fetch_resource::bios))
        .route("/cli", get(api::fetch_resource::cli))
        .route("/flush", get(api::fetch_resource::flush))
        .route("/_info_card_", get(api::fetch_resource::info_card));
    Router::new()
        .merge(api)
        .merge(well_known)
        .merge(oauth)
        .merge(fetch_resource)
        // streaming websocket hub (drive-agent2: publishMainStream /
        // urlUploadFinished — upstream StreamingApiServerService entry).
        .route("/streaming", get(api::fetch_resource::streaming_get))
        // ff-arena: the AP collections the ff-visibility.ts AP probes hit
        // (upstream ActivityPubServerService.ts /users/:user/{following,followers}).
        .route("/users/{user}/following", get(api::ff_vis::ap_following))
        .route("/users/{user}/followers", get(api::ff_vis::ap_followers))
        // endpoints-arena: the AP note Document the drive/files/create
        // getWebpublicType legs probe — arena-fetchres now negotiates the
        // HTML mini-render arm upstream serves otherwise (ClientServerService
        // /notes/:note SSR + ActivityPubServerService apOrHtml constraint).
        .route("/notes/{id}", get(api::fetch_resource::notes_get))
        // endpoints-arena: the /url preview route (ClientServerService.ts
        // L450 fastify.get '/url' → UrlPreviewService.handle; sits outside
        // /api like the upstream web server).
        .route("/url", get(api::url_preview))
        // arena-exports: the internal-storage byte route the exported drive
        // files are fetched through (upstream FileServerService /files/*).
        .route("/files/{*rest}", get(api::exports::files_serve))
        .route("/healthz", get(healthz))
        // fed-min: the inbox POST rows (upstream ActivityPubServerService.ts
        // fastify.post '/inbox' + '/users/:user/inbox' — parse, enqueue, 202;
        // the signature verify + per-type work run worker-side upstream, so
        // both rows answer 202 unconditionally, unknown types included).
        // Non-POST methods keep the pre-fed-min fall-through (the GET rows
        // ride the client catch-all upstream → fallback here): a post()-only
        // MethodRouter would 405 the GET /inbox HTML rows (CI 39424566365).
        .route(
            "/inbox",
            post(inbox_post).fallback(api::fetch_resource::fallback),
        )
        .route(
            "/users/{user}/inbox",
            post(inbox_post).fallback(api::fetch_resource::fallback),
        )
        // arena-fetchres: the fastify.get('*') renderBase tail (also the
        // POST-only /inbox + /users/:id/inbox GET rows — they fall through to
        // the client ' catch-all upstream).
        .fallback(api::fetch_resource::fallback)
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
            // vapid: generate-and-persist the sw keypair once (upstream
            // MetaService.init) so the push identity is stable thereafter.
            match mk_core::services::config::ensure_vapid_keys(&pool).await {
                Ok(_) => tracing::info!("vapid: sw keypair ready"),
                Err(e) => tracing::warn!("vapid: ensure_vapid_keys failed: {e}"),
            }
            // arena-exports: the exportClips consumer (upstream's db-queue
            // BullMQ worker lane) — PG-brokered via mk_queue. config.url is
            // the base the exported drive files' /files urls embed.
            api::exports::spawn_export_worker(pool.clone(), config.url.clone());
            // arena-move: the relationship worker — follow/block copy jobs +
            // the delayed (test: 10s / prod: 24h) unfollow of the moved
            // account's own followings (AccountMoveService + queueService).
            api::move_account::spawn_move_worker(pool.clone());
            // queue-min: the endedPollNotification + deleteAccount consumers
            // (upstream's BullMQ endedPollNotification queue + the db-queue
            // `deleteAccount` processor) — one PG-brokered worker by kind.
            api::queue_processors::spawn_queue_workers(pool.clone());
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
    // meili-idx: the engine_from_config wiring the search_bridge module doc
    // spells out (borrow before the `config` move, hence the hoisted let).
    let search_engine = mk_core::services::search_bridge::engine_from_config(&config.raw_json);
    let state = AppState {
        config,
        pool,
        rate_limiter: std::sync::Arc::new(ratelimit::RateLimiter::new()),
        search_engine,
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

/// fed-min: both inbox POST rows in one handler — upstream
/// ActivityPubServerService.ts fastify.post '/inbox' + '/users/:user/inbox'
/// hand the body to InboxProcessorService (a queue job: signature verify in
/// `verify_http_signature` terms + ApInboxService.performOneActivity) and
/// answer 202. Unknown/unparseable bodies never become 4xx: the enqueue
/// already happened, the failure modes are worker-side job reasons.
/// fed-2: with a pool the route now runs the worker lane inline —
/// mk_ap::processor::verify_and_route (keyId → user_keypair, the RSA verify,
/// the Create{Note}/Follow/Like/Undo arms) — and STILL answers 202 on every
/// outcome: Err (remote keyId, unverifiable signature, DB error) is the
/// worker job reason upstream logs, never an HTTP status.
async fn inbox_post(
    State(state): State<AppState>,
    req: axum::extract::Request,
) -> (StatusCode, &'static str) {
    let method = req.method().as_str().to_string();
    // the request-target the signing string covered: path + query verbatim
    let path = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| req.uri().path().to_string());
    let headers: Vec<(String, String)> = req
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                v.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let body = axum::body::to_bytes(req.into_body(), 1024 * 1024).await;
    match (&state.pool, body) {
        (Some(pool), Ok(body)) => {
            match mk_ap::processor::verify_and_route(
                &method,
                &path,
                &headers,
                &body,
                &state.config.host(),
                pool,
            )
            .await
            {
                Ok(effect) => tracing::info!(?effect, "inbox activity routed"),
                Err(()) => tracing::info!("inbox activity not verifiable (202 anyway)"),
            }
        }
        // no-pool surface keeps the classify-only skeleton (parse + dispatch)
        (None, Ok(body)) => {
            let outcome = mk_ap::inbox::handle(&body);
            tracing::info!(kind = outcome.kind.as_str(), "inbox activity dispatched");
        }
        // oversized frame: DefaultBodyLimit rejects before this runs; a
        // transport-level body error still rides the 202 (nothing upstream
        // answers 4xx from this row — the queue took the job)
        (_, Err(err)) => tracing::warn!(%err, "inbox body read failed"),
    }
    (StatusCode::ACCEPTED, "inbox")
}
