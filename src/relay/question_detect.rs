//! Handler `POST /interviews/:id/ai/question-detect`.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::Json;

use crate::app::AppState;
use crate::auth::{bearer_token, decode_claims, validate_claims};
use crate::cerebras;
use crate::error::RelayError;
use crate::relay::body::{QuestionDetectBody, RelayValues};
use crate::session_memory::{normalize_question, QuestionSessionStore};

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

fn build_user_payload(
    paragraphs: &[String],
    last_question: Option<&str>,
    answered: &[String],
) -> String {
    let last = last_question
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("(ninguna)");

    let history = if answered.is_empty() {
        "(ninguna)".to_string()
    } else {
        answered
            .iter()
            .enumerate()
            .map(|(i, q)| format!("{}. {q}", i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        "paragraphs:\n{}\n\nlastQuestion:\n{last}\n\nansweredQuestions:\n{history}",
        paragraphs
            .iter()
            .enumerate()
            .map(|(i, p)| format!("{}. {p}", i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    )
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

    // Fast-path: ruido/ack obvio → respuesta inmediata sin llamar al modelo
    // y sin ensuciar la memoria de párrafos de la sesión.
    if is_obvious_filler(&body.text) {
        tracing::debug!(
            interview_id,
            session_id = %body.session_id,
            "question-detect fast-path: filler"
        );
        return Ok(Json(serde_json::json!({ "shouldRespond": false })));
    }

    let configs = st
        .cerebras
        .as_ref()
        .ok_or_else(|| RelayError::AiProvider("CEREBRAS_API_KEY no configurada".into()))?;

    let session_key = QuestionSessionStore::session_key(&user_id, &body.session_id);
    let paragraphs = st
        .question_sessions
        .push_paragraph(&session_key, &body.text)
        .await;
    let answered = st.question_sessions.answered_questions(&session_key).await;
    let last_question = st.question_sessions.last_question(&session_key).await;

    let defaults = default_detect_values();
    let values = body.values.as_ref().unwrap_or(&defaults);
    let system_prompt = st.prompts.render_question_detect(values, body.kind);
    let user_content = build_user_payload(&paragraphs, last_question.as_deref(), &answered);

    tracing::info!(
        interview_id,
        session_id = %body.session_id,
        user_id = %user_id,
        model = %configs.detect.model,
        paragraphs = paragraphs.len(),
        answered_count = answered.len(),
        interview_kind = %body.kind.label(),
        "question-detect request"
    );

    let mut result = cerebras::complete_detect(&configs.detect, &system_prompt, &user_content)
        .await
        .map_err(RelayError::AiProvider)?;

    // Normalizar: si no hay pregunta, no hay nada que responder.
    let Some(question) = result.question.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(Json(serde_json::json!({ "shouldRespond": false })));
    };
    let mut question = question.to_string();

    // Si el modelo detectó una continuación, combinar con lastQuestion.
    if result.is_continuation {
        if let Some(ref last) = last_question {
            let combined = combine_continuation(last, &question);
            if !same_question(last, &combined) {
                question = combined;
                result.should_respond = true;
            }
        }
    }

    // Determinar si realmente debemos responder.
    let decision = evaluate_question(&st, &session_key, &question, &answered, last_question.as_deref()).await;

    let response = match decision {
        DetectDecision::Respond { question } => {
            // Si había una pregunta pendiente distinta y la nueva NO la contiene
            // (no es continuación), la anterior quedó superada → márcala respondida.
            // Así answeredQuestions crece en el flujo normal y las referencias a
            // preguntas pasadas ("eso", "la primera", "that project") tienen contexto.
            if let Some(prev) = last_question.as_deref() {
                let nprev = normalize_question(prev);
                if !nprev.is_empty() && !normalize_question(&question).contains(&nprev) {
                    st.question_sessions.record_answered(&session_key, prev).await;
                }
            }
            st.question_sessions
                .set_last_question(&session_key, &question)
                .await;
            serde_json::json!({
                "shouldRespond": true,
                "question": question,
                "intelligible": result.intelligible,
            })
        }
        DetectDecision::AlreadyAnswered { question } => {
            // La pregunta (o su combinación) ya fue respondida: marcarla formalmente.
            st.question_sessions.record_answered(&session_key, &question).await;
            serde_json::json!({
                "shouldRespond": false,
                "question": "",
                "intelligible": false,
            })
        }
        DetectDecision::Repeat => {
            // Repetición exacta de la última pregunta enviada: confirmar como respondida.
            if let Some(ref last) = last_question {
                st.question_sessions.record_answered(&session_key, last).await;
            }
            serde_json::json!({
                "shouldRespond": false,
                "question": "",
                "intelligible": false,
            })
        }
        DetectDecision::NoQuestion => {
            serde_json::json!({ "shouldRespond": false })
        }
    };

    Ok(Json(response))
}

#[derive(Debug)]
enum DetectDecision {
    /// Pregunta nueva o continuación ampliada que aún no respondimos.
    Respond { question: String },
    /// La pregunta ya está en answeredQuestions (respuesta real anterior).
    AlreadyAnswered { question: String },
    /// Igual a lastQuestion: repetición, nada que hacer.
    Repeat,
    /// Sin pregunta inteligible.
    NoQuestion,
}

async fn evaluate_question(
    st: &AppState,
    session_key: &str,
    question: &str,
    answered: &[String],
    last_question: Option<&str>,
) -> DetectDecision {
    // 1. Repetición exacta de la última pregunta pendiente → no responder.
    if last_question.is_some_and(|last| same_question(last, question)) {
        return DetectDecision::Repeat;
    }

    // 2. Ya respondida en el historial de la sesión → no responder.
    if st.question_sessions.already_answered(session_key, question).await {
        return DetectDecision::AlreadyAnswered {
            question: question.into(),
        };
    }

    // 3. Si no hay lastQuestion, pero la nueva pregunta parece una continuación
    //    de algo respondido anteriormente, considerarla respondida.
    if last_question.is_none() {
        for answered_q in answered {
            if questions_same(answered_q, question) {
                return DetectDecision::AlreadyAnswered {
                    question: question.into(),
                };
            }
        }
    }

    DetectDecision::Respond {
        question: question.into(),
    }
}

fn same_question(a: &str, b: &str) -> bool {
    normalize_question(a) == normalize_question(b)
}

/// Ruido/ack obvio que jamás es pregunta: "ok", "vale", "ajá", "gracias", "perfecto"...
/// Conservador: cualquier signo de interrogación o palabra fuera de la lista → va al modelo.
/// Así follow-ups cortos como "¿Cómo?" siempre llegan al detector.
fn is_obvious_filler(text: &str) -> bool {
    let raw = text.trim();
    if raw.is_empty() {
        return true;
    }
    if raw.contains('?') || raw.contains('¿') {
        return false;
    }
    let normalized = normalize_question(raw);
    if normalized.is_empty() {
        return true;
    }
    let words: Vec<&str> = normalized.split_whitespace().collect();
    if words.len() > 4 {
        return false;
    }
    const FILLER: &[&str] = &[
        "ok", "okay", "vale", "aja", "ajá", "si", "sí", "no", "gracias", "thanks", "thank",
        "you", "perfecto", "perfect", "entendido", "claro", "sure", "bueno", "listo", "genial",
        "great", "de", "acuerdo", "mmm", "mhm", "exacto", "exactly", "correcto", "right",
        "interesante", "interesting", "ya", "veo", "see", "got", "it", "bien", "dale", "ah",
        "oh", "wow", "good", "nice", "cool", "fine", "alright", "yep", "yeah", "nop", "nope",
    ];
    words.iter().all(|w| FILLER.contains(w))
}

fn questions_same(a: &str, b: &str) -> bool {
    let na = normalize_question(a);
    let nb = normalize_question(b);
    na == nb || na.contains(&nb) || nb.contains(&na)
}

/// Combina lastQuestion con la continuación sin repetir palabras superpuestas.
fn combine_continuation(last: &str, continuation: &str) -> String {
    let last_trim = last.trim();
    let cont_trim = continuation.trim();

    if last_trim.is_empty() {
        return cont_trim.to_string();
    }
    if cont_trim.is_empty() {
        return last_trim.to_string();
    }

    // Si la continuación ya contiene toda la última pregunta, devolver la continuación.
    if cont_trim.to_lowercase().contains(&last_trim.to_lowercase()) {
        return cont_trim.to_string();
    }

    // Buscar el mayor sufijo de `last` que coincide con prefijo de `continuation`,
    // ignorando puntuación y mayúsculas.
    let cont_words: Vec<String> = cont_trim.split_whitespace().map(clean_word).collect();
    let last_words: Vec<String> = last_trim.split_whitespace().map(clean_word).collect();
    let mut overlap = 0usize;
    let last_chars: Vec<char> = last_trim.chars().collect();

    for i in (1..=cont_words.len().min(last_words.len())).rev() {
        let suffix = &last_words[last_words.len() - i..];
        let prefix = &cont_words[..i];
        if suffix != prefix {
            continue;
        }

        // Si la continuación contiene toda la pregunta anterior como prefijo, usarla tal cual.
        if i == last_words.len() {
            return cont_trim.to_string();
        }

        // Evitar overlaps que terminen en puntuación terminal en `last` (dejaría signos raros).
        let suffix_start = last_words.len() - i;
        let last_word_idx = suffix_start + i - 1;
        if let Some(last_word_raw) = last_trim.split_whitespace().nth(last_word_idx) {
            if let Some(last_char) = last_word_raw.chars().last() {
                if ".!?;".contains(last_char) {
                    continue;
                }
            }
        }

        overlap = i;
        break;
    }

    if overlap > 0 {
        let rest = cont_trim
            .split_whitespace()
            .skip(overlap)
            .collect::<Vec<_>>()
            .join(" ");
        return format!("{} {}", last_trim, rest).trim().to_string();
    }

    format!("{} {}", last_trim, cont_trim).trim().to_string()
}

fn clean_word(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combine_continuation_appends_unique_part() {
        let combined = combine_continuation(
            "Tell me about a project that failed.",
            "What happened, and what did you learn?",
        );
        assert_eq!(
            combined,
            "Tell me about a project that failed. What happened, and what did you learn?"
        );
    }

    #[test]
    fn combine_continuation_avoids_overlap() {
        let combined = combine_continuation(
            "What happened, and what did you learn?",
            "What happened, and what did you learn from it?",
        );
        assert_eq!(combined, "What happened, and what did you learn from it?");
    }

    #[test]
    fn combine_continuation_uses_full_when_cont_contains_last() {
        let combined = combine_continuation(
            "What happened?",
            "Tell me about a project that failed. What happened?",
        );
        assert_eq!(
            combined,
            "Tell me about a project that failed. What happened?"
        );
    }

    #[test]
    fn filler_detects_obvious_acks() {
        assert!(is_obvious_filler("ok"));
        assert!(is_obvious_filler("Vale, perfecto"));
        assert!(is_obvious_filler("thank you"));
        assert!(is_obvious_filler("ajá"));
        assert!(is_obvious_filler("..."));
    }

    #[test]
    fn filler_never_blocks_short_followups() {
        // Follow-ups cortos SIEMPRE van al modelo aunque sean 1-2 palabras.
        assert!(!is_obvious_filler("¿Cómo?"));
        assert!(!is_obvious_filler("Why?"));
        assert!(!is_obvious_filler("¿Y eso?"));
        assert!(!is_obvious_filler("How so?"));
        assert!(!is_obvious_filler("¿cuál prefieres tú"));
    }

    #[test]
    fn filler_ignores_longer_or_mixed_text() {
        assert!(!is_obvious_filler("ok entonces cuéntame de tu experiencia"));
        assert!(!is_obvious_filler("gracias, ahora hablemos de arquitectura de software"));
    }
}
