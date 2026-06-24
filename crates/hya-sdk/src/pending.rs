//! A late-bound [`Client`] used while the backend is still starting.
//!
//! The TUI is handed a `PendingClient` immediately so it can render and accept input before the
//! backend (and its possibly-slow MCP servers) finishes connecting. Once the real client is ready
//! it is installed into the shared [`PendingSlot`]; every call before that returns
//! [`SdkError::NotReady`] so callers can queue work instead of blocking startup.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use crate::client::Client;
use crate::error::{Result, SdkError};
use crate::store::StoredPart;
use crate::types::{Agent, Config, Message, Session};

/// Shared cell holding the real client once the backend connects.
#[derive(Default)]
pub struct PendingSlot {
    inner: RwLock<Option<Arc<dyn Client>>>,
}

impl PendingSlot {
    /// Install the real client. Idempotent; later calls replace the stored client.
    pub fn set(&self, client: Arc<dyn Client>) {
        if let Ok(mut guard) = self.inner.write() {
            *guard = Some(client);
        }
    }

    fn current(&self) -> Result<Arc<dyn Client>> {
        self.inner
            .read()
            .ok()
            .and_then(|guard| guard.clone())
            .ok_or(SdkError::NotReady)
    }
}

/// A [`Client`] that forwards to the real client once [`PendingSlot::set`] has been called.
pub struct PendingClient {
    slot: Arc<PendingSlot>,
    directory: String,
    base_url: String,
}

impl PendingClient {
    /// Build a not-yet-ready client plus the slot used to install the real client later.
    #[must_use]
    pub fn create(directory: impl Into<String>) -> (Arc<dyn Client>, Arc<PendingSlot>) {
        let slot = Arc::new(PendingSlot::default());
        let client: Arc<dyn Client> = Arc::new(Self {
            slot: Arc::clone(&slot),
            directory: directory.into(),
            base_url: "http://hya.pending".to_owned(),
        });
        (client, slot)
    }
}

#[async_trait]
impl Client for PendingClient {
    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn directory(&self) -> &str {
        &self.directory
    }

    async fn config_get(&self) -> Result<Config> {
        self.slot.current()?.config_get().await
    }

    async fn session_list(&self) -> Result<Vec<Session>> {
        self.slot.current()?.session_list().await
    }

    async fn session_get(&self, session_id: &str) -> Result<Session> {
        self.slot.current()?.session_get(session_id).await
    }

    async fn session_messages(&self, session_id: &str) -> Result<Vec<(Message, Vec<StoredPart>)>> {
        self.slot.current()?.session_messages(session_id).await
    }

    async fn session_todo(&self, session_id: &str) -> Result<Vec<serde_json::Value>> {
        self.slot.current()?.session_todo(session_id).await
    }

    async fn session_diff(&self, session_id: &str) -> Result<Vec<serde_json::Value>> {
        self.slot.current()?.session_diff(session_id).await
    }

    async fn agents(&self) -> Result<Vec<Agent>> {
        self.slot.current()?.agents().await
    }

    async fn find_files(&self, query: &str) -> Result<Vec<String>> {
        self.slot.current()?.find_files(query).await
    }

    async fn commands(&self) -> Result<Vec<String>> {
        self.slot.current()?.commands().await
    }

    async fn models(&self) -> Result<Vec<(String, String, String, i64, Vec<String>)>> {
        self.slot.current()?.models().await
    }

    async fn mcp_status(&self) -> Result<Vec<(String, String)>> {
        self.slot.current()?.mcp_status().await
    }

    async fn lsp_status(&self) -> Result<Vec<(String, String, String)>> {
        self.slot.current()?.lsp_status().await
    }

    async fn formatter_status(&self) -> Result<Vec<String>> {
        self.slot.current()?.formatter_status().await
    }

    async fn plugins(&self) -> Result<Vec<(String, Option<String>)>> {
        self.slot.current()?.plugins().await
    }

    async fn session_create(&self) -> Result<Session> {
        self.slot.current()?.session_create().await
    }

    async fn session_prompt(
        &self,
        session_id: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.slot.current()?.session_prompt(session_id, body).await
    }

    async fn session_shell(
        &self,
        session_id: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.slot.current()?.session_shell(session_id, body).await
    }

    async fn session_command(
        &self,
        session_id: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.slot.current()?.session_command(session_id, body).await
    }

    async fn permission_reply(
        &self,
        request_id: &str,
        reply: &str,
        message: Option<&str>,
    ) -> Result<()> {
        self.slot
            .current()?
            .permission_reply(request_id, reply, message)
            .await
    }

    async fn question_reply(
        &self,
        request_id: &str,
        answers: &[Vec<String>],
        directory: Option<&str>,
    ) -> Result<()> {
        self.slot
            .current()?
            .question_reply(request_id, answers, directory)
            .await
    }

    async fn question_reject(&self, request_id: &str, directory: Option<&str>) -> Result<()> {
        self.slot
            .current()?
            .question_reject(request_id, directory)
            .await
    }

    async fn session_rename(&self, session_id: &str, title: &str) -> Result<()> {
        self.slot.current()?.session_rename(session_id, title).await
    }

    async fn session_delete(&self, session_id: &str) -> Result<()> {
        self.slot.current()?.session_delete(session_id).await
    }

    async fn session_compact(
        &self,
        session_id: &str,
        provider_id: &str,
        model_id: &str,
    ) -> Result<()> {
        self.slot
            .current()?
            .session_compact(session_id, provider_id, model_id)
            .await
    }

    async fn session_revert(&self, session_id: &str, message_id: &str) -> Result<()> {
        self.slot
            .current()?
            .session_revert(session_id, message_id)
            .await
    }

    async fn session_unrevert(&self, session_id: &str) -> Result<()> {
        self.slot.current()?.session_unrevert(session_id).await
    }

    async fn session_abort(&self, session_id: &str) -> Result<()> {
        self.slot.current()?.session_abort(session_id).await
    }
}
