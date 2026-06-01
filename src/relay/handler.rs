//! Handler `POST /interviews/:id/ai/assistant-relay`.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::header::{self, HeaderName, HeaderValue};
use axum::http::HeaderMap;
use axum::response::sse::{KeepAlive, Sse};
use axum::response::{AppendHeaders, IntoResponse};
use axum::Json;

use crate::app::AppState;
use crate::auth::{bearer_token, decode_claims, validate_claims};
use crate::error::RelayError;
use crate::perf::{relay_perf, step};
use crate::providers;
use crate::relay::body::RelayBody;
use crate::relay::messages::{build_upstream_messages, system_prompt_len_chars, validate_request};
use crate::streaming::log::StreamLogCtx;

pub async fn assistant_relay(
    State(st): State<AppState>,
    Path((interview_id,)): Path<(i64,)>,
    headers: HeaderMap,
    Json(body): Json<RelayBody>,
) -> Result<impl IntoResponse, RelayError> {
    let mut perf = relay_perf("handler");
    step(&mut perf, "enter");

    let token = bearer_token(&headers)?;
    let claims = decode_claims(&token, &st.decoding_key)?;
    validate_claims(&claims, interview_id)?;
    step(&mut perf, "jwt_ok");

    let user_id = claims.sub.as_key_segment();
    let rate_key = format!("{user_id}:{}", claims.interview_id);
    match st.limiter.check_allowed(&rate_key).await {
        Ok(true) => {}
        Ok(false) => return Err(RelayError::Rate(st.rate_limit_max)),
        Err(e) => {
            tracing::error!(error = %e, user_id = %user_id, interview_id, "redis rate limit");
            return Err(RelayError::RateLimitBackend);
        }
    }
    step(&mut perf, "rate_limit_ok");

    validate_request(body.agent_type, &body.messages)
        .map_err(RelayError::BadRequest)?;

    let system_prompt = st
        .prompts
        .render(body.agent_type, &body.values)
        .map_err(RelayError::BadRequest)?;

    let upstream_messages = build_upstream_messages(
        &system_prompt,
        body.messages,
        st.ai_config.max_history_messages,
    );

    let agent_cfg = st.ai_config.agent(body.agent_type);
    let max_out = agent_cfg.max_tokens;
    let sys_len = system_prompt_len_chars(&system_prompt);
    let model = agent_cfg.model.clone();
    let upstream = agent_cfg.upstream;

    let req_ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let agent_label = body.agent_type.label().to_string();

    tracing::info!(
        timestamp = %req_ts,
        interview_id,
        user_id = %user_id,
        agent_type = %agent_label,
        max_output_tokens = max_out,
        system_prompt_len_chars = sys_len,
        upstream = ?upstream,
        model = %model,
        "relay request"
    );

    let stream_log = Arc::new(StreamLogCtx::new(
        req_ts,
        max_out,
        sys_len,
        interview_id,
        user_id.clone(),
        upstream,
        model.clone(),
        agent_label,
    ));

    let stream = providers::stream_agent(
        st.ai_config.as_ref(),
        body.agent_type,
        upstream_messages,
        Some(stream_log),
    )
    .await
    .map_err(RelayError::AiProvider)?;

    step(&mut perf, "upstream_ready");

    let sse = Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)));

    Ok((
        AppendHeaders([
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache, no-transform")),
            (header::CONNECTION, HeaderValue::from_static("keep-alive")),
            (
                HeaderName::from_static("x-accel-buffering"),
                HeaderValue::from_static("no"),
            ),
        ]),
        sse,
    ))
}
