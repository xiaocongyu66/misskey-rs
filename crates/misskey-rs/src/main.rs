mod api;
mod config;
mod error;
mod ratelimit;

use std::net::SocketAddr;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::routing::{any, get};
use axum::Router;
use tower_http::cors::CorsLayer;

use crate::config::Config;

#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub pool: Option<mk_db::sqlx::PgPool>,
    pub rate_limiter: ratelimit::RateLimiter,
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
        rate_limiter: ratelimit::RateLimiter::new(),
    };

    let cors = CorsLayer::permissive();

    let app = Router::new()
        .route("/api/{*name}", any(api::dispatch))
        .route("/healthz", get(healthz))
        .layer(cors)
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("serving at {} (listening on {})", url, addr);
    axum::serve(listener, app).await?;
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
