//! High-level E2E environment: FakeLlm + BackendProcess + hya-client.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use hya_client::Client;
use hya_proto::SessionId;
use hya_proto::api::{CreateSessionRequest, PromptRequest};

use crate::backend::{BackendProcess, BackendSpec, default_backend_bin};
use crate::error::E2eError;
use crate::fake_llm::{FakeLlm, ScriptStep};

/// Poll helper with deadline.
pub async fn wait_until<F, Fut>(
    label: &str,
    timeout: Duration,
    mut check: F,
) -> Result<(), E2eError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<bool, E2eError>>,
{
    let start = Instant::now();
    loop {
        if check().await? {
            return Ok(());
        }
        if start.elapsed() > timeout {
            return Err(E2eError::Timeout(label.to_string()));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Builder for an isolated E2E environment.
pub struct E2eEnvBuilder {
    scripts: Vec<ScriptStep>,
    yolo: bool,
    agent: String,
    binary: Option<PathBuf>,
}

impl Default for E2eEnvBuilder {
    fn default() -> Self {
        Self {
            scripts: Vec::new(),
            yolo: true,
            agent: "build".into(),
            binary: None,
        }
    }
}

impl E2eEnvBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn scripts(mut self, scripts: Vec<ScriptStep>) -> Self {
        self.scripts = scripts;
        self
    }

    #[must_use]
    pub fn yolo(mut self, yolo: bool) -> Self {
        self.yolo = yolo;
        self
    }

    #[must_use]
    pub fn agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = agent.into();
        self
    }

    #[must_use]
    pub fn binary(mut self, path: PathBuf) -> Self {
        self.binary = Some(path);
        self
    }

    pub async fn build(self) -> Result<E2eEnv, E2eError> {
        let fake = FakeLlm::start(self.scripts).await?;
        let mut spec = BackendSpec::new(
            self.binary.unwrap_or_else(default_backend_bin),
            fake.base_url(),
        );
        spec.yolo = self.yolo;
        // BackendProcess::start is blocking (stdio). Offload to spawn_blocking.
        let backend = tokio::task::spawn_blocking(move || BackendProcess::start(&spec))
            .await
            .map_err(|e| E2eError::Other(format!("join backend spawn: {e}")))??;
        let client = Client::new(backend.url.clone());
        Ok(E2eEnv {
            fake,
            backend,
            client,
            agent: self.agent,
            model: "fake/model".into(),
        })
    }
}

/// Ready environment for scenarios.
pub struct E2eEnv {
    pub fake: FakeLlm,
    pub backend: BackendProcess,
    pub client: Client,
    pub agent: String,
    pub model: String,
}

impl E2eEnv {
    pub async fn create_session(&self) -> Result<SessionId, E2eError> {
        let resp = self
            .client
            .create_session(&CreateSessionRequest {
                agent: self.agent.clone(),
                model: self.model.clone(),
                workdir: self.backend.workdir_str(),
                parent: None,
            })
            .await?;
        Ok(resp.session)
    }

    pub async fn prompt(
        &self,
        session: SessionId,
        text: impl Into<String>,
    ) -> Result<hya_proto::api::PromptResponse, E2eError> {
        Ok(self
            .client
            .prompt(session, &PromptRequest { text: text.into() })
            .await?)
    }

    pub async fn events(
        &self,
        session: SessionId,
        since_seq: Option<u64>,
    ) -> Result<Vec<hya_proto::Envelope>, E2eError> {
        Ok(self.client.events(session, since_seq).await?)
    }

    /// Read a file under the backend project workdir.
    pub fn read_project_file(&self, relative: &str) -> Result<String, E2eError> {
        let path = self.backend.project.join(relative);
        Ok(std::fs::read_to_string(path)?)
    }

    pub fn project_path(&self, relative: &str) -> PathBuf {
        self.backend.project.join(relative)
    }

    /// True when FakeLlm received at least one chat request.
    pub fn fake_saw_request(&self) -> Result<bool, E2eError> {
        Ok(!self.fake.requests()?.is_empty())
    }
}
