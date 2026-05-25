//! Proxy optimizado para OpenAI, Claude o DeepSeek; `speed: fast` en el body usa Groq (API OpenAI-compatible).
//!
//! - **JWT** (`RELAY_JWT_SECRET`): HS256, claims `sub`, `interviewId`, `exp`, `iat`.
//! - **Rate limit** (Redis): `RATE_LIMIT_MAX` / `RATE_LIMIT_WINDOW_SECS` por usuario+entrevista.
//! - **Streaming SSE**: `text/event-stream`; cada `data:` es un JSON array de strings (solo fragmentos de texto).
//! - **Proveedores**: OpenAI (default), Claude o DeepSeek via env var `AI_PROVIDER`.
//! - **Configuración**:
//!   - `AI_PROVIDER`: "openai" (default) | "claude" | "anthropic" | "deepseek"
//!   - `INTERVIEW_AGENT_MODEL`: modelo a usar (defaults: OpenAI `gpt-4o-mini`, Claude `claude-sonnet-4-20250514`, DeepSeek `deepseek-v4-flash`)
//!   - `INTERVIEW_AGENT_MAX_TOKENS`: techo de tokens de salida por petición — el valor efectivo llega opcionalmente en el JSON (`max_output_tokens` / `maxOutputTokens`), recortado entre 64 y este techo si hace falta
//!   - **Relay body — `speed`**: **`medium`** (default) usa el proveedor de `AI_PROVIDER`; **`fast`** usa **Groq** (API OpenAI-compatible en streaming, modelo por defecto **`llama-3.1-8b-instant`**, **`GROQ_API_KEY`**). Alias JSON opcional **`rapidez`**.
//!   - `INTERVIEW_AGENT_TEMPERATURE`: 0.0-2.0 (default: 0.6; **OpenAI** y **DeepSeek**; Anthropic omite el campo: muchos modelos nuevos deprecan `temperature`)
//!   - `INTERVIEW_AGENT_MAX_HISTORY`: mensajes en historial (default: 10, min: 4, max: 32)
//!   - `OPENAI_API_KEY`, `ANTHROPIC_API_KEY` o `DEEPSEEK_API_KEY` según proveedor
//!   - DeepSeek: `DEEPSEEK_CHAT_COMPLETIONS_URL` — URI completa del endpoint (default: `https://api.deepseek.com/chat/completions`)
//!   - `RELAY_DOTENV_PATH` (opcional): ruta absoluta a un archivo `.env` si el proceso no arranca con `WorkingDirectory` donde está el proyecto (p. ej. systemd).
//!   - **`RELAY_FIRST_CHUNK_DEADLINE_SECS`** (default: **10**): máximo tiempo desde el POST hasta el **primer fragmento de texto modelo** (no cuenta chunks solo con rol / vacíos); si expira se cancela ese intento.
//!   - **`RELAY_FIRST_CHUNK_RETRY_MAX`** (default **1`): reintentos extra; intentos upstream totales ≈ **1 + este valor**.
//!   - **`RELAY_PERF_LOG`**: actívalo con `1` o `true` para trazas por fase con colores (Δ y acumulado desde el inicio de esa medición). Por defecto desactivado.
//!   - DeepSeek: **`DEEPSEEK_DISABLE_THINKING=1`** o **`DEEPSEEK_THINKING_MODE=disabled`** inyecta `thinking: { "type": "disabled" }` en el body (menos contenido reasoning antes del texto útil para el cliente; puede mejorar TTFT perceptible).
//!   - **`speed=fast`**: **`GROQ_API_KEY`** obligatoria; opcional **`GROQ_CHAT_COMPLETIONS_URL`** (default `https://api.groq.com/openai/v1/chat/completions`) y **`GROQ_FAST_MODEL`**.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::Instant;

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
use tokio::time::timeout;
use tower::limit::ConcurrencyLimitLayer;
use tower::ServiceBuilder;
use tower_http::cors::CorsLayer;
use colored::Colorize;
use tracing::info;
use async_stream::try_stream;
use eventsource_stream::Eventsource;

const MAX_TOKEN_TTL: Duration = Duration::from_secs(5 * 60);

