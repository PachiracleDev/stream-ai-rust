//! Parser SSE estilo OpenAI Chat Completions (OpenAI, DeepSeek, Groq).

use std::sync::Arc;

use async_stream::try_stream;
use axum::response::sse::Event;
use eventsource_stream::Eventsource;
use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::time::timeout;

use crate::config::{relay_first_chunk_deadline, upstream_attempt_count};
use crate::perf::{relay_perf, step};
use crate::streaming::log::StreamLogCtx;
use crate::streaming::BoxedStream;

#[derive(Debug, Deserialize, Serialize)]
struct OpenAiStreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct OpenAiStreamChoice {
    delta: OpenAiStreamDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamEvent {
    #[serde(default)]
    choices: Vec<OpenAiStreamChoice>,
}

enum SseDecision {
    Skip,
    YieldPayload(Event),
    StreamDoneSentinel,
}

fn process_raw_sse(
    raw_msg: &eventsource_stream::Event,
    log_ctx: Option<&Arc<StreamLogCtx>>,
) -> Result<SseDecision, String> {
    let data_trim = raw_msg.data.trim();
    if data_trim.is_empty() {
        return Ok(SseDecision::Skip);
    }
    if data_trim == "[DONE]" {
        return Ok(SseDecision::StreamDoneSentinel);
    }

    let Ok(event) = serde_json::from_str::<OpenAiStreamEvent>(data_trim) else {
        return Ok(SseDecision::Skip);
    };

    let fragments: Vec<&str> = event
        .choices
        .iter()
        .flat_map(|c| {
            [
                c.delta
                    .reasoning_content
                    .as_deref()
                    .filter(|s| !s.is_empty()),
                c.delta.content.as_deref().filter(|s| !s.is_empty()),
            ]
            .into_iter()
            .flatten()
        })
        .collect();

    if fragments.is_empty() {
        return Ok(SseDecision::Skip);
    }

    let body = serde_json::to_string(&fragments)
        .map_err(|e| format!("serialize stream chunk: {e}"))?;
    let ev_payload = Event::default().data(body.clone());
    if let Some(c) = log_ctx {
        c.on_sse_data_payload(&body);
    }
    Ok(SseDecision::YieldPayload(ev_payload))
}

async fn drain_first_nonempty<E, S>(
    raw_ess: &mut S,
    log_ctx: Option<&Arc<StreamLogCtx>>,
) -> Result<Vec<Event>, String>
where
    E: std::fmt::Display,
    S: futures::stream::Stream<Item = Result<eventsource_stream::Event, E>> + Unpin,
{
    loop {
        match StreamExt::next(raw_ess).await {
            None => {
                return Err(
                    "upstream cerró el stream SSE sin emitir contenido del modelo".into(),
                );
            }
            Some(Err(e)) => return Err(format!("Event parse error: {e}")),
            Some(Ok(raw)) => match process_raw_sse(&raw, log_ctx)? {
                SseDecision::Skip => {}
                SseDecision::YieldPayload(ev) => return Ok(vec![ev]),
                SseDecision::StreamDoneSentinel => {
                    return Err(
                        "upstream emitió fin de stream antes de contenido del modelo".into(),
                    );
                }
            },
        }
    }
}

async fn stream_one_attempt(
    deadline: std::time::Duration,
    provider_label: &'static str,
    client: reqwest::Client,
    url: String,
    api_key: String,
    req_body: serde_json::Value,
    log_ctx: Option<Arc<StreamLogCtx>>,
    attempt_no: u32,
) -> Result<BoxedStream, String> {
    let mut perf = relay_perf(format!("{provider_label}↑{attempt_no}"));
    match timeout(deadline, async move {
        step(&mut perf, "B_before_http_send");
        let log_ctx_follow = log_ctx.clone();

        let res = client
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .json(&req_body)
            .send()
            .await
            .map_err(|e| format!("{provider_label} request failed: {e}"))?;

        if !res.status().is_success() {
            let err_text = res.text().await.unwrap_or_else(|_| "unknown".into());
            return Err(format!("{provider_label} API error: {err_text}"));
        }

        if let Some(c) = &log_ctx {
            c.mark_upstream_ready();
        }

        let mut raw_ess = res.bytes_stream().eventsource();
        let prelude_events = drain_first_nonempty(&mut raw_ess, log_ctx.as_ref()).await?;

        let events_stream = try_stream! {
            for ev in prelude_events {
                yield ev;
            }
            while let Some(msg) = StreamExt::next(&mut raw_ess).await {
                match msg {
                    Ok(raw) => match process_raw_sse(&raw, log_ctx_follow.as_ref())? {
                        SseDecision::Skip => {}
                        SseDecision::YieldPayload(ev) => yield ev,
                        SseDecision::StreamDoneSentinel => {
                            yield Event::default().data("[DONE]");
                            break;
                        }
                    },
                    Err(e) => Err::<(), String>(format!("Event parse error: {e}"))?,
                }
            }
        };

        Ok::<BoxedStream, String>(Box::pin(events_stream))
    })
    .await
    {
        Ok(inner) => inner,
        Err(_) => {
            tracing::warn!(
                provider = provider_label,
                attempt = attempt_no,
                secs = deadline.as_secs_f64(),
                "timeout esperando primer fragmento del modelo"
            );
            Err(format!(
                "{provider_label}: timeout ({:.3}s) esperando primera salida del modelo",
                deadline.as_secs_f64()
            ))
        }
    }
}

pub async fn stream_chat_completions(
    provider_label: &'static str,
    url: &str,
    api_key: &str,
    req_body: serde_json::Value,
    log_ctx: Option<Arc<StreamLogCtx>>,
) -> Result<BoxedStream, String> {
    let deadline = relay_first_chunk_deadline();
    let attempts = upstream_attempt_count();
    let mut last_err = format!("{provider_label}: agotados reintentos primer fragmento");
    let url_owned = url.to_string();
    let key_owned = api_key.to_string();
    let client = reqwest::Client::new();

    for idx in 0..attempts {
        match stream_one_attempt(
            deadline,
            provider_label,
            client.clone(),
            url_owned.clone(),
            key_owned.clone(),
            req_body.clone(),
            log_ctx.clone(),
            idx + 1,
        )
        .await
        {
            Ok(stream) => return Ok(stream),
            Err(e) if idx + 1 < attempts => {
                tracing::warn!(
                    attempt = idx + 1,
                    attempts,
                    provider = provider_label,
                    error = %e,
                    "reintento upstream (primer fragmento)"
                );
                last_err = e;
                tokio::time::sleep(std::time::Duration::from_millis(75)).await;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_err)
}
