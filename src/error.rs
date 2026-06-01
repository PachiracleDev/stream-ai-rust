use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RelayError {
    #[error("Authorization Bearer faltante o inválido")]
    Auth,
    #[error("Token inválido: {0}")]
    Token(#[from] jsonwebtoken::errors::Error),
    #[error("entrevista del token no coincide con la ruta")]
    IdMismatch,
    #[error("vida del token (exp-iat) supera 5 minutos")]
    TtlTooLong,
    #[error("rate limit: máximo {0} peticiones / ventana por usuario y entrevista")]
    Rate(u32),
    #[error("servicio de rate limit no disponible")]
    RateLimitBackend,
    #[error("AI provider error: {0}")]
    AiProvider(String),
    #[error("solicitud inválida: {0}")]
    BadRequest(String),
}

impl IntoResponse for RelayError {
    fn into_response(self) -> Response {
        use RelayError::*;

        let (status, msg) = match &self {
            Auth => (StatusCode::UNAUTHORIZED, self.to_string()),
            Token(e) => (StatusCode::UNAUTHORIZED, e.to_string()),
            IdMismatch => (StatusCode::FORBIDDEN, self.to_string()),
            BadRequest(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            TtlTooLong => (StatusCode::UNAUTHORIZED, self.to_string()),
            Rate(n) => {
                tracing::warn!(limit = n, "rate limit por usuario y entrevista");
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    format!("Máximo {n} peticiones por ventana (usuario + entrevista)"),
                )
            }
            RateLimitBackend => (StatusCode::SERVICE_UNAVAILABLE, self.to_string()),
            AiProvider(e) => {
                tracing::error!(error = %e, "AI provider failed");
                (StatusCode::BAD_GATEWAY, e.clone())
            }
        };

        (
            status,
            Json(serde_json::json!({
                "message": msg,
                "error": self.to_string(),
            })),
        )
            .into_response()
    }
}
