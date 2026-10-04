//! Handler `POST /interviews/:id/ai/question-detect`.
//!
//! El modelo decide todo (pregunta nueva, continuación, escenario, eco o relleno)
//! en cualquier idioma y para cualquier puesto. Rust solo guarda el estado de la
//! sesión y aplica la decisión.

use std::time::Instant;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

use crate::app::AppState;
use crate::auth::{bearer_token, decode_claims, validate_claims};
use crate::cerebras;
use crate::cerebras::detect::DetectAction;
use crate::error::RelayError;
use crate::relay::body::{QuestionDetectBody, RelayValues};
use crate::session_memory::{QuestionSessionStore, SessionSnapshot};

fn validate_body(body: &QuestionDetectBody) -> Result<(), String> {
    if body.session_id.trim().is_empty() {
        return Err("sessionId es obligatorio".into());
    }
    if body.text.trim().is_empty() {
        return Err("text es obligatorio".into());
    }
    Ok(())
}

fn default_detect_values() -> RelayValues {
    RelayValues {
        job_position: "Candidato".into(),
        regionalism: "Neutro".into(),
        response_language: "es".into(),
        profile_minimal: None,
        last_jobs: None,
        role_keywords: None,
    }
}

fn build_user_payload(current_fragment: &str, session: &SessionSnapshot) -> String {
    json!({
        "currentFragment": current_fragment.trim(),
        "pendingContext": session.pending_context.as_deref().unwrap_or(""),
        "lastQuestion": session.last_question.as_deref().unwrap_or(""),
        "answeredQuestions": session.answered,
        "previousFragments": session.paragraphs,
    })
    .to_string()
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

    validate_body(&body).map_err(RelayError::BadRequest)?;

    let configs = st
        .cerebras
        .as_ref()
        .ok_or_else(|| RelayError::AiProvider("CEREBRAS_API_KEY no configurada".into()))?;

    let session_key = QuestionSessionStore::session_key(&user_id, &body.session_id);
    let session = st.question_sessions.begin_turn(&session_key, &body.text).await;

    let defaults = default_detect_values();
    let values = body.values.as_ref().unwrap_or(&defaults);
    let system_prompt = st.prompts.render_question_detect(values, body.kind);
    let user_content = build_user_payload(&body.text, &session);

    let started = Instant::now();
    let result = cerebras::complete_detect(&configs.detect, &system_prompt, &user_content)
        .await
        .map_err(RelayError::AiProvider)?;

    tracing::info!(
        interview_id,
        session_id = %body.session_id,
        user_id = %user_id,
        model = %configs.detect.model,
        interview_kind = %body.kind.label(),
        action = ?result.action,
        continues_last = result.continues_last,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "question-detect"
    );

    let response = match result.action {
        DetectAction::Respond => {
            st.question_sessions
                .record_question(&session_key, &result.question, result.continues_last)
                .await;
            json!({
                "shouldRespond": true,
                "question": result.question,
                "intelligible": true,
            })
        }
        DetectAction::Premise => {
            st.question_sessions
                .set_pending_context(&session_key, &result.context)
                .await;
            json!({ "shouldRespond": false })
        }
        DetectAction::Ignore => json!({ "shouldRespond": false }),
    };

    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_exposes_session_state_to_the_model() {
        let session = SessionSnapshot {
            paragraphs: vec!["Hola".into()],
            answered: vec!["Q0".into()],
            last_question: Some("Q1".into()),
            pending_context: Some("Escenario".into()),
        };
        let payload: serde_json::Value =
            serde_json::from_str(&build_user_payload(" nuevo ", &session)).unwrap();
        assert_eq!(payload["currentFragment"], "nuevo");
        assert_eq!(payload["pendingContext"], "Escenario");
        assert_eq!(payload["lastQuestion"], "Q1");
        assert_eq!(payload["answeredQuestions"][0], "Q0");
        assert_eq!(payload["previousFragments"][0], "Hola");
    }
}
