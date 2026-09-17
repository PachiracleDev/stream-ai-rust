//! Image solver vía Cerebras (Qwen multimodal) en SSE.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use async_stream::try_stream;
use axum::response::sse::Event;
use base64::Engine;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE};
use serde_json::json;

use crate::config::CerebrasConfig;
use crate::relay::body::RelayMessage;
use crate::streaming::BoxedStream;

const DEFAULT_IMAGE_HOST: &str = "temporalassets.s3.us-east-1.amazonaws.com";
const DEFAULT_MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_IMAGE_TIMEOUT_SECS: u64 = 10;

fn image_allowed_hosts() -> HashSet<String> {
    std::env::var("IMAGE_FETCH_ALLOWED_HOSTS")
        .unwrap_or_else(|_| DEFAULT_IMAGE_HOST.to_string())
        .split(',')
        .map(|host| host.trim().trim_end_matches('.').to_ascii_lowercase())
        .filter(|host| !host.is_empty())
        .collect()
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn validate_remote_image_url(raw: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw).map_err(|_| "imageUrl no es una URL válida".to_string())?;
    if url.scheme() != "https" {
        return Err("imageUrl debe usar HTTPS".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("imageUrl no puede incluir credenciales".into());
    }
    if url.port_or_known_default() != Some(443) {
        return Err("imageUrl debe usar el puerto HTTPS estándar".into());
    }
    let host = url
        .host_str()
        .map(|value| value.trim_end_matches('.').to_ascii_lowercase())
        .ok_or_else(|| "imageUrl no contiene un host".to_string())?;
    if !image_allowed_hosts().contains(&host) {
        return Err(format!("host de imageUrl no permitido: {host}"));
    }
    Ok(url)
}

fn validate_image_mime(raw: &str) -> Result<&str, String> {
    let mime = raw.split(';').next().unwrap_or("").trim();
    match mime {
        "image/png" | "image/jpeg" | "image/webp" | "image/gif" => Ok(mime),
        _ => Err(format!("tipo de imagen no permitido: {mime}")),
    }
}

async fn remote_image_to_data_uri(raw: &str) -> Result<String, String> {
    let max_bytes = env_usize("IMAGE_FETCH_MAX_BYTES", DEFAULT_MAX_IMAGE_BYTES);
    let raw = raw.trim();
    if raw.starts_with("data:image/") {
        let (metadata, payload) = raw
            .split_once(',')
            .ok_or_else(|| "data URI de imagen inválida".to_string())?;
        let mime = metadata
            .strip_prefix("data:")
            .and_then(|value| value.split(';').next())
            .ok_or_else(|| "data URI de imagen sin MIME".to_string())?;
        validate_image_mime(mime)?;
        if !metadata.split(';').any(|part| part == "base64") {
            return Err("la data URI de imagen debe usar base64".into());
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .map_err(|_| "base64 de imagen inválido".to_string())?;
        if decoded.is_empty() {
            return Err("la data URI contiene una imagen vacía".into());
        }
        if decoded.len() > max_bytes {
            return Err(format!("la imagen excede el límite de {max_bytes} bytes"));
        }
        return Ok(raw.to_string());
    }

    let url = validate_remote_image_url(raw)?;
    let timeout_secs = env_u64("IMAGE_FETCH_TIMEOUT_SECS", DEFAULT_IMAGE_TIMEOUT_SECS);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("no se pudo preparar la descarga de imagen: {e}"))?;

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("no se pudo descargar imageUrl: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("imageUrl respondió HTTP {}", response.status()));
    }
    if let Some(length) = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
    {
        if length > max_bytes {
            return Err(format!("la imagen excede el límite de {max_bytes} bytes"));
        }
    }
    let mime = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| "imageUrl no devolvió Content-Type".to_string())?;
    let mime = validate_image_mime(mime)?.to_string();

    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("falló la lectura de imageUrl: {e}"))?;
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(format!("la imagen excede el límite de {max_bytes} bytes"));
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Err("imageUrl devolvió una imagen vacía".into());
    }

    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(format!("data:{mime};base64,{encoded}"))
}

