//! Pipeline entrevista: opener → deepener en un solo SSE.
//! La pregunta ya viene extraída en el último mensaje user.

use std::sync::Arc;

use async_stream::try_stream;
use futures::StreamExt;

use crate::config::AiConfig;
use crate::providers;
use crate::relay::body::{AgentType, RelayMessage, RelayValues};
use crate::relay::messages::{build_upstream_messages, split_interview_messages};
use crate::relay::prompts::PromptStore;
use crate::streaming::log::StreamLogCtx;
use crate::streaming::{stream_interview_finish_events, text_chunk_event, BoxedStream};

pub async fn stream_opener_then_deepener(
    config: Arc<AiConfig>,
    prompts: Arc<PromptStore>,
    values: RelayValues,
    client_messages: Vec<RelayMessage>,
    opener_log: Arc<StreamLogCtx>,
    deepener_log: Arc<StreamLogCtx>,
) -> Result<BoxedStream, String> {
    let (question, prior_history) = split_interview_messages(&client_messages);
    let question = question.trim();
    if question.is_empty() {
        return Err("el último mensaje user debe tener contenido".into());
    }
    let question = question.to_string();

    let opener_system = prompts.render_with_transcript(AgentType::Opener, &values, &question)?;
    let deepener_system =
        prompts.render_with_transcript(AgentType::Deepener, &values, &question)?;

    let stream = try_stream! {
        let mut opener_input = prior_history.clone();
        opener_input.push(RelayMessage {
            role: "user".into(),
            content: Some(question.clone()),
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

        let opener_text = opener_log.accumulated_output();

        let mut deepener_input = prior_history;
        deepener_input.extend([
            RelayMessage {
                role: "user".into(),
                content: Some(format!("PREGUNTA: {}", question.trim())),
                image_url: None,
            },
            RelayMessage {
                role: "assistant".into(),
                content: Some(opener_text.clone()),
                image_url: None,
            },
            RelayMessage {
                role: "user".into(),
                content: Some("[continúa]".into()),
                image_url: None,
            },
        ]);

        let deepener_upstream = build_upstream_messages(
            &deepener_system,
            deepener_input,
            config.max_history_messages,
        );

        let mut deepener_stream = providers::stream_agent(
            config.as_ref(),
            AgentType::Deepener,
            deepener_upstream,
            Some(deepener_log.clone()),
            false,
        )
        .await?;

        yield text_chunk_event(" ");

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

        for ev in stream_interview_finish_events(None, &opener_log, &deepener_log) {
            yield ev;
        }
    };

    Ok(Box::pin(stream))
}
