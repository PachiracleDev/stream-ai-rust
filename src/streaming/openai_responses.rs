//! OpenAI / Azure **Responses API** (`/responses`): `input` + streaming por eventos.

use std::sync::Arc;

use async_stream::try_stream;
use axum::response::sse::Event;
use eventsource_stream::Eventsource;
use futures::stream::StreamExt;
use serde_json::{json, Value};
use tokio::time::timeout;

use crate::config::{relay_first_chunk_deadline, upstream_attempt_count};
use crate::perf::{relay_perf, step};
use crate::streaming::log::StreamLogCtx;
use crate::streaming::BoxedStream;

enum SseDecision {
    Skip,
    YieldPayload(Event),
    StreamDoneSentinel,
}

/// Convierte mensajes estilo Chat Completions a cuerpo Responses API.
pub fn chat_messages_to_responses_body(
    model: &str,
    messages: Vec<Value>,
    temperature: f64,
    max_output_tokens: u32,
) -> Result<Value, String> {
    let mut instructions: Option<String> = None;
    let mut input_items: Vec<Value> = Vec::new();

    for msg in messages {
        let role = msg
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("user");
        let content = msg.get("content").cloned().unwrap_or(Value::Null);

        if role == "system" {
            let text = message_content_as_string(&content)?;
            if !text.is_empty() {
                instructions = Some(text);
            }
            continue;
        }

        if role == "assistant" {
            let text = message_content_as_string(&content)?;
            if !text.is_empty() {
                input_items.push(json!({ "role": "assistant", "content": text }));
            }
            continue;
        }

        let parts = convert_user_content(&content)?;
        if !parts.is_empty() {
            input_items.push(json!({ "role": "user", "content": parts }));
        }
    }

    if input_items.is_empty() {
        return Err("Responses API: input vacío tras convertir mensajes".into());
    }

    let mut body = json!({
        "model": model,
        "input": input_items,
        "stream": true,
        "temperature": temperature,
        "max_output_tokens": max_output_tokens,
    });
    if let Some(inst) = instructions {
        body["instructions"] = json!(inst);
    }
    Ok(body)
}

fn message_content_as_string(content: &Value) -> Result<String, String> {
    match content {
        Value::String(s) => Ok(s.clone()),
        Value::Null => Ok(String::new()),
        Value::Array(parts) => {
            let mut s = String::new();
            for p in parts {
                let Some(o) = p.as_object() else { continue };
                match o.get("type").and_then(|t| t.as_str()) {
                    Some("text") | Some("input_text") | Some("output_text") => {
                        if let Some(t) = o.get("text").and_then(|x| x.as_str()) {
                            s.push_str(t);
                        }
                    }
                    _ => {}
                }
            }
            Ok(s)
        }
        _ => Err("content: se esperaba string, null o lista de bloques".into()),
    }
}

fn convert_user_content(content: &Value) -> Result<Vec<Value>, String> {
    match content {
        Value::String(s) if !s.trim().is_empty() => {
            Ok(vec![json!({ "type": "input_text", "text": s })])
        }
        Value::String(_) => Ok(vec![]),
        Value::Array(parts) => {
            let mut out = Vec::new();
            for p in parts {
                let o = p
                    .as_object()
                    .ok_or_else(|| "bloque de content inválido".to_string())?;
                match o.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(t) = o.get("text").and_then(|x| x.as_str()) {
                            if !t.is_empty() {
                                out.push(json!({ "type": "input_text", "text": t }));
                            }
                        }
                    }
                    Some("image_url") => {
                        let url = o
                            .get("image_url")
                            .and_then(|iu| {
                                iu.as_str()
                                    .map(str::to_string)
                                    .or_else(|| iu.get("url").and_then(|u| u.as_str()).map(str::to_string))
                            })
                            .ok_or_else(|| "image_url sin url".to_string())?;
                        out.push(json!({ "type": "input_image", "image_url": url }));
                    }
                    Some(other) => {
                        return Err(format!("bloque no soportado para Responses API: {other}"));
                    }
                    None => {}
                }
            }
            Ok(out)
        }
        Value::Null => Ok(vec![]),
        _ => Err("content inválido para mensaje user".into()),
    }
}

fn text_delta_from_responses_event(data: &Value) -> Option<&str> {
    let event_type = data.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match event_type {
        "response.output_text.delta" | "response.refusal.delta" => data
            .get("delta")
            .and_then(|d| d.as_str())
            .filter(|s| !s.is_empty()),
        _ => None,
    }
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

    let Ok(event) = serde_json::from_str::<Value>(data_trim) else {
        return Ok(SseDecision::Skip);
    };

    if let Some(event_type) = event.get("type").and_then(|t| t.as_str()) {
        if matches!(
            event_type,
            "response.completed" | "response.incomplete" | "response.failed"
        ) {
            return Ok(SseDecision::StreamDoneSentinel);
        }
    }

    let Some(fragment) = text_delta_from_responses_event(&event) else {
        return Ok(SseDecision::Skip);
    };

    let body = serde_json::to_string(&vec![fragment])
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
    client: reqwest::Client,
    url: String,
    api_key: String,
    req_body: Value,
    log_ctx: Option<Arc<StreamLogCtx>>,
    attempt_no: u32,
) -> Result<BoxedStream, String> {
    let mut perf = relay_perf(format!("OpenAI-Responses↑{attempt_no}"));
    match timeout(deadline, async move {
        step(&mut perf, "B_before_http_send");
        let log_ctx_follow = log_ctx.clone();

        let res = client
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .json(&req_body)
            .send()
            .await
            .map_err(|e| format!("OpenAI Responses request failed: {e}"))?;

        if !res.status().is_success() {
            let err_text = res.text().await.unwrap_or_else(|_| "unknown".into());
            return Err(format!("OpenAI API error: {err_text}"));
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
                provider = "OpenAI-Responses",
                attempt = attempt_no,
                secs = deadline.as_secs_f64(),
                "timeout esperando primer fragmento del modelo"
            );
            Err(format!(
                "OpenAI Responses: timeout ({:.3}s) esperando primera salida del modelo",
                deadline.as_secs_f64()
            ))
        }
    }
}

pub async fn stream_responses(
    url: &str,
    api_key: &str,
    req_body: Value,
    log_ctx: Option<Arc<StreamLogCtx>>,
) -> Result<BoxedStream, String> {
    let deadline = relay_first_chunk_deadline();
    let attempts = upstream_attempt_count();
    let mut last_err = "OpenAI Responses: agotados reintentos primer fragmento".to_string();
    let url_owned = url.to_string();
    let key_owned = api_key.to_string();
    let client = reqwest::Client::new();

    for idx in 0..attempts {
        match stream_one_attempt(
            deadline,
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
                    provider = "OpenAI-Responses",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_vision_message_to_responses_input() {
        let messages = vec![
            json!({ "role": "system", "content": "Eres un asistente." }),
            json!({
                "role": "user",
                "content": [
                    { "type": "text", "text": "Qué es eso" },
                    { "type": "image_url", "image_url": { "url": "https://example.com/a.png" } }
                ]
            }),
        ];
        let body = chat_messages_to_responses_body("gpt-5.4-nano", messages, 0.6, 4000).unwrap();
        assert_eq!(body["instructions"], "Eres un asistente.");
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(body["input"][0]["content"][1]["type"], "input_image");
        assert_eq!(
            body["input"][0]["content"][1]["image_url"],
            "https://example.com/a.png"
        );
        assert_eq!(body["max_output_tokens"], 4000);
    }
}
