//! Configuración desde variables de entorno.
//!
//! Cambia solo `MODEL_OPENER` / `MODEL_DEEPENER` / `MODEL_IMAGE_SOLVER` (y sus API keys/URLs)
//! para alternar entre Groq, Claude, DeepSeek u OpenAI/Azure.

const DEFAULT_OPENAI_URL: &str = "https://api.openai.com/v1/chat/completions";
const DEFAULT_DEEPSEEK_URL: &str = "https://api.deepseek.com/chat/completions";
const DEFAULT_GROQ_URL: &str = "https://api.groq.com/openai/v1/chat/completions";
const DEFAULT_GROQ_MODEL: &str = "openai/gpt-oss-20b";

const DEFAULT_MODEL_DETECTOR: &str = "groq";
const DEFAULT_MAX_TOKENS_DETECTOR: u32 = 256;
const DEFAULT_MODEL_OPENER: &str = "groq";
const DEFAULT_MAX_TOKENS_OPENER: u32 = 384;
const DEFAULT_MODEL_DEEPENER: &str = "groq";
const DEFAULT_MAX_TOKENS_DEEPENER: u32 = 512;
const DEFAULT_MODEL_IMAGE_SOLVER: &str = "gpt-5.4-nano";
const DEFAULT_MAX_TOKENS_IMAGE_SOLVER: u32 = 4000;

/// Proveedor upstream inferido del nombre en `MODEL_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamKind {
    OpenAi,
    Anthropic,
    DeepSeek,
    Groq,
}

impl UpstreamKind {
    pub fn log_label(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic",
            Self::DeepSeek => "DeepSeek",
            Self::Groq => "Groq",
        }
    }
}

/// Modelo + tokens para un agente (`opener`, `deepener`, `image-solver`).
#[derive(Debug, Clone)]
pub struct AgentModelConfig {
    pub upstream: UpstreamKind,
    /// Nombre del deployment/modelo que se envía al API upstream.
    pub model: String,
    pub max_tokens: u32,
}

#[derive(Debug, Clone)]
pub struct AiConfig {
    pub detector: AgentModelConfig,
    pub opener: AgentModelConfig,
    pub deepener: AgentModelConfig,
    pub image_solver: AgentModelConfig,
    pub temperature: f64,
    pub max_history_messages: usize,
    pub credentials: UpstreamCredentials,
}

#[derive(Debug, Clone)]
pub struct UpstreamCredentials {
    pub openai_api_key: Option<String>,
    pub openai_chat_url: String,
    pub anthropic_api_key: Option<String>,
    pub deepseek_api_key: Option<String>,
    pub deepseek_url: String,
    pub groq_api_key: Option<String>,
    pub groq_chat_url: String,
}

impl UpstreamCredentials {
    pub fn from_env() -> Self {
        Self {
            openai_api_key: non_empty_env("OPENAI_API_KEY"),
            openai_chat_url: std::env::var("OPENAI_CHAT_URL")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_OPENAI_URL.to_string()),
            anthropic_api_key: non_empty_env("ANTHROPIC_API_KEY"),
            deepseek_api_key: non_empty_env("DEEPSEEK_API_KEY"),
            deepseek_url: std::env::var("DEEPSEEK_URL")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_DEEPSEEK_URL.to_string()),
            groq_api_key: non_empty_env("GROQ_API_KEY"),
            groq_chat_url: DEFAULT_GROQ_URL.to_string(),
        }
    }

    pub fn api_key_for(&self, upstream: UpstreamKind) -> Result<&str, String> {
        match upstream {
            UpstreamKind::OpenAi => self
                .openai_api_key
                .as_deref()
                .ok_or_else(|| "OPENAI_API_KEY no configurada".into()),
            UpstreamKind::Anthropic => self
                .anthropic_api_key
                .as_deref()
                .ok_or_else(|| "ANTHROPIC_API_KEY no configurada".into()),
            UpstreamKind::DeepSeek => self
                .deepseek_api_key
                .as_deref()
                .ok_or_else(|| "DEEPSEEK_API_KEY no configurada".into()),
            UpstreamKind::Groq => self
                .groq_api_key
                .as_deref()
                .ok_or_else(|| "GROQ_API_KEY no configurada".into()),
        }
    }

    pub fn chat_url_for(&self, upstream: UpstreamKind) -> &str {
        match upstream {
            UpstreamKind::OpenAi => self.openai_chat_url.trim(),
            UpstreamKind::DeepSeek => self.deepseek_url.trim(),
            UpstreamKind::Groq => self.groq_chat_url.trim(),
            UpstreamKind::Anthropic => "https://api.anthropic.com/v1/messages",
        }
    }
}

