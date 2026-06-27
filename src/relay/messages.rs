//! Normalización de mensajes del cliente al formato Chat Completions / Messages API.

use serde_json::{json, Value};

use crate::relay::body::RelayMessage;

pub fn build_upstream_messages(
    system_prompt: &str,
    raw: Vec<RelayMessage>,
    max_history: usize,
) -> Vec<Value> {
    let mut out = vec![json!({ "role": "system", "content": system_prompt })];
    let start = raw.len().saturating_sub(max_history);
    for msg in raw.into_iter().skip(start) {
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

pub fn validate_interview_messages(messages: &[RelayMessage]) -> Result<(), String> {
    if messages.is_empty() {
        return Err("messages es obligatorio y no puede estar vacío".into());
    }
    Ok(())
}

pub fn messages_have_image(messages: &[RelayMessage]) -> bool {
    messages.iter().any(|m| {
        m.image_url
            .as_ref()
            .is_some_and(|u| !u.trim().is_empty())
    })
}

pub fn validate_image_solver(messages: &[RelayMessage]) -> Result<(), String> {
    if messages.is_empty() {
        return Err("messages es obligatorio y no puede estar vacío".into());
    }
    if !messages_have_image(messages) {
        return Err("imageUrl es obligatorio en al menos un mensaje".into());
    }
    Ok(())
}

pub fn system_prompt_len_chars(system: &str) -> usize {
    system.chars().count()
}

/// Separa el historial previo del turno actual.
/// - `transcript`: contenido del último mensaje `user` (audio transcrito nuevo).
/// - `prior`: mensajes `user` / `assistant` anteriores (respuestas ya dadas).
pub fn split_interview_messages(messages: &[RelayMessage]) -> (String, Vec<RelayMessage>) {
    let last_user_idx = messages.iter().rposition(|m| {
        m.role == "user" && m.content.as_deref().is_some_and(|c| !c.trim().is_empty())
    });

    let Some(idx) = last_user_idx else {
        return (String::new(), Vec::new());
    };

    let transcript = messages[idx]
        .content
        .clone()
        .unwrap_or_default();

    let prior = messages[..idx]
        .iter()
        .filter(|m| is_history_role(&m.role) && message_has_content(m))
        .cloned()
        .collect();

    (transcript, prior)
}

fn is_history_role(role: &str) -> bool {
    matches!(role, "user" | "assistant")
}

fn message_has_content(msg: &RelayMessage) -> bool {
    msg.content
        .as_deref()
        .is_some_and(|c| !c.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> RelayMessage {
        RelayMessage {
            role: role.into(),
            content: Some(content.into()),
            image_url: None,
        }
    }

    #[test]
    fn split_takes_last_user_as_transcript_and_prior_history() {
        let messages = vec![
            msg("user", "¿Qué es DDD?"),
            msg("assistant", "Fíjate, DDD entra cuando el dominio se vuelve denso —"),
            msg("user", "ahora cuéntame de event sourcing"),
        ];
        let (transcript, prior) = split_interview_messages(&messages);
        assert_eq!(transcript, "ahora cuéntame de event sourcing");
        assert_eq!(prior.len(), 2);
        assert_eq!(prior[0].role, "user");
        assert_eq!(prior[1].role, "assistant");
    }

    #[test]
    fn split_ignores_system_and_empty_messages() {
        let messages = vec![
            msg("system", "ignorado"),
            msg("assistant", "respuesta previa"),
            msg("user", ""),
            msg("user", "nueva pregunta"),
        ];
        let (transcript, prior) = split_interview_messages(&messages);
        assert_eq!(transcript, "nueva pregunta");
        assert_eq!(prior.len(), 1);
        assert_eq!(prior[0].role, "assistant");
    }

    #[test]
    fn build_upstream_keeps_most_recent_messages() {
        let raw: Vec<RelayMessage> = (0..12)
            .map(|i| msg("user", &format!("msg-{i}")))
            .collect();
        let upstream = build_upstream_messages("sys", raw, 10);
        // system + 10 mensajes más recientes (msg-2 .. msg-11)
        assert_eq!(upstream.len(), 11);
        assert_eq!(upstream[1]["content"], "msg-2");
        assert_eq!(upstream[10]["content"], "msg-11");
    }
}