/// POST → primer trozo modelo (headers + primera línea SSE con contenido)
const DEFAULT_RELAY_FIRST_CHUNK_DEADLINE_SECS: u64 = 10;
/// Cuántos reintentos adicionales si no hay primer fragmento antes del deadline
const DEFAULT_RELAY_FIRST_CHUNK_RETRY_MAX: u32 = 1;

fn relay_first_chunk_deadline() -> Duration {
    std::env::var("RELAY_FIRST_CHUNK_DEADLINE_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|v| Duration::from_secs(v.clamp(1, 600)))
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_RELAY_FIRST_CHUNK_DEADLINE_SECS))
}

fn upstream_attempt_count() -> u32 {
    let extra = std::env::var("RELAY_FIRST_CHUNK_RETRY_MAX")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(DEFAULT_RELAY_FIRST_CHUNK_RETRY_MAX)
        .min(10);
    1 + extra
}

fn relay_perf_enabled() -> bool {
    match std::env::var("RELAY_PERF_LOG") {
        Ok(s) if s == "1" || s.eq_ignore_ascii_case("true") => true,
        _ => false,
    }
}

fn deepseek_thinking_disabled_from_env() -> bool {
    match std::env::var("DEEPSEEK_DISABLE_THINKING") {
        Ok(s) if s == "1" || s.eq_ignore_ascii_case("true") => return true,
        _ => {}
    }
    match std::env::var("DEEPSEEK_THINKING_MODE") {
        Ok(s)
            if s.eq_ignore_ascii_case("disabled") || s.eq_ignore_ascii_case("off") =>
        {
            true
        }
        _ => false,
    }
}

fn groq_chat_completions_url_from_env() -> String {
    std::env::var("GROQ_CHAT_COMPLETIONS_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "https://api.groq.com/openai/v1/chat/completions".to_string())
}

fn groq_fast_model_from_env() -> String {
    std::env::var("GROQ_FAST_MODEL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "llama-3.1-8b-instant".to_string())
}

/// Marca de tiempo por fase con colores (`target` = `relay_perf`, filtrable con `RUST_LOG`).
struct RelayPerf {
    scope: String,
    origin: Instant,
    seg: Instant,
    seq: u32,
}

impl RelayPerf {
    fn new(scope: impl Into<String>) -> Self {
        let t = Instant::now();
        Self {
            scope: scope.into(),
            origin: t,
            seg: t,
            seq: 0,
        }
    }

    fn step(&mut self, phase: &'static str) {
        let now = Instant::now();
        let dt_ms = now.duration_since(self.seg).as_secs_f64() * 1000.0;
        let cum_ms = now.duration_since(self.origin).as_secs_f64() * 1000.0;
        self.seg = now;
        self.seq += 1;
        let phase_c = match self.seq % 5 {
            0 => phase.bright_cyan(),
            1 => phase.bright_green(),
            2 => phase.bright_yellow(),
            3 => phase.bright_magenta(),
            _ => phase.bright_blue(),
        };
        let line = format!(
            "⏱ {} #{} {} Δ{:>7.2} ms │ cum{:>9.2} ms",
            self.scope.bright_white(),
            self.seq,
            phase_c,
            dt_ms,
            cum_ms
        );
        tracing::info!(target: "relay_perf", "{}", line);
    }
}

fn relay_perf(scope: impl Into<String>) -> Option<RelayPerf> {
    relay_perf_enabled().then(|| RelayPerf::new(scope))
}

fn perf_step(p: &mut Option<RelayPerf>, label: &'static str) {
    if let Some(r) = p {
        r.step(label);
    }
}

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
    /// Límite superior de tokens de salida (`.env`). Si el cliente no envía valor en el body, se usa este techo tal cual como límite efectivo (comportamiento anterior).
    max_output_tokens_cap: u32,
    temperature: f64,
    max_history_messages: usize,
}

#[derive(Debug, Clone, PartialEq)]
enum AiProvider {
    OpenAi,
    Claude,
    DeepSeek,
}

impl AiProvider {
    fn from_env() -> Self {
        match std::env::var("AI_PROVIDER")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "claude" | "anthropic" => Self::Claude,
            "deepseek" => Self::DeepSeek,
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
                AiProvider::DeepSeek => "deepseek-v4-flash".to_string(),
            }
        });
        
        let max_output_tokens_cap = std::env::var("INTERVIEW_AGENT_MAX_TOKENS")
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
            max_output_tokens_cap,
            temperature,
            max_history_messages,
        }
    }
}

