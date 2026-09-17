//! Respuesta de entrevista vía Cerebras Chat Completions en SSE (sin detectar pregunta).

use std::time::Instant;

use async_stream::try_stream;
use axum::response::sse::Event;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::cerebras::shared_client;
use crate::config::CerebrasConfig;
use crate::streaming::{text_chunk_event, BoxedStream};

#[derive(Debug, Default, Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Debug, Default, Deserialize)]
struct StreamUsage {
    #[serde(default)]
    completion_tokens: Option<u32>,
    #[serde(default)]
    total_tokens: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<StreamUsage>,
}

pub async fn stream_answer(
    config: &CerebrasConfig,
    system_prompt: &str,
    messages: Vec<Value>,
) -> Result<BoxedStream, String> {
    let mut chat_messages = vec![json!({ "role": "system", "content": system_prompt })];
    chat_messages.extend(messages);

    let body = json!({
        "model": config.model,
        "stream": true,
        "max_tokens": config.max_tokens,
        "temperature": config.temperature,
        "top_p": config.top_p,
        "reasoning_effort": config.reasoning_effort,
        "messages": chat_messages,
    });

    let response = shared_client()
        .post(&config.chat_url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Cerebras request failed: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        return Err(format!("Cerebras HTTP {status}: {detail}"));
    }

    let mut upstream = response.bytes_stream().eventsource();

    let output = try_stream! {
        let started_at = Instant::now();
        let mut completion_tokens: Option<u32> = None;
        let mut total_tokens: Option<u32> = None;
        let mut output_chars: usize = 0;

        while let Some(message) = upstream.next().await {
            let message = message.map_err(|e| format!("Cerebras SSE inválido: {e}"))?;
            let raw = message.data.trim();
            if raw.is_empty() || raw == "[DONE]" {
                continue;
            }

            let chunk: StreamChunk = match serde_json::from_str(raw) {
                Ok(chunk) => chunk,
                Err(_) => continue,
            };
            if let Some(usage) = chunk.usage {
                completion_tokens = usage.completion_tokens.or(completion_tokens);
                total_tokens = usage.total_tokens.or(total_tokens);
            }

            for token in chunk
                .choices
                .into_iter()
                .filter_map(|choice| choice.delta.content)
                .filter(|token| !token.is_empty())
            {
                output_chars += token.chars().count();
                yield text_chunk_event(&token);
            }
        }

        let estimated = ((output_chars as f64) / 4.0).ceil() as u32;
        let answer_tokens = completion_tokens.unwrap_or(estimated);
        let total = total_tokens.unwrap_or(answer_tokens);
        yield Event::default().event("metadata").data(json!({
            "openerTokens": 0,
            "deepenerTokens": answer_tokens,
            "totalTokens": total,
            "elapsedMs": started_at.elapsed().as_millis(),
        }).to_string());
        yield Event::default().data("[DONE]");
    };

    Ok(Box::pin(output))
}
