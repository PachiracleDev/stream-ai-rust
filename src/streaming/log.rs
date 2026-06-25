use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

use tracing::info;

use crate::config::UpstreamKind;

/// Tokens consumidos (input + output) reportados por el upstream.
#[derive(Debug, Default)]
pub struct TokenUsage {
    input_tokens: AtomicU32,
    output_tokens: AtomicU32,
    total_tokens: AtomicU32,
}

impl TokenUsage {
    pub fn record_openai_style(&self, prompt: Option<u32>, completion: Option<u32>, total: Option<u32>) {
        if let Some(n) = prompt {
            self.input_tokens.store(n, Ordering::Relaxed);
        }
        if let Some(n) = completion {
            self.output_tokens.store(n, Ordering::Relaxed);
        }
        if let Some(n) = total {
            self.total_tokens.store(n, Ordering::Relaxed);
        }
    }

    pub fn record_input(&self, n: u32) {
        self.input_tokens.store(n, Ordering::Relaxed);
    }

    pub fn record_output(&self, n: u32) {
        self.output_tokens.store(n, Ordering::Relaxed);
    }

    pub fn total(&self) -> Option<u32> {
        let explicit = self.total_tokens.load(Ordering::Relaxed);
        if explicit > 0 {
            return Some(explicit);
        }
        let input = self.input_tokens.load(Ordering::Relaxed);
        let output = self.output_tokens.load(Ordering::Relaxed);
        if input > 0 || output > 0 {
            Some(input + output)
        } else {
            None
        }
    }
}

/// Contexto de logging por petición (TTFT, metadatos).
#[derive(Debug)]
pub struct StreamLogCtx {
    pub request_ts_rfc3339: String,
    upstream_body_started: Mutex<Option<std::time::Instant>>,
    pub max_output_tokens: u32,
    pub system_prompt_len_chars: usize,
    pub interview_id: i64,
    pub user_id: String,
    pub upstream: UpstreamKind,
    pub model: String,
    pub agent_type: String,
    first_fragment_logged: AtomicBool,
    pub token_usage: TokenUsage,
    accumulated_text: Mutex<String>,
}

impl StreamLogCtx {
    pub fn new(
        request_ts_rfc3339: String,
        max_output_tokens: u32,
        system_prompt_len_chars: usize,
        interview_id: i64,
        user_id: String,
        upstream: UpstreamKind,
        model: String,
        agent_type: String,
    ) -> Self {
        Self {
            request_ts_rfc3339,
            upstream_body_started: Mutex::new(None),
            max_output_tokens,
            system_prompt_len_chars,
            interview_id,
            user_id,
            upstream,
            model,
            agent_type,
            first_fragment_logged: AtomicBool::new(false),
            token_usage: TokenUsage::default(),
            accumulated_text: Mutex::new(String::new()),
        }
    }

    pub fn accumulated_output(&self) -> String {
        self.accumulated_text
            .lock()
            .expect("accumulated_text mutex poisoned")
            .clone()
    }

    pub fn total_tokens(&self) -> Option<u32> {
        self.token_usage.total()
    }

    pub fn mark_upstream_ready(&self) {
        let mut guard = self
            .upstream_body_started
            .lock()
            .expect("upstream_body_started mutex poisoned");
        *guard = Some(std::time::Instant::now());
    }

    pub fn on_sse_data_payload(&self, data: &str) {
        let t = data.trim();
        if t.is_empty() || t == "[DONE]" {
            return;
        }
        if let Some(chunk) = crate::streaming::event_text_chunk(t) {
            self.accumulated_text
                .lock()
                .expect("accumulated_text mutex poisoned")
                .push_str(&chunk);
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
            upstream = ?self.upstream,
            model = %self.model,
            agent_type = %self.agent_type,
            "relay primera respuesta modelo (TTFT)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_usage_sums_input_and_output() {
        let usage = TokenUsage::default();
        usage.record_input(100);
        usage.record_output(50);
        assert_eq!(usage.total(), Some(150));
    }

    #[test]
    fn token_usage_prefers_explicit_total() {
        let usage = TokenUsage::default();
        usage.record_openai_style(Some(100), Some(50), Some(999));
        assert_eq!(usage.total(), Some(999));
    }
}
