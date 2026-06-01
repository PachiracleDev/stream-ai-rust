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
        for agent in [AgentType::Opener, AgentType::Deepener, AgentType::ImageSolver] {
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

fn render_template(template: &str, v: &RelayValues) -> String {
    template
        .replace("{{jobPosition}}", &v.job_position)
        .replace("{{regionalism}}", &v.regionalism)
        .replace("{{responseLanguage}}", &v.response_language)
        .replace("{{profileMinimal}}", &v.profile_minimal)
        .replace("{{lastJobs}}", &v.last_jobs)
        .replace("{{lastRole}}", &v.last_jobs)
}
