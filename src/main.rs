//! Proxy optimizado para OpenAI/Claude: `POST /interviews/:id/ai/assistant-relay`
//!
//! - **JWT** (`RELAY_JWT_SECRET`): HS256, claims `sub`, `interviewId`, `exp`, `iat`.
//! - **Rate limit** (Redis): `RATE_LIMIT_MAX` / `RATE_LIMIT_WINDOW_SECS` por usuario+entrevista.
//! - **Streaming SSE**: `text/event-stream`; cada `data:` es un JSON array de strings (solo fragmentos de texto).
//! - **Proveedores**: OpenAI (default), Claude via env var `AI_PROVIDER`.
//! - **Configuración**:
//!   - `AI_PROVIDER`: "openai" (default) | "claude" | "anthropic"
//!   - `INTERVIEW_AGENT_MODEL`: modelo a usar (defaults: OpenAI `gpt-4o-mini`, Claude `claude-sonnet-4-20250514`)
//!   - `INTERVIEW_AGENT_MAX_TOKENS`: max output tokens (default: 512, max: 4096)
//!   - `INTERVIEW_AGENT_TEMPERATURE`: 0.0-2.0 (default: 0.6; solo **OpenAI** — Anthropic omite el campo: muchos modelos nuevos deprecan `temperature`)
//!   - `INTERVIEW_AGENT_MAX_HISTORY`: mensajes en historial (default: 10, min: 4, max: 32)
//!   - `OPENAI_API_KEY` o `ANTHROPIC_API_KEY` según proveedor

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::Path;
use axum::extract::State;
use axum::http::header;
use axum::http::header::HeaderValue;
use axum::http::header::AUTHORIZATION;
use axum::http::HeaderMap;
use axum::http::HeaderName;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::AppendHeaders;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::post;
use axum::Json;
use axum::Router;
use futures::stream::StreamExt;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use redis::aio::ConnectionManager;
use serde_json::{json, Value};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::signal;
use tower::limit::ConcurrencyLimitLayer;
use tower::ServiceBuilder;
use tower_http::cors::CorsLayer;
use tracing::info;
use async_stream::try_stream;
use eventsource_stream::Eventsource;

const MAX_TOKEN_TTL: Duration = Duration::from_secs(5 * 60);
const RATE_LIMIT_SCRIPT: &str = r#"
local c = redis.call('INCR', KEYS[1])
if c == 1 then
  redis.call('EXPIRE', KEYS[1], tonumber(ARGV[1]))
end
if c > tonumber(ARGV[2]) then
  return 0
end
return 1
"#;

#[derive(Debug, Clone)]
struct AiConfig {
    provider: AiProvider,
    model: String,
    max_output_tokens: u32,
    temperature: f64,
    max_history_messages: usize,
}

#[derive(Debug, Clone, PartialEq)]
enum AiProvider {
    OpenAi,
    Claude,
}

impl AiProvider {
    fn from_env() -> Self {
        match std::env::var("AI_PROVIDER")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "claude" | "anthropic" => Self::Claude,
            _ => Self::OpenAi,
        }
    }
}

