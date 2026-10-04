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
use crate::relay::interview_cerebras;
use crate::relay::messages::{
    build_upstream_messages, messages_have_image, system_prompt_len_chars, validate_image_solver,
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

    let transcript = body
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .and_then(|m| m.content.as_deref())
        .unwrap_or("");
    let system_prompt = st
        .prompts
        .render_with_transcript(AgentType::ImageSolver, &body.values, transcript)
        .map_err(RelayError::BadRequest)?;

    if let Some(configs) = st.cerebras.as_ref() {
        tracing::info!(
            timestamp = %req_ts,
            interview_id,
            user_id = %user_id,
            agent_type = "image-solver",
            upstream = "Cerebras",
            model = %configs.image.model,
            max_output_tokens = configs.image.max_tokens,
            system_prompt_len_chars = system_prompt_len_chars(&system_prompt),
            "relay request"
        );

        let stream =
            crate::cerebras::stream_image_solver(&configs.image, &system_prompt, body.messages)
                .await
                .map_err(RelayError::AiProvider)?;

        return Ok(sse_response(stream));
    }

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

pub async fn assistant_relay(
    State(st): State<AppState>,
    Path((interview_id,)): Path<(i64,)>,
    headers: HeaderMap,
    Json(body): Json<RelayBody>,
) -> Result<impl IntoResponse, RelayError> {
    let mut perf = relay_perf("handler");
    step(&mut perf, "enter");

    let user_id = if st.skip_jwt {
        "dev".to_string()
    } else {
        let token = bearer_token(&headers)?;
        let claims = decode_claims(&token, &st.decoding_key)?;
        validate_claims(&claims, interview_id)?;
        claims.sub.as_key_segment()
    };
    step(&mut perf, "jwt_ok");

    let rate_key = format!("{user_id}:{interview_id}");
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

    if messages_have_image(&body.messages) {
        step(&mut perf, "upstream_ready");
        return Ok(
            stream_image_solver(&st, body, interview_id, user_id, req_ts)
                .await?
                .into_response(),
        );
    }

    let stream =
        interview_cerebras::stream_cerebras_interview(&st, body, interview_id, &user_id).await?;
    step(&mut perf, "cerebras_ok");
    Ok(sse_response(stream).into_response())
}