const OUTPUT_TOKENS_MIN: u32 = 64;

/// `requested` viene del cliente; no puede pasar del techo `cap` (`.env`) ni bajar de [`OUTPUT_TOKENS_MIN`].
fn effective_output_max_tokens(requested: Option<u32>, cap: u32) -> u32 {
    let cap = cap.max(OUTPUT_TOKENS_MIN);
    match requested {
        Some(n) => n.min(cap).max(OUTPUT_TOKENS_MIN),
        None => cap,
    }
}

/// Tokens de tamaño sistema (solo mensajes `role: system`, longitud en **caracteres Unicode** del `content`).
fn json_value_chars_len(value: &Value) -> usize {
    match value {
        Value::String(s) => s.chars().count(),
        Value::Array(parts) => parts
            .iter()
            .map(|part| match part.as_object() {
                Some(o) if o.get("type").and_then(|t| t.as_str()) == Some("text") => o
                    .get("text")
                    .map(json_value_chars_len)
                    .unwrap_or(0),
                Some(o) => serde_json::to_string(o)
                    .map(|s| s.chars().count())
                    .unwrap_or(0),
                None => part.to_string().chars().count(),
            })
            .sum(),
        Value::Null => 0,
        _ => value.to_string().chars().count(),
    }
}

fn total_system_prompt_len_chars(messages: &[Value]) -> usize {
    messages
        .iter()
        .filter_map(|msg| msg.as_object())
        .filter(|o| o.get("role").and_then(|r| r.as_str()) == Some("system"))
        .map(|o| o.get("content").map(json_value_chars_len).unwrap_or(0))
        .sum()
}

/// TTFT: primer fragmento de texto del modelo; el reloj arranca al recibir HTTP OK del proveedor y empezar a leer el cuerpo en streaming.
#[derive(Debug)]
struct StreamLogCtx {
    request_ts_rfc3339: String,
    upstream_body_started: Mutex<Option<std::time::Instant>>,
    max_output_tokens: u32,
    system_prompt_len_chars: usize,
    interview_id: i64,
    user_id: String,
    provider: AiProvider,
    model: String,
    /// Para trazas: `configured_provider` vs `groq_fast` (`speed` = `fast`).
    upstream_lane: &'static str,
    first_fragment_logged: AtomicBool,
}

impl StreamLogCtx {
    fn mark_upstream_ready(&self) {
        let mut guard = self
            .upstream_body_started
            .lock()
            .expect("upstream_body_started mutex poisoned");
        *guard = Some(std::time::Instant::now());
    }

