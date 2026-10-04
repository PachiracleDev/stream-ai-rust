use std::sync::Arc;

use jsonwebtoken::DecodingKey;

use crate::config::{AiConfig, CerebrasConfigs};
use crate::rate_limit::RateLimiter;
use crate::relay::prompts::PromptStore;
use crate::session_memory::QuestionSessionStore;

#[derive(Clone)]
pub struct AppState {
    pub decoding_key: DecodingKey,
    pub skip_jwt: bool,
    pub limiter: Arc<RateLimiter>,
    pub expand_limiter: Arc<RateLimiter>,
    pub translation_limiter: Arc<RateLimiter>,
    pub translation_rate_limit_max: u32,
    pub rate_limit_max: u32,
    pub ai_config: Arc<AiConfig>,
    pub cerebras: Option<CerebrasConfigs>,
    pub question_sessions: QuestionSessionStore,
    pub prompts: Arc<PromptStore>,
}
