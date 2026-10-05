//! Handler `POST /interviews/:id/ai/question-detect`.
//!
//! El front manda el texto nuevo y el disparador. El backend arma el resto con
//! el estado de la entrevista y valida la salida antes de guardarla.

use std::time::Instant;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};

use crate::app::AppState;
use crate::auth::{bearer_token, decode_claims, validate_claims};
use crate::cerebras;
use crate::cerebras::detect::DetectAction;
use crate::error::RelayError;
use crate::relay::body::{DetectTrigger, QuestionDetectBody, RelayValues};
use crate::relay::detect_validate::apply_detect_validation;
use crate::session_memory::{QuestionSessionStore, SessionAction, SessionSnapshot};
use crate::turn_log;

fn default_detect_values() -> RelayValues {
    RelayValues {
        job_position: "Candidato".into(),
        regionalism: "Neutro".into(),
        response_language: "es".into(),
        profile_minimal: None,
        last_jobs: None,
        role_keywords: None,
        regional_expressions: None,
        regional_avoid: None,
        salary_expectation: None,
    }
}

fn build_user_payload(
    new_text: &str,
    recent_context: &str,
    trigger: DetectTrigger,
    session: &SessionSnapshot,
) -> Value {
    json!({
        "newText": new_text.trim(),
        "recentContext": recent_context.trim(),
        "trigger": match trigger {
            DetectTrigger::Auto => "auto",
            DetectTrigger::SilenceAfterPremise => "silence_after_premise",
            DetectTrigger::Manual => "manual",
        },
        "pendingContext": session.pending_context.as_deref().unwrap_or(""),
        "lastQuestion": session.last_question.as_deref().unwrap_or(""),
        "answeredQuestions": session.answered,
    })
}

fn response_json(output: &crate::cerebras::detect::DetectOutput) -> Value {
    json!({
        "action": output.action.as_str(),
        "question": output.question,
        "continuesLast": output.continues_last,
        "kind": output.kind.as_str(),
        "confidence": output.confidence.as_str(),
    })
}

pub async fn question_detect(
    State(st): State<AppState>,
    Path((interview_id,)): Path<(i64,)>,
    headers: HeaderMap,
    Json(body): Json<QuestionDetectBody>,
) -> Result<impl IntoResponse, RelayError> {
    let user_id = if st.skip_jwt {
        "dev".to_string()
    } else {
        let token = bearer_token(&headers)?;
        let claims = decode_claims(&token, &st.decoding_key)?;
        validate_claims(&claims, interview_id)?;
        claims.sub.as_key_segment()
    };

    let configs = st
        .cerebras
        .as_ref()
        .ok_or_else(|| RelayError::AiProvider("CEREBRAS_API_KEY no configurada".into()))?;

    let session_key = QuestionSessionStore::session_key(&user_id, interview_id);
    if let Some(values) = body.values.clone() {
        st.question_sessions
            .remember_prompt(&session_key, values, body.kind)
            .await;
    }

    let session = st
        .question_sessions
        .begin_seq(&session_key, body.seq)
        .await
        .map_err(|last_seq| RelayError::StaleSeq { last_seq })?;

    let stored = st.question_sessions.prompt_config(&session_key).await;
    let defaults = default_detect_values();
    let values = body
        .values
        .as_ref()
        .or(stored.as_ref().map(|cfg| &cfg.values))
        .unwrap_or(&defaults);
    let kind = body.values.as_ref().map(|_| body.kind).unwrap_or_else(|| {
        stored
            .as_ref()
            .map(|cfg| cfg.kind)
            .unwrap_or(body.kind)
    });
    let system_prompt = st.prompts.render_question_detect(values, kind);
    let user_payload = build_user_payload(&body.new_text, &body.recent_context, body.trigger, &session);

    let started = Instant::now();
    let model_result = cerebras::complete_detect(
        &configs.detect,
        &system_prompt,
        &user_payload.to_string(),
    )
    .await
    .map_err(RelayError::AiProvider)?;
    let latency_ms = started.elapsed().as_millis() as u64;

    let (result, corrections) = apply_detect_validation(
        model_result,
        body.trigger,
        &body.new_text,
        session.last_question.as_deref().unwrap_or(""),
    );

    let latest = st.question_sessions.is_latest(&session_key, body.seq).await;
    if latest {
        let action = match result.action {
            DetectAction::Respond => SessionAction::Respond {
                question: result.question.clone(),
                continues_last: result.continues_last,
            },
            DetectAction::Premise => SessionAction::Premise {
                context: result.context.clone(),
            },
            DetectAction::Wait => SessionAction::Wait,
            DetectAction::Ignore => SessionAction::Ignore,
        };
        st.question_sessions
            .commit_if_latest(&session_key, body.seq, action)
            .await;
    }

    tracing::info!(
        interview_id,
        user_id = %user_id,
        seq = body.seq,
        model = %configs.detect.model,
        action = result.action.as_str(),
        continues_last = result.continues_last,
        corrections = ?corrections,
        latency_ms,
        applied = latest,
        "question-detect"
    );

    turn_log::append_turn_log(turn_log::detect_log(
        interview_id,
        body.seq,
        user_payload,
        response_json(&result),
        latency_ms,
        &corrections,
    ));

    Ok(Json(response_json(&result)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_uses_backend_state_and_the_front_fields() {
        let session = SessionSnapshot {
            last_question: Some("Q1".into()),
            last_question_continues: false,
            answered: vec!["Q0".into()],
            pending_context: Some("Escenario".into()),
            last_seq: 3,
        };
        let payload = build_user_payload(" nuevo ", " antes ", DetectTrigger::Auto, &session);
        assert_eq!(payload["newText"], "nuevo");
        assert_eq!(payload["recentContext"], "antes");
        assert_eq!(payload["trigger"], "auto");
        assert_eq!(payload["pendingContext"], "Escenario");
        assert_eq!(payload["lastQuestion"], "Q1");
        assert_eq!(payload["answeredQuestions"][0], "Q0");
        assert!(payload.get("previousFragments").is_none());
    }
}
