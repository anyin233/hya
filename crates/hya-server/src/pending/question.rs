use std::collections::BTreeMap;
use std::sync::Arc;

use hya_proto::SessionId;
use hya_tool::{
    QuestionAnswer, QuestionInfo as ToolQuestionInfo, QuestionKind, QuestionPrompt, QuestionReply,
    QuestionRequest,
};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, broadcast, mpsc};

#[derive(Clone)]
pub(crate) struct QuestionRequests {
    inner: Arc<Mutex<BTreeMap<String, PendingQuestion>>>,
    events: broadcast::Sender<Value>,
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
    fn new() -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            inner: Arc::default(),
            events,
        }
    }

    #[must_use]
    pub(crate) fn spawn(mut rx: mpsc::UnboundedReceiver<QuestionRequest>) -> Self {
        let requests = Self::new();
        let inner = requests.inner.clone();
        let events = requests.events.clone();
        std::mem::drop(tokio::spawn(async move {
            while let Some(req) = rx.recv().await {
                let entry = PendingQuestion {
                    session: req.session,
                    questions: req.questions,
                    reply: req.reply,
                };
                let request_id = req.id.to_string();
                let asked = question_asked_event(&request_id, &entry);
                inner.lock().await.insert(request_id, entry);
                if let Some(asked) = asked {
                    let _published = events.send(asked);
                }
            }
        }));
        requests
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.events.subscribe()
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
        let event_answers = answers.clone();
        let ok = entry
            .reply
            .send_many(answers_from_reply(entry.questions, answers))
            .is_ok();
        if ok {
            self.publish_replied(Some(session), id, event_answers);
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
            self.publish_rejected(Some(session), id);
        }
        ok
    }

    pub(crate) async fn contains(&self, id: &str) -> bool {
        self.inner.lock().await.contains_key(id)
    }

    pub(crate) async fn reply_any(&self, id: &str, answers: Vec<Vec<String>>) -> bool {
        let entry = self.take_any(id).await;
        let Some(entry) = entry else {
            return false;
        };
        let session = entry.session;
        let event_answers = answers.clone();
        let ok = entry
            .reply
            .send_many(answers_from_reply(entry.questions, answers))
            .is_ok();
        if ok {
            self.publish_replied(session, id, event_answers);
        }
        ok
    }

    pub(crate) async fn reject_any(&self, id: &str) -> bool {
        let entry = self.take_any(id).await;
        let Some(entry) = entry else {
            return false;
        };
        let session = entry.session;
        let ok = reject_entry(entry);
        if ok {
            self.publish_rejected(session, id);
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

    async fn take_any(&self, id: &str) -> Option<PendingQuestion> {
        self.inner.lock().await.remove(id)
    }

    fn publish_replied(&self, session: Option<SessionId>, id: &str, answers: Vec<Vec<String>>) {
        let _published = self.events.send(json!({
            "id": format!("evt_hya_question_complete_{id}"),
            "type": "question.replied",
            "properties": {
                "sessionID": session.map(|session| session.to_string()).unwrap_or_default(),
                "requestID": id,
                "answers": answers,
            },
        }));
    }

    fn publish_rejected(&self, session: Option<SessionId>, id: &str) {
        let _published = self.events.send(json!({
            "id": format!("evt_hya_question_complete_{id}"),
            "type": "question.rejected",
            "properties": {
                "sessionID": session.map(|session| session.to_string()).unwrap_or_default(),
                "requestID": id,
            },
        }));
    }
}

impl Default for QuestionRequests {
    fn default() -> Self {
        Self::new()
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

fn question_asked_event(id: &str, entry: &PendingQuestion) -> Option<Value> {
    let properties = question_view(id, entry)?;
    Some(json!({
        "id": format!("evt_hya_question_{id}"),
        "type": "question.asked",
        "properties": properties,
    }))
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
