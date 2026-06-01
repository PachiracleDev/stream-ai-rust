use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tracing::info;

use crate::config::UpstreamKind;

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
        }
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
