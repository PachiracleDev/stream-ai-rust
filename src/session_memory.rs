//! Memoria local por sesión: párrafos STT, preguntas respondidas y última pregunta.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

const MAX_PARAGRAPHS: usize = 8;

#[derive(Clone, Default)]
struct SessionState {
    paragraphs: Vec<String>,
    answered: Vec<String>,
    last_question: Option<String>,
}

#[derive(Clone, Default)]
pub struct QuestionSessionStore {
    inner: Arc<RwLock<HashMap<String, SessionState>>>,
}

impl QuestionSessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn session_key(user_id: &str, session_id: &str) -> String {
        format!("{user_id}:{}", session_id.trim())
    }

    /// Añade un fragmento STT y devuelve los párrafos actuales (máx. 8, viejo → nuevo).
    pub async fn push_paragraph(&self, key: &str, text: &str) -> Vec<String> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return self.paragraphs(key).await;
        }
        let mut guard = self.inner.write().await;
        let state = guard.entry(key.to_string()).or_default();
        state.paragraphs.push(trimmed.to_string());
        if state.paragraphs.len() > MAX_PARAGRAPHS {
            let drop = state.paragraphs.len() - MAX_PARAGRAPHS;
            state.paragraphs.drain(0..drop);
        }
        state.paragraphs.clone()
    }

    pub async fn paragraphs(&self, key: &str) -> Vec<String> {
        self.inner
            .read()
            .await
            .get(key)
            .map(|s| s.paragraphs.clone())
            .unwrap_or_default()
    }

    pub async fn last_question(&self, key: &str) -> Option<String> {
        self.inner
            .read()
            .await
            .get(key)
            .and_then(|s| s.last_question.clone())
    }

    pub async fn set_last_question(&self, key: &str, question: &str) {
        let trimmed = question.trim();
        if trimmed.is_empty() {
            return;
        }
        let mut guard = self.inner.write().await;
        guard.entry(key.to_string()).or_default().last_question = Some(trimmed.to_string());
    }

    pub async fn answered_questions(&self, key: &str) -> Vec<String> {
        self.inner
            .read()
            .await
            .get(key)
            .map(|s| s.answered.clone())
            .unwrap_or_default()
    }

    pub async fn record_answered(&self, key: &str, question: &str) {
        let normalized = normalize_question(question);
        if normalized.is_empty() {
            return;
        }
        let mut guard = self.inner.write().await;
        let state = guard.entry(key.to_string()).or_default();
        if !state
            .answered
            .iter()
            .any(|q| questions_equivalent(q, &normalized))
        {
            state.answered.push(normalized);
        }
        state.last_question = Some(question.trim().to_string());
    }

    pub async fn already_answered(&self, key: &str, question: &str) -> bool {
        let normalized = normalize_question(question);
        if normalized.is_empty() {
            return false;
        }
        self.inner.read().await.get(key).is_some_and(|state| {
            state
                .answered
                .iter()
                .any(|q| questions_equivalent(q, &normalized))
        })
    }
}

pub fn normalize_question(raw: &str) -> String {
    raw.to_lowercase()
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn questions_equivalent(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    a.contains(b) || b.contains(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn push_paragraph_keeps_last_eight() {
        let store = QuestionSessionStore::new();
        let key = "u:1";
        for i in 0..10 {
            store.push_paragraph(key, &format!("p{i}")).await;
        }
        let paragraphs = store.paragraphs(key).await;
        assert_eq!(paragraphs.len(), 8);
        assert_eq!(paragraphs[0], "p2");
        assert_eq!(paragraphs[7], "p9");
    }

    #[test]
    fn normalize_strips_punctuation_and_case() {
        assert_eq!(
            normalize_question("¿How would you design a rate limiter?"),
            "how would you design a rate limiter"
        );
    }
}