impl AiConfig {
    fn from_env() -> Self {
        let provider = AiProvider::from_env();
        let model = std::env::var("INTERVIEW_AGENT_MODEL").unwrap_or_else(|_| {
            match provider {
                AiProvider::OpenAi => "gpt-4o-mini".to_string(),
                AiProvider::Claude => "claude-sonnet-4-20250514".to_string(),
            }
        });

        let max_output_tokens = std::env::var("INTERVIEW_AGENT_MAX_TOKENS")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .map(|v| v.min(4096).max(64))
            .unwrap_or(512);

        let temperature = std::env::var("INTERVIEW_AGENT_TEMPERATURE")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .map(|v| v.min(2.0).max(0.0))
            .unwrap_or(0.6);

        let max_history_messages = std::env::var("INTERVIEW_AGENT_MAX_HISTORY")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .map(|v| v.min(32).max(4))
            .unwrap_or(10);

        Self {
            provider,
            model,
            max_output_tokens,
            temperature,
            max_history_messages,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct RelayClaims {
    sub: SubClaim,
    #[serde(rename = "interviewId")]
    interview_id: i64,
    #[serde(default)]
    iat: Option<i64>,
    exp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum SubClaim {
    Str(String),
    Int(i64),
}

impl SubClaim {
    fn as_key_segment(&self) -> String {
        match self {
            SubClaim::Str(s) => s.clone(),
            SubClaim::Int(n) => n.to_string(),
        }
    }
}

#[derive(Clone)]
struct RedisRateLimiter {
    conn: ConnectionManager,
    script: Arc<redis::Script>,
    max: u32,
    window_secs: u64,
}

impl RedisRateLimiter {
    async fn connect(redis_url: &str, max: u32, window_secs: u64) -> Result<Self, redis::RedisError> {
        let client = redis::Client::open(redis_url)?;
        let conn = ConnectionManager::new(client).await?;
        Ok(Self {
            conn,
            script: Arc::new(redis::Script::new(RATE_LIMIT_SCRIPT)),
            max,
            window_secs,
        })
    }

    /// `key` debe ser única por usuario y recurso (p. ej. `sub:interviewId`).
    async fn check_allowed(&self, key: &str) -> Result<bool, redis::RedisError> {
        let redis_key = format!("relay:rl:{key}");
        let mut conn = self.conn.clone();
        let allowed: i64 = self
            .script
            .key(redis_key)
            .arg(self.window_secs as i64)
            .arg(self.max as i64)
            .invoke_async(&mut conn)
            .await?;
        Ok(allowed == 1)
    }
}

#[derive(Debug, Deserialize)]
struct RelayBody {
    messages: Vec<serde_json::Value>,
}

#[derive(Clone)]
struct AppState {
    decoding_key: DecodingKey,
    limiter: Arc<RedisRateLimiter>,
    rate_limit_max: u32,
    ai_config: Arc<AiConfig>,
}

#[derive(Debug, Error)]
enum RelayError {
    #[error("Authorization Bearer faltante o inválido")]
    Auth,
    #[error("Token inválido: {0}")]
    Token(#[from] jsonwebtoken::errors::Error),
    #[error("entrevista del token no coincide con la ruta")]
    IdMismatch,
    #[error("messages es obligatorio y no puede estar vacío")]
    EmptyMessages,
    #[error("vida del token (exp-iat) supera 5 minutos")]
    TtlTooLong,
    #[error("rate limit: máximo {0} peticiones / ventana por usuario y entrevista")]
    Rate(u32),
    #[error("servicio de límites (Redis) no disponible")]
    RateLimitBackend,
    #[error("AI provider error: {0}")]
    AiProvider(String),
}

#[derive(Debug, Deserialize, Serialize)]
struct OpenAiStreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<Value>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct OpenAiStreamChoice {
    index: u32,
    delta: OpenAiStreamDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamEvent {
    choices: Vec<OpenAiStreamChoice>,
}

type BoxedStream = std::pin::Pin<Box<dyn futures::stream::Stream<Item = Result<Event, String>> + Send>>;

/// OpenAI exige `content` como string; muchos clientes envían `null` en mensajes `assistant` con tool_calls.
fn normalize_openai_message_contents(messages: &mut [Value]) {
    for msg in messages.iter_mut() {
        let Some(obj) = msg.as_object_mut() else {
            continue;
        };
        if let Some(content) = obj.get_mut("content") {
            if content.is_null() {
                *content = Value::String(String::new());
            }
        }
    }
}

fn anthropic_last_role_is(out: &[Value], role: &str) -> bool {
    out.last()
        .and_then(|m| m.get("role"))
        .and_then(|r| r.as_str())
        == Some(role)
}

fn value_to_anthropic_user_blocks(v: Value) -> Vec<Value> {
    match v {
        Value::String(s) if s.is_empty() => vec![],
        Value::String(s) => vec![json!({"type": "text", "text": s})],
        Value::Array(a) => a,
        Value::Null => vec![],
        _ => vec![json!({"type": "text", "text": v.to_string()})],
    }
}

fn merge_anthropic_user_content(a: Value, b: Value) -> Value {
    let mut blocks = value_to_anthropic_user_blocks(a);
    blocks.extend(value_to_anthropic_user_blocks(b));
    if blocks.is_empty() {
        return Value::String(String::new());
    }
    if blocks.len() == 1
        && blocks[0].get("type").and_then(|t| t.as_str()) == Some("text")
    {
        return Value::String(
            blocks[0]
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
        );
    }
    Value::Array(blocks)
}

fn value_to_anthropic_assistant_blocks(v: Value) -> Vec<Value> {
    match v {
        Value::String(s) if s.is_empty() => vec![],
        Value::String(s) => vec![json!({"type": "text", "text": s})],
        Value::Array(a) => a,
        Value::Null => vec![],
        _ => vec![json!({"type": "text", "text": v.to_string()})],
    }
}

fn merge_anthropic_assistant_content(a: Value, b: Value) -> Value {
    let mut blocks = value_to_anthropic_assistant_blocks(a);
    blocks.extend(value_to_anthropic_assistant_blocks(b));
    if blocks.is_empty() {
        return Value::String(String::new());
    }
    if blocks.len() == 1
        && blocks[0].get("type").and_then(|t| t.as_str()) == Some("text")
    {
        return Value::String(
            blocks[0]
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
        );
    }
    Value::Array(blocks)
}

fn anthropic_user_content_is_empty(c: &Value) -> bool {
    match c {
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Null => true,
        _ => false,
    }
}

fn anthropic_push_user(out: &mut Vec<Value>, content: Value) {
    if anthropic_user_content_is_empty(&content) {
        return;
    }
    if anthropic_last_role_is(out, "user") {
        let last = out.last_mut().expect("non-empty");
        let obj = last.as_object_mut().expect("object message");
        let prev = obj.get("content").cloned().unwrap_or(Value::Null);
        obj.insert(
            "content".to_string(),
            merge_anthropic_user_content(prev, content),
        );
    } else {
        out.push(json!({ "role": "user", "content": content }));
    }
}

fn anthropic_push_assistant(out: &mut Vec<Value>, content: Value) {
    if matches!(&content, Value::String(s) if s.is_empty())
        || matches!(&content, Value::Array(a) if a.is_empty())
        || matches!(&content, Value::Null)
    {
        return;
    }
    if anthropic_last_role_is(out, "assistant") {
        let last = out.last_mut().expect("non-empty");
        let obj = last.as_object_mut().expect("object message");
        let prev = obj.get("content").cloned().unwrap_or(Value::Null);
        obj.insert(
            "content".to_string(),
            merge_anthropic_assistant_content(prev, content),
        );
    } else {
        out.push(json!({ "role": "assistant", "content": content }));
    }
}

fn openai_message_content_as_string(c: &Value) -> Result<String, String> {
    match c {
        Value::String(s) => Ok(s.clone()),
        Value::Null => Ok(String::new()),
        Value::Array(parts) => {
            let mut s = String::new();
            for p in parts {
                let Some(o) = p.as_object() else { continue };
                if o.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(t) = o.get("text").and_then(|x| x.as_str()) {
                        s.push_str(t);
                    }
                }
            }
            Ok(s)
        }
        _ => Err("content: se esperaba string, null o lista de bloques text".to_string()),
    }
}

fn openai_content_to_anthropic_user(c: &Value) -> Result<Value, String> {
    match c {
        Value::String(s) => Ok(Value::String(s.clone())),
        Value::Null => Ok(Value::String(String::new())),
        Value::Array(parts) => {
            let mut blocks = Vec::new();
            for p in parts {
                let o = p
                    .as_object()
                    .ok_or_else(|| "bloque de content inválido".to_string())?;
                match o.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        let text = o.get("text").and_then(|x| x.as_str()).unwrap_or("");
                        blocks.push(json!({ "type": "text", "text": text }));
                    }
                    Some(other) => {
                        return Err(format!(
                            "bloque OpenAI no soportado para Claude (solo text): {}",
                            other
                        ));
                    }
                    None => {}
                }
            }
            if blocks.is_empty() {
                Ok(Value::String(String::new()))
            } else if blocks.len() == 1 {
                Ok(Value::String(
                    blocks[0]
                        .get("text")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string(),
                ))
            } else {
                Ok(Value::Array(blocks))
            }
        }
        _ => Err("content inválido para mensaje user".to_string()),
    }
}

fn openai_assistant_to_anthropic_content(
    obj: &serde_json::Map<String, Value>,
) -> Result<Value, String> {
    let mut blocks: Vec<Value> = Vec::new();

    let text = match obj.get("content") {
        None | Some(Value::Null) => String::new(),
        Some(c) => openai_message_content_as_string(c)?,
    };
    if !text.is_empty() {
        blocks.push(json!({ "type": "text", "text": text }));
    }

    if let Some(Value::Array(tcalls)) = obj.get("tool_calls") {
        for tc in tcalls {
            let id = tc
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "tool_call sin id".to_string())?;
            let func = tc
                .get("function")
                .and_then(|f| f.as_object())
                .ok_or_else(|| "tool_call sin function".to_string())?;
            let name = func
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "tool_call sin name".to_string())?;
            let args_str = func
                .get("arguments")
                .and_then(|v| v.as_str())
                .unwrap_or("{}");
            let input: Value =
                serde_json::from_str(args_str).unwrap_or_else(|_| json!({}));
            blocks.push(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input
            }));
        }
    }

    if blocks.is_empty() {
        Ok(Value::String(String::new()))
    } else if blocks.len() == 1 && blocks[0].get("type").and_then(|t| t.as_str()) == Some("text")
    {
        Ok(Value::String(
            blocks[0]
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
        ))
    } else {
        Ok(Value::Array(blocks))
    }
}

