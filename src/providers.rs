//! Enrutamiento upstream: un `MODEL_*` → proveedor + URL + API key.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::config::{deepseek_thinking_disabled, openai_uses_responses_api, AiConfig, UpstreamKind};
use crate::relay::body::AgentType;
use crate::streaming::anthropic;
use crate::streaming::log::StreamLogCtx;
use crate::streaming::openai_compat;
use crate::streaming::openai_responses;
use crate::streaming::BoxedStream;

pub async fn stream_agent(
    config: &AiConfig,
    agent: AgentType,
    messages: Vec<Value>,
    log_ctx: Option<Arc<StreamLogCtx>>,
    emit_finish: bool,
) -> Result<BoxedStream, String> {
    let binding = config.agent(agent);
    let max_output_tokens = binding.max_tokens;
    let upstream = binding.upstream;
    let model = binding.model.clone();
    let label = upstream.log_label();

    let api_key = config.credentials.api_key_for(upstream)?.to_string();

    match upstream {
        UpstreamKind::Anthropic => {
            anthropic::stream_messages(&model, messages, max_output_tokens, log_ctx, emit_finish)
                .await
        }
        UpstreamKind::OpenAi => {
            let url = config.credentials.chat_url_for(upstream);
            if openai_uses_responses_api(url) {
                let req_body = openai_responses::chat_messages_to_responses_body(
                    &model,
                    messages,
                    config.temperature,
                    max_output_tokens,
                )?;
                openai_responses::stream_responses(url, &api_key, req_body, log_ctx, emit_finish)
                    .await
            } else {
                let req_body = json!({
                    "model": model,
                    "messages": messages,
                    "stream": true,
                    "stream_options": { "include_usage": true },
                    "temperature": config.temperature,
                    "max_completion_tokens": max_output_tokens,
                });
                openai_compat::stream_chat_completions(
                    label,
                    url,
                    &api_key,
                    req_body,
                    log_ctx,
                    emit_finish,
                )
                .await
            }
        }
        UpstreamKind::Groq => {
            let url = config.credentials.chat_url_for(upstream);
            let max_tokens = if crate::config::is_gpt_oss_model(&model) {
                crate::config::groq_gpt_oss_completion_tokens(max_output_tokens)
            } else {
                max_output_tokens
            };
            let mut req_body = json!({
                "model": model,
                "messages": messages,
                "stream": true,
                "stream_options": { "include_usage": true },
                "temperature": config.temperature,
                "max_tokens": max_tokens,
            });
            if crate::config::is_gpt_oss_model(&model) {
                if let Some(obj) = req_body.as_object_mut() {
                    // Sin esto Groq emite delta.reasoning y no delta.content → stream vacío.
                    obj.insert("reasoning_format".into(), json!("hidden"));
                }
            }
            openai_compat::stream_chat_completions(
                label,
                url,
                &api_key,
                req_body,
                log_ctx,
                emit_finish,
            )
            .await
        }
        UpstreamKind::DeepSeek => {
            let url = config.credentials.chat_url_for(upstream);
            let mut req_body = json!({
                "model": model,
                "messages": messages,
                "stream": true,
                "stream_options": { "include_usage": true },
                "temperature": config.temperature,
                "max_tokens": max_output_tokens,
            });
            if deepseek_thinking_disabled() {
                if let Some(obj) = req_body.as_object_mut() {
                    obj.insert("thinking".into(), json!({ "type": "disabled" }));
                }
            }
            openai_compat::stream_chat_completions(
                label,
                url,
                &api_key,
                req_body,
                log_ctx,
                emit_finish,
            )
            .await
        }
    }
}
