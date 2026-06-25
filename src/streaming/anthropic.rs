//! Streaming SSE de Anthropic Messages API.

use std::sync::Arc;

use async_stream::try_stream;
use axum::http::header;
use axum::response::sse::Event;
use eventsource_stream::Eventsource;
use futures::stream::StreamExt;
use serde_json::{json, Value};
use tokio::time::timeout;

use crate::config::{relay_first_chunk_deadline, upstream_attempt_count};
use crate::perf::{relay_perf, step};
use crate::streaming::anthropic_convert::openai_style_to_anthropic;
use crate::streaming::log::StreamLogCtx;
use crate::streaming::{finish_events, BoxedStream};

enum SseDecision {
    Skip,
    Yield(Event),
    MessageStop,
}

fn handle_payload(data: &str, log_ctx: Option<&Arc<StreamLogCtx>>) -> Result<SseDecision, String> {
    let d = data.trim();
    if d.is_empty() {
        return Ok(SseDecision::Skip);
    }
    let v: Value = match serde_json::from_str(d) {
        Ok(v) => v,
        Err(_) => return Ok(SseDecision::Skip),
    };
    let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match ty {
        "message_start" => {
            if let Some(c) = log_ctx {
                if let Some(input) = v
                    .pointer("/message/usage/input_tokens")
                    .and_then(|n| n.as_u64())
                {
                    c.token_usage.record_input(input as u32);
                }
            }
            Ok(SseDecision::Skip)
        }
        "message_delta" => {
            if let Some(c) = log_ctx {
                if let Some(output) = v
                    .pointer("/usage/output_tokens")
                    .and_then(|n| n.as_u64())
                {
                    c.token_usage.record_output(output as u32);
                }
            }
            Ok(SseDecision::Skip)
        }
        "content_block_delta" => {
            let Some(delta) = v.get("delta").and_then(|dv| dv.as_object()) else {
                return Ok(SseDecision::Skip);
            };
            if delta.get("type").and_then(|t| t.as_str()) == Some("text_delta") {
                if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                    if !text.is_empty() {
                        let body = serde_json::to_string(&vec![text])
                            .map_err(|e| format!("serialize stream chunk: {e}"))?;
                        let ev = Event::default().data(body.clone());
                        if let Some(c) = log_ctx {
                            c.on_sse_data_payload(&body);
                        }
                        return Ok(SseDecision::Yield(ev));
                    }
                }
            }
            Ok(SseDecision::Skip)
        }
        "message_stop" => Ok(SseDecision::MessageStop),
        "error" => {
            let detail = v
                .pointer("/error/message")
                .and_then(|m| m.as_str())
                .map(String::from)
                .unwrap_or_else(|| v.to_string());
            Err(format!("Anthropic stream error: {detail}"))
        }
        _ => Ok(SseDecision::Skip),
    }
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
            None => return Err("Anthropic SSE terminó antes de contenido del modelo".into()),
            Some(Err(e)) => return Err(format!("Anthropic SSE parse error: {e}")),
            Some(Ok(msg)) => match handle_payload(&msg.data, log_ctx)? {
                SseDecision::Skip => {}
                SseDecision::Yield(ev) => return Ok(vec![ev]),
                SseDecision::MessageStop => {
                    return Err("Anthropic message_stop antes de contenido".into());
                }
            },
        }
    }
}

async fn stream_one_attempt(
    deadline: std::time::Duration,
    client: reqwest::Client,
    api_key: String,
    req_body: Value,
    log_ctx: Option<Arc<StreamLogCtx>>,
    attempt_no: u32,
    emit_finish: bool,
) -> Result<BoxedStream, String> {
    let mut perf = relay_perf(format!("Anthropic↑{attempt_no}"));
    match timeout(deadline, async move {
        step(&mut perf, "B_before_http_send");
        let log_ctx_follow = log_ctx.clone();

        let res = client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header(header::CONTENT_TYPE, "application/json")
            .json(&req_body)
            .send()
            .await
            .map_err(|e| format!("Anthropic request failed: {e}"))?;

        if !res.status().is_success() {
            let err_text = res.text().await.unwrap_or_else(|_| "unknown".into());
            return Err(format!("Anthropic API error: {err_text}"));
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
            let mut done = false;
            while let Some(msg) = StreamExt::next(&mut raw_ess).await {
                match msg {
                    Ok(raw) => match handle_payload(&raw.data, log_ctx_follow.as_ref())? {
                        SseDecision::Skip => {}
                        SseDecision::Yield(ev) => yield ev,
                        SseDecision::MessageStop => {
                            for ev in finish_events(log_ctx_follow.as_ref(), emit_finish) {
                                yield ev;
                            }
                            done = true;
                            break;
                        }
                    },
                    Err(e) => Err::<(), String>(format!("Anthropic SSE parse error: {e}"))?,
                }
            }
            if !done {
                for ev in finish_events(log_ctx_follow.as_ref(), emit_finish) {
                    yield ev;
                }
            }
        };

        Ok::<BoxedStream, String>(Box::pin(events_stream))
    })
    .await
    {
        Ok(inner) => inner,
        Err(_) => Err(format!(
            "Anthropic: timeout ({:.3}s) esperando primera salida del modelo",
            deadline.as_secs_f64()
        )),
    }
}

pub async fn stream_messages(
    model: &str,
    messages: Vec<Value>,
    max_output_tokens: u32,
    log_ctx: Option<Arc<StreamLogCtx>>,
    emit_finish: bool,
) -> Result<BoxedStream, String> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| "ANTHROPIC_API_KEY no configurada".to_string())?;

    let (system, anth_messages) = openai_style_to_anthropic(messages)?;
    let mut req_body = json!({
        "model": model,
        "max_tokens": max_output_tokens,
        "messages": anth_messages,
        "stream": true,
    });
    if let Some(s) = system.filter(|s| !s.is_empty()) {
        req_body["system"] = json!(s);
    }

    let deadline = relay_first_chunk_deadline();
    let attempts = upstream_attempt_count();
    let mut last_err = "Anthropic: agotados reintentos".to_string();
    let client = reqwest::Client::new();

    for idx in 0..attempts {
        match stream_one_attempt(
            deadline,
            client.clone(),
            api_key.clone(),
            req_body.clone(),
            log_ctx.clone(),
            idx + 1,
            emit_finish,
        )
        .await
        {
            Ok(stream) => return Ok(stream),
            Err(e) if idx + 1 < attempts => {
                tracing::warn!(attempt = idx + 1, error = %e, "reintento Anthropic");
                last_err = e;
                tokio::time::sleep(std::time::Duration::from_millis(75)).await;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_err)
}