/// Convierte historial estilo OpenAI chat (system / user / assistant / tool) al formato Messages API.
fn openai_style_to_anthropic(messages: Vec<Value>) -> Result<(Option<String>, Vec<Value>), String> {
    let mut system_parts: Vec<String> = Vec::new();
    let mut out: Vec<Value> = Vec::new();

    for msg in messages {
        let obj = msg
            .as_object()
            .ok_or_else(|| "cada mensaje debe ser un objeto JSON".to_string())?;
        let role = obj
            .get("role")
            .and_then(|r| r.as_str())
            .ok_or_else(|| "mensaje sin role".to_string())?;

        match role {
            "system" => {
                if let Some(c) = obj.get("content") {
                    let s = openai_message_content_as_string(c)?;
                    if !s.is_empty() {
                        system_parts.push(s);
                    }
                }
            }
            "tool" => {
                let tool_call_id = obj
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "mensaje tool sin tool_call_id".to_string())?;
                let result_text =
                    openai_message_content_as_string(obj.get("content").unwrap_or(&Value::Null))?;
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": tool_call_id,
                    "content": result_text
                });
                anthropic_push_user(&mut out, Value::Array(vec![block]));
            }
            "user" => {
                let c = obj
                    .get("content")
                    .ok_or_else(|| "mensaje user sin content".to_string())?;
                let anth_c = openai_content_to_anthropic_user(c)?;
                anthropic_push_user(&mut out, anth_c);
            }
            "assistant" => {
                let anth_c = openai_assistant_to_anthropic_content(obj)?;
                anthropic_push_assistant(&mut out, anth_c);
            }
            _ => {
                return Err(format!("rol no soportado para Anthropic: {}", role));
            }
        }
    }

    let system = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };

    if out.is_empty() {
        return Err(
            "no quedaron mensajes user/assistant para Claude (¿solo system?)".to_string(),
        );
    }

    if out[0].get("role").and_then(|r| r.as_str()) != Some("user") {
        out.insert(0, json!({ "role": "user", "content": "" }));
    }

    Ok((system, out))
}