    fn on_sse_data_payload(&self, data: &str) {
        let t = data.trim();
        if t.is_empty() || t == "[DONE]" {
            return;
        }
        if self
            .first_fragment_logged
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let t0 = {
            let g = self
                .upstream_body_started
                .lock()
                .expect("upstream_body_started mutex poisoned");
            g.unwrap_or_else(std::time::Instant::now)
        };
        let ttft_ms = t0.elapsed().as_secs_f64() * 1000.0;

        info!(
            timestamp = %self.request_ts_rfc3339,
            ttft_ms,
            max_output_tokens = self.max_output_tokens,
            system_prompt_len_chars = self.system_prompt_len_chars,
            interview_id = self.interview_id,
            user_id = %self.user_id,
            provider = ?self.provider,
            model = %self.model,
            upstream_lane = self.upstream_lane,
            "relay primera respuesta modelo (TTFT)"
        );
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
    /// Límite deseado de tokens de salida (recortado al techo `INTERVIEW_AGENT_MAX_TOKENS`).
    #[serde(default, rename = "maxOutputTokens", alias = "max_output_tokens")]
    max_output_tokens: Option<u32>,
    /// `medium`: proveedor habitual (`AI_PROVIDER`). `fast`: Groq (Llama instant, API OpenAI-compat).
    #[serde(default, alias = "rapidez")]
    speed: RelaySpeed,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
enum RelaySpeed {
    #[default]
    Medium,
    Fast,
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
    /// DeepSeek thinking / algunos gateways envían reasoning antes que `content`.
    #[serde(default)]
    reasoning_content: Option<String>,
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
    #[serde(default)]
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

enum OpenAiCompatSseDecision {
    Skip,
    YieldPayload(Event),
    StreamDoneSentinel,
}

fn openai_compat_process_raw_sse(
    raw_msg: &eventsource_stream::Event,
    log_ctx: Option<&Arc<StreamLogCtx>>,
) -> Result<OpenAiCompatSseDecision, String> {
    let data_trim = raw_msg.data.trim();
    if data_trim.is_empty() {
        return Ok(OpenAiCompatSseDecision::Skip);
    }

    if data_trim == "[DONE]" {
        return Ok(OpenAiCompatSseDecision::StreamDoneSentinel);
    }

    let Ok(event) = serde_json::from_str::<OpenAiStreamEvent>(data_trim) else {
        return Ok(OpenAiCompatSseDecision::Skip);
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
        return Ok(OpenAiCompatSseDecision::Skip);
    }

    let body =
        serde_json::to_string(&fragments).map_err(|e| format!("serialize stream chunk: {}", e))?;
    let ev_payload = Event::default().data(body.clone());
    if let Some(c) = log_ctx {
        c.on_sse_data_payload(&body);
    }

    Ok(OpenAiCompatSseDecision::YieldPayload(ev_payload))
}

async fn openai_compat_drain_first_nonempty<E, S>(
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
                    "upstream cerró el stream SSE sin emitir contenido del modelo".to_string(),
                );
            }
            Some(Err(e)) => return Err(format!("Event parse error: {}", e)),
            Some(Ok(raw)) => {
                match openai_compat_process_raw_sse(&raw, log_ctx)? {
                    OpenAiCompatSseDecision::Skip => {}
                    OpenAiCompatSseDecision::YieldPayload(ev) => return Ok(vec![ev]),
                    OpenAiCompatSseDecision::StreamDoneSentinel => {
                        return Err(
                            "upstream emitió fin de stream antes de contenido del modelo".to_string(),
                        );
                    }
                }
            }
        }
    }
}

async fn openai_compat_stream_one_attempt(
    deadline: Duration,
    provider_label: &'static str,
    client: reqwest::Client,
    url: String,
    api_key: String,
    req_body: Value,
    log_ctx: Option<Arc<StreamLogCtx>>,
    attempt_no: u32,
) -> Result<BoxedStream, String> {
    let mut perf = relay_perf(format!("{}↑{}", provider_label, attempt_no));
    match timeout(deadline, async move {
        perf_step(&mut perf, "A_timeout_block_enter");
        let log_ctx_follow = log_ctx.clone();

        perf_step(&mut perf, "B_before_http_send");
        let res = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .json(&req_body)
            .send()
            .await
            .map_err(|e| format!("{provider_label} request failed: {e}"))?;

        perf_step(&mut perf, "C_http_response_received");
        if !res.status().is_success() {
            let err_text = res
                .text()
                .await
                .unwrap_or_else(|_| "unknown error".to_string());
            return Err(format!("{provider_label} API error: {}", err_text));
        }

        perf_step(&mut perf, "D_status_success");
        if let Some(c) = &log_ctx {
            c.mark_upstream_ready();
        }

        perf_step(&mut perf, "E_upstream_mark_ttft_anchor");
        let mut raw_ess = res.bytes_stream().eventsource();

        perf_step(&mut perf, "F_before_sse_first_nonempty_drain");
        let prelude_events = openai_compat_drain_first_nonempty(
            &mut raw_ess,
            log_ctx.as_ref(),
        )
        .await?;

        perf_step(&mut perf, "G_after_first_nonempty_chunk");

        let events_stream = try_stream! {
            for ev in prelude_events {
                yield ev;
            }
            while let Some(msg) = StreamExt::next(&mut raw_ess).await {
                match msg {
                    Ok(raw) => match openai_compat_process_raw_sse(&raw, log_ctx_follow.as_ref())? {
                        OpenAiCompatSseDecision::Skip => {}
                        OpenAiCompatSseDecision::YieldPayload(ev) => yield ev,
                        OpenAiCompatSseDecision::StreamDoneSentinel => {
                            yield Event::default().data("[DONE]");
                            break;
                        }
                    },
                    Err(e) => Err::<(), String>(format!("Event parse error: {}", e))?,
                }
            }
        };

        perf_step(&mut perf, "H_sse_stream_spawned_ready");
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
                "timeout esperando primer fragmento del modelo (upstream SSE)"
            );
            Err(format!(
                "{provider_label}: timeout ({:.3}s) esperando primera salida del modelo",
                deadline.as_secs_f64()
            ))
        }
    }
}

