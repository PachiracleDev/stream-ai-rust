//! Detección de preguntas nuevas vía Cerebras JSON schema.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::cerebras::shared_client;
use crate::config::CerebrasConfig;

#[derive(Debug, Clone, Deserialize)]
pub struct DetectOutput {
    #[serde(default)]
    pub should_respond: bool,
    #[serde(default)]
    pub question: Option<String>,
    #[serde(default)]
    pub intelligible: bool,
    /// Si la pregunta del último turno es una continuación/complemento de lastQuestion.
    #[serde(default)]
    pub is_continuation: bool,
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
                    "shouldRespond": {
                        "type": "boolean",
                        "description": "true solo si hay una pregunta que el candidato aún no ha respondido. Incluye continuaciones/complementos de la última pregunta."
                    },
                    "question": {
                        "type": "string",
                        "description": "Pregunta completa y autocontenida. Si el fragmento es un follow-up corto (\"¿Cómo?\", \"Why?\") o referencia algo previo, expándelo usando el contexto. Si es continuación de lastQuestion, combínalas en orden. Vacío si no hay pregunta."
                    },
                    "intelligible": {
                        "type": "boolean",
                        "description": "false solo si el audio no contiene pregunta real. true si contiene pregunta, aunque ya haya sido respondida o sea continuación."
                    },
                    "isContinuation": {
                        "type": "boolean",
                        "description": "true solo si el fragmento complementa una lastQuestion AÚN NO respondida. false si es pregunta nueva o follow-up sobre algo ya respondido."
                    }
                },
                "required": ["shouldRespond", "question", "intelligible", "isContinuation"],
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
        "max_tokens": config.max_tokens,
        "temperature": config.temperature,
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

    Ok(DetectOutput {
        should_respond: raw_out
            .get("shouldRespond")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        question: raw_out
            .get("question")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        intelligible: raw_out
            .get("intelligible")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        is_continuation: raw_out
            .get("isContinuation")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    })
}