async fn stream_anthropic(config: &AiConfig, messages: Vec<Value>) -> Result<BoxedStream, String> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| "ANTHROPIC_API_KEY no configurada".to_string())?;

    let (system, anth_messages) = openai_style_to_anthropic(messages)?;

    // No enviar `temperature`: modelos recientes de Anthropic devuelven invalid_request_error
    // ("`temperature` is deprecated for this model").
    let mut req_body = json!({
        "model": &config.model,
        "max_tokens": config.max_output_tokens,
        "messages": anth_messages,
        "stream": true,
    });
    if let Some(s) = system {
        if !s.is_empty() {
            req_body["system"] = json!(s);
        }
    }

    let client = reqwest::Client::new();
    let res = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header(header::CONTENT_TYPE, "application/json")
        .json(&req_body)
        .send()
        .await
        .map_err(|e| format!("Anthropic request failed: {}", e))?;

    if !res.status().is_success() {
        let err_text = res
            .text()
            .await
            .unwrap_or_else(|_| "unknown error".to_string());
        return Err(format!("Anthropic API error: {}", err_text));
    }

    let stream = res.bytes_stream();

    let events_stream = try_stream! {
        let mut ess = stream.eventsource();
        let mut done = false;
        while let Some(msg) = ess.next().await {
            match msg {
                Ok(msg) => {
                    let data = msg.data.trim();
                    if data.is_empty() {
                        continue;
                    }
                    if let Ok(v) = serde_json::from_str::<Value>(data) {
                        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        if ty == "content_block_delta" {
                            if let Some(delta) = v.get("delta").and_then(|d| d.as_object()) {
                                if delta.get("type").and_then(|t| t.as_str()) == Some("text_delta") {
                                    if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                                        if !text.is_empty() {
                                            let body = serde_json::to_string(&vec![text])
                                                .map_err(|e| format!("serialize stream chunk: {}", e))?;
                                            yield Event::default().data(body);
                                        }
                                    }
                                }
                            }
                        } else if ty == "message_stop" {
                            yield Event::default().data("[DONE]");
                            done = true;
                            break;
                        } else if ty == "error" {
                            let detail = v
                                .pointer("/error/message")
                                .and_then(|m| m.as_str())
                                .map(String::from)
                                .unwrap_or_else(|| v.to_string());
                            Err::<(), String>(format!("Anthropic stream error: {}", detail))?;
                        }
                    }
                }
                Err(e) => {
                    Err::<(), String>(format!("Anthropic SSE parse error: {}", e))?;
                }
            }
        }
        if !done {
            yield Event::default().data("[DONE]");
        }
    };

    Ok(Box::pin(events_stream))
}

