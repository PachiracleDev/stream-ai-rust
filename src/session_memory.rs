//! Estado de la entrevista que el backend posee.
//!
//! El cliente no manda `lastQuestion` ni `pendingContext`: cada decisión del
//! detector se aplica aquí, y el agente de respuesta lee el historial desde aquí.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::RwLock;

use crate::relay::body::{InterviewKind, RelayValues};

const MAX_ANSWERED: usize = 5;
const MAX_HISTORY_PAIRS: usize = 6;
const PENDING_TTL: Duration = Duration::from_secs(120);

#[derive(Clone, Debug)]
pub struct SessionSnapshot {
    pub last_question: Option<String>,
    pub last_question_continues: bool,
    pub answered: Vec<String>,
    /// Vacío si el escenario tiene más de dos minutos.
    pub pending_context: Option<String>,
    pub last_seq: u64,
}

#[derive(Clone, Debug)]
pub struct PromptConfig {
    pub values: RelayValues,
    pub kind: InterviewKind,
}

#[derive(Clone, Debug)]
pub struct AnswerPair {
    pub question: String,
    pub answer: String,
}

#[derive(Clone, Debug)]
pub enum SessionAction {
    Respond {
        question: String,
        continues_last: bool,
    },
    Premise {
        context: String,
    },
    Wait,
    Ignore,
}

#[derive(Clone, Debug)]
struct SessionState {
    last_question: Option<String>,
    last_question_continues: bool,
    answered: Vec<String>,
    pending_context: Option<String>,
    pending_context_at: Option<SystemTime>,
    last_seq: u64,
    /// Seq más alto aceptado, aunque su respuesta todavía no se haya aplicado.
    highest_seq: u64,
    history: Vec<AnswerPair>,
    prompt: Option<PromptConfig>,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            last_question: None,
            last_question_continues: false,
            answered: Vec::new(),
            pending_context: None,
            pending_context_at: None,
            last_seq: 0,
            highest_seq: 0,
            history: Vec::new(),
            prompt: None,
        }
    }
}

impl SessionState {
    fn snapshot_at(&self, now: SystemTime) -> SessionSnapshot {
        SessionSnapshot {
            last_question: self.last_question.clone(),
            last_question_continues: self.last_question_continues,
            answered: self.answered.clone(),
            pending_context: visible_pending(self, now),
            last_seq: self.last_seq,
        }
    }
}

fn visible_pending(state: &SessionState, now: SystemTime) -> Option<String> {
    let context = state.pending_context.as_ref()?.trim();
    if context.is_empty() {
        return None;
    }
    let fresh = state.pending_context_at.is_some_and(|at| {
        now.duration_since(at)
            .map(|age| age <= PENDING_TTL)
            .unwrap_or(false)
    });
    fresh.then(|| context.to_string())
}

#[derive(Clone, Default)]
pub struct QuestionSessionStore {
    inner: Arc<RwLock<HashMap<String, SessionState>>>,
}

impl QuestionSessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn session_key(user_id: &str, interview_id: i64) -> String {
        format!("{user_id}:{interview_id}")
    }

    /// Rechaza un seq ya aplicado. El error es el `last_seq` vigente para que el cliente continúe.
    pub async fn begin_seq(&self, key: &str, seq: u64) -> Result<SessionSnapshot, u64> {
        let mut guard = self.inner.write().await;
        let state = guard.entry(key.to_string()).or_default();
        if seq <= state.last_seq {
            return Err(state.last_seq);
        }
        if seq > state.highest_seq {
            state.highest_seq = seq;
        }
        Ok(state.snapshot_at(SystemTime::now()))
    }

    pub async fn is_latest(&self, key: &str, seq: u64) -> bool {
        let guard = self.inner.read().await;
        guard
            .get(key)
            .is_some_and(|state| seq == state.highest_seq && seq > state.last_seq)
    }

    /// Aplica la acción solo si este seq sigue siendo el más alto aceptado.
    pub async fn commit_if_latest(&self, key: &str, seq: u64, action: SessionAction) -> bool {
        let mut guard = self.inner.write().await;
        let state = guard.entry(key.to_string()).or_default();
        if seq != state.highest_seq || seq <= state.last_seq {
            return false;
        }
        apply_action(state, action);
        state.last_seq = seq;
        true
    }

    pub async fn remember_prompt(&self, key: &str, values: RelayValues, kind: InterviewKind) {
        let mut guard = self.inner.write().await;
        guard.entry(key.to_string()).or_default().prompt = Some(PromptConfig { values, kind });
    }

    pub async fn prompt_config(&self, key: &str) -> Option<PromptConfig> {
        let guard = self.inner.read().await;
        guard.get(key).and_then(|state| state.prompt.clone())
    }

    pub async fn history(&self, key: &str) -> Vec<AnswerPair> {
        let guard = self.inner.read().await;
        guard
            .get(key)
            .map(|state| state.history.clone())
            .unwrap_or_default()
    }

    /// Lo que el candidato llegó a oír, aunque el front haya cortado la generación.
    pub async fn push_answer(&self, key: &str, question: &str, answer: &str) {
        let question = question.trim();
        let answer = answer.trim();
        if question.is_empty() || answer.is_empty() {
            return;
        }
        let mut guard = self.inner.write().await;
        let state = guard.entry(key.to_string()).or_default();
        if state
            .history
            .last()
            .is_some_and(|pair| pair.question == question && pair.answer == answer)
        {
            return;
        }
        state.history.push(AnswerPair {
            question: question.to_string(),
            answer: answer.to_string(),
        });
        if state.history.len() > MAX_HISTORY_PAIRS {
            let drop_n = state.history.len() - MAX_HISTORY_PAIRS;
            state.history.drain(0..drop_n);
        }
    }
}

