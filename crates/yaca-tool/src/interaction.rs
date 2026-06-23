use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use yaca_proto::{QuestionRequestId, SessionId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionInfo {
    pub question: String,
    pub header: String,
    pub options: Vec<QuestionOption>,
    pub multiple: bool,
    pub custom: Option<bool>,
}

impl QuestionInfo {
    #[must_use]
    pub fn from_prompt_kind(prompt: &str, kind: &QuestionKind) -> Self {
        let trimmed = prompt.trim();
        let header = if trimmed.is_empty() {
            "Question".to_string()
        } else {
            trimmed.chars().take(30).collect()
        };
        let options = match kind {
            QuestionKind::FreeText { .. } => Vec::new(),
            QuestionKind::Select { options, .. } => options
                .iter()
                .map(|label| QuestionOption {
                    label: label.clone(),
                    description: String::new(),
                })
                .collect(),
        };
        let custom = match kind {
            QuestionKind::FreeText { .. } => Some(true),
            QuestionKind::Select { allow_custom, .. } => (*allow_custom).then_some(true),
        };
        Self {
            question: prompt.to_string(),
            header,
            options,
            multiple: false,
            custom,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuestionKind {
    FreeText {
        default: Option<String>,
    },
    Select {
        options: Vec<String>,
        allow_custom: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuestionAnswer {
    Selected(usize),
    SelectedMany(Vec<usize>),
    FreeText(String),
    Cancelled,
}

pub struct QuestionRequest {
    pub id: QuestionRequestId,
    pub session: Option<SessionId>,
    pub prompt: String,
    pub info: QuestionInfo,
    pub kind: QuestionKind,
    pub reply: oneshot::Sender<QuestionAnswer>,
}

#[derive(Error, Debug)]
pub enum InteractionError {
    #[error("interaction channel unavailable")]
    Unavailable,
}

#[derive(Clone)]
pub struct InteractionPlane {
    asks: mpsc::UnboundedSender<QuestionRequest>,
    session: Option<SessionId>,
}

impl InteractionPlane {
    #[must_use]
    pub fn new() -> (Self, mpsc::UnboundedReceiver<QuestionRequest>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                asks: tx,
                session: None,
            },
            rx,
        )
    }

    #[must_use]
    pub fn for_session(&self, session: SessionId) -> Self {
        let mut plane = self.clone();
        plane.session = Some(session);
        plane
    }

    pub async fn ask(
        &self,
        prompt: String,
        kind: QuestionKind,
    ) -> Result<QuestionAnswer, InteractionError> {
        let info = QuestionInfo::from_prompt_kind(&prompt, &kind);
        self.ask_with_info(info, kind).await
    }

    pub async fn ask_with_info(
        &self,
        info: QuestionInfo,
        kind: QuestionKind,
    ) -> Result<QuestionAnswer, InteractionError> {
        let (tx, rx) = oneshot::channel();
        let prompt = info.question.clone();
        let req = QuestionRequest {
            id: QuestionRequestId::new(),
            session: self.session,
            prompt,
            info,
            kind,
            reply: tx,
        };
        self.asks
            .send(req)
            .map_err(|_| InteractionError::Unavailable)?;
        rx.await.map_err(|_| InteractionError::Unavailable)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ask_round_trips_free_text() {
        let (plane, mut rx) = InteractionPlane::new();
        let task = tokio::spawn(async move {
            plane
                .ask(
                    "name?".to_string(),
                    QuestionKind::FreeText { default: None },
                )
                .await
        });
        let req = rx.recv().await.expect("request");
        assert_eq!(req.prompt, "name?");
        req.reply
            .send(QuestionAnswer::FreeText("yaca".to_string()))
            .expect("reply");
        assert_eq!(
            task.await.expect("join").expect("answer"),
            QuestionAnswer::FreeText("yaca".to_string())
        );
    }

    #[tokio::test]
    async fn dropped_reply_is_unavailable() {
        let (plane, mut rx) = InteractionPlane::new();
        let task = tokio::spawn(async move {
            plane
                .ask("x?".to_string(), QuestionKind::FreeText { default: None })
                .await
        });
        let req = rx.recv().await.expect("request");
        drop(req.reply);
        assert!(matches!(
            task.await.expect("join"),
            Err(InteractionError::Unavailable)
        ));
    }
}