async fn stream_openai(config: &AiConfig, messages: Vec<Value>) -> Result<BoxedStream, String> {
    let api_key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| "OPENAI_API_KEY no configurada".to_string())?;

    let client = reqwest::Client::new();
    let req_body = json!({
        "model": &config.model,
        "messages": messages,
        "stream": true,
        "temperature": config.temperature,
        "max_completion_tokens": config.max_output_tokens,
    });

    let res = client
        .post("https://api.openai.com/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&req_body)
        .send()
        .await
        .map_err(|e| format!("OpenAI request failed: {}", e))?;

    if !res.status().is_success() {
        let err_text = res
            .text()
            .await
            .unwrap_or_else(|_| "unknown error".to_string());
        return Err(format!("OpenAI API error: {}", err_text));
    }

    let stream = res.bytes_stream();

    let events_stream = try_stream! {
        let mut ess = stream.eventsource();
        while let Some(msg) = ess.next().await {
            match msg {
                Ok(msg) => {
                    if msg.data == "[DONE]" {
                        yield Event::default().data("[DONE]");
                        break;
                    }

                    if let Ok(event) = serde_json::from_str::<OpenAiStreamEvent>(&msg.data) {
                        let fragments: Vec<&str> = event
                            .choices
                            .iter()
                            .filter_map(|c| c.delta.content.as_deref())
                            .filter(|s| !s.is_empty())
                            .collect();
                        if !fragments.is_empty() {
                            let body = serde_json::to_string(&fragments)
                                .map_err(|e| format!("serialize stream chunk: {}", e))?;
                            yield Event::default().data(body);
                        }
                    }
                }
                Err(e) => {
                    Err::<(), String>(format!("Event parse error: {}", e))?;
                }
            }
        }
    };

    Ok(Box::pin(events_stream))
}

fn bearer_token(headers: &HeaderMap) -> Result<String, RelayError> {
    let h = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or(RelayError::Auth)?;
    let rest = h
        .strip_prefix("Bearer ")
        .or_else(|| h.strip_prefix("bearer "))
        .ok_or(RelayError::Auth)?;
    if rest.is_empty() {
        return Err(RelayError::Auth);
    }
    Ok(rest.to_string())
}

