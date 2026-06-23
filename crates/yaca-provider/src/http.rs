//! `HttpProvider` — drives a `Protocol` over reqwest + SSE into the canonical
//! `Event` stream. One provider per upstream route (OpenAI-compatible or
//! Anthropic), selected by the model id it serves.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use secrecy::{ExposeSecret as _, SecretString};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use yaca_proto::{Event, MessageId, ModelRef, SessionId};

mod stream;

use crate::anthropic::AnthropicMessagesProtocol;
use crate::google::GoogleProtocol;
use crate::openai::OpenAiChatProtocol;
use crate::{
    Capabilities, CompletionRequest, EventStream, Protocol, Provider, ProviderError, ProviderModel,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    OpenAiCompatible,
    Anthropic,
    Google,
}

enum AuthStyle {
    Bearer(SecretString),
    Anthropic { key: SecretString, version: String },
    Google(SecretString),
}

pub struct HttpProvider {
    id: String,
    protocol: Box<dyn Protocol>,
    client: reqwest::Client,
    endpoint: String,
    google_base: Option<String>,
    auth: AuthStyle,
    models: HashSet<String>,
    caps: Capabilities,
}

fn sensitive(value: &str) -> Result<HeaderValue, ProviderError> {
    let mut header = HeaderValue::from_str(value)
        .map_err(|_| ProviderError::Http("invalid auth header value".to_string()))?;
    header.set_sensitive(true);
    Ok(header)
}

fn request_header_value(value: &str) -> Result<HeaderValue, ProviderError> {
    let mut header = HeaderValue::from_str(value)
        .map_err(|_| ProviderError::Http("invalid request header value".to_string()))?;
    header.set_sensitive(true);
    Ok(header)
}

impl HttpProvider {
    pub fn new(
        id: impl Into<String>,
        kind: ProviderKind,
        base_url: &str,
        api_key: String,
        models: impl IntoIterator<Item = String>,
    ) -> Result<Self, ProviderError> {
        // Security: never follow redirects (reqwest keeps `x-api-key` across a
        // cross-origin 3xx). Connect-timeout only — a read timeout would abort
        // long streaming completions.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| ProviderError::Http(e.to_string()))?;
        let base = base_url.trim_end_matches('/');
        let key = SecretString::new(api_key);
        let (protocol, endpoint, auth): (Box<dyn Protocol>, String, AuthStyle) = match kind {
            ProviderKind::OpenAiCompatible => (
                Box::new(OpenAiChatProtocol),
                format!("{base}/chat/completions"),
                AuthStyle::Bearer(key),
            ),
            ProviderKind::Anthropic => (
                Box::new(AnthropicMessagesProtocol),
                format!("{base}/messages"),
                AuthStyle::Anthropic {
                    key,
                    version: "2023-06-01".to_string(),
                },
            ),
            ProviderKind::Google => (
                Box::new(GoogleProtocol),
                String::new(),
                AuthStyle::Google(key),
            ),
        };
        let google_base = if kind == ProviderKind::Google {
            Some(base.to_string())
        } else {
            None
        };
        Ok(Self {
            id: id.into(),
            protocol,
            client,
            endpoint,
            google_base,
            auth,
            models: models.into_iter().collect(),
            caps: Capabilities {
                streaming_tool_calls: true,
                parallel_tool_calls: true,
                usage_reporting: false,
                reasoning_request: true,
                max_context: 200_000,
                ..Capabilities::default()
            },
        })
    }

    fn auth_headers(&self) -> Result<HeaderMap, ProviderError> {
        let mut headers = HeaderMap::new();
        match &self.auth {
            AuthStyle::Bearer(key) => {
                headers.insert(
                    AUTHORIZATION,
                    sensitive(&format!("Bearer {}", key.expose_secret()))?,
                );
            }
            AuthStyle::Anthropic { key, version } => {
                headers.insert(
                    HeaderName::from_static("x-api-key"),
                    sensitive(key.expose_secret())?,
                );
                headers.insert(
                    HeaderName::from_static("anthropic-version"),
                    HeaderValue::from_str(version)
                        .map_err(|_| ProviderError::Http("invalid version header".to_string()))?,
                );
            }
            AuthStyle::Google(key) => {
                headers.insert(
                    HeaderName::from_static("x-goog-api-key"),
                    sensitive(key.expose_secret())?,
                );
            }
        }
        Ok(headers)
    }

    fn request_headers(
        &self,
        extra: &BTreeMap<String, String>,
    ) -> Result<HeaderMap, ProviderError> {
        let mut headers = self.auth_headers()?;
        for (name, value) in extra {
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| ProviderError::Http("invalid request header name".to_string()))?;
            headers.insert(header_name, request_header_value(value)?);
        }
        Ok(headers)
    }
}

#[async_trait]
impl Provider for HttpProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        self.models
            .contains(model.as_str())
            .then(|| self.caps.clone())
    }

    fn catalog(&self) -> Vec<ProviderModel> {
        self.models
            .iter()
            .map(|model| ProviderModel {
                provider_id: self.id.clone(),
                model_id: model.clone(),
                capabilities: self.caps.clone(),
            })
            .collect()
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let body = self.protocol.encode(&req)?;
        let decoder = self.protocol.decoder(session, message);
        let url = match &self.google_base {
            Some(base) => format!(
                "{base}/v1beta/models/{}:streamGenerateContent?alt=sse",
                req.model.as_str()
            ),
            None => self.endpoint.clone(),
        };
        let resp = self
            .client
            .post(&url)
            .headers(self.request_headers(&req.headers)?)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            let snippet = text.get(..500).unwrap_or(text.as_str());
            return Err(ProviderError::Http(format!("{status}: {snippet}")));
        }
        let (tx, rx) = mpsc::channel::<Result<Event, ProviderError>>(64);
        tokio::spawn(stream::pump(resp, decoder, tx));
        Ok(Box::pin(ReceiverStream::new(rx)))
    }
}
