//! Normalización de mensajes del cliente al formato Chat Completions / Messages API.

use serde_json::{json, Value};

use crate::relay::body::{AgentType, RelayMessage};

pub fn build_upstream_messages(
    system_prompt: &str,
    raw: Vec<RelayMessage>,
    max_history: usize,
) -> Vec<Value> {
    let mut out = vec![json!({ "role": "system", "content": system_prompt })];
    for msg in raw.into_iter().take(max_history) {
        if let Some(v) = relay_message_to_openai(msg) {
            out.push(v);
        }
    }
    out
}

fn relay_message_to_openai(msg: RelayMessage) -> Option<Value> {
    let role = msg.role;
    let text = msg.content.unwrap_or_default();
    let image = msg.image_url.filter(|u| !u.trim().is_empty());

    match (text.trim().is_empty(), image) {
        (true, None) => None,
        (false, None) => Some(json!({ "role": role, "content": text })),
        (true, Some(url)) => Some(json!({
            "role": role,
            "content": [{
                "type": "image_url",
                "image_url": { "url": url }
            }]
        })),
        (false, Some(url)) => Some(json!({
            "role": role,
            "content": [
                { "type": "text", "text": text },
                { "type": "image_url", "image_url": { "url": url } }
            ]
        })),
    }
}

pub fn validate_request(agent: AgentType, messages: &[RelayMessage]) -> Result<(), String> {
    if messages.is_empty() {
        return Err("messages es obligatorio y no puede estar vacío".into());
    }
    if agent.uses_vision() {
        let has_image = messages.iter().any(|m| {
            m.image_url
                .as_ref()
                .is_some_and(|u| !u.trim().is_empty())
        });
        if !has_image {
            return Err(
                "type image-solver requiere imageUrl en al menos un mensaje".into(),
            );
        }
    }
    Ok(())
}

pub fn system_prompt_len_chars(system: &str) -> usize {
    system.chars().count()
}