async fn assistant_relay(
    State(st): State<AppState>,
    Path((interview_id,)): Path<(i64,)>,
    headers: HeaderMap,
    Json(body): Json<RelayBody>,
) -> Result<impl IntoResponse, RelayError> {
    let token = bearer_token(&headers)?;
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = 5;
    let token_data =
        jsonwebtoken::decode::<RelayClaims>(&token, &st.decoding_key, &validation)?;
    let claims = token_data.claims;

    if claims.interview_id != interview_id {
        return Err(RelayError::IdMismatch);
    }
    if let Some(iat) = claims.iat {
        let ttl = claims.exp.saturating_sub(iat);
        if ttl > MAX_TOKEN_TTL.as_secs() as i64 {
            return Err(RelayError::TtlTooLong);
        }
    }

    let user_id = claims.sub.as_key_segment();
    let key = format!("{user_id}:{}", claims.interview_id);
    match st.limiter.check_allowed(&key).await {
        Ok(true) => {}
        Ok(false) => return Err(RelayError::Rate(st.rate_limit_max)),
        Err(e) => {
            tracing::error!(error = %e, user_id = %user_id, interview_id = claims.interview_id, "redis rate limit");
            return Err(RelayError::RateLimitBackend);
        }
    }

    if body.messages.is_empty() {
        return Err(RelayError::EmptyMessages);
    }

    let mut messages = body
        .messages
        .into_iter()
        .take(st.ai_config.max_history_messages)
        .collect::<Vec<_>>();
    normalize_openai_message_contents(&mut messages);

    let stream = match st.ai_config.provider {
        AiProvider::OpenAi => {
            let config = st.ai_config.as_ref();
            stream_openai(config, messages)
                .await
                .map_err(|e| RelayError::AiProvider(e))?
        }
        AiProvider::Claude => {
            let config = st.ai_config.as_ref();
            stream_anthropic(config, messages)
                .await
                .map_err(|e| RelayError::AiProvider(e))?
        }
    };

    let sse = Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)));

    Ok((
        AppendHeaders([
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache, no-transform")),
            (header::CONNECTION, HeaderValue::from_static("keep-alive")),
            (HeaderName::from_static("x-accel-buffering"), HeaderValue::from_static("no")),
        ]),
        sse,
    )
        .into_response())
}

impl IntoResponse for RelayError {
    fn into_response(self) -> Response {
        use RelayError::*;

        let (status, msg) = match &self {
            Auth => (StatusCode::UNAUTHORIZED, self.to_string()),
            Token(e) => (StatusCode::UNAUTHORIZED, e.to_string()),
            IdMismatch => (StatusCode::FORBIDDEN, self.to_string()),
            EmptyMessages => (StatusCode::BAD_REQUEST, self.to_string()),
            TtlTooLong => (StatusCode::UNAUTHORIZED, self.to_string()),
            Rate(n) => {
                tracing::warn!(limit = n, "rate limit por usuario y entrevista");
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    format!("Máximo {n} peticiones por ventana (usuario + entrevista)"),
                )
            }
            RateLimitBackend => (
                StatusCode::SERVICE_UNAVAILABLE,
                self.to_string(),
            ),
            AiProvider(e) => {
                tracing::error!(error = %e, "AI provider failed");
                (StatusCode::BAD_GATEWAY, e.clone())
            }
        };

        (status, Json(serde_json::json!({ "message": msg, "error": self.to_string() })))
            .into_response()
    }
}

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("interview_relay_sim=info".parse().unwrap()),
        )
        .init();

    let secret = std::env::var("RELAY_JWT_SECRET").expect("RELAY_JWT_SECRET debe estar definida");
    let redis_url = std::env::var("REDIS_URL").expect("REDIS_URL debe estar definida (ej. redis://127.0.0.1:6379)");
    let rate_limit_max = env_u32("RATE_LIMIT_MAX", 10);
    let rate_window_secs = env_u64("RATE_LIMIT_WINDOW_SECS", 60);
    if rate_limit_max == 0 || rate_window_secs == 0 {
        panic!("RATE_LIMIT_MAX y RATE_LIMIT_WINDOW_SECS deben ser > 0");
    }

    let key = DecodingKey::from_secret(secret.as_bytes());
    let limiter = RedisRateLimiter::connect(&redis_url, rate_limit_max, rate_window_secs).await?;
    let ai_config = AiConfig::from_env();

    info!(
        provider = ?ai_config.provider,
        model = %ai_config.model,
        max_tokens = ai_config.max_output_tokens,
        temperature = ai_config.temperature,
        max_history = ai_config.max_history_messages,
        "AI config loaded"
    );

    let state = AppState {
        decoding_key: key,
        limiter: Arc::new(limiter),
        rate_limit_max,
        ai_config: Arc::new(ai_config),
    };

    let app = Router::new()
        .route(
            "/interviews/:id/ai/assistant-relay",
            post(assistant_relay),
        )
        .layer(
            ServiceBuilder::new()
                .layer(ConcurrencyLimitLayer::new(500))
                .layer(CorsLayer::permissive()),
        )
        .with_state(state);

    let port: u16 = std::env::var("PORT")
        .or_else(|_| std::env::var("LISTEN"))
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3001);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;
    info!(
        max = rate_limit_max,
        window_secs = rate_window_secs,
        "interview-relay-sim: POST http://{addr}/interviews/:id/ai/assistant-relay (JWT 5m, rate limit Redis)"
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = signal::ctrl_c().await;
            info!("apagado");
        })
        .await?;

    Ok(())
}