async fn normalize_message_images(messages: &mut [RelayMessage]) -> Result<(), String> {
    for message in messages {
        let Some(raw) = message
            .image_url
            .as_deref()
            .filter(|url| !url.trim().is_empty())
        else {
            continue;
        };
        message.image_url = Some(remote_image_to_data_uri(raw).await?);
    }
    Ok(())
}

fn relay_message_to_cerebras(msg: &RelayMessage) -> Option<serde_json::Value> {
    let text = msg.content.as_deref().unwrap_or("").trim();
    let image = msg.image_url.as_deref().filter(|u| !u.trim().is_empty());

    match (text.is_empty(), image) {
        (true, None) => None,
        (false, None) => Some(json!({ "role": msg.role, "content": text })),
        (true, Some(url)) => Some(json!({
            "role": msg.role,
            "content": [
                { "type": "image_url", "image_url": { "url": url } }
            ]
        })),
        (false, Some(url)) => Some(json!({
            "role": msg.role,
            "content": [
                { "type": "text", "text": text },
                { "type": "image_url", "image_url": { "url": url } }
            ]
        })),
    }
}

pub async fn stream_image_solver(
    config: &CerebrasConfig,
    system_prompt: &str,
    mut messages: Vec<RelayMessage>,
) -> Result<BoxedStream, String> {
    normalize_message_images(&mut messages).await?;
    let mut chat_messages = vec![json!({ "role": "system", "content": system_prompt })];
    for msg in messages {
        if let Some(v) = relay_message_to_cerebras(&msg) {
            chat_messages.push(v);
        }
    }

    let body = json!({
        "model": config.model,
        "stream": true,
        "max_tokens": config.max_tokens,
        "temperature": config.temperature,
        "top_p": config.top_p,
        "reasoning_effort": config.reasoning_effort,
        "messages": chat_messages,
    });

    let response = crate::cerebras::shared_client()
        .post(&config.chat_url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Cerebras vision request failed: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        return Err(format!("Cerebras vision HTTP {status}: {detail}"));
    }

    let model = config.model.clone();
    let mut upstream = response.bytes_stream().eventsource();
    let started_at = Instant::now();

    let output = try_stream! {
        while let Some(message) = upstream.next().await {
            let message = message.map_err(|e| format!("Cerebras vision SSE inválido: {e}"))?;
            let raw = message.data.trim();
            if raw.is_empty() || raw == "[DONE]" {
                continue;
            }
            let chunk: serde_json::Value = match serde_json::from_str(raw) {
                Ok(v) => v,
                Err(_) => continue,
            };
            for token in chunk
                .get("choices")
                .and_then(|c| c.as_array())
                .into_iter()
                .flatten()
                .filter_map(|choice| choice.get("delta"))
                .filter_map(|delta| delta.get("content"))
                .filter_map(|c| c.as_str())
                .filter(|s| !s.is_empty())
            {
                yield Event::default().data(json!([token]).to_string());
            }
        }

        let elapsed = started_at.elapsed();
        yield Event::default().event("metadata").data(json!({
            "model": model,
            "elapsedMs": elapsed.as_millis(),
        }).to_string());
        yield Event::default().data("[DONE]");
    };

    Ok(Box::pin(output))
}

#[cfg(test)]
mod tests {
    use super::{validate_image_mime, validate_remote_image_url};

    #[test]
    fn accepts_configured_s3_host() {
        let url = validate_remote_image_url(
            "https://temporalassets.s3.us-east-1.amazonaws.com/uploads/35/example.png",
        )
        .expect("S3 URL should be allowed");
        assert_eq!(url.scheme(), "https");
    }

    #[test]
    fn rejects_untrusted_or_non_https_urls() {
        assert!(validate_remote_image_url(
            "http://temporalassets.s3.us-east-1.amazonaws.com/a.png"
        )
        .is_err());
        assert!(validate_remote_image_url("https://127.0.0.1/a.png").is_err());
        assert!(validate_remote_image_url("https://example.com/a.png").is_err());
    }

    #[test]
    fn accepts_only_supported_image_mimes() {
        assert_eq!(
            validate_image_mime("image/png; charset=binary").unwrap(),
            "image/png"
        );
        assert!(validate_image_mime("text/html").is_err());
        assert!(validate_image_mime("image/svg+xml").is_err());
    }
}
