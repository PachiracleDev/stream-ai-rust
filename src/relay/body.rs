//! Contrato HTTP del relay de entrevistas.

use serde::{Deserialize, Deserializer};

/// Tipo de agente entrevistador (define system prompt, modelo y presupuesto de tokens).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentType {
    Detector,
    Opener,
    Deepener,
    #[serde(rename = "image-solver")]
    ImageSolver,
}

/// Idioma canónico que usa el pipeline de entrevista.
///
/// El cliente puede enviar nombres, códigos o locales; los prompts y eventos
/// SSE usan siempre uno de estos dos códigos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterviewLanguage {
    English,
    Spanish,
}

impl InterviewLanguage {
    pub fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Spanish => "es",
        }
    }

    pub fn prompt_label(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Spanish => "Spanish",
        }
    }
}

/// Normaliza una preferencia del cliente. El español conserva compatibilidad
/// con los clientes existentes cuando el valor falta o no es reconocible.
pub fn language_from_hint(value: &str) -> InterviewLanguage {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized == "en"
        || normalized.starts_with("en-")
        || normalized.contains("english")
        || normalized.contains("inglés")
        || normalized.contains("ingles")
    {
        InterviewLanguage::English
    } else {
        InterviewLanguage::Spanish
    }
}

/// Prioriza exclusivamente los códigos emitidos por el detector y usa la
/// preferencia del cliente como fallback para respuestas no inteligibles o
/// modelos antiguos que no devuelvan `language`.
pub fn language_from_detector_or_hint(detected: Option<&str>, fallback: &str) -> InterviewLanguage {
    match detected.map(|value| value.trim().to_ascii_lowercase()) {
        Some(value) if value == "en" => InterviewLanguage::English,
        Some(value) if value == "es" => InterviewLanguage::Spanish,
        _ => language_from_hint(fallback),
    }
}

impl AgentType {
    pub fn prompt_filename(self) -> &'static str {
        match self {
            Self::Detector => "detector.md",
            Self::Opener => "opener.md",
            Self::Deepener => "deepener.md",
            Self::ImageSolver => "image-solver.md",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Detector => "detector",
            Self::Opener => "opener",
            Self::Deepener => "deepener",
            Self::ImageSolver => "image-solver",
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StringOrStringList {
    One(String),
    Many(Vec<String>),
}

fn normalize_string_or_list(value: StringOrStringList) -> Option<String> {
    match value {
        StringOrStringList::One(s) => {
            let t = s.trim();
            (!t.is_empty()).then_some(s)
        }
        StringOrStringList::Many(items) => {
            let parts: Vec<String> = items
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            (!parts.is_empty()).then(|| parts.join(", "))
        }
    }
}

fn deserialize_optional_string_or_list<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<StringOrStringList>::deserialize(deserializer)?;
    Ok(value.and_then(normalize_string_or_list))
}

/// Variables de plantilla inyectadas en los `.md` de `prompts/`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayValues {
    pub job_position: String,
    pub regionalism: String,
    pub response_language: String,
    #[serde(default)]
    pub profile_minimal: Option<String>,
    #[serde(default)]
    pub last_jobs: Option<String>,
    #[serde(
        default,
        alias = "role_keywords",
        alias = "techKeywords",
        alias = "tech_keywords",
        deserialize_with = "deserialize_optional_string_or_list"
    )]
    pub role_keywords: Option<String>,
}

/// Mensaje del cliente (texto y/o imagen).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayMessage {
    pub role: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default, alias = "image_url")]
    pub image_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RelayBody {
    pub messages: Vec<RelayMessage>,
    pub values: RelayValues,
}

/// Body de `POST /interviews/:id/ai/expand-response`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpandResponseBody {
    pub question: String,
    pub response: String,
    pub values: RelayValues,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_values_accepts_missing_optional_fields() {
        let v: RelayValues = serde_json::from_str(
            r#"{
                "jobPosition": "Backend",
                "regionalism": "es-MX",
                "responseLanguage": "español"
            }"#,
        )
        .unwrap();
        assert!(v.profile_minimal.is_none());
        assert!(v.last_jobs.is_none());
        assert!(v.role_keywords.is_none());
    }

    #[test]
    fn relay_values_accepts_legacy_tech_keywords_alias() {
        let v: RelayValues = serde_json::from_str(
            r#"{
                "jobPosition": "Enfermería",
                "regionalism": "es-MX",
                "responseLanguage": "español",
                "techKeywords": "triage, protocolo, signos vitales"
            }"#,
        )
        .unwrap();
        assert_eq!(
            v.role_keywords.as_deref(),
            Some("triage, protocolo, signos vitales")
        );
    }

    #[test]
    fn relay_values_accepts_role_keywords_as_array() {
        let v: RelayValues = serde_json::from_str(
            r#"{
                "jobPosition": "Backend",
                "regionalism": "es-MX",
                "responseLanguage": "español",
                "roleKeywords": ["goroutines", "channels", "backpressure"]
            }"#,
        )
        .unwrap();
        assert_eq!(
            v.role_keywords.as_deref(),
            Some("goroutines, channels, backpressure")
        );
    }

    #[test]
    fn relay_body_deserializes() {
        let body: RelayBody = serde_json::from_str(
            r#"{
                "values": {
                    "jobPosition": "Backend",
                    "regionalism": "es-MX",
                    "responseLanguage": "español"
                },
                "messages": [{ "role": "user", "content": "Hola" }]
            }"#,
        )
        .unwrap();
        assert_eq!(body.messages.len(), 1);
    }

    #[test]
    fn normalizes_english_hints_and_defaults_unknown_values_to_spanish() {
        assert_eq!(language_from_hint("en-US"), InterviewLanguage::English);
        assert_eq!(language_from_hint("English"), InterviewLanguage::English);
        assert_eq!(language_from_hint("español"), InterviewLanguage::Spanish);
        assert_eq!(language_from_hint(""), InterviewLanguage::Spanish);
    }

    #[test]
    fn detector_language_only_accepts_canonical_codes() {
        assert_eq!(
            language_from_detector_or_hint(Some("en"), "es"),
            InterviewLanguage::English
        );
        assert_eq!(
            language_from_detector_or_hint(Some("English"), "es"),
            InterviewLanguage::Spanish
        );
    }
}
