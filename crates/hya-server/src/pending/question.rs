use std::collections::BTreeMap;
use std::sync::Arc;

use hya_proto::SessionId;
use hya_tool::{
    QuestionAnswer, QuestionInfo as ToolQuestionInfo, QuestionKind, QuestionPrompt, QuestionReply,
    QuestionRequest,
};
use serde::Serialize;
use tokio::sync::{Mutex, broadcast, mpsc};

use crate::state::ServerHandlers;

use super::PendingEvent;

#[derive(Clone)]
pub(crate) struct QuestionRequests {
    inner: Arc<Mutex<BTreeMap<String, PendingQuestion>>>,
    events: broadcast::Sender<PendingEvent>,
}

struct PendingQuestion {
    session: Option<SessionId>,
    questions: Vec<QuestionPrompt>,
    reply: QuestionReply,
}

#[derive(Clone)]
pub(crate) struct QuestionRequestView {
    pub(crate) id: String,
    pub(crate) session: SessionId,
    pub(crate) questions: Vec<QuestionInfo>,
}

#[derive(Clone, Serialize)]
pub(crate) struct QuestionInfo {
    pub(crate) question: String,
    pub(crate) header: String,
    pub(crate) options: Vec<QuestionOption>,
    #[serde(skip_serializing_if = "Option::is_none")]
    multiple: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    custom: Option<bool>,
}

#[derive(Clone, Serialize)]
pub(crate) struct QuestionOption {
    pub(crate) label: String,
    description: String,
}

impl QuestionRequests {
    #[must_use]
    pub(crate) fn new() -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            inner: Arc::default(),
            events,
        }
    }

    #[must_use]
    pub(crate) fn spawn(
        mut rx: mpsc::UnboundedReceiver<QuestionRequest>,
        handlers: &ServerHandlers,
    ) -> Self {
        let requests = Self::new();
        let inner = requests.inner.clone();
        let events = requests.events.clone();
        handlers.spawn(async move {
            while let Some(req) = rx.recv().await {
                let entry = PendingQuestion {
                    session: req.session,
                    questions: req.questions,
                    reply: req.reply,
                };
                let request_id = req.id.to_string();
                let asked = question_view(&request_id, &entry);
                inner.lock().await.insert(request_id, entry);
                if let Some(asked) = asked {
                    let _published = events.send(PendingEvent::QuestionRequested(asked));
                }
            }
        });
        requests
    }

    /// Subscribe to live v1 interaction events.
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<PendingEvent> {
        self.events.subscribe()
    }

    #[cfg(test)]
    pub(crate) fn receiver_count(&self) -> usize {
        self.events.receiver_count()
    }

    pub(crate) async fn list(&self) -> Vec<QuestionRequestView> {
        self.inner
            .lock()
            .await
            .iter()
            .filter_map(|(id, entry)| question_view(id, entry))
            .collect()
    }

    pub(crate) async fn session_for(&self, id: &str) -> Option<SessionId> {
        self.inner.lock().await.get(id)?.session
    }

    pub(crate) async fn count(&self) -> usize {
        self.inner
            .lock()
            .await
            .values()
            .filter(|entry| entry.session.is_some())
            .count()
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
        let ok = entry
            .reply
            .send_many(answers_from_reply(entry.questions, answers))
            .is_ok();
        if ok {
            self.publish_resolved(session, id);
        }
        ok
    }

    pub(crate) async fn reject(&self, session: SessionId, id: &str) -> bool {
        let entry = self.take(session, id).await;
        let Some(entry) = entry else {
            return false;
        };
        let ok = reject_entry(entry);
        if ok {
            self.publish_resolved(session, id);
        }
        ok
    }

    async fn take(&self, session: SessionId, id: &str) -> Option<PendingQuestion> {
        let mut pending = self.inner.lock().await;
        let entry = pending.get(id)?;
        if entry.session != Some(session) {
            return None;
        }
        pending.remove(id)
    }

    fn publish_resolved(&self, session: SessionId, id: &str) {
        let _published = self.events.send(PendingEvent::Resolved {
            session: Some(session),
            id: id.to_owned(),
        });
    }
}

fn question_view(id: &str, entry: &PendingQuestion) -> Option<QuestionRequestView> {
    Some(QuestionRequestView {
        id: id.to_string(),
        session: entry.session?,
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
