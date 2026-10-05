//! Registro por turno para evaluar cambios de prompt con casos reales.

use std::io::Write;
use std::path::Path;

use serde_json::{json, Value};

pub fn append_turn_log(entry: Value) {
    let path = std::env::var("INTERVIEW_TURN_LOG")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "logs/interview-turns.jsonl".to_string());
    if let Err(error) = append_line(Path::new(&path), &entry) {
        tracing::warn!(error = %error, path = %path, "no se pudo guardar el log del turno");
    }
}

fn append_line(path: &Path, entry: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let mut line = entry.to_string();
    line.push('\n');
    file.write_all(line.as_bytes()).map_err(|e| e.to_string())
}

pub fn detect_log(
    interview_id: i64,
    seq: u64,
    detector_input: Value,
    detector_output: Value,
    latency_ms: u64,
    corrections: &[String],
) -> Value {
    json!({
        "phase": "detect",
        "at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "interviewId": interview_id,
        "seq": seq,
        "detectorInput": detector_input,
        "detectorOutput": detector_output,
        "latencyMs": latency_ms,
        "corrections": corrections,
    })
}

pub fn answer_log(
    interview_id: i64,
    question: &str,
    response: &str,
    cancelled: bool,
    regenerated: bool,
) -> Value {
    json!({
        "phase": "answer",
        "at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "interviewId": interview_id,
        "question": question,
        "response": response,
        "cancelled": cancelled,
        "regenerated": regenerated,
    })
}
