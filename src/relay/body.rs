//! Contrato HTTP del relay de entrevistas.

use serde::{Deserialize, Deserializer};

/// Tipo de agente (prompt + modelo). El relay de entrevista ya no usa detector/opener.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentType {
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

impl AgentType {
    pub fn prompt_filename(self) -> &'static str {
        match self {
            Self::Deepener => "deepener.md",
            Self::ImageSolver => "image-solver.md",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
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

/// Tipo de entrevista que adapta los prompts del pipeline de respuesta.
/// Solo dos tipos: el cliente lo envía en cada request y puede cambiarlo mid-conversación.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum InterviewKind {
    #[serde(alias = "hr", alias = "recursos-humanos", alias = "recursos_humanos")]
    Hr,
    #[default]
    #[serde(alias = "tecnica", alias = "tech", alias = "technical")]
    Technical,
}

impl InterviewKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Hr => "recursos humanos",
            Self::Technical => "técnica",
        }
    }

    pub fn instructions(self) -> &'static str {
        match self {
            Self::Hr => "MODO ACTUAL: ENTREVISTA DE RECURSOS HUMANOS\n\
- Este modo manda sobre todo lo demás. Si el historial previo suena técnico, NO lo imites: a partir de ahora respondes en modo RRHH.\n\
- VOCABULARIO: cero jerga técnica. Nada de frameworks, arquitecturas, patrones ni siglas. Si la pregunta menciona una tecnología, habla del impacto humano o del resultado, no de la implementación.\n\
- CONTENIDO: actitud, motivación, fit cultural, comunicación, trabajo en equipo, liderazgo, manejo de presión y conflictos.\n\
- EJEMPLOS: situaciones reales con personas (un equipo, un conflicto, una entrega difícil), no detalles de código.\n\
- TONO: cercano y humano, autoconocimiento sin sonar ensayado. Nada de frases hechas tipo \"soy proactivo\" ni manual de RRHH.",
            Self::Technical => "MODO ACTUAL: ENTREVISTA TÉCNICA\n\
- Este modo manda sobre todo lo demás. Si el historial previo suena blando o genérico, NO lo imites: a partir de ahora respondes en modo técnico.\n\
- VOCABULARIO: usa los términos del puesto con naturalidad y precisión. Nada de respuestas vagas tipo \"usé buenas prácticas\".\n\
- CONTENIDO: decisiones de ingeniería, trade-offs, arquitectura, bugs reales, producción, performance, diseño.\n\
- EJEMPLOS: experiencias vividas concretas (qué construiste, qué se rompió, cómo lo resolviste).\n\
- TONO: criterio de senior que lo implementó, no de quien lo leyó en un blog. Concreto sobre abstracto; evita definiciones de libro.",
        }
    }
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
    #[serde(default)]
    pub regional_expressions: Option<String>,
    #[serde(default)]
    pub regional_avoid: Option<String>,
    #[serde(default)]
    pub salary_expectation: Option<String>,
}

/// Cómo se cerró el turno que el front está evaluando.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DetectTrigger {
    #[default]
    Auto,
    SilenceAfterPremise,
    Manual,
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
#[serde(rename_all = "camelCase")]
pub struct RelayBody {
    pub messages: Vec<RelayMessage>,
    pub values: RelayValues,
    /// Tipo de entrevista: "recursos-humanos" | "tecnica". Obligatorio; el cliente
    /// lo envía en cada request, así un cambio de tipo mid-conversación aplica al instante.
    pub kind: InterviewKind,
    /// Pregunta ya armada por el detector. Si viene, el historial lo pone el backend.
    #[serde(default)]
    pub question: Option<String>,
    #[serde(default)]
    pub continues_last: bool,
    /// `technical` usa el prompt técnico; el resto de kinds de pregunta usan RRHH.
    #[serde(default)]
    pub question_kind: Option<String>,
}

/// Body de `POST /interviews/:id/ai/question-detect`.
///
/// El turno manda solo `seq`, `newText`, `recentContext` y `trigger`.
/// `values` y `kind` son la configuración del prompt (puesto e idioma): el relay
/// no tiene la ficha de la entrevista, así que la recuerda la primera vez.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionDetectBody {
    pub seq: u64,
    #[serde(default)]
    pub new_text: String,
    #[serde(default)]
    pub recent_context: String,
    #[serde(default)]
    pub trigger: DetectTrigger,
    #[serde(default)]
    pub kind: InterviewKind,
    #[serde(default)]
    pub values: Option<RelayValues>,
}

/// Traducción incremental dedicada a Cerebras.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationBody {
    pub text: String,
    pub target_language: String,
    #[serde(default)]
    pub source_language: Option<String>,
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
                "kind": "tecnica",
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
        assert_eq!(body.kind, InterviewKind::Technical);
    }

    #[test]
    fn relay_body_requires_kind() {
        let result = serde_json::from_str::<RelayBody>(
            r#"{
                "values": {
                    "jobPosition": "Backend",
                    "regionalism": "es-MX",
                    "responseLanguage": "español"
                },
                "messages": [{ "role": "user", "content": "Hola" }]
            }"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn relay_body_rejects_mixta() {
        let result = serde_json::from_str::<RelayBody>(
            r#"{
                "kind": "mixta",
                "values": {
                    "jobPosition": "Backend",
                    "regionalism": "es-MX",
                    "responseLanguage": "español"
                },
                "messages": [{ "role": "user", "content": "Hola" }]
            }"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn question_detect_body_is_the_four_turn_fields() {
        let body: QuestionDetectBody = serde_json::from_str(
            r#"{
                "seq": 42,
                "newText": "¿Por qué?",
                "recentContext": "¿Usarías microservicios?",
                "trigger": "auto"
            }"#,
        )
        .unwrap();
        assert_eq!(body.seq, 42);
        assert_eq!(body.new_text, "¿Por qué?");
        assert_eq!(body.recent_context, "¿Usarías microservicios?");
        assert_eq!(body.trigger, DetectTrigger::Auto);
        assert!(body.values.is_none());
    }

    #[test]
    fn interview_kind_accepts_aliases() {
        let hr: InterviewKind = serde_json::from_str(r#""recursos-humanos""#).unwrap();
        assert_eq!(hr, InterviewKind::Hr);
        let tech: InterviewKind = serde_json::from_str(r#""tecnica""#).unwrap();
        assert_eq!(tech, InterviewKind::Technical);
    }

    #[test]
    fn normalizes_english_hints_and_defaults_unknown_values_to_spanish() {
        assert_eq!(language_from_hint("en-US"), InterviewLanguage::English);
        assert_eq!(language_from_hint("English"), InterviewLanguage::English);
        assert_eq!(language_from_hint("español"), InterviewLanguage::Spanish);
        assert_eq!(language_from_hint(""), InterviewLanguage::Spanish);
    }

}
