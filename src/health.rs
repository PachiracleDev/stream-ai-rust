use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

use crate::app::AppState;
use crate::rate_limit::RateLimitBackend;

pub async fn health(State(st): State<AppState>) -> impl IntoResponse {
    let backend = st.limiter.backend();
    let mut body = json!({
        "status": "ok",
        "rate_limit": {
            "backend": backend.label(),
            "max": st.rate_limit_max,
        },
    });

    match backend {
        RateLimitBackend::Redis => match st.limiter.ping().await {
            Ok(()) => {
                body["redis"] = json!("ok");
            }
            Err(e) => {
                body["status"] = json!("degraded");
                body["redis"] = json!("unavailable");
                body["error"] = json!(e.to_string());
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(body),
                );
            }
        },
        RateLimitBackend::Memory => {
            body["redis"] = json!("not_used");
        }
        RateLimitBackend::Disabled => {
            body["redis"] = json!("not_used");
        }
    }

    (StatusCode::OK, Json(body))
}
