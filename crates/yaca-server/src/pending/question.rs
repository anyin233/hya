use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{Mutex, mpsc};
use yaca_proto::SessionId;
use yaca_tool::{
    QuestionAnswer, QuestionInfo as ToolQuestionInfo, QuestionKind, QuestionPrompt, QuestionReply,
    QuestionRequest,
};

#[derive(Clone, Default)]
pub(crate) struct QuestionRequests {
    inner: Arc<Mutex<BTreeMap<String, PendingQuestion>>>,
}

struct PendingQuestion {
    session: Option<SessionId>,
    questions: Vec<QuestionPrompt>,
    reply: QuestionReply,
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
                    questions: req.questions,
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
            .send_many(answers_from_reply(entry.questions, answers))
            .is_ok()
    }

    pub(crate) async fn reject(&self, session: SessionId, id: &str) -> bool {
        let entry = self.take(session, id).await;
        let Some(entry) = entry else {
            return false;
        };
        reject_entry(entry)
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
            .send_many(answers_from_reply(entry.questions, answers))
            .is_ok()
    }

    pub(crate) async fn reject_any(&self, id: &str) -> bool {
        let entry = self.take_any(id).await;
        let Some(entry) = entry else {
            return false;
        };
        reject_entry(entry)
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
        questions: entry
            .questions
            .iter()
            .map(|question| question_info(&question.info))
            .collect(),
    })
}

fn question_info(info: &ToolQuestionInfo) -> QuestionInfo {
    QuestionInfo {
        question: info.question.clone(),
        header: info.header.clone(),
        options: info
            .options
            .iter()
            .map(|option| QuestionOption {
                label: option.label.clone(),
                description: option.description.clone(),
            })
            .collect(),
        multiple: info.multiple.then_some(true),
        custom: info.custom,
    }
}

fn answers_from_reply(
    questions: Vec<QuestionPrompt>,
    answers: Vec<Vec<String>>,
) -> Vec<QuestionAnswer> {
    questions
        .into_iter()
        .enumerate()
        .map(|(index, question)| {
            answer_from_labels(
                question.kind,
                answers.get(index).cloned().unwrap_or_default(),
            )
        })
        .collect()
}

fn answer_from_labels(kind: QuestionKind, answer: Vec<String>) -> QuestionAnswer {
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

fn reject_entry(entry: PendingQuestion) -> bool {
    entry
        .reply
        .send_many(vec![QuestionAnswer::Cancelled; entry.questions.len()])
        .is_ok()
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