async fn stream_groq_fast(
    config: &AiConfig,
    messages: Vec<Value>,
    max_output_tokens: u32,
    log_ctx: Option<Arc<StreamLogCtx>>,
) -> Result<BoxedStream, String> {
    let api_key =
        std::env::var("GROQ_API_KEY").map_err(|_| "speed=fast requiere GROQ_API_KEY".to_string())?;

    let url = groq_chat_completions_url_from_env();
    let model = groq_fast_model_from_env();

    let req_body = json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "max_tokens": max_output_tokens,
        "temperature": config.temperature,
    });

    stream_openai_compatible_chat_completions(
        "Groq",
        url.trim(),
        &api_key,
        req_body,
        log_ctx,
    )
    .await
}

enum AnthropicSseDecision {
    Skip,
    Yield(Event),
    MessageStop,
}

fn anthropic_handle_raw_sse_payload(
    data: &str,
    log_ctx: Option<&Arc<StreamLogCtx>>,
) -> Result<AnthropicSseDecision, String> {
    let d = data.trim();
    if d.is_empty() {
        return Ok(AnthropicSseDecision::Skip);
    }

    let v = match serde_json::from_str::<Value>(d) {
        Ok(v) => v,
        Err(_) => return Ok(AnthropicSseDecision::Skip),
    };

    let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    if ty == "content_block_delta" {
        let Some(delta) = v.get("delta").and_then(|dv| dv.as_object()) else {
            return Ok(AnthropicSseDecision::Skip);
        };

        if delta.get("type").and_then(|t| t.as_str()) == Some("text_delta") {
            if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                if !text.is_empty() {
                    let body =
                        serde_json::to_string(&vec![text]).map_err(|e| format!("serialize stream chunk: {}", e))?;
                    let ev = Event::default().data(body.clone());
                    if let Some(c) = log_ctx {
                        c.on_sse_data_payload(&body);
                    }
                    return Ok(AnthropicSseDecision::Yield(ev));
                }
            }
        }
        Ok(AnthropicSseDecision::Skip)
    } else if ty == "message_stop" {
        Ok(AnthropicSseDecision::MessageStop)
    } else if ty == "error" {
        let detail = v
            .pointer("/error/message")
            .and_then(|m| m.as_str())
            .map(String::from)
            .unwrap_or_else(|| v.to_string());
        Err(format!("Anthropic stream error: {}", detail))
    } else {
        Ok(AnthropicSseDecision::Skip)
    }
}

async fn anthropic_drain_first_nonempty<E, S>(
    raw_ess: &mut S,
    log_ctx: Option<&Arc<StreamLogCtx>>,
) -> Result<Vec<Event>, String>
where
    E: std::fmt::Display,
    S: futures::stream::Stream<Item = Result<eventsource_stream::Event, E>> + Unpin,
{
    loop {
        match StreamExt::next(raw_ess).await {
            None => return Err("Anthropic SSE terminó antes de contenido del modelo".to_string()),
            Some(Err(e)) => return Err(format!("Anthropic SSE parse error: {}", e)),
            Some(Ok(msg)) => match anthropic_handle_raw_sse_payload(&msg.data, log_ctx)? {
                AnthropicSseDecision::Skip => {}
                AnthropicSseDecision::Yield(ev) => return Ok(vec![ev]),
                AnthropicSseDecision::MessageStop => {
                    return Err(
                        "Anthropic envió message_stop antes de contenido del modelo".to_string(),
                    );
                }
            },
        }
    }
}

