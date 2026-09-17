//! `POST /interviews/:id/ai/translation-relay`: traducción Cerebras vía SSE.

use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::header::{self, HeaderName, HeaderValue};
use axum::http::HeaderMap;
use axum::response::sse::{KeepAlive, Sse};
use axum::response::AppendHeaders;
use axum::Json;

use crate::app::AppState;
use crate::auth::{bearer_token, decode_claims, validate_claims};
use crate::cerebras;
use crate::error::RelayError;
use crate::relay::body::TranslationBody;
use crate::streaming::BoxedStream;

pub async fn translation_relay(
    State(st): State<AppState>,
    Path((interview_id,)): Path<(i64,)>,
    headers: HeaderMap,
    Json(body): Json<TranslationBody>,
) -> Result<
    (
        AppendHeaders<[(HeaderName, HeaderValue); 3]>,
        Sse<BoxedStream>,
    ),
    RelayError,
> {
    let user_id = if st.skip_jwt {
        "dev".to_string()
    } else {
        let token = bearer_token(&headers)?;
        let claims = decode_claims(&token, &st.decoding_key)?;
        validate_claims(&claims, interview_id)?;
        claims.sub.as_key_segment()
    };

    let text = body.text.trim();
    if text.is_empty() || text.chars().count() > 4000 {
        return Err(RelayError::BadRequest(
            "text debe contener entre 1 y 4000 caracteres".into(),
        ));
    }

    let rate_key = format!("translation:{user_id}:{interview_id}");
    match st.translation_limiter.check_allowed(&rate_key).await {
        Ok(true) => {}
        Ok(false) => return Err(RelayError::Rate(st.translation_rate_limit_max)),
        Err(_) => return Err(RelayError::RateLimitBackend),
    }

    let config = &st
        .cerebras
        .as_ref()
        .ok_or_else(|| RelayError::AiProvider("CEREBRAS_API_KEY no configurada".into()))?
        .translation;

    tracing::info!(
        interview_id,
        user_id = %user_id,
        model = %config.model,
        target_language = %body.target_language,
        text_len = text.len(),
        "translation-relay request"
    );

    let stream = cerebras::stream_translation(
        config,
        text,
        &body.target_language,
        body.source_language.as_deref(),
    )
    .await
    .map_err(RelayError::AiProvider)?;

    Ok((
        AppendHeaders([
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-cache, no-transform"),
            ),
            (header::CONNECTION, HeaderValue::from_static("keep-alive")),
            (
                HeaderName::from_static("x-accel-buffering"),
                HeaderValue::from_static("no"),
            ),
        ]),
        Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(10))),
    ))
}
