//! Respuesta de entrevista vía Cerebras Chat Completions en SSE (sin detectar pregunta).

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_stream::try_stream;
use futures::stream::Stream;
use axum::response::sse::Event;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::cerebras::shared_client;
use crate::config::CerebrasConfig;
use crate::relay::answer_filter::{first_sentence, opening_rejected, strip_markup};
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

#[allow(dead_code)]
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

struct AnswerFinish {
    text: Arc<Mutex<String>>,
    regenerated: Arc<AtomicBool>,
    saved: Arc<AtomicBool>,
    on_finish: Arc<dyn Fn(String, bool, bool) + Send + Sync>,
}

impl Drop for AnswerFinish {
    fn drop(&mut self) {
        if self.saved.swap(true, Ordering::SeqCst) {
            return;
        }
        let text = self
            .text
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        (self.on_finish)(text, true, self.regenerated.load(Ordering::SeqCst));
    }
}

async fn open_token_stream(
    config: &CerebrasConfig,
    system_prompt: &str,
    messages: &[Value],
) -> Result<Pin<Box<dyn Stream<Item = Result<String, String>> + Send>>, String> {
    let mut chat_messages = vec![json!({ "role": "system", "content": system_prompt })];
    chat_messages.extend(messages.iter().cloned());
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
            for token in chunk
                .choices
                .into_iter()
                .filter_map(|choice| choice.delta.content)
                .filter(|token| !token.is_empty())
            {
                yield token;
            }
        }
    };
    Ok(Box::pin(output))
}

/// Respuesta hablada: retiene la primera frase y, si no sirve, regenera una vez.
pub async fn stream_spoken_answer(
    config: &CerebrasConfig,
    system_prompt: &str,
    messages: Vec<Value>,
    on_finish: Arc<dyn Fn(String, bool, bool) + Send + Sync>,
) -> Result<BoxedStream, String> {
    let config = config.clone();
    let system_prompt = system_prompt.to_string();
    let text = Arc::new(Mutex::new(String::new()));
    let regenerated = Arc::new(AtomicBool::new(false));
    let saved = Arc::new(AtomicBool::new(false));
    let finish = AnswerFinish {
        text: text.clone(),
        regenerated: regenerated.clone(),
        saved: saved.clone(),
        on_finish: on_finish.clone(),
    };

    let output = try_stream! {
        let _finish = finish;
        let mut attempt = 0u8;
        loop {
            attempt += 1;
            let mut upstream = open_token_stream(&config, &system_prompt, &messages).await?;
            let mut buffer = String::new();
            let mut streaming_rest = false;
            let mut rejected = false;
            while let Some(token) = upstream.next().await {
                let token = token?;
                if !streaming_rest {
                    buffer.push_str(&token);
                    if let Some((end, sentence)) = first_sentence(&buffer) {
                        if attempt == 1 && opening_rejected(&sentence) {
                            rejected = true;
                            break;
                        }
                        if let Some(event) = take_clean(&sentence, &text) {
                            yield event;
                        }
                        let rest = buffer[end..].to_string();
                        if let Some(event) = take_clean(&rest, &text) {
                            yield event;
                        }
                        streaming_rest = true;
                        buffer.clear();
                    }
                } else if let Some(event) = take_clean(&token, &text) {
                    yield event;
                }
            }
            if rejected {
                regenerated.store(true, Ordering::SeqCst);
                continue;
            }
            if !streaming_rest {
                if attempt == 1 && opening_rejected(&buffer) && !buffer.trim().is_empty() {
                    regenerated.store(true, Ordering::SeqCst);
                    continue;
                }
                if let Some(event) = take_clean(&buffer, &text) {
                    yield event;
                }
            }
            break;
        }

        let produced = text.lock().map(|guard| guard.clone()).unwrap_or_default();
        saved.store(true, Ordering::SeqCst);
        on_finish(produced.clone(), false, regenerated.load(Ordering::SeqCst));
        let estimated = ((produced.chars().count() as f64) / 4.0).ceil() as u32;
        yield Event::default().event("metadata").data(json!({
            "openerTokens": 0,
            "deepenerTokens": estimated,
            "totalTokens": estimated,
            "regenerated": regenerated.load(Ordering::SeqCst),
        }).to_string());
        yield Event::default().data("[DONE]");
    };
    Ok(Box::pin(output))
}

fn take_clean(piece: &str, text: &Mutex<String>) -> Option<Event> {
    let clean = strip_markup(piece);
    if clean.is_empty() {
        return None;
    }
    if let Ok(mut guard) = text.lock() {
        guard.push_str(&clean);
    }
    Some(text_chunk_event(&clean))
}
