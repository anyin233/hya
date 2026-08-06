//! Human interaction plane for structured and free-text questions.

use hya_proto::{QuestionRequestId, SessionId};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

/// One selectable option shown to the operator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionOption {
    /// Short label returned when selected.
    pub label: String,
    /// Longer help text for the option.
    pub description: String,
}

/// Display metadata for a single question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionInfo {
    /// Full question text.
    pub question: String,
    /// Short header for the UI chrome.
    pub header: String,
    /// Structured options (may be empty for free-text).
    pub options: Vec<QuestionOption>,
    /// When true, the UI may accept multiple selections.
    pub multiple: bool,
    /// When `Some(true)`, free-text outside the option list is allowed.
    pub custom: Option<bool>,
}

impl QuestionInfo {
    /// Build UI metadata from a simple prompt string and kind.
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

/// Shape of input the operator is asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuestionKind {
    /// Free-form text with optional default.
    FreeText {
        /// Prefill / default value when the UI supports it.
        default: Option<String>,
    },
    /// Choose among labeled options.
    Select {
        /// Option labels in display order.
        options: Vec<String>,
        /// Whether answers outside the list are accepted.
        allow_custom: bool,
    },
}

/// Operator response for one question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuestionAnswer {
    /// Single index into the select options.
    Selected(usize),
    /// Multiple indices for multi-select.
    SelectedMany(Vec<usize>),
    /// Free-text answer.
    FreeText(String),
    /// User cancelled or the channel closed.
    Cancelled,
}

/// One question in a batch (`info` + interaction kind).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionPrompt {
    /// Display metadata.
    pub info: QuestionInfo,
    /// Free-text vs select semantics.
    pub kind: QuestionKind,
}

impl QuestionPrompt {
    /// Pair metadata with a kind.
    #[must_use]
    pub fn new(info: QuestionInfo, kind: QuestionKind) -> Self {
        Self { info, kind }
    }
}

/// Oneshot reply channel for a single answer or a batch.
pub enum QuestionReply {
    /// One answer expected.
    Single(oneshot::Sender<QuestionAnswer>),
    /// One answer per question in a batch.
    Many(oneshot::Sender<Vec<QuestionAnswer>>),
}

impl QuestionReply {
    /// Deliver a single answer (wraps into a one-element batch for [`Self::Many`]).
    ///
    /// # Errors
    /// Returns the answer if the receiver was dropped.
    pub fn send(self, answer: QuestionAnswer) -> Result<(), QuestionAnswer> {
        match self {
            Self::Single(tx) => tx.send(answer),
            Self::Many(tx) => tx
                .send(vec![answer])
                .map_err(|mut answers| answers.pop().unwrap_or(QuestionAnswer::Cancelled)),
        }
    }

    /// Deliver a full batch of answers.
    ///
    /// # Errors
    /// Returns the answers if the receiver was dropped.
    pub fn send_many(self, answers: Vec<QuestionAnswer>) -> Result<(), Vec<QuestionAnswer>> {
        match self {
            Self::Single(tx) => {
                let answer = answers
                    .into_iter()
                    .next()
                    .unwrap_or(QuestionAnswer::Cancelled);
                tx.send(answer).map_err(|answer| vec![answer])
            }
            Self::Many(tx) => tx.send(answers),
        }
    }
}

/// Ask delivered to the host for the operator to answer.
pub struct QuestionRequest {
    /// Correlation id for this interaction.
    pub id: QuestionRequestId,
    /// Session that initiated the ask.
    pub session: Option<SessionId>,
    /// First question text (legacy single-prompt field).
    pub prompt: String,
    /// First question display metadata.
    pub info: QuestionInfo,
    /// First question kind.
    pub kind: QuestionKind,
    /// Full batch of questions (length ≥ 1 for multi-ask).
    pub questions: Vec<QuestionPrompt>,
    /// Reply channel for the host.
    pub reply: QuestionReply,
}

/// Failure when the interaction channel is disconnected.
#[derive(Error, Debug)]
pub enum InteractionError {
    /// No receiver is listening for questions.
    #[error("interaction channel unavailable")]
    Unavailable,
}

/// Session-scoped channel that tools use to ask the human operator.
#[derive(Clone)]
pub struct InteractionPlane {
    asks: mpsc::UnboundedSender<QuestionRequest>,
    session: Option<SessionId>,
}

impl InteractionPlane {
    /// Create a plane and the host-side receiver for [`QuestionRequest`]s.
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

    /// Scope subsequent asks to a session id.
    #[must_use]
    pub fn for_session(&self, session: SessionId) -> Self {
        let mut plane = self.clone();
        plane.session = Some(session);
        plane
    }

    /// Ask one question derived from `prompt` and `kind`.
    ///
    /// # Errors
    /// Returns [`InteractionError::Unavailable`] when the host is gone.
    pub async fn ask(
        &self,
        prompt: String,
        kind: QuestionKind,
    ) -> Result<QuestionAnswer, InteractionError> {
        let info = QuestionInfo::from_prompt_kind(&prompt, &kind);
        self.ask_with_info(info, kind).await
    }

    /// Ask one question with explicit display metadata.
    ///
    /// # Errors
    /// Returns [`InteractionError::Unavailable`] when the host is gone.
    pub async fn ask_with_info(
        &self,
        info: QuestionInfo,
        kind: QuestionKind,
    ) -> Result<QuestionAnswer, InteractionError> {
        let mut answers = self.ask_many(vec![QuestionPrompt::new(info, kind)]).await?;
        Ok(answers.pop().unwrap_or(QuestionAnswer::Cancelled))
    }

    /// Ask a batch of questions in one host round-trip.
    ///
    /// # Errors
    /// Returns [`InteractionError::Unavailable`] when the host is gone.
    pub async fn ask_many(
        &self,
        questions: Vec<QuestionPrompt>,
    ) -> Result<Vec<QuestionAnswer>, InteractionError> {
        let Some(first) = questions.first().cloned() else {
            return Ok(Vec::new());
        };
        let (tx, rx) = oneshot::channel();
        let req = QuestionRequest {
            id: QuestionRequestId::new(),
            session: self.session,
            prompt: first.info.question.clone(),
            info: first.info,
            kind: first.kind,
            questions,
            reply: QuestionReply::Many(tx),
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
            .send(QuestionAnswer::FreeText("hya".to_string()))
            .expect("reply");
        assert_eq!(
            task.await.expect("join").expect("answer"),
            QuestionAnswer::FreeText("hya".to_string())
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
