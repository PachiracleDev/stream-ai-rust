//! Pipeline `assistant-relay` con Cerebras: responde la pregunta ya extraída (SSE).

use std::sync::Arc;

use serde_json::{json, Value};

use crate::app::AppState;
use crate::cerebras::stream_spoken_answer;
use crate::error::RelayError;
use crate::relay::body::{InterviewKind, RelayBody};
use crate::relay::messages::{split_interview_messages, validate_interview_messages};
use crate::session_memory::{AnswerPair, QuestionSessionStore};
use crate::streaming::BoxedStream;
use crate::turn_log;

const CONTINUATION_MARK: &str = "(Continuación de la pregunta anterior)";

fn prompt_kind(session_kind: InterviewKind, question_kind: Option<&str>) -> InterviewKind {
    match question_kind.unwrap_or("").trim() {
        "technical" => InterviewKind::Technical,
        "behavioral" | "logistics" | "candidate_questions" | "smalltalk" => InterviewKind::Hr,
        _ => session_kind,
    }
}

fn user_question(question: &str, continues_last: bool) -> String {
    let question = question.trim();
    if continues_last {
        format!("{CONTINUATION_MARK}\n{question}")
    } else {
        question.to_string()
    }
}

fn history_messages(pairs: &[AnswerPair]) -> Vec<Value> {
    pairs
        .iter()
        .flat_map(|pair| {
            [
                json!({ "role": "user", "content": pair.question }),
                json!({ "role": "assistant", "content": pair.answer }),
            ]
        })
        .collect()
}

fn build_cerebras_messages(question: &str, prior_history: &[crate::relay::body::RelayMessage]) -> Vec<Value> {
    let mut out: Vec<Value> = prior_history
        .iter()
        .filter_map(|message| {
            let content = message.content.as_deref()?.trim();
            if content.is_empty() {
                return None;
            }
            Some(json!({ "role": message.role, "content": content }))
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
    let config = &st
        .cerebras
        .as_ref()
        .ok_or_else(|| RelayError::AiProvider("CEREBRAS_API_KEY no configurada".into()))?
        .relay;

    let session_key = QuestionSessionStore::session_key(user_id, interview_id);
    st.question_sessions
        .remember_prompt(&session_key, body.values.clone(), body.kind)
        .await;

    let (question, messages) = if let Some(question) = body.question.as_deref().map(str::trim).filter(|q| !q.is_empty())
    {
        let sent = user_question(question, body.continues_last);
        let mut messages = history_messages(&st.question_sessions.history(&session_key).await);
        messages.push(json!({ "role": "user", "content": sent.clone() }));
        (sent, messages)
    } else {
        validate_interview_messages(&body.messages).map_err(RelayError::BadRequest)?;
        let (question, prior_history) = split_interview_messages(&body.messages);
        let question = question.trim();
        if question.is_empty() {
            return Err(RelayError::BadRequest(
                "el último mensaje user debe tener contenido".into(),
            ));
        }
        let question = question.to_string();
        let messages = build_cerebras_messages(&question, &prior_history);
        (question, messages)
    };

    let kind = prompt_kind(body.kind, body.question_kind.as_deref());
    let system_prompt = st.prompts.render_cerebras_qa(&body.values, kind);

    let req_ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    tracing::info!(
        timestamp = %req_ts,
        interview_id,
        user_id = %user_id,
        relay_mode = "cerebras",
        interview_kind = %kind.label(),
        model = %config.model,
        max_tokens = config.max_tokens,
        prior_messages = messages.len().saturating_sub(1),
        question_len = question.len(),
        continues_last = body.continues_last,
        "assistant-relay request"
    );

    let store = st.question_sessions.clone();
    let question_for_log = question.clone();
    let on_finish: Arc<dyn Fn(String, bool, bool) + Send + Sync> =
        Arc::new(move |response, cancelled, regenerated| {
            let store = store.clone();
            let key = session_key.clone();
            let question = question_for_log.clone();
            tokio::spawn(async move {
                store.push_answer(&key, &question, &response).await;
                turn_log::append_turn_log(turn_log::answer_log(
                    interview_id,
                    &question,
                    &response,
                    cancelled,
                    regenerated,
                ));
            });
        });

    stream_spoken_answer(config, &system_prompt, messages, on_finish)
        .await
        .map_err(RelayError::AiProvider)
}
