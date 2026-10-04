//! Memoria local por sesión de `question-detect`.
//!
//! Solo guarda estado; las decisiones (repetida, continuación, escenario, nueva)
//! las toma el modelo.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

const MAX_PARAGRAPHS: usize = 8;
const MAX_ANSWERED: usize = 12;

#[derive(Clone, Default, Debug)]
pub struct SessionSnapshot {
    /// Fragmentos STT anteriores al actual, viejo → nuevo.
    pub paragraphs: Vec<String>,
    pub answered: Vec<String>,
    pub last_question: Option<String>,
    /// Escenario planteado sin pregunta todavía.
    pub pending_context: Option<String>,
}

#[derive(Clone, Default)]
pub struct QuestionSessionStore {
    inner: Arc<RwLock<HashMap<String, SessionSnapshot>>>,
}

impl QuestionSessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn session_key(user_id: &str, session_id: &str) -> String {
        format!("{user_id}:{}", session_id.trim())
    }

    /// Estado previo al fragmento actual y registro del fragmento en el historial.
    pub async fn begin_turn(&self, key: &str, fragment: &str) -> SessionSnapshot {
        let mut guard = self.inner.write().await;
        let state = guard.entry(key.to_string()).or_default();
        let snapshot = state.clone();
        let trimmed = fragment.trim();
        if !trimmed.is_empty() {
            state.paragraphs.push(trimmed.to_string());
            if state.paragraphs.len() > MAX_PARAGRAPHS {
                let drop = state.paragraphs.len() - MAX_PARAGRAPHS;
                state.paragraphs.drain(0..drop);
            }
        }
        snapshot
    }

    /// Pregunta a responder: pasa a ser la pendiente; la anterior queda como
    /// respondida salvo que la nueva la continúe (ya la incluye).
    pub async fn record_question(&self, key: &str, question: &str, continues_last: bool) {
        let question = question.trim();
        if question.is_empty() {
            return;
        }
        let mut guard = self.inner.write().await;
        let state = guard.entry(key.to_string()).or_default();
        if let Some(prev) = state.last_question.take() {
            if !continues_last && !state.answered.contains(&prev) {
                state.answered.push(prev);
                if state.answered.len() > MAX_ANSWERED {
                    let drop = state.answered.len() - MAX_ANSWERED;
                    state.answered.drain(0..drop);
                }
            }
        }
        state.last_question = Some(question.to_string());
        state.pending_context = None;
    }

    /// Guarda el escenario completo (el modelo ya lo devuelve fusionado).
    pub async fn set_pending_context(&self, key: &str, context: &str) {
        let context = context.trim();
        if context.is_empty() {
            return;
        }
        let mut guard = self.inner.write().await;
        guard.entry(key.to_string()).or_default().pending_context = Some(context.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn begin_turn_returns_previous_state_and_keeps_last_eight() {
        let store = QuestionSessionStore::new();
        for i in 0..10 {
            store.begin_turn("k", &format!("p{i}")).await;
        }
        let snap = store.begin_turn("k", "p10").await;
        assert_eq!(snap.paragraphs.len(), 8);
        assert_eq!(snap.paragraphs[0], "p2");
        assert_eq!(snap.paragraphs[7], "p9");
    }

    #[tokio::test]
    async fn record_question_moves_previous_to_answered_unless_continuation() {
        let store = QuestionSessionStore::new();
        store.record_question("k", "Q1", false).await;
        store.record_question("k", "Q1 + detalle", true).await;
        store.record_question("k", "Q2", false).await;
        let snap = store.begin_turn("k", "").await;
        assert_eq!(snap.answered, vec!["Q1 + detalle".to_string()]);
        assert_eq!(snap.last_question.as_deref(), Some("Q2"));
    }

    #[tokio::test]
    async fn pending_context_is_cleared_when_a_question_is_recorded() {
        let store = QuestionSessionStore::new();
        store.set_pending_context("k", "Escenario").await;
        assert_eq!(
            store.begin_turn("k", "").await.pending_context.as_deref(),
            Some("Escenario")
        );
        store.record_question("k", "Escenario. ¿Qué harías?", false).await;
        assert!(store.begin_turn("k", "").await.pending_context.is_none());
    }
}
