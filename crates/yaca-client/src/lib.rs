//! `yaca-client` — typed HTTP client for the yaca server (used by the TUI).

use yaca_proto::api::{CreateSessionRequest, CreateSessionResponse, PromptRequest, PromptResponse};
use yaca_proto::{Envelope, SessionId};

#[derive(thiserror::Error, Debug)]
pub enum ClientError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
}

pub struct Client {
    base: String,
    http: reqwest::Client,
}

impl Client {
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base: base_url.into(),
            http: reqwest::Client::new(),
        }
    }

    pub async fn create_session(
        &self,
        req: &CreateSessionRequest,
    ) -> Result<CreateSessionResponse, ClientError> {
        let resp = self
            .http
            .post(format!("{}/sessions", self.base))
            .json(req)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }

    pub async fn prompt(
        &self,
        session: SessionId,
        req: &PromptRequest,
    ) -> Result<PromptResponse, ClientError> {
        let url = format!("{}/sessions/{}/prompt", self.base, session.as_uuid());
        let resp = self
            .http
            .post(url)
            .json(req)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }

    pub async fn events(
        &self,
        session: SessionId,
        since_seq: Option<u64>,
    ) -> Result<Vec<Envelope>, ClientError> {
        let mut url = format!("{}/sessions/{}/events", self.base, session.as_uuid());
        if let Some(seq) = since_seq {
            url.push_str(&format!("?since_seq={seq}"));
        }
        let resp = self
            .http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }
}