fn apply_action(state: &mut SessionState, action: SessionAction) {
    match action {
        SessionAction::Respond {
            question,
            continues_last,
        } => {
            let question = question.trim().to_string();
            if question.is_empty() {
                return;
            }
            if let Some(prev) = state.last_question.take() {
                if !continues_last && !state.answered.iter().any(|item| item == &prev) {
                    state.answered.push(prev);
                    if state.answered.len() > MAX_ANSWERED {
                        let drop_n = state.answered.len() - MAX_ANSWERED;
                        state.answered.drain(0..drop_n);
                    }
                }
            }
            state.last_question = Some(question);
            state.last_question_continues = continues_last;
            state.pending_context = None;
            state.pending_context_at = None;
        }
        SessionAction::Premise { context } => {
            let context = context.trim().to_string();
            if context.is_empty() {
                return;
            }
            state.pending_context = Some(context);
            state.pending_context_at = Some(SystemTime::now());
        }
        SessionAction::Wait | SessionAction::Ignore => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stale_seq_is_rejected_and_only_the_latest_commit_changes_state() {
        let store = QuestionSessionStore::new();
        store.begin_seq("k", 1).await.unwrap();
        store.begin_seq("k", 2).await.unwrap();
        assert!(!store
            .commit_if_latest(
                "k",
                1,
                SessionAction::Respond {
                    question: "vieja".into(),
                    continues_last: false,
                },
            )
            .await);
        assert!(store
            .commit_if_latest(
                "k",
                2,
                SessionAction::Respond {
                    question: "nueva".into(),
                    continues_last: false,
                },
            )
            .await);
        assert!(store.begin_seq("k", 2).await.is_err());
        let snap = store.begin_seq("k", 3).await.unwrap();
        assert_eq!(snap.last_question.as_deref(), Some("nueva"));
    }

    #[tokio::test]
    async fn respond_keeps_five_answered_and_clears_pending() {
        let store = QuestionSessionStore::new();
        store
            .commit_if_latest(
                "k",
                {
                    store.begin_seq("k", 1).await.unwrap();
                    1
                },
                SessionAction::Premise {
                    context: "Escenario".into(),
                },
            )
            .await;
        for i in 2..=8 {
            store.begin_seq("k", i).await.unwrap();
            store
                .commit_if_latest(
                    "k",
                    i,
                    SessionAction::Respond {
                        question: format!("Q{i}"),
                        continues_last: false,
                    },
                )
                .await;
        }
        let snap = store.begin_seq("k", 9).await.unwrap();
        assert_eq!(snap.answered, vec!["Q3", "Q4", "Q5", "Q6", "Q7"]);
        assert_eq!(snap.last_question.as_deref(), Some("Q8"));
        assert!(snap.pending_context.is_none());
    }

    #[tokio::test]
    async fn continuation_does_not_archive_the_question_it_extends() {
        let store = QuestionSessionStore::new();
        store.begin_seq("k", 1).await.unwrap();
        store
            .commit_if_latest(
                "k",
                1,
                SessionAction::Respond {
                    question: "Q1".into(),
                    continues_last: false,
                },
            )
            .await;
        store.begin_seq("k", 2).await.unwrap();
        store
            .commit_if_latest(
                "k",
                2,
                SessionAction::Respond {
                    question: "Q1 ¿Por qué?".into(),
                    continues_last: true,
                },
            )
            .await;
        let snap = store.begin_seq("k", 3).await.unwrap();
        assert!(snap.answered.is_empty());
        assert!(snap.last_question_continues);
    }

    #[tokio::test]
    async fn pending_context_older_than_two_minutes_is_empty() {
        let store = QuestionSessionStore::new();
        {
            let mut guard = store.inner.write().await;
            let state = guard.entry("k".to_string()).or_default();
            state.pending_context = Some("viejo".into());
            state.pending_context_at = Some(SystemTime::now() - Duration::from_secs(121));
        }
        let snap = store.begin_seq("k", 1).await.unwrap();
        assert!(snap.pending_context.is_none());
    }

    #[tokio::test]
    async fn history_keeps_the_last_six_pairs() {
        let store = QuestionSessionStore::new();
        for i in 0..8 {
            store.push_answer("k", &format!("Q{i}"), &format!("A{i}")).await;
        }
        let history = store.history("k").await;
        assert_eq!(history.len(), 6);
        assert_eq!(history[0].question, "Q2");
        assert_eq!(history[5].answer, "A7");
    }
}
