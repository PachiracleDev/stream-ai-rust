//! Pipeline entrevista: detector → opener en un solo SSE.

use std::sync::Arc;

use async_stream::try_stream;
use axum::response::sse::Event;
use futures::StreamExt;
use serde::Deserialize;

use crate::config::AiConfig;
use crate::providers;
use crate::relay::body::{
    language_from_detector_or_hint, AgentType, InterviewLanguage, RelayMessage, RelayValues,
};
use crate::relay::messages::{build_upstream_messages, split_interview_messages};
use crate::relay::prompts::PromptStore;
use crate::streaming::log::StreamLogCtx;
use crate::streaming::{stream_interview_finish_events, BoxedStream};

// ── Salida del detector ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DetectorOutput {
    question: Option<String>,
    intelligible: bool,
    #[serde(default)]
    language: Option<String>,
}

// ── Ejecución del detector (no streaming: drena el stream y parsea JSON) ───────

async fn run_detector(
    config: &AiConfig,
    prompts: &PromptStore,
    values: &RelayValues,
    transcript: &str,
    log: Arc<StreamLogCtx>,
) -> Result<DetectorOutput, String> {
    let system = prompts.render(AgentType::Detector, values)?;
    let messages = build_upstream_messages(
        &system,
        vec![RelayMessage {
            role: "user".into(),
            content: Some(transcript.to_string()),
            image_url: None,
        }],
        1,
    );

    let mut stream = providers::stream_agent(
        config,
        AgentType::Detector,
        messages,
        Some(log.clone()),
        false,
    )
    .await?;

    // Drena el stream para que StreamLogCtx acumule el texto; no emitimos nada al cliente.
    while stream.next().await.is_some() {}

    let raw = log.accumulated_output();
    parse_detector_output(&raw)
}

fn parse_detector_output(raw: &str) -> Result<DetectorOutput, String> {
    // El modelo a veces añade ```json ... ``` — lo limpiamos antes de parsear.
    let trimmed = raw.trim();
    let json_str = trimmed
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    serde_json::from_str::<DetectorOutput>(json_str)
        .map_err(|e| format!("detector: JSON inválido ({e}) — raw: {raw:?}"))
}

fn localized_recovery(language: InterviewLanguage) -> &'static str {
    match language {
        InterviewLanguage::English => "Sorry, I didn't catch that. Could you repeat the question?",
        InterviewLanguage::Spanish => "Perdona, no te escuché bien, ¿me lo repites?",
    }
}

// ── Pipeline completo ──────────────────────────────────────────────────────────

pub async fn stream_detector_then_opener(
    config: Arc<AiConfig>,
    prompts: Arc<PromptStore>,
    values: RelayValues,
    client_messages: Vec<RelayMessage>,
    detector_log: Arc<StreamLogCtx>,
    opener_log: Arc<StreamLogCtx>,
) -> Result<BoxedStream, String> {
    // Extrae transcripción nueva + historial previo (user/assistant).
    let (transcript, prior_history) = split_interview_messages(&client_messages);

    // Ejecuta el detector antes de abrir el stream SSE.
    let detector_result = run_detector(
        config.as_ref(),
        prompts.as_ref(),
        &values,
        &transcript,
        detector_log.clone(),
    )
    .await?;

    // La pregunta detectada determina el idioma de toda la respuesta. Si el
    // detector no puede darlo, conservamos `responseLanguage` por compatibilidad.
    let language = language_from_detector_or_hint(
        detector_result.language.as_deref(),
        &values.response_language,
    );
    let mut response_values = values;
    response_values.response_language = language.prompt_label().to_string();

    // Pre-renderiza el prompt del opener (puede fallar antes de emitir).
    let opener_system = prompts.render(AgentType::Opener, &response_values)?;

    let stream = try_stream! {
        // ── Evento: pregunta detectada ────────────────────────────────────────
        let question_data = serde_json::json!({
            "question": detector_result.question,
            "intelligible": detector_result.intelligible,
            "language": language.code(),
        })
        .to_string();
        yield Event::default().event("question").data(question_data);

        if !detector_result.intelligible {
            // Audio ininteligible → respuesta de recuperación y cierre.
            let msg = serde_json::json!([localized_recovery(language)]).to_string();
            yield Event::default().data(msg);
            yield Event::default().data("[DONE]");
        } else {
            let clean_question = detector_result.question
                .filter(|q| !q.trim().is_empty())
                .unwrap_or_else(|| transcript.clone());

            // ── Opener ────────────────────────────────────────────────────────
            let mut opener_input = prior_history.clone();
            opener_input.push(RelayMessage {
                role: "user".into(),
                content: Some(clean_question.clone()),
                image_url: None,
            });

            let opener_upstream = build_upstream_messages(
                &opener_system,
                opener_input,
                config.max_history_messages,
            );

            let mut opener_stream = providers::stream_agent(
                config.as_ref(),
                AgentType::Opener,
                opener_upstream,
                Some(opener_log.clone()),
                false,
            )
            .await?;

            while let Some(item) = opener_stream.next().await {
                yield item?;
            }

            // ── Metadata + DONE ───────────────────────────────────────────────
            for ev in stream_interview_finish_events(
                Some(&detector_log),
                &opener_log,
            ) {
                yield ev;
            }
        }
    };

    Ok(Box::pin(stream))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_detector_output_with_optional_language_and_fences() {
        let output = parse_detector_output(
            "```json\n{\"question\":\"What is your approach?\",\"intelligible\":true,\"language\":\"en\"}\n```",
        )
        .unwrap();

        assert_eq!(output.question.as_deref(), Some("What is your approach?"));
        assert_eq!(output.language.as_deref(), Some("en"));
    }

    #[test]
    fn parses_legacy_detector_output_without_fences() {
        let output =
            parse_detector_output(r#"{"question":"¿Qué priorizas?","intelligible":true}"#).unwrap();

        assert_eq!(output.question.as_deref(), Some("¿Qué priorizas?"));
        assert_eq!(output.language, None);
    }

    #[test]
    fn recovery_is_localized() {
        assert!(localized_recovery(InterviewLanguage::English).starts_with("Sorry"));
        assert!(localized_recovery(InterviewLanguage::Spanish).starts_with("Perdona"));
    }
}
