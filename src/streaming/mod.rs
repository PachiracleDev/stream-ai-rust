pub mod anthropic;
pub mod anthropic_convert;
pub mod log;
pub mod openai_compat;
pub mod openai_responses;

use std::sync::Arc;

use axum::response::sse::Event;
use futures::stream::Stream;

use crate::streaming::log::StreamLogCtx;

pub type BoxedStream =
    std::pin::Pin<Box<dyn Stream<Item = Result<Event, String>> + Send>>;

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

/// Metadata final del pipeline detector + opener + deepener.
///
/// Detector y opener comparten modelo (Groq por defecto), por lo que sus tokens
/// se suman bajo `openerTokens` para simplificar la factura del cliente.
pub fn stream_interview_finish_events(
    detector: Option<&StreamLogCtx>,
    opener: &StreamLogCtx,
    deepener: &StreamLogCtx,
) -> Vec<Event> {
    let mut events = Vec::new();
    let detector_tokens = detector.and_then(|c| c.total_tokens()).unwrap_or(0);
    let opener_tokens = opener.total_tokens().unwrap_or(0);
    let deepener_tokens = deepener.total_tokens().unwrap_or(0);

    // Suma detector al opener: mismo modelo, mismo presupuesto.
    let opener_combined = detector_tokens + opener_tokens;
    let total = opener_combined + deepener_tokens;

    if total > 0 {
        let mut meta = serde_json::Map::new();
        meta.insert("openerTokens".into(), serde_json::json!(opener_combined));
        meta.insert("deepenerTokens".into(), serde_json::json!(deepener_tokens));
        meta.insert("totalTokens".into(), serde_json::json!(total));
        let data = serde_json::Value::Object(meta).to_string();
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
