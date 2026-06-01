//! Contrato HTTP del relay de entrevistas.

use serde::Deserialize;

/// Tipo de agente entrevistador (define system prompt, modelo y presupuesto de tokens).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentType {
    Opener,
    Deepener,
    #[serde(rename = "image-solver")]
    ImageSolver,
}

impl AgentType {
    pub fn prompt_filename(self) -> &'static str {
        match self {
            Self::Opener => "opener.md",
            Self::Deepener => "deepener.md",
            Self::ImageSolver => "image-solver.md",
        }
    }

    pub fn uses_vision(self) -> bool {
        matches!(self, Self::ImageSolver)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Opener => "opener",
            Self::Deepener => "deepener",
            Self::ImageSolver => "image-solver",
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
    pub profile_minimal: String,
    pub last_jobs: String,
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
    #[serde(rename = "type")]
    pub agent_type: AgentType,
    pub values: RelayValues,
}
