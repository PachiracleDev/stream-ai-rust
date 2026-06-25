use std::sync::Arc;

use jsonwebtoken::DecodingKey;

use crate::config::AiConfig;
use crate::rate_limit::RateLimiter;
use crate::relay::prompts::PromptStore;

#[derive(Clone)]
pub struct AppState {
    pub decoding_key: DecodingKey,
    pub limiter: Arc<RateLimiter>,
    pub expand_limiter: Arc<RateLimiter>,
    pub rate_limit_max: u32,
    pub ai_config: Arc<AiConfig>,
    pub prompts: Arc<PromptStore>,
}
