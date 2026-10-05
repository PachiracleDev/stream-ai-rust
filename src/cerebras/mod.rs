//! Cliente Cerebras: entrevista (SSE), detección de pregunta y traducción.

pub mod detect;
pub mod qa;
pub mod vision;

pub use detect::complete_detect;
pub use qa::stream_spoken_answer;
pub use vision::stream_image_solver;

use std::time::Instant;

use async_stream::try_stream;
use axum::response::sse::Event;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::json;

use crate::config::CerebrasConfig;
use crate::streaming::BoxedStream;

/// Cliente HTTP compartido: reutiliza conexiones TCP/TLS entre requests
/// (evita handshake por llamada, clave para latencia del detector).
pub(crate) fn shared_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .pool_max_idle_per_host(8)
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("reqwest client válido")
    })
}

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
struct StreamTiming {
    #[serde(default)]
    completion_time: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<StreamUsage>,
    #[serde(default)]
    time_info: Option<StreamTiming>,
}

fn language_name(code: &str) -> Option<&'static str> {
    match code.trim().to_ascii_lowercase().as_str() {
        "es" | "es-419" => Some("Spanish"),
        "en" | "en-us" => Some("English"),
        "pt" | "pt-br" => Some("Portuguese"),
        _ => None,
    }
}

pub async fn stream_translation(
    config: &CerebrasConfig,
    text: &str,
    target_language: &str,
    source_language: Option<&str>,
) -> Result<BoxedStream, String> {
    let target = language_name(target_language)
        .ok_or_else(|| "targetLanguage debe ser es, en o pt".to_string())?;
    let source_hint = source_language
        .and_then(language_name)
        .map(|lang| format!(" The source language is {lang}."))
        .unwrap_or_default();

    let body = json!({
        "model": config.model,
        "stream": true,
        "max_completion_tokens": config.max_tokens,
        "temperature": config.temperature,
        "top_p": config.top_p,
        "reasoning_effort": config.reasoning_effort,
        "messages": [
            {
                "role": "system",
                "content": format!(
                    "You are a simultaneous interpreter. Translate the user's text into {target}.{source_hint} Output only the translation. Preserve meaning, names, technical terms, tone and incomplete sentence endings. Never explain, answer or add quotation marks."
                )
            },
            { "role": "user", "content": text.trim() }
        ]
    });

    let response = shared_client()
        .post(&config.chat_url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Cerebras translation request failed: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        return Err(format!("Cerebras translation HTTP {status}: {detail}"));
    }

    let model = config.model.clone();
    let mut upstream = response.bytes_stream().eventsource();
    let started_at = Instant::now();

    let output = try_stream! {
        let mut first_token_at: Option<Instant> = None;
        let mut output_chars: usize = 0;
        let mut completion_tokens: Option<u32> = None;
        let mut total_tokens: Option<u32> = None;
        let mut upstream_completion_seconds: Option<f64> = None;

        while let Some(message) = upstream.next().await {
            let message = message.map_err(|e| format!("Cerebras translation SSE inválido: {e}"))?;
            let raw = message.data.trim();
            if raw.is_empty() {
                continue;
            }
            if raw == "[DONE]" {
                break;
            }

            let chunk: StreamChunk = match serde_json::from_str(raw) {
                Ok(chunk) => chunk,
                Err(_) => continue,
            };
            if let Some(usage) = chunk.usage {
                completion_tokens = usage.completion_tokens.or(completion_tokens);
                total_tokens = usage.total_tokens.or(total_tokens);
            }
            if let Some(seconds) = chunk.time_info.and_then(|timing| timing.completion_time) {
                upstream_completion_seconds = Some(seconds);
            }

            for token in chunk
                .choices
                .into_iter()
                .filter_map(|choice| choice.delta.content)
                .filter(|token| !token.is_empty())
            {
                first_token_at.get_or_insert_with(Instant::now);
                output_chars += token.chars().count();
                yield Event::default().data(json!({ "token": token }).to_string());
            }
        }

        let elapsed = started_at.elapsed();
        let generation_seconds = upstream_completion_seconds
            .filter(|seconds| *seconds > 0.0)
            .or_else(|| first_token_at.map(|first| first.elapsed().as_secs_f64().max(0.001)))
            .unwrap_or_else(|| elapsed.as_secs_f64().max(0.001));
        let estimated_tokens = ((output_chars as f64) / 4.0).ceil() as u32;
        let measured_tokens = completion_tokens.unwrap_or(estimated_tokens);
        let tokens_per_second = measured_tokens as f64 / generation_seconds;

        yield Event::default().event("metadata").data(json!({
            "model": model,
            "completionTokens": measured_tokens,
            "totalTokens": total_tokens,
            "tokensPerSecond": (tokens_per_second * 10.0).round() / 10.0,
            "elapsedMs": elapsed.as_millis(),
            "estimatedTokens": completion_tokens.is_none()
        }).to_string());
        yield Event::default().data("[DONE]");
    };

    Ok(Box::pin(output))
}

#[cfg(test)]
mod tests {
    use super::language_name;

    #[test]
    fn accepts_supported_language_codes() {
        assert_eq!(language_name("es-419"), Some("Spanish"));
        assert_eq!(language_name("en"), Some("English"));
        assert_eq!(language_name("pt-BR"), Some("Portuguese"));
        assert_eq!(language_name("fr"), None);
    }
}