/// `OPENAI_CHAT_URL` apunta a Responses API (`/responses`) en lugar de Chat Completions.
pub fn openai_uses_responses_api(url: &str) -> bool {
    url.to_ascii_lowercase().contains("/responses")
}

impl AiConfig {
    pub fn from_env() -> Self {
        let credentials = UpstreamCredentials::from_env();
        Self {
            detector: resolve_agent_model(
                "MODEL_DETECTOR",
                DEFAULT_MODEL_DETECTOR,
                "MAX_TOKENS_DETECTOR",
                DEFAULT_MAX_TOKENS_DETECTOR,
            ),
            opener: resolve_agent_model(
                "MODEL_OPENER",
                DEFAULT_MODEL_OPENER,
                "MAX_TOKENS_OPENER",
                DEFAULT_MAX_TOKENS_OPENER,
            ),
            deepener: resolve_agent_model(
                "MODEL_DEEPENER",
                DEFAULT_MODEL_DEEPENER,
                "MAX_TOKENS_DEEPENER",
                DEFAULT_MAX_TOKENS_DEEPENER,
            ),
            image_solver: resolve_agent_model(
                "MODEL_IMAGE_SOLVER",
                DEFAULT_MODEL_IMAGE_SOLVER,
                "MAX_TOKENS_IMAGE_SOLVER",
                DEFAULT_MAX_TOKENS_IMAGE_SOLVER,
            ),
            temperature: env_f64("INTERVIEW_AGENT_TEMPERATURE", 0.6).clamp(0.0, 2.0),
            max_history_messages: env_usize("INTERVIEW_AGENT_MAX_HISTORY", 10).clamp(4, 32),
            credentials,
        }
    }

    pub fn agent(&self, agent: crate::relay::body::AgentType) -> &AgentModelConfig {
        use crate::relay::body::AgentType;
        match agent {
            AgentType::Detector => &self.detector,
            AgentType::Opener => &self.opener,
            AgentType::Deepener => &self.deepener,
            AgentType::ImageSolver => &self.image_solver,
        }
    }
}

/// Infiere proveedor a partir del valor de `MODEL_*`.
///
/// Valores soportados explícitamente:
/// - `groq` → Groq + `openai/gpt-oss-20b`
/// - `openai/gpt-oss-20b`, `gpt-oss-*` → Groq
/// - `claude-opus-4-7`, `claude-*` → Anthropic
/// - `DeepSeek-V4-Flash`, `deepseek-*` → DeepSeek
/// - `gpt-5.4-nano`, `gpt-*` (excepto gpt-oss) → OpenAI / Azure (`OPENAI_CHAT_URL`)
/// - `llama-*`, `mixtral-*`, … → Groq con ese id de modelo
pub fn classify_model(raw: &str) -> (UpstreamKind, String) {
    let trimmed = raw.trim();
    let lower = trimmed.to_lowercase();

    if lower == "groq" {
        return (UpstreamKind::Groq, DEFAULT_GROQ_MODEL.to_string());
    }
    if lower.contains("deepseek") {
        return (UpstreamKind::DeepSeek, trimmed.to_string());
    }
    if lower.starts_with("claude") || lower.contains("opus") {
        return (UpstreamKind::Anthropic, trimmed.to_string());
    }
    if lower.contains("gpt-oss") || lower.starts_with("openai/gpt-oss") {
        return (UpstreamKind::Groq, trimmed.to_string());
    }
    if lower.starts_with("llama")
        || lower.starts_with("mixtral")
        || lower.starts_with("gemma")
        || lower.starts_with("qwen")
    {
        return (UpstreamKind::Groq, trimmed.to_string());
    }
    if lower.starts_with("gpt") || lower.starts_with('o') && lower.chars().nth(1).is_some_and(|c| c.is_ascii_digit()) {
        return (UpstreamKind::OpenAi, trimmed.to_string());
    }

    // Deployments Azure/OpenAI con nombres custom (p. ej. gpt-5.4-nano ya cubierto por gpt*)
    (UpstreamKind::OpenAi, trimmed.to_string())
}