async fn anthropic_stream_one_attempt(
    deadline: Duration,
    client: reqwest::Client,
    api_key: String,
    req_body: Value,
    log_ctx: Option<Arc<StreamLogCtx>>,
    attempt_no: u32,
) -> Result<BoxedStream, String> {
    let mut perf = relay_perf(format!("Anthropic↑{}", attempt_no));
    match timeout(deadline, async move {
        perf_step(&mut perf, "A_timeout_block_enter");
        let log_ctx_follow = log_ctx.clone();

        perf_step(&mut perf, "B_before_http_send");
        let res = client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header(header::CONTENT_TYPE, "application/json")
            .json(&req_body)
            .send()
            .await
            .map_err(|e| format!("Anthropic request failed: {}", e))?;

        perf_step(&mut perf, "C_http_response_received");
        if !res.status().is_success() {
            let err_text = res
                .text()
                .await
                .unwrap_or_else(|_| "unknown error".to_string());
            return Err(format!("Anthropic API error: {}", err_text));
        }

        perf_step(&mut perf, "D_status_success");
        if let Some(c) = &log_ctx {
            c.mark_upstream_ready();
        }

        perf_step(&mut perf, "E_upstream_mark_ttft_anchor");
        let mut raw_ess = res.bytes_stream().eventsource();

        perf_step(&mut perf, "F_before_sse_first_nonempty_drain");
        let prelude_events = anthropic_drain_first_nonempty(&mut raw_ess, log_ctx.as_ref()).await?;

        perf_step(&mut perf, "G_after_first_nonempty_chunk");

        let events_stream = try_stream! {
            for ev in prelude_events {
                yield ev;
            }
            let mut done = false;
            while let Some(msg) = StreamExt::next(&mut raw_ess).await {
                match msg {
                    Ok(raw) => match anthropic_handle_raw_sse_payload(&raw.data, log_ctx_follow.as_ref())? {
                        AnthropicSseDecision::Skip => {}
                        AnthropicSseDecision::Yield(ev) => yield ev,
                        AnthropicSseDecision::MessageStop => {
                            yield Event::default().data("[DONE]");
                            done = true;
                            break;
                        }
                    },
                    Err(e) => Err::<(), String>(format!("Anthropic SSE parse error: {}", e))?,
                }
            }
            if !done {
                yield Event::default().data("[DONE]");
            }
        };

        perf_step(&mut perf, "H_sse_stream_spawned_ready");
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

async fn stream_anthropic(
    config: &AiConfig,
    messages: Vec<Value>,
    max_output_tokens: u32,
    log_ctx: Option<Arc<StreamLogCtx>>,
) -> Result<BoxedStream, String> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| "ANTHROPIC_API_KEY no configurada".to_string())?;

    let mut perf_a = relay_perf("anthropic_prep");
    perf_step(&mut perf_a, "A00_start");
    let (system, anth_messages) = openai_style_to_anthropic(messages)?;
    perf_step(&mut perf_a, "A01_openai_to_anthropic_done");

    let mut req_body = json!({
        "model": &config.model,
        "max_tokens": max_output_tokens,
        "messages": anth_messages,
        "stream": true,
    });
    if let Some(s) = system {
        if !s.is_empty() {
            req_body["system"] = json!(s);
        }
    }

    perf_step(&mut perf_a, "A02_upstream_request_json_ready");

    let deadline = relay_first_chunk_deadline();
    let attempts = upstream_attempt_count();
    let mut last_err = "Anthropic: agotados reintentos de primer fragmento".to_string();
    let client = reqwest::Client::new();

    perf_step(&mut perf_a, "A03_before_upstream_retry_loop");

    for idx in 0..attempts {
        match anthropic_stream_one_attempt(
            deadline,
            client.clone(),
            api_key.clone(),
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
                    error = %e,
                    deadline_secs = deadline.as_secs_f64(),
                    "reintento Anthropic upstream (primer fragmento)"
                );
                last_err = e;
                tokio::time::sleep(Duration::from_millis(75)).await;
            }
            Err(e) => return Err(e),
        }
    }

    Err(last_err)
}

