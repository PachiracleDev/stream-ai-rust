//! Pipeline `assistant-relay` con Cerebras: responde la pregunta ya extraída (SSE).

use crate::app::AppState;
use crate::cerebras;
use crate::error::RelayError;
use crate::relay::body::RelayBody;
use crate::relay::messages::{split_interview_messages, validate_interview_messages};
use crate::streaming::BoxedStream;

fn build_cerebras_messages(
    question: &str,
    prior_history: &[crate::relay::body::RelayMessage],
) -> Vec<serde_json::Value> {
    use serde_json::json;

    let mut out: Vec<serde_json::Value> = prior_history
        .iter()
        .filter_map(|m| {
            let content = m.content.as_deref()?.trim();
            if content.is_empty() {
                return None;
            }
            Some(json!({ "role": m.role, "content": content }))
        })
        .collect();

    out.push(json!({ "role": "user", "content": question }));
    out
}

pub async fn stream_cerebras_interview(
    st: &AppState,
    body: RelayBody,
    interview_id: i64,
    user_id: &str,
) -> Result<BoxedStream, RelayError> {
    validate_interview_messages(&body.messages).map_err(RelayError::BadRequest)?;

    let config = &st
        .cerebras
        .as_ref()
        .ok_or_else(|| RelayError::AiProvider("CEREBRAS_API_KEY no configurada".into()))?
        .relay;

    let (question, prior_history) = split_interview_messages(&body.messages);
    let question = question.trim();
    if question.is_empty() {
        return Err(RelayError::BadRequest(
            "el último mensaje user debe tener contenido".into(),
        ));
    }

    let system_prompt = st.prompts.render_cerebras_qa(&body.values, body.kind);
    let messages = build_cerebras_messages(question, &prior_history);

    let req_ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    tracing::info!(
        timestamp = %req_ts,
        interview_id,
        user_id = %user_id,
        relay_mode = "cerebras",
        interview_kind = %body.kind.label(),
        model = %config.model,
        max_tokens = config.max_tokens,
        prior_messages = prior_history.len(),
        question_len = question.len(),
        "assistant-relay request"
    );

    cerebras::stream_answer(config, &system_prompt, messages)
        .await
        .map_err(RelayError::AiProvider)
}
