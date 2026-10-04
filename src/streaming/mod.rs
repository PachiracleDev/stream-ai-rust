pub mod anthropic;
pub mod anthropic_convert;
pub mod log;
pub mod openai_compat;
pub mod openai_responses;

use std::sync::Arc;

use axum::response::sse::Event;
use futures::stream::Stream;

use crate::streaming::log::StreamLogCtx;

pub type BoxedStream = std::pin::Pin<Box<dyn Stream<Item = Result<Event, String>> + Send>>;

/// Metadata final del relay con un solo agente deepener (p. ej. expand-response).
pub fn stream_deepener_finish_events(log_ctx: &StreamLogCtx) -> Vec<Event> {
    let mut events = Vec::new();
    if let Some(deepener_tokens) = log_ctx.total_tokens() {
        let data = serde_json::json!({
            "deepenerTokens": deepener_tokens,
            "totalTokens": deepener_tokens,
        })
        .to_string();
        events.push(Event::default().event("metadata").data(data));
    }
    events.push(Event::default().data("[DONE]"));
    events
}

/// Eventos finales del relay: metadata de tokens (si hay) y cierre `[DONE]`.
pub fn stream_finish_events(log_ctx: Option<&Arc<StreamLogCtx>>) -> Vec<Event> {
    let mut events = Vec::new();
    if let Some(total) = log_ctx.and_then(|c| c.total_tokens()) {
        let data = serde_json::json!({ "totalTokens": total }).to_string();
        events.push(Event::default().event("metadata").data(data));
    }
    events.push(Event::default().data("[DONE]"));
    events
}

pub(crate) fn finish_events(log_ctx: Option<&Arc<StreamLogCtx>>, emit_finish: bool) -> Vec<Event> {
    if emit_finish {
        stream_finish_events(log_ctx)
    } else {
        Vec::new()
    }
}

/// Extrae texto concatenable de un payload SSE del relay (`["fragmento"]`).
pub fn event_text_chunk(data: &str) -> Option<String> {
    let t = data.trim();
    if t.is_empty() || t == "[DONE]" {
        return None;
    }
    if let Ok(parts) = serde_json::from_str::<Vec<String>>(t) {
        if parts.is_empty() {
            return None;
        }
        return Some(parts.join(""));
    }
    None
}

/// Evento SSE con un fragmento de texto del modelo (`["..."]`).
pub fn text_chunk_event(text: &str) -> Event {
    Event::default().data(serde_json::json!([text]).to_string())
}

