//! Carga y renderizado de system prompts desde `prompts/*.md`.

use std::collections::HashMap;
use std::path::Path;

use crate::relay::body::{AgentType, InterviewKind, RelayValues};
use crate::relay::language::{normalize_language_label, response_language_instructions};

pub struct PromptStore {
    templates: HashMap<AgentType, String>,
    cerebras_qa_technical: String,
    cerebras_qa_hr: String,
    question_detect: String,
}

impl PromptStore {
    pub fn load(dir: &Path) -> Result<Self, String> {
        let mut templates = HashMap::new();
        for agent in [AgentType::Deepener, AgentType::ImageSolver] {
            let path = dir.join(agent.prompt_filename());
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| format!("no se pudo leer prompt {}: {e}", path.display()))?;
            templates.insert(agent, raw);
        }
        Ok(Self {
            templates,
            cerebras_qa_technical: read_prompt(dir, "cerebras_qa_technical.md")?,
            cerebras_qa_hr: read_prompt(dir, "cerebras_qa_hr.md")?,
            question_detect: read_prompt(dir, "question_detect.md")?,
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
        let template = match kind {
            InterviewKind::Technical => &self.cerebras_qa_technical,
            InterviewKind::Hr => &self.cerebras_qa_hr,
        };
        render_with_kind(template, values, kind, "")
    }

    pub fn render_question_detect(&self, values: &RelayValues, kind: InterviewKind) -> String {
        render_with_kind(&self.question_detect, values, kind, "")
    }
}

fn read_prompt(dir: &Path, filename: &str) -> Result<String, String> {
    let path = dir.join(filename);
    std::fs::read_to_string(&path).map_err(|e| format!("no se pudo leer prompt {}: {e}", path.display()))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_values() -> RelayValues {
        RelayValues {
            job_position: "Full Stack Developer".into(),
            regionalism: "Peruano".into(),
            response_language: "es".into(),
            profile_minimal: Some("3 años en TypeScript y React.".into()),
            last_jobs: Some("On Road Technology Solutions: Full Stack.".into()),
            role_keywords: Some("TypeScript, React, NestJS".into()),
        }
    }

    #[test]
    fn technical_prompt_uses_spoken_technical_contract() {
        let store = PromptStore::load(Path::new("prompts")).unwrap();
        let prompt = store.render_cerebras_qa(&sample_values(), InterviewKind::Technical);
        assert!(prompt.contains("MODO: ENTREVISTA TÉCNICA"));
        assert!(prompt.contains("locking optimista"));
        assert!(prompt.contains("On Road Technology Solutions"));
        assert!(prompt.contains("Solo el texto hablado"));
    }

    #[test]
    fn hr_prompt_keeps_no_jargon_rule() {
        let store = PromptStore::load(Path::new("prompts")).unwrap();
        let prompt = store.render_cerebras_qa(&sample_values(), InterviewKind::Hr);
        assert!(prompt.contains("ENTREVISTA DE RECURSOS HUMANOS"));
        assert!(prompt.contains("Cero jerga técnica"));
    }
}
