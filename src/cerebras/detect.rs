//! Detección de preguntas nuevas vía Cerebras JSON schema.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::cerebras::shared_client;
use crate::config::CerebrasConfig;

/// Decisión del modelo sobre el fragmento actual.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectAction {
    /// Hay una pregunta o tarea nueva para el candidato.
    Respond,
    /// Solo se planteó un escenario; esperar la pregunta.
    Premise,
    /// La frase quedó a medias.
    Wait,
    /// Repetición, eco, relleno o comentario sin pregunta.
    Ignore,
}

impl DetectAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Respond => "respond",
            Self::Premise => "premise",
            Self::Wait => "wait",
            Self::Ignore => "ignore",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionKind {
    Technical,
    Behavioral,
    Logistics,
    CandidateQuestions,
    Smalltalk,
    None,
}

impl QuestionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Technical => "technical",
            Self::Behavioral => "behavioral",
            Self::Logistics => "logistics",
            Self::CandidateQuestions => "candidate_questions",
            Self::Smalltalk => "smalltalk",
            Self::None => "none",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim() {
            "technical" => Self::Technical,
            "behavioral" => Self::Behavioral,
            "logistics" => Self::Logistics,
            "candidate_questions" => Self::CandidateQuestions,
            "smalltalk" => Self::Smalltalk,
            _ => Self::None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    High,
    Medium,
    Low,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim() {
            "high" => Self::High,
            "low" => Self::Low,
            _ => Self::Medium,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DetectOutput {
    pub action: DetectAction,
    /// Pregunta completa y autocontenida (con escenario/continuación ya fusionados).
    pub question: String,
    /// Escenario acumulado cuando `action == Premise`.
    pub context: String,
    /// La pregunta amplía lastQuestion (no la reemplaza).
    pub continues_last: bool,
    pub kind: QuestionKind,
    pub confidence: Confidence,
}

fn detect_json_schema() -> Value {
    json!({
        "type": "json_schema",
        "json_schema": {
            "name": "question_detect_schema",
            "strict": true,
            "schema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["respond", "premise", "wait", "ignore"],
                        "description": "respond: hay pregunta o tarea nueva. premise: newText solo plantea un escenario. wait: la frase quedó a medias. ignore: eco, relleno o comentario sin pedido."
                    },
                    "question": {
                        "type": "string",
                        "description": "Si action=respond: la pregunta completa, en el idioma de la entrevista. Vacío en otro caso."
                    },
                    "context": {
                        "type": "string",
                        "description": "Si action=premise: el escenario completo acumulado, en el idioma de la entrevista. Vacío en otro caso."
                    },
                    "continuesLast": {
                        "type": "boolean",
                        "description": "true si question amplía lastQuestion. false si es otra pregunta."
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["technical", "behavioral", "logistics", "candidate_questions", "smalltalk", "none"],
                        "description": "Tipo de la pregunta cuando action=respond. none en las demás acciones."
                    },
                    "confidence": {
                        "type": "string",
                        "enum": ["high", "medium", "low"]
                    }
                },
                "required": ["action", "question", "context", "continuesLast", "kind", "confidence"],
                "additionalProperties": false
            }
        }
    })
}

pub async fn complete_detect(
    config: &CerebrasConfig,
    system_prompt: &str,
    user_content: &str,
) -> Result<DetectOutput, String> {
    let body = json!({
        "model": config.model,
        "stream": false,
        "max_tokens": 300,
        "temperature": 0,
        "top_p": config.top_p,
        "reasoning_effort": config.reasoning_effort,
        "response_format": detect_json_schema(),
        "messages": [
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": user_content }
        ]
    });

    let resp = shared_client()
        .post(&config.chat_url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .header("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(4))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Cerebras detect request failed: {e}"))?;

    let status = resp.status();
    let raw = resp
        .text()
        .await
        .map_err(|e| format!("Cerebras detect response read failed: {e}"))?;

    if !status.is_success() {
        return Err(format!("Cerebras HTTP {status}: {raw}"));
    }

    #[derive(Deserialize)]
    struct Response {
        choices: Vec<Choice>,
    }
    #[derive(Deserialize)]
    struct Choice {
        message: Message,
    }
    #[derive(Deserialize)]
    struct Message {
        content: String,
    }

    let parsed: Response = serde_json::from_str(&raw)
        .map_err(|e| format!("Cerebras detect JSON inválido ({e}): {raw}"))?;

    let content = parsed
        .choices
        .first()
        .map(|c| c.message.content.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("Cerebras detect sin contenido: {raw}"))?;

    let raw_out: serde_json::Value = serde_json::from_str(content)
        .map_err(|e| format!("Cerebras detect schema inválido ({e}): {content}"))?;

    Ok(parse_detect_output(&raw_out))
}

fn parse_detect_output(raw: &Value) -> DetectOutput {
    let text = |field: &str| {
        raw.get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string()
    };
    let question = text("question");
    let context = text("context");
    let action = match raw.get("action").and_then(Value::as_str) {
        Some("respond") => DetectAction::Respond,
        Some("premise") => DetectAction::Premise,
        Some("wait") => DetectAction::Wait,
        _ => DetectAction::Ignore,
    };
    DetectOutput {
        action,
        question,
        context,
        continues_last: raw
            .get("continuesLast")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        kind: QuestionKind::parse(raw.get("kind").and_then(Value::as_str).unwrap_or("none")),
        confidence: Confidence::parse(
            raw.get("confidence").and_then(Value::as_str).unwrap_or("medium"),
        ),
    }
}

#[cfg(test)]
pub fn parse_detect_output_for_test(raw: &Value) -> DetectOutput {
    parse_detect_output(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_actions_and_rejects_empty_payloads() {
        let out = parse_detect_output(&json!({
            "action": "respond", "question": " Q ", "context": "", "continuesLast": true
        }));
        assert_eq!(out.action, DetectAction::Respond);
        assert_eq!(out.question, "Q");
        assert!(out.continues_last);

        let out = parse_detect_output(&json!({
            "action": "premise", "question": "", "context": "Escenario", "continuesLast": false
        }));
        assert_eq!(out.action, DetectAction::Premise);

        let out = parse_detect_output(&json!({
            "action": "wait", "question": "", "context": "", "continuesLast": false, "confidence": "low"
        }));
        assert_eq!(out.action, DetectAction::Wait);
        assert_eq!(out.confidence, Confidence::Low);

        let out = parse_detect_output(&json!({
            "action": "respond", "question": "", "context": "", "continuesLast": false
        }));
        assert_eq!(out.action, DetectAction::Respond);
        assert!(out.question.is_empty());
    }
}
