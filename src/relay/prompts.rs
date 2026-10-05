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
        Ok(strip_unresolved_placeholders(&render_template(
            tpl, values, transcript,
        )))
    }

    pub fn render_cerebras_qa(&self, values: &RelayValues, kind: InterviewKind) -> String {
        let template = match kind {
            InterviewKind::Technical => &self.cerebras_qa_technical,
            InterviewKind::Hr => &self.cerebras_qa_hr,
        };
        render_with_kind(template, values, kind, "")
    }

    pub fn render_question_detect(&self, values: &RelayValues, kind: InterviewKind) -> String {
        let language = session_language_code(&values.response_language);
        let rendered = render_template(&self.question_detect, values, "")
            .replace("{{interviewKind}}", kind.label())
            .replace("{{interviewKindInstructions}}", kind.instructions())
            .replace("{{genericApproachAsk}}", generic_approach_ask(language))
            .replace("{{detectorExamples}}", &detector_examples(language));
        strip_unresolved_placeholders(&rendered)
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
    let rendered = render_template(template, values, transcript)
        .replace("{{interviewKind}}", kind.label())
        .replace("{{interviewKindInstructions}}", kind.instructions());
    strip_unresolved_placeholders(&rendered)
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
    let regional_expressions = optional_value(&v.regional_expressions);
    let regional_avoid = optional_value(&v.regional_avoid);
    let salary_expectation = optional_value(&v.salary_expectation);
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
        .replace("{{regionalExpressions}}", regional_expressions)
        .replace("{{regionalAvoid}}", regional_avoid)
        .replace("{{salaryExpectation}}", salary_expectation)
}

