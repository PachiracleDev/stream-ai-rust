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
            (!t.is_empty()).then(|| s)
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

fn deserialize_optional_string_or_list<'de, D>(
    deserializer: D,
) -> Result<Option<String>, D::Error>
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
    /// Solo `image-solver` activa visión. Omitir `type` → opener + deepener en un request.
    #[serde(default, rename = "type")]
    pub request_type: Option<RelayRequestType>,
    pub values: RelayValues,
}

/// Tipo opcional del request. Sin `type` (o legacy `opener`/`deepener`) → pipeline completo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RelayRequestType {
    Opener,
    Deepener,
    #[serde(rename = "image-solver")]
    ImageSolver,
}

impl RelayBody {
    pub fn is_image_solver(&self) -> bool {
        matches!(self.request_type, Some(RelayRequestType::ImageSolver))
    }
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
    fn relay_body_accepts_missing_type() {
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
        assert!(body.request_type.is_none());
        assert!(!body.is_image_solver());
    }
}
