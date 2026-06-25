//! Pipeline entrevista: detector → opener → deepener en un solo SSE.

use std::sync::Arc;

use async_stream::try_stream;
use axum::response::sse::Event;
use futures::StreamExt;
use serde::Deserialize;

use crate::config::AiConfig;
use crate::providers;
use crate::relay::body::{AgentType, RelayMessage, RelayValues};
use crate::relay::messages::build_upstream_messages;
use crate::relay::prompts::PromptStore;
use crate::streaming::log::StreamLogCtx;
use crate::streaming::{stream_interview_finish_events, text_chunk_event, BoxedStream};

// ── Salida del detector ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DetectorOutput {
    question: Option<String>,
    intelligible: bool,
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

    let mut stream = providers::stream_agent(config, AgentType::Detector, messages, Some(log.clone()), false).await?;

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

// ── Pipeline completo ──────────────────────────────────────────────────────────

pub async fn stream_opener_then_deepener(
    config: Arc<AiConfig>,
    prompts: Arc<PromptStore>,
    values: RelayValues,
    client_messages: Vec<RelayMessage>,
    detector_log: Arc<StreamLogCtx>,
    opener_log: Arc<StreamLogCtx>,
    deepener_log: Arc<StreamLogCtx>,
) -> Result<BoxedStream, String> {
    // Extrae la transcripción del último mensaje de usuario.
    let transcript = client_messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .and_then(|m| m.content.as_deref())
        .unwrap_or("")
        .to_string();

    // Ejecuta el detector antes de abrir el stream SSE.
    let detector_result = run_detector(
        config.as_ref(),
        prompts.as_ref(),
        &values,
        &transcript,
        detector_log.clone(),
    )
    .await?;

    // Pre-renderiza los prompts del opener y deepener (puede fallar antes de emitir).
    let opener_system = prompts.render(AgentType::Opener, &values)?;
    let deepener_system = prompts.render(AgentType::Deepener, &values)?;

    let stream = try_stream! {
        // ── Evento: pregunta detectada ────────────────────────────────────────
        let question_data = serde_json::json!({
            "question": detector_result.question,
            "intelligible": detector_result.intelligible,
        })
        .to_string();
        yield Event::default().event("question").data(question_data);

        if !detector_result.intelligible {
            // Audio ininteligible → respuesta de recuperación y cierre.
            let msg = serde_json::json!(["Perdona, no te escuché bien, ¿me lo repites?"]).to_string();
            yield Event::default().data(msg);
            yield Event::default().data("[DONE]");
        } else {
            let clean_question = detector_result.question
                .filter(|q| !q.trim().is_empty())
                .unwrap_or_else(|| transcript.clone());

            // ── Opener ────────────────────────────────────────────────────────
            let opener_upstream = build_upstream_messages(
                &opener_system,
                vec![RelayMessage {
                    role: "user".into(),
                    content: Some(clean_question.clone()),
                    image_url: None,
                }],
                1,
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

            let opener_text = opener_log.accumulated_output();

            // ── Deepener ──────────────────────────────────────────────────────
            // Solo recibe el arranque del opener y la señal de continuación.
            // La pregunta no se pasa: el deepener solo necesita continuar lo que el opener dejó.
            // Las APIs exigen que el primer mensaje sea "user", por eso usamos [continúa] como
            // dummy inicial; el prompt del deepener ya instruye al modelo a ignorarlo.
            let deepener_input = if opener_text.trim().is_empty() {
                vec![RelayMessage {
                    role: "user".into(),
                    content: Some("[continúa]".into()),
                    image_url: None,
                }]
            } else {
                vec![
                    RelayMessage {
                        role: "user".into(),
                        content: Some("[continúa]".into()),
                        image_url: None,
                    },
                    RelayMessage {
                        role: "assistant".into(),
                        content: Some(opener_text),
                        image_url: None,
                    },
                    RelayMessage {
                        role: "user".into(),
                        content: Some("[continúa]".into()),
                        image_url: None,
                    },
                ]
            };

            let deepener_upstream = build_upstream_messages(&deepener_system, deepener_input, 4);

            let mut deepener_stream = providers::stream_agent(
                config.as_ref(),
                AgentType::Deepener,
                deepener_upstream,
                Some(deepener_log.clone()),
                false,
            )
            .await?;

            // El deepener va en negrita en el front (markdown **...**).
            let mut bold_open = false;
            while let Some(item) = deepener_stream.next().await {
                if !bold_open {
                    yield text_chunk_event("**");
                    bold_open = true;
                }
                yield item?;
            }
            if bold_open {
                yield text_chunk_event("**");
            }

            // ── Metadata + DONE ───────────────────────────────────────────────
            for ev in stream_interview_finish_events(
                Some(&detector_log),
                &opener_log,
                &deepener_log,
            ) {
                yield ev;
            }
        }
    };

    Ok(Box::pin(stream))
}
