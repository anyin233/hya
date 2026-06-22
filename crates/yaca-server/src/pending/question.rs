use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{Mutex, mpsc, oneshot};
use yaca_proto::SessionId;
use yaca_tool::{QuestionAnswer, QuestionKind, QuestionRequest};

#[derive(Clone, Default)]
pub(crate) struct QuestionRequests {
    inner: Arc<Mutex<BTreeMap<String, PendingQuestion>>>,
}

struct PendingQuestion {
    session: Option<SessionId>,
    prompt: String,
    kind: QuestionKind,
    reply: oneshot::Sender<QuestionAnswer>,
}

#[derive(Clone, Serialize)]
pub(crate) struct QuestionRequestView {
    id: String,
    #[serde(rename = "sessionID")]
    session_id: String,
    questions: Vec<QuestionInfo>,
}

#[derive(Clone, Serialize)]
struct QuestionInfo {
    question: String,
    header: String,
    options: Vec<QuestionOption>,
    #[serde(skip_serializing_if = "Option::is_none")]
    multiple: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    custom: Option<bool>,
}

#[derive(Clone, Serialize)]
struct QuestionOption {
    label: String,
    description: String,
}

impl QuestionRequests {
    #[must_use]
    pub(crate) fn spawn(mut rx: mpsc::UnboundedReceiver<QuestionRequest>) -> Self {
        let requests = Self::default();
        let inner = requests.inner.clone();
        std::mem::drop(tokio::spawn(async move {
            while let Some(req) = rx.recv().await {
                let entry = PendingQuestion {
                    session: req.session,
                    prompt: req.prompt,
                    kind: req.kind,
                    reply: req.reply,
                };
                inner.lock().await.insert(req.id.to_string(), entry);
            }
        }));
        requests
    }

    pub(crate) async fn list(&self) -> Vec<QuestionRequestView> {
        self.inner
            .lock()
            .await
            .iter()
            .filter_map(|(id, entry)| question_view(id, entry))
            .collect()
    }

    pub(crate) async fn list_session(&self, session: SessionId) -> Vec<QuestionRequestView> {
        self.inner
            .lock()
            .await
            .iter()
            .filter_map(|(id, entry)| {
                (entry.session == Some(session))
                    .then(|| question_view(id, entry))
                    .flatten()
            })
            .collect()
    }

    pub(crate) async fn reply(
        &self,
        session: SessionId,
        id: &str,
        answers: Vec<Vec<String>>,
    ) -> bool {
        let entry = self.take(session, id).await;
        let Some(entry) = entry else {
            return false;
        };
        entry
            .reply
            .send(answer_from_reply(entry.kind, answers))
            .is_ok()
    }

    pub(crate) async fn reject(&self, session: SessionId, id: &str) -> bool {
        let entry = self.take(session, id).await;
        let Some(entry) = entry else {
            return false;
        };
        entry.reply.send(QuestionAnswer::Cancelled).is_ok()
    }

    pub(crate) async fn contains(&self, id: &str) -> bool {
        self.inner.lock().await.contains_key(id)
    }

    pub(crate) async fn reply_any(&self, id: &str, answers: Vec<Vec<String>>) -> bool {
        let entry = self.take_any(id).await;
        let Some(entry) = entry else {
            return false;
        };
        entry
            .reply
            .send(answer_from_reply(entry.kind, answers))
            .is_ok()
    }

    pub(crate) async fn reject_any(&self, id: &str) -> bool {
        let entry = self.take_any(id).await;
        let Some(entry) = entry else {
            return false;
        };
        entry.reply.send(QuestionAnswer::Cancelled).is_ok()
    }

    async fn take(&self, session: SessionId, id: &str) -> Option<PendingQuestion> {
        let mut pending = self.inner.lock().await;
        let entry = pending.get(id)?;
        if entry.session != Some(session) {
            return None;
        }
        pending.remove(id)
    }

    async fn take_any(&self, id: &str) -> Option<PendingQuestion> {
        self.inner.lock().await.remove(id)
    }
}

fn question_view(id: &str, entry: &PendingQuestion) -> Option<QuestionRequestView> {
    Some(QuestionRequestView {
        id: id.to_string(),
        session_id: entry.session?.to_string(),
        questions: vec![question_info(&entry.prompt, &entry.kind)],
    })
}

fn question_info(prompt: &str, kind: &QuestionKind) -> QuestionInfo {
    match kind {
        QuestionKind::FreeText { .. } => QuestionInfo {
            question: prompt.to_string(),
            header: header(prompt),
            options: Vec::new(),
            multiple: None,
            custom: Some(true),
        },
        QuestionKind::Select {
            options,
            allow_custom,
        } => QuestionInfo {
            question: prompt.to_string(),
            header: header(prompt),
            options: options
                .iter()
                .map(|label| QuestionOption {
                    label: label.clone(),
                    description: String::new(),
                })
                .collect(),
            multiple: None,
            custom: (*allow_custom).then_some(true),
        },
    }
}

fn header(prompt: &str) -> String {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        return "Question".to_string();
    }
    trimmed.chars().take(30).collect()
}

fn answer_from_reply(kind: QuestionKind, answers: Vec<Vec<String>>) -> QuestionAnswer {
    let answer = answers.into_iter().next().unwrap_or_default();
    match kind {
        QuestionKind::FreeText { default } => {
            QuestionAnswer::FreeText(answer.into_iter().next().or(default).unwrap_or_default())
        }
        QuestionKind::Select {
            options,
            allow_custom,
        } => select_answer(options, allow_custom, answer),
    }
}

fn select_answer(options: Vec<String>, allow_custom: bool, answer: Vec<String>) -> QuestionAnswer {
    let mut selected = Vec::new();
    for label in answer {
        if let Some(index) = options.iter().position(|option| option == &label) {
            selected.push(index);
        } else if allow_custom {
            return QuestionAnswer::FreeText(label);
        }
    }
    if selected.len() == 1 {
        QuestionAnswer::Selected(selected[0])
    } else {
        QuestionAnswer::SelectedMany(selected)
    }
}
