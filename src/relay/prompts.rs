//! Carga y renderizado de system prompts desde `prompts/*.md`.

use std::collections::HashMap;
use std::path::Path;

use crate::relay::body::{AgentType, RelayValues};

pub struct PromptStore {
    templates: HashMap<AgentType, String>,
}

impl PromptStore {
    pub fn load(dir: &Path) -> Result<Self, String> {
        let mut templates = HashMap::new();
        for agent in [AgentType::Detector, AgentType::Opener, AgentType::Deepener, AgentType::ImageSolver] {
            let path = dir.join(agent.prompt_filename());
            let raw = std::fs::read_to_string(&path).map_err(|e| {
                format!("no se pudo leer prompt {}: {e}", path.display())
            })?;
            templates.insert(agent, raw);
        }
        Ok(Self { templates })
    }

    pub fn render(&self, agent: AgentType, values: &RelayValues) -> Result<String, String> {
        let tpl = self
            .templates
            .get(&agent)
            .ok_or_else(|| format!("plantilla no cargada para {agent:?}"))?;
        Ok(render_template(tpl, values))
    }
}

fn optional_value(value: &Option<String>) -> &str {
    value
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("")
}

fn render_template(template: &str, v: &RelayValues) -> String {
    let profile_minimal = optional_value(&v.profile_minimal);
    let last_jobs = optional_value(&v.last_jobs);
    let role_keywords = optional_value(&v.role_keywords);
    template
        .replace("{{jobPosition}}", &v.job_position)
        .replace("{{regionalism}}", &v.regionalism)
        .replace("{{responseLanguage}}", &v.response_language)
        .replace("{{profileMinimal}}", profile_minimal)
        .replace("{{lastJobs}}", last_jobs)
        .replace("{{lastRole}}", last_jobs)
        .replace("{{roleKeywords}}", role_keywords)
        .replace("{{techKeywords}}", role_keywords)
}
