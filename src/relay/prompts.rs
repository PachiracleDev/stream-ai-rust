//! Carga y renderizado de system prompts desde `prompts/*.md`.

use std::collections::HashMap;
use std::path::Path;

use crate::relay::body::{AgentType, InterviewKind, RelayValues};
use crate::relay::language::{normalize_language_label, response_language_instructions};

pub struct PromptStore {
    templates: HashMap<AgentType, String>,
    cerebras_qa: String,
    question_detect: String,
}

impl PromptStore {
    pub fn load(dir: &Path) -> Result<Self, String> {
        let mut templates = HashMap::new();
        for agent in [
            AgentType::Detector,
            AgentType::Opener,
            AgentType::Deepener,
            AgentType::ImageSolver,
        ] {
            let path = dir.join(agent.prompt_filename());
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| format!("no se pudo leer prompt {}: {e}", path.display()))?;
            templates.insert(agent, raw);
        }
        let cerebras_path = dir.join("cerebras_qa.md");
        let cerebras_qa = std::fs::read_to_string(&cerebras_path)
            .map_err(|e| format!("no se pudo leer prompt {}: {e}", cerebras_path.display()))?;
        let detect_path = dir.join("question_detect.md");
        let question_detect = std::fs::read_to_string(&detect_path)
            .map_err(|e| format!("no se pudo leer prompt {}: {e}", detect_path.display()))?;
        Ok(Self {
            templates,
            cerebras_qa,
            question_detect,
        })
    }

    pub fn render(&self, agent: AgentType, values: &RelayValues) -> Result<String, String> {
        self.render_with_transcript(agent, values, "")
    }

    pub fn render_with_transcript(
        &self,
        agent: AgentType,
        values: &RelayValues,
        transcript: &str,
    ) -> Result<String, String> {
        let tpl = self
            .templates
            .get(&agent)
            .ok_or_else(|| format!("plantilla no cargada para {agent:?}"))?;
        Ok(render_template(tpl, values, transcript))
    }

    pub fn render_cerebras_qa(&self, values: &RelayValues, kind: InterviewKind) -> String {
        render_with_kind(&self.cerebras_qa, values, kind, "")
    }

    pub fn render_question_detect(&self, values: &RelayValues, kind: InterviewKind) -> String {
        render_with_kind(&self.question_detect, values, kind, "")
    }
}

fn render_with_kind(
    template: &str,
    values: &RelayValues,
    kind: InterviewKind,
    transcript: &str,
) -> String {
    render_template(template, values, transcript)
        .replace("{{interviewKind}}", kind.label())
        .replace("{{interviewKindInstructions}}", kind.instructions())
}

fn optional_value(value: &Option<String>) -> &str {
    value
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("")
}

fn render_template(template: &str, v: &RelayValues, transcript: &str) -> String {
    let profile_minimal = optional_value(&v.profile_minimal);
    let last_jobs = optional_value(&v.last_jobs);
    let role_keywords = optional_value(&v.role_keywords);
    let response_language = normalize_language_label(&v.response_language);
    let language_instructions = response_language_instructions(&v.response_language, transcript);
    template
        .replace("{{jobPosition}}", &v.job_position)
        .replace("{{regionalism}}", &v.regionalism)
        .replace("{{responseLanguage}}", &response_language)
        .replace("{{responseLanguageInstructions}}", &language_instructions)
        .replace("{{profileMinimal}}", profile_minimal)
        .replace("{{lastJobs}}", last_jobs)
        .replace("{{lastRole}}", last_jobs)
        .replace("{{roleKeywords}}", role_keywords)
        .replace("{{techKeywords}}", role_keywords)
}
