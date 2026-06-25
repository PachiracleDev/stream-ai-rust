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
use crate::relay::body::{AgentType, RelayBody};
use crate::relay::interview_pipeline;
use crate::relay::messages::{
    build_upstream_messages, system_prompt_len_chars, validate_image_solver,
    validate_interview_messages,
};
use crate::streaming::log::StreamLogCtx;
use crate::streaming::BoxedStream;

fn new_stream_log(
    req_ts: String,
    agent: AgentType,
    interview_id: i64,
    user_id: String,
    ai_config: &crate::config::AiConfig,
    system_prompt: &str,
) -> Arc<StreamLogCtx> {
    let agent_cfg = ai_config.agent(agent);
    Arc::new(StreamLogCtx::new(
        req_ts,
        agent_cfg.max_tokens,
        system_prompt_len_chars(system_prompt),
        interview_id,
        user_id,
        agent_cfg.upstream,
        agent_cfg.model.clone(),
        agent.label().to_string(),
    ))
}

fn sse_response(
    stream: BoxedStream,
) -> (
    AppendHeaders<[(HeaderName, HeaderValue); 3]>,
    Sse<BoxedStream>,
) {
    let sse = Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)));
    (
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
        sse,
    )
}

async fn stream_image_solver(
    st: &AppState,
    body: RelayBody,
    interview_id: i64,
    user_id: String,
    req_ts: String,
) -> Result<
    (
        AppendHeaders<[(HeaderName, HeaderValue); 3]>,
        Sse<BoxedStream>,
    ),
    RelayError,
> {
    validate_image_solver(&body.messages).map_err(RelayError::BadRequest)?;

    let system_prompt = st
        .prompts
        .render(AgentType::ImageSolver, &body.values)
        .map_err(RelayError::BadRequest)?;

    let upstream_messages = build_upstream_messages(
        &system_prompt,
        body.messages,
        st.ai_config.max_history_messages,
    );

    let agent_cfg = st.ai_config.agent(AgentType::ImageSolver);
    tracing::info!(
        timestamp = %req_ts,
        interview_id,
        user_id = %user_id,
        agent_type = "image-solver",
        max_output_tokens = agent_cfg.max_tokens,
        system_prompt_len_chars = system_prompt_len_chars(&system_prompt),
        upstream = ?agent_cfg.upstream,
        model = %agent_cfg.model,
        "relay request"
    );

    let stream_log = new_stream_log(
        req_ts,
        AgentType::ImageSolver,
        interview_id,
        user_id,
        st.ai_config.as_ref(),
        &system_prompt,
    );

    let stream = providers::stream_agent(
        st.ai_config.as_ref(),
        AgentType::ImageSolver,
        upstream_messages,
        Some(stream_log),
        true,
    )
    .await
    .map_err(RelayError::AiProvider)?;

    Ok(sse_response(stream))
}

async fn stream_interview(
    st: &AppState,
    body: RelayBody,
    interview_id: i64,
    user_id: String,
    req_ts: String,
) -> Result<
    (
        AppendHeaders<[(HeaderName, HeaderValue); 3]>,
        Sse<BoxedStream>,
    ),
    RelayError,
> {
    validate_interview_messages(&body.messages).map_err(RelayError::BadRequest)?;

    let detector_system = st
        .prompts
        .render(AgentType::Detector, &body.values)
        .map_err(RelayError::BadRequest)?;
    let opener_system = st
        .prompts
        .render(AgentType::Opener, &body.values)
        .map_err(RelayError::BadRequest)?;
    let deepener_system = st
        .prompts
        .render(AgentType::Deepener, &body.values)
        .map_err(RelayError::BadRequest)?;

    let detector_cfg = st.ai_config.agent(AgentType::Detector);
    let opener_cfg = st.ai_config.agent(AgentType::Opener);
    let deepener_cfg = st.ai_config.agent(AgentType::Deepener);

    tracing::info!(
        timestamp = %req_ts,
        interview_id,
        user_id = %user_id,
        pipeline = "detector+opener+deepener",
        detector_model = %detector_cfg.model,
        detector_upstream = ?detector_cfg.upstream,
        opener_model = %opener_cfg.model,
        opener_upstream = ?opener_cfg.upstream,
        opener_max_tokens = opener_cfg.max_tokens,
        deepener_model = %deepener_cfg.model,
        deepener_upstream = ?deepener_cfg.upstream,
        deepener_max_tokens = deepener_cfg.max_tokens,
        "relay request"
    );

    let detector_log = new_stream_log(
        req_ts.clone(),
        AgentType::Detector,
        interview_id,
        user_id.clone(),
        st.ai_config.as_ref(),
        &detector_system,
    );
    let opener_log = new_stream_log(
        req_ts.clone(),
        AgentType::Opener,
        interview_id,
        user_id.clone(),
        st.ai_config.as_ref(),
        &opener_system,
    );
    let deepener_log = new_stream_log(
        req_ts,
        AgentType::Deepener,
        interview_id,
        user_id,
        st.ai_config.as_ref(),
        &deepener_system,
    );

    let stream = interview_pipeline::stream_opener_then_deepener(
        st.ai_config.clone(),
        st.prompts.clone(),
        body.values,
        body.messages,
        detector_log,
        opener_log,
        deepener_log,
    )
    .await
    .map_err(RelayError::AiProvider)?;

    Ok(sse_response(stream))
}

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

    let req_ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

    let response = if body.is_image_solver() {
        stream_image_solver(&st, body, interview_id, user_id, req_ts).await?
    } else {
        stream_interview(&st, body, interview_id, user_id, req_ts).await?
    };

    step(&mut perf, "upstream_ready");
    Ok(response)
}