/// Streaming SSE estilo OpenAI (`choices[].delta.content`), compartido por OpenAI y DeepSeek.
async fn stream_openai_compatible_chat_completions(
    provider_label: &'static str,
    url: &str,
    api_key: &str,
    req_body: Value,
    log_ctx: Option<Arc<StreamLogCtx>>,
) -> Result<BoxedStream, String> {
    let deadline = relay_first_chunk_deadline();
    let attempts = upstream_attempt_count();
    let mut last_err =
        format!("{provider_label}: agotados reintentos esperando primer fragmento");
    let url_owned = url.to_string();
    let key_owned = api_key.to_string();
    let client = reqwest::Client::new();

    for idx in 0..attempts {
        match openai_compat_stream_one_attempt(
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
                    deadline_secs = deadline.as_secs_f64(),
                    "reintento compat-OpenAI upstream (primer fragmento)"
                );
                last_err = e;
                tokio::time::sleep(Duration::from_millis(75)).await;
            }
            Err(e) => return Err(e),
        }
    }

    Err(last_err)
}

async fn stream_openai(
    config: &AiConfig,
    messages: Vec<Value>,
    max_output_tokens: u32,
    log_ctx: Option<Arc<StreamLogCtx>>,
) -> Result<BoxedStream, String> {
    let api_key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| "OPENAI_API_KEY no configurada".to_string())?;

    let req_body = json!({
        "model": &config.model,
        "messages": messages,
        "stream": true,
        "temperature": config.temperature,
        "max_completion_tokens": max_output_tokens,
    });

    stream_openai_compatible_chat_completions(
        "OpenAI",
        "https://api.openai.com/v1/chat/completions",
        &api_key,
        req_body,
        log_ctx,
    )
    .await
}

