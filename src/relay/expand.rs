//! Handler `POST /interviews/:id/ai/expand-response`.

use std::sync::Arc;
use std::time::Duration;

use async_stream::try_stream;
use axum::extract::{Path, State};
use axum::http::header::{self, HeaderName, HeaderValue};
use axum::http::HeaderMap;
use axum::response::sse::{KeepAlive, Sse};
use axum::response::{AppendHeaders, IntoResponse};
use axum::Json;
use futures::StreamExt;

use crate::app::AppState;
use crate::auth::{bearer_token, decode_claims, validate_claims};
use crate::error::RelayError;
use crate::perf::{relay_perf, step};
use crate::providers;
use crate::relay::body::{AgentType, ExpandResponseBody, RelayMessage};
use crate::relay::messages::{build_upstream_messages, system_prompt_len_chars};
use crate::streaming::log::StreamLogCtx;
use crate::streaming::{stream_deepener_finish_events, BoxedStream};

fn validate_expand_body(body: &ExpandResponseBody) -> Result<(), String> {
    if body.question.trim().is_empty() {
        return Err("question es obligatorio".into());
    }
    if body.response.trim().is_empty() {
        return Err("response es obligatorio".into());
    }
    Ok(())
}

fn build_expand_messages(question: &str, response: &str) -> Vec<RelayMessage> {
    vec![
        RelayMessage {
            role: "user".into(),
            content: Some(format!("PREGUNTA: {}", question.trim())),
            image_url: None,
        },
        RelayMessage {
            role: "assistant".into(),
            content: Some(response.trim().to_string()),
            image_url: None,
        },
        RelayMessage {
            role: "user".into(),
            content: Some("[continúa]".into()),
            image_url: None,
        },
    ]
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

pub async fn expand_response(
    State(st): State<AppState>,
    Path((interview_id,)): Path<(i64,)>,
    headers: HeaderMap,
    Json(body): Json<ExpandResponseBody>,
) -> Result<impl IntoResponse, RelayError> {
    let mut perf = relay_perf("expand_handler");
    step(&mut perf, "enter");

    let token = bearer_token(&headers)?;
    let claims = decode_claims(&token, &st.decoding_key)?;
    validate_claims(&claims, interview_id)?;
    step(&mut perf, "jwt_ok");

    validate_expand_body(&body).map_err(RelayError::BadRequest)?;

    let user_id = claims.sub.as_key_segment();
    let expand_key = format!("{user_id}:expand");
    if !st.expand_limiter.check_allowed(&expand_key).await.unwrap_or(false) {
        return Err(RelayError::ExpandRate);
    }
    step(&mut perf, "expand_rate_limit_ok");

    let system_prompt = st
        .prompts
        .render(AgentType::Deepener, &body.values)
        .map_err(RelayError::BadRequest)?;

    let upstream_messages = build_upstream_messages(
        &system_prompt,
        build_expand_messages(&body.question, &body.response),
        4,
    );

    let agent_cfg = st.ai_config.agent(AgentType::Deepener);
    let req_ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

    tracing::info!(
        timestamp = %req_ts,
        interview_id,
        user_id = %user_id,
        agent_type = "expand-response",
        max_output_tokens = agent_cfg.max_tokens,
        system_prompt_len_chars = system_prompt_len_chars(&system_prompt),
        upstream = ?agent_cfg.upstream,
        model = %agent_cfg.model,
        "expand response request"
    );

    let stream_log = Arc::new(StreamLogCtx::new(
        req_ts,
        agent_cfg.max_tokens,
        system_prompt_len_chars(&system_prompt),
        interview_id,
        user_id,
        agent_cfg.upstream,
        agent_cfg.model.clone(),
        "expand-response".to_string(),
    ));

    let mut inner = providers::stream_agent(
        st.ai_config.as_ref(),
        AgentType::Deepener,
        upstream_messages,
        Some(stream_log.clone()),
        false,
    )
    .await
    .map_err(RelayError::AiProvider)?;

    let stream = try_stream! {
        while let Some(item) = inner.next().await {
            yield item?;
        }
        for ev in stream_deepener_finish_events(&stream_log) {
            yield ev;
        }
    };

    step(&mut perf, "upstream_ready");
    Ok(sse_response(Box::pin(stream)))
}
