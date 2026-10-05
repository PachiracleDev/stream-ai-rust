//! Correcciones deterministas de la salida del detector.
//!
//! El modelo propone; estas reglas impiden que un turno manual se quede sin
//! respuesta y que un `respond` o un `premise` vacíos lleguen al front.

use crate::cerebras::detect::{Confidence, DetectAction, DetectOutput, QuestionKind};
use crate::relay::body::DetectTrigger;

const MAX_FIELD_CHARS: usize = 1500;

pub fn apply_detect_validation(
    mut output: DetectOutput,
    trigger: DetectTrigger,
    new_text: &str,
    last_question: &str,
) -> (DetectOutput, Vec<String>) {
    let mut corrections = Vec::new();
    let new_text = new_text.trim();
    let last_question = last_question.trim();

    if trigger == DetectTrigger::Manual && output.action != DetectAction::Respond {
        corrections.push("manual_force_respond".to_string());
        output.action = DetectAction::Respond;
        output.question = manual_question(new_text, last_question);
        output.context.clear();
        output.continues_last = false;
        output.kind = QuestionKind::None;
    }

    if output.action == DetectAction::Respond && output.question.trim().is_empty() {
        if trigger == DetectTrigger::Manual {
            corrections.push("manual_empty_question".to_string());
            output.question = manual_question(new_text, last_question);
        } else {
            corrections.push("empty_respond_to_ignore".to_string());
            output.action = DetectAction::Ignore;
            output.question.clear();
            output.continues_last = false;
            output.kind = QuestionKind::None;
        }
    }

    if output.action == DetectAction::Premise && output.context.trim().is_empty() {
        corrections.push("empty_premise_to_ignore".to_string());
        output.action = DetectAction::Ignore;
        output.context.clear();
        output.kind = QuestionKind::None;
    }

    let question = clamp_field(output.question.trim());
    let context = clamp_field(output.context.trim());
    if question != output.question || context != output.context {
        corrections.push("trim_limit".to_string());
    }
    output.question = question;
    output.context = context;

    if output.action != DetectAction::Respond {
        output.kind = QuestionKind::None;
        if output.action != DetectAction::Premise {
            output.context.clear();
        }
        if output.action != DetectAction::Respond {
            output.continues_last = false;
        }
    }

    (output, corrections)
}

fn manual_question(new_text: &str, last_question: &str) -> String {
    if new_text.is_empty() {
        last_question.to_string()
    } else {
        new_text.to_string()
    }
}

fn clamp_field(value: &str) -> String {
    value.chars().take(MAX_FIELD_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn output(action: DetectAction, question: &str, context: &str) -> DetectOutput {
        DetectOutput {
            action,
            question: question.to_string(),
            context: context.to_string(),
            continues_last: false,
            kind: QuestionKind::Technical,
            confidence: Confidence::High,
        }
    }

    #[test]
    fn manual_trigger_always_responds() {
        let (out, corrections) = apply_detect_validation(
            output(DetectAction::Ignore, "", ""),
            DetectTrigger::Manual,
            "cuéntame de un conflicto",
            "pregunta previa",
        );
        assert_eq!(out.action, DetectAction::Respond);
        assert_eq!(out.question, "cuéntame de un conflicto");
        assert_eq!(corrections, vec!["manual_force_respond".to_string()]);
    }

    #[test]
    fn manual_without_new_text_reuses_last_question() {
        let (out, _) = apply_detect_validation(
            output(DetectAction::Wait, "", ""),
            DetectTrigger::Manual,
            "",
            "¿Cuál es tu disponibilidad?",
        );
        assert_eq!(out.question, "¿Cuál es tu disponibilidad?");
    }

    #[test]
    fn automatic_empty_respond_becomes_ignore() {
        let (out, corrections) = apply_detect_validation(
            output(DetectAction::Respond, "  ", ""),
            DetectTrigger::Auto,
            "ajá",
            "",
        );
        assert_eq!(out.action, DetectAction::Ignore);
        assert!(corrections.iter().any(|c| c == "empty_respond_to_ignore"));
    }

    #[test]
    fn empty_premise_becomes_ignore_and_fields_are_clamped() {
        let (out, corrections) = apply_detect_validation(
            output(DetectAction::Premise, "", "   "),
            DetectTrigger::Auto,
            "algo",
            "",
        );
        assert_eq!(out.action, DetectAction::Ignore);
        assert!(corrections.iter().any(|c| c == "empty_premise_to_ignore"));

        let long = "á".repeat(1600);
        let (out, corrections) = apply_detect_validation(
            output(DetectAction::Respond, &long, ""),
            DetectTrigger::Auto,
            "pregunta",
            "",
        );
        assert_eq!(out.question.chars().count(), 1500);
        assert!(corrections.iter().any(|c| c == "trim_limit"));
    }

    #[test]
    fn fixture_cases_match_the_validator() {
        let raw = include_str!("../../cases/detector-cases.json");
        let file: serde_json::Value = serde_json::from_str(raw).unwrap();
        for case in file["cases"].as_array().unwrap() {
            let model = &case["model"];
            let parsed = crate::cerebras::detect::parse_detect_output_for_test(model);
            let trigger = match case["trigger"].as_str().unwrap() {
                "manual" => DetectTrigger::Manual,
                "silence_after_premise" => DetectTrigger::SilenceAfterPremise,
                _ => DetectTrigger::Auto,
            };
            let (out, corrections) = apply_detect_validation(
                parsed,
                trigger,
                case["newText"].as_str().unwrap_or(""),
                case["lastQuestion"].as_str().unwrap_or(""),
            );
            assert_eq!(
                out.action.as_str(),
                case["expect"]["action"].as_str().unwrap(),
                "{}",
                case["name"]
            );
            assert_eq!(
                out.question,
                case["expect"]["question"].as_str().unwrap_or(""),
                "{}",
                case["name"]
            );
            let expected: Vec<String> = case["expect"]["corrections"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();
            assert_eq!(corrections, expected, "{}", case["name"]);
            let _ = json!({});
        }
    }
}