/// Modelos GPT-OSS en Groq reservan tokens de completion para razonamiento interno.
pub fn is_gpt_oss_model(model: &str) -> bool {
    model.to_ascii_lowercase().contains("gpt-oss")
}

/// Piso de `max_tokens` para que GPT-OSS alcance a emitir `content` tras el razonamiento.
pub fn groq_gpt_oss_completion_tokens(requested: u32) -> u32 {
    requested.max(512)
}

fn resolve_agent_model(
    model_env: &str,
    default_model: &str,
    max_tokens_env: &str,
    default_max_tokens: u32,
) -> AgentModelConfig {
    let raw = std::env::var(model_env)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default_model.to_string());
    let (upstream, model) = classify_model(&raw);
    let max_tokens = env_u32(max_tokens_env, default_max_tokens).max(1);
    AgentModelConfig {
        upstream,
        model,
        max_tokens,
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn prompts_dir() -> std::path::PathBuf {
    std::env::var("RELAY_PROMPTS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("prompts"))
}

pub fn relay_first_chunk_deadline() -> std::time::Duration {
    const DEFAULT_SECS: u64 = 10;
    std::env::var("RELAY_FIRST_CHUNK_DEADLINE_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|v| std::time::Duration::from_secs(v.clamp(1, 600)))
        .unwrap_or_else(|| std::time::Duration::from_secs(DEFAULT_SECS))
}

pub fn upstream_attempt_count() -> u32 {
    const DEFAULT_EXTRA: u32 = 1;
    let extra = std::env::var("RELAY_FIRST_CHUNK_RETRY_MAX")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(DEFAULT_EXTRA)
        .min(10);
    1 + extra
}

pub fn deepseek_thinking_disabled() -> bool {
    match std::env::var("DEEPSEEK_DISABLE_THINKING") {
        Ok(s) if s == "1" || s.eq_ignore_ascii_case("true") => return true,
        _ => {}
    }
    matches!(
        std::env::var("DEEPSEEK_THINKING_MODE").ok().as_deref(),
        Some(s) if s.eq_ignore_ascii_case("disabled") || s.eq_ignore_ascii_case("off")
    )
}

pub fn load_dotenv_files() -> Option<std::path::PathBuf> {
    if let Ok(raw) = std::env::var("RELAY_DOTENV_PATH") {
        let path = raw.trim();
        if !path.is_empty() && dotenvy::from_path(path).is_ok() {
            return Some(std::path::PathBuf::from(path));
        }
    }
    dotenvy::dotenv().ok()
}

pub fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

pub fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_supported_models() {
        assert_eq!(
            classify_model("groq"),
            (UpstreamKind::Groq, DEFAULT_GROQ_MODEL.to_string())
        );
        assert_eq!(
            classify_model("claude-opus-4-7").0,
            UpstreamKind::Anthropic
        );
        assert_eq!(
            classify_model("DeepSeek-V4-Flash").0,
            UpstreamKind::DeepSeek
        );
        assert_eq!(
            classify_model("gpt-5.4-nano").0,
            UpstreamKind::OpenAi
        );
        assert_eq!(
            classify_model("openai/gpt-oss-20b"),
            (UpstreamKind::Groq, "openai/gpt-oss-20b".to_string())
        );
        assert!(is_gpt_oss_model("openai/gpt-oss-20b"));
        assert_eq!(groq_gpt_oss_completion_tokens(120), 512);
        assert_eq!(groq_gpt_oss_completion_tokens(512), 512);
    }
}