fn strip_unresolved_placeholders(template: &str) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find("}}") {
            Some(end) => rest = &rest[start + 2 + end + 2..],
            None => {
                out.push_str(&rest[start..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

pub fn session_language_code(raw: &str) -> &'static str {
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.starts_with("en")
        || normalized.contains("english")
        || normalized.contains("ingl")
    {
        "en"
    } else if normalized.starts_with("pt") || normalized.contains("portugu") {
        "pt"
    } else {
        "es"
    }
}

/// Código de Deepgram para el único idioma de la sesión.
pub fn deepgram_language_code(raw: &str) -> &'static str {
    match session_language_code(raw) {
        "en" => "en",
        "pt" => "pt-BR",
        _ => "es-419",
    }
}

fn generic_approach_ask(language: &str) -> &'static str {
    match language {
        "en" => "How would you approach it?",
        "pt" => "Como você abordaria isso?",
        _ => "¿Cómo lo abordarías?",
    }
}

fn detector_examples(language: &str) -> &'static str {
    match language {
        "en" => r#"newText: "You have a Java API that makes 5 external HTTP calls in sequence and takes 4 seconds. What alternatives would you look at to cut the latency?"
→ respond, technical, continuesLast=false, question: "You have a Java API that makes 5 external HTTP calls in sequence and takes 4 seconds. What alternatives would you look at to cut the latency?"

newText: "You have a Java API that makes 5 external HTTP calls in sequence and takes 4 seconds?"
→ premise, context: "You have a Java API that makes 5 external HTTP calls in sequence and takes 4 seconds."

pendingContext: "You have a Java API that makes 5 external HTTP calls in sequence and takes 4 seconds."
newText: "", trigger: "silence_after_premise"
→ respond, technical, continuesLast=false, question: "You have a Java API that makes 5 external HTTP calls in sequence and takes 4 seconds. How would you approach it?"

newText: "Imagine you have to design a booking system for a hotel chain."
→ respond, technical, question: "Imagine you have to design a booking system for a hotel chain." (it already asks to design)

newText: "Imagine a patient's family is upset because the discharge got delayed twice."
→ premise, context: "Imagine a patient's family is upset because the discharge got delayed twice."

pendingContext: "Imagine a patient's family is upset because the discharge got delayed twice."
newText: "How would you handle that conversation?"
→ respond, behavioral, continuesLast=false, question: "Imagine a patient's family is upset because the discharge got delayed twice. How would you handle that conversation?"

pendingContext: "Imagine a patient's family is upset because the discharge got delayed twice."
newText: "Actually, before that, what's your availability to start?"
→ respond, logistics, continuesLast=false, question: "What's your availability to start?" (different topic: do not use pendingContext)

newText: "And what I want to know is how, for example"
→ wait

lastQuestion: "Tell me about a difficult negotiation with a client."
newText: "And what did you learn from that?"
→ respond, behavioral, continuesLast=true, question: "Tell me about a difficult negotiation with a client. And what did you learn from that?"

lastQuestion: "Would you use microservices or a monolith for this case?"
newText: "Why?"
→ respond, technical, continuesLast=true, question: "Would you use microservices or a monolith for this case? Why?"

newText: "How do you prioritize when you have several deliveries at once? No, tell me about a time you missed a date."
→ respond, behavioral, continuesLast=false, question: "Tell me about a time you missed a date."

newText: "Perfect, thanks. Let me tell you how the accounting team works here. There are five of us, and you know what's hardest? Year-end close."
→ ignore (explanation with a rhetorical question)

newText: "Uh-huh, ok."
→ ignore

newText: "Can you hear me well?"
→ respond, smalltalk, question: "Can you hear me well?""#,
        "pt" => r#"newText: "Você tem uma API Java que faz 5 chamadas HTTP externas em sequência e demora 4 segundos. Que alternativas você analisaria para reduzir a latência?"
→ respond, technical, continuesLast=false, question: "Você tem uma API Java que faz 5 chamadas HTTP externas em sequência e demora 4 segundos. Que alternativas você analisaria para reduzir a latência?"

newText: "Você tem uma API Java que faz 5 chamadas HTTP externas em sequência e demora 4 segundos?"
→ premise, context: "Você tem uma API Java que faz 5 chamadas HTTP externas em sequência e demora 4 segundos."

pendingContext: "Você tem uma API Java que faz 5 chamadas HTTP externas em sequência e demora 4 segundos."
newText: "", trigger: "silence_after_premise"
→ respond, technical, continuesLast=false, question: "Você tem uma API Java que faz 5 chamadas HTTP externas em sequência e demora 4 segundos. Como você abordaria isso?"

newText: "Imagine que você precisa desenhar um sistema de reservas para uma rede de hotéis."
→ respond, technical, question: "Imagine que você precisa desenhar um sistema de reservas para uma rede de hotéis." (já pede para desenhar)

newText: "Imagine que a família de um paciente está chateada porque a alta atrasou duas vezes."
→ premise, context: "Imagine que a família de um paciente está chateada porque a alta atrasou duas vezes."

pendingContext: "Imagine que a família de um paciente está chateada porque a alta atrasou duas vezes."
newText: "Como você conduziria essa conversa?"
→ respond, behavioral, continuesLast=false, question: "Imagine que a família de um paciente está chateada porque a alta atrasou duas vezes. Como você conduziria essa conversa?"

pendingContext: "Imagine que a família de um paciente está chateada porque a alta atrasou duas vezes."
newText: "Antes disso, qual é a sua disponibilidade para começar?"
→ respond, logistics, continuesLast=false, question: "Qual é a sua disponibilidade para começar?" (outro tema: não usa pendingContext)

newText: "E o que eu quero saber é como, por exemplo"
→ wait

lastQuestion: "Fale sobre uma negociação difícil com um cliente."
newText: "E o que você aprendeu com isso?"
→ respond, behavioral, continuesLast=true, question: "Fale sobre uma negociação difícil com um cliente. E o que você aprendeu com isso?"

lastQuestion: "Você usaria microsserviços ou um monólito neste caso?"
newText: "Por quê?"
→ respond, technical, continuesLast=true, question: "Você usaria microsserviços ou um monólito neste caso? Por quê?"

newText: "Como você prioriza quando tem várias entregas ao mesmo tempo? Não, melhor me conta de uma vez que você não cumpriu uma data."
→ respond, behavioral, continuesLast=false, question: "Me conta de uma vez que você não cumpriu uma data."

newText: "Perfeito, obrigado. Deixa eu te contar como trabalha a equipe de contabilidade aqui. Somos cinco, e sabe o que é mais difícil? O fechamento de fim de ano."
→ ignore (explicação com pergunta retórica)

newText: "Aham, ok."
→ ignore

newText: "Você está me ouvindo bem?"
→ respond, smalltalk, question: "Você está me ouvindo bem?""#,
        _ => r#"newText: "Tienes una API Java que hace 5 llamadas http externas en secuencia y tarda 4 segundos. ¿Qué alternativas analizarías para reducir la latencia?"
→ respond, technical, continuesLast=false, question: "Tienes una API Java que hace 5 llamadas HTTP externas en secuencia y tarda 4 segundos. ¿Qué alternativas analizarías para reducir la latencia?"

newText: "¿Tienes una API Java que hace 5 llamadas http externas en secuencia y tarda 4 segundos?"
→ premise, context: "Tienes una API Java que hace 5 llamadas HTTP externas en secuencia y tarda 4 segundos."

pendingContext: "Tienes una API Java que hace 5 llamadas HTTP externas en secuencia y tarda 4 segundos."
newText: "", trigger: "silence_after_premise"
→ respond, technical, continuesLast=false, question: "Tienes una API Java que hace 5 llamadas HTTP externas en secuencia y tarda 4 segundos. ¿Cómo lo abordarías?"

newText: "Imagina que tienes que diseñar un sistema de reservas para una cadena de hoteles."
→ respond, technical, question: "Imagina que tienes que diseñar un sistema de reservas para una cadena de hoteles." (ya pide diseñar)

newText: "Imagina que la familia de un paciente está molesta porque el alta se retrasó dos veces."
→ premise, context: "Imagina que la familia de un paciente está molesta porque el alta se retrasó dos veces."

pendingContext: "Imagina que la familia de un paciente está molesta porque el alta se retrasó dos veces."
newText: "¿Cómo manejarías esa conversación?"
→ respond, behavioral, continuesLast=false, question: "Imagina que la familia de un paciente está molesta porque el alta se retrasó dos veces. ¿Cómo manejarías esa conversación?"

pendingContext: "Imagina que la familia de un paciente está molesta porque el alta se retrasó dos veces."
newText: "Antes de eso, ¿cuál es tu disponibilidad para empezar?"
→ respond, logistics, continuesLast=false, question: "¿Cuál es tu disponibilidad para empezar?" (otro tema: no usa pendingContext)

newText: "Y lo que me interesa saber es cómo, por ejemplo"
→ wait

lastQuestion: "Cuéntame de una negociación difícil con un cliente."
newText: "¿Y qué aprendiste de eso?"
→ respond, behavioral, continuesLast=true, question: "Cuéntame de una negociación difícil con un cliente. ¿Y qué aprendiste de eso?"

lastQuestion: "¿Usarías microservicios o un monolito para este caso?"
newText: "¿Por qué?"
→ respond, technical, continuesLast=true, question: "¿Usarías microservicios o un monolito para este caso? ¿Por qué?"

newText: "¿Cómo priorizas cuando tienes varias entregas a la vez? No, mejor cuéntame de una vez que no llegaste a una fecha."
→ respond, behavioral, continuesLast=false, question: "Cuéntame de una vez que no llegaste a una fecha."

newText: "Perfecto, gracias. Te cuento un poco cómo trabaja el equipo de contabilidad aquí. Somos cinco, y ¿sabes qué es lo más difícil? El cierre de fin de año."
→ ignore (explicación con pregunta retórica)

newText: "Ajá, ok."
→ ignore

newText: "¿Me escuchas bien?"
→ respond, smalltalk, question: "¿Me escuchas bien?""#,
    }
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
            regional_expressions: None,
            regional_avoid: None,
            salary_expectation: None,
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
        assert!(prompt.contains("la primera parte ya la respondiste"));
        assert!(!prompt.contains("{{"));
    }

    #[test]
    fn detector_prompt_is_single_language_and_has_no_placeholders() {
        let store = PromptStore::load(Path::new("prompts")).unwrap();
        let prompt = store.render_question_detect(&sample_values(), InterviewKind::Technical);
        assert!(prompt.contains("Funciona igual para cualquier puesto o industria"));
        assert!(!prompt.contains("cualquier idioma"));
        assert!(prompt.contains("en el mismo idioma de la entrevista"));
        assert!(prompt.contains("¿Cómo lo abordarías?"));
        assert!(!prompt.contains("How would you approach it?"));
        assert!(!prompt.contains("Como você abordaria isso?"));
        assert!(prompt.contains("Cuéntame de una negociación difícil"));
        assert!(!prompt.contains("{{"));
        assert_eq!(deepgram_language_code("es"), "es-419");
        assert_eq!(deepgram_language_code("en-US"), "en");
        assert_eq!(deepgram_language_code("pt-BR"), "pt-BR");
    }
}
