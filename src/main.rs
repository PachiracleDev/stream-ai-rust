//! Proxy SSE para agentes de entrevista (opener, deepener, image-solver).
//!
//! Modelos por agente vía env (`MODEL_OPENER`, `MODEL_DEEPENER`, `MODEL_IMAGE_SOLVER`).
//! El proveedor (Groq / Claude / DeepSeek / OpenAI) se infiere del nombre del modelo.

mod app;
mod auth;
mod config;
mod error;
mod health;
mod perf;
mod providers;
mod rate_limit;
mod relay;
mod streaming;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use jsonwebtoken::DecodingKey;
use tokio::net::TcpListener;
use tokio::signal;
use tower::limit::ConcurrencyLimitLayer;
use tower::ServiceBuilder;
use tower_http::cors::CorsLayer;
use tracing::info;

use app::AppState;
use config::{env_u32, env_u64, load_dotenv_files, prompts_dir, AiConfig};
use rate_limit::{RateLimitBackend, RateLimiter};
use relay::expand::expand_response;
use relay::handler::assistant_relay;
use relay::prompts::PromptStore;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dotenv_loaded_from = load_dotenv_files();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("interview_relay_sim=info".parse().unwrap())
                .add_directive("relay_perf=info".parse().unwrap()),
        )
        .init();

    if let Some(ref p) = dotenv_loaded_from {
        info!(path = %p.display(), ".env cargado");
    }

    let secret = std::env::var("RELAY_JWT_SECRET").expect("RELAY_JWT_SECRET requerida");
    let rate_limit_backend = RateLimitBackend::from_env();
    let rate_limit_max = env_u32("RATE_LIMIT_MAX", 10);
    let rate_window_secs = env_u64("RATE_LIMIT_WINDOW_SECS", 60);
    if rate_limit_backend != RateLimitBackend::Disabled
        && (rate_limit_max == 0 || rate_window_secs == 0)
    {
        panic!("RATE_LIMIT_MAX y RATE_LIMIT_WINDOW_SECS deben ser > 0");
    }

    let prompts = PromptStore::load(&prompts_dir()).expect("cargar prompts markdown");
    let ai_config = AiConfig::from_env();

    info!(
        detector = %ai_config.detector.model,
        detector_upstream = ?ai_config.detector.upstream,
        detector_max_tokens = ai_config.detector.max_tokens,
        opener = %ai_config.opener.model,
        opener_upstream = ?ai_config.opener.upstream,
        opener_max_tokens = ai_config.opener.max_tokens,
        deepener = %ai_config.deepener.model,
        deepener_upstream = ?ai_config.deepener.upstream,
        deepener_max_tokens = ai_config.deepener.max_tokens,
        image_solver = %ai_config.image_solver.model,
        image_solver_upstream = ?ai_config.image_solver.upstream,
        image_solver_max_tokens = ai_config.image_solver.max_tokens,
        prompts_dir = %prompts_dir().display(),
        "config loaded"
    );

    let limiter = RateLimiter::from_env(rate_limit_max, rate_window_secs).await?;
    let expand_limiter = Arc::new(RateLimiter::memory_only(1, 60));

    info!(
        rate_limit_backend = rate_limit_backend.label(),
        rate_limit_max,
        rate_window_secs,
        expand_rate_limit = "memory 1/min per user",
        "rate limit configured"
    );

    let state = AppState {
        decoding_key: DecodingKey::from_secret(secret.as_bytes()),
        limiter: Arc::new(limiter),
        expand_limiter,
        rate_limit_max,
        ai_config: Arc::new(ai_config),
        prompts: Arc::new(prompts),
    };

    let app = Router::new()
        .route("/health", get(health::health))
        .route(
            "/interviews/:id/ai/assistant-relay",
            post(assistant_relay),
        )
        .route(
            "/interviews/:id/ai/expand-response",
            post(expand_response),
        )
        .layer(
            ServiceBuilder::new()
                .layer(ConcurrencyLimitLayer::new(500))
                .layer(CorsLayer::permissive()),
        )
        .with_state(state);

    let port: u16 = std::env::var("PORT")
        .or_else(|_| std::env::var("LISTEN"))
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3001);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;

    info!(
        "listening GET http://{addr}/health | POST http://{addr}/interviews/:id/ai/assistant-relay | POST http://{addr}/interviews/:id/ai/expand-response"
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = signal::ctrl_c().await;
            info!("shutdown");
        })
        .await?;

    Ok(())
}