async fn stream_deepseek(
    config: &AiConfig,
    messages: Vec<Value>,
    max_output_tokens: u32,
    log_ctx: Option<Arc<StreamLogCtx>>,
) -> Result<BoxedStream, String> {
    let api_key = std::env::var("DEEPSEEK_API_KEY")
        .map_err(|_| "DEEPSEEK_API_KEY no configurada".to_string())?;

    let url = std::env::var("DEEPSEEK_CHAT_COMPLETIONS_URL").unwrap_or_else(|_| {
        "https://api.deepseek.com/chat/completions".to_string()
    });

    // DeepSeek documenta `max_tokens` (compatible OpenAI en el resto del contrato).
    let mut req_body = json!({
        "model": &config.model,
        "messages": messages,
        "stream": true,
        "temperature": config.temperature,
        "max_tokens": max_output_tokens,
    });

    if deepseek_thinking_disabled_from_env() {
        if let Some(obj) = req_body.as_object_mut() {
            obj.insert("thinking".to_string(), json!({ "type": "disabled" }));
        }
        tracing::debug!("DeepSeek: thinking.type=disabled inyectado (env)");
    }

    stream_openai_compatible_chat_completions("DeepSeek", &url, &api_key, req_body, log_ctx).await
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
    let mut perf = relay_perf("handler");
    perf_step(&mut perf, "00_enter_handler");

    let token = bearer_token(&headers)?;
    perf_step(&mut perf, "01_bearer_token_ok");
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = 5;
    let token_data =
        jsonwebtoken::decode::<RelayClaims>(&token, &st.decoding_key, &validation)?;
    let claims = token_data.claims;
    perf_step(&mut perf, "02_jwt_decoded");

    if claims.interview_id != interview_id {
        return Err(RelayError::IdMismatch);
    }
    if let Some(iat) = claims.iat {
        let ttl = claims.exp.saturating_sub(iat);
        if ttl > MAX_TOKEN_TTL.as_secs() as i64 {
            return Err(RelayError::TtlTooLong);
        }
    }
    perf_step(&mut perf, "03_claims_validated");

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
    perf_step(&mut perf, "04_redis_rate_limit_ok");

    let RelayBody {
        messages: body_msgs,
        max_output_tokens: body_tokens,
        speed,
    } = body;

    if body_msgs.is_empty() {
        return Err(RelayError::EmptyMessages);
    }

    let mut messages = body_msgs
        .into_iter()
        .take(st.ai_config.max_history_messages)
        .collect::<Vec<_>>();
    normalize_openai_message_contents(&mut messages);

    let cap = st.ai_config.max_output_tokens_cap;
    let max_out = effective_output_max_tokens(body_tokens, cap);
    if let Some(requested) = body_tokens {
        if requested != max_out {
            tracing::debug!(
                requested,
                effective = max_out,
                cap,
                "max_output_tokens ajustado (mínimo 64 o techo .env)"
            );
        }
    }

    perf_step(&mut perf, "05_msgs_and_token_budget_ready");

    let req_ts =
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let sys_len = total_system_prompt_len_chars(&messages);
    let (log_model, upstream_lane): (String, &'static str) = match speed {
        RelaySpeed::Fast => (groq_fast_model_from_env(), "groq_fast"),
        RelaySpeed::Medium => (
            st.ai_config.model.clone(),
            "configured_provider",
        ),
    };
    info!(
        timestamp = %req_ts,
        interview_id,
        user_id = %user_id,
        max_output_tokens = max_out,
        system_prompt_len_chars = sys_len,
        speed = ?speed,
        upstream_lane,
        provider = ?st.ai_config.provider,
        model = %log_model,
        "relay request"
    );
    let stream_log = Arc::new(StreamLogCtx {
        request_ts_rfc3339: req_ts,
        upstream_body_started: Mutex::new(None),
        max_output_tokens: max_out,
        system_prompt_len_chars: sys_len,
        interview_id,
        user_id: user_id.clone(),
        provider: st.ai_config.provider.clone(),
        model: log_model.clone(),
        upstream_lane,
        first_fragment_logged: AtomicBool::new(false),
    });

    perf_step(&mut perf, "06_stream_log_ctx_ready");

    let stream = if speed == RelaySpeed::Fast {
        stream_groq_fast(
            st.ai_config.as_ref(),
            messages,
            max_out,
            Some(stream_log.clone()),
        )
        .await
        .map_err(RelayError::AiProvider)?
    } else {
        match st.ai_config.provider {
            AiProvider::OpenAi => {
                let config = st.ai_config.as_ref();
                stream_openai(config, messages, max_out, Some(stream_log.clone()))
                    .await
                    .map_err(RelayError::AiProvider)?
            }
            AiProvider::Claude => {
                let config = st.ai_config.as_ref();
                stream_anthropic(config, messages, max_out, Some(stream_log.clone()))
                    .await
                    .map_err(RelayError::AiProvider)?
            }
            AiProvider::DeepSeek => {
                let config = st.ai_config.as_ref();
                stream_deepseek(config, messages, max_out, Some(stream_log.clone()))
                    .await
                    .map_err(RelayError::AiProvider)?
            }
        }
    };

    perf_step(&mut perf, "07_upstream_boxed_stream_ready");

    let sse = Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)));

    perf_step(&mut perf, "08_response_sse_wrapped");

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

/// Carga variables desde disco antes del resto del arranque (incl. filtros de tracing).
/// systemd suele no tener `.env` en el cwd: usa `WorkingDirectory=...` o `RELAY_DOTENV_PATH=/ruta/.env`.
fn load_dotenv_files() -> Option<std::path::PathBuf> {
    if let Ok(raw) = std::env::var("RELAY_DOTENV_PATH") {
        let path = raw.trim();
        if !path.is_empty() {
            if dotenvy::from_path(path).is_ok() {
                return Some(std::path::PathBuf::from(path));
            }
        }
    }
    dotenvy::dotenv().ok()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dotenv_loaded_from = load_dotenv_files();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("interview_relay_sim=info".parse().unwrap())
                .add_directive("relay_perf=info".parse().unwrap()),
        )
        .init();

    if let Some(ref p) = dotenv_loaded_from {
        info!(path = %p.display(), ".env cargado desde disco");
    }

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
        AI_PROVIDER_env = %std::env::var("AI_PROVIDER").unwrap_or_else(|_| "(no definida)".to_string()),
        provider = ?ai_config.provider,
        model = %ai_config.model,
        max_output_tokens_cap = ai_config.max_output_tokens_cap,
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

