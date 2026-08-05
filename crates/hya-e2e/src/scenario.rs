//! High-level E2E environment: FakeLlm + BackendProcess + hya-client + HTTP helpers.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use hya_client::Client;
use hya_proto::SessionId;
use hya_proto::api::{CreateSessionRequest, PromptRequest};
use serde_json::Value;

use crate::backend::{
    BackendProcess, BackendSpec, McpFixture, MCP_ECHO_SCRIPT_REL, default_backend_bin,
    mcp_echo_command, mcp_echo_script,
};
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
    permission_model: String,
    mcp: Vec<McpFixture>,
    skill_files: Vec<(String, String)>,
    project_files: Vec<(String, Vec<u8>)>,
    preinstall_bundles: Vec<PathBuf>,
}

impl Default for E2eEnvBuilder {
    fn default() -> Self {
        Self {
            scripts: Vec::new(),
            yolo: true,
            agent: "build".into(),
            binary: None,
            permission_model: "allow".into(),
            mcp: Vec::new(),
            skill_files: Vec::new(),
            project_files: Vec::new(),
            preinstall_bundles: Vec::new(),
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
    pub fn permission_model(mut self, model: impl Into<String>) -> Self {
        self.permission_model = model.into();
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

    #[must_use]
    pub fn with_mcp_echo(mut self) -> Self {
        self.project_files.push((
            MCP_ECHO_SCRIPT_REL.into(),
            mcp_echo_script().as_bytes().to_vec(),
        ));
        self.mcp.push(McpFixture {
            name: "echo".into(),
            command: mcp_echo_command(),
        });
        self
    }

    #[must_use]
    pub fn skill_file(mut self, relative: impl Into<String>, body: impl Into<String>) -> Self {
        self.skill_files.push((relative.into(), body.into()));
        self
    }

    #[must_use]
    pub fn project_file(mut self, relative: impl Into<String>, body: impl Into<Vec<u8>>) -> Self {
        self.project_files.push((relative.into(), body.into()));
        self
    }

    #[must_use]
    pub fn preinstall_bundle(mut self, package: PathBuf) -> Self {
        self.preinstall_bundles.push(package);
        self
    }

    pub async fn build(self) -> Result<E2eEnv, E2eError> {
        let fake = FakeLlm::start(self.scripts).await?;
        let mut spec = BackendSpec::new(
            self.binary.unwrap_or_else(default_backend_bin),
            fake.base_url(),
        );
        spec.yolo = self.yolo;
        spec.permission_model = self.permission_model;
        spec.mcp = self.mcp;
        spec.skill_files = self.skill_files;
        spec.project_files = self.project_files;
        spec.preinstall_bundles = self.preinstall_bundles;
        let backend = tokio::task::spawn_blocking(move || BackendProcess::start(&spec))
            .await
            .map_err(|e| E2eError::Other(format!("join backend spawn: {e}")))??;
        let client = Client::new(backend.url.clone());
        let http = reqwest::Client::new();
        Ok(E2eEnv {
            fake,
            backend,
            client,
            http,
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
    pub http: reqwest::Client,
    pub agent: String,
    pub model: String,
}

impl E2eEnv {
    pub async fn create_session(&self) -> Result<SessionId, E2eError> {
        self.create_session_with_agent(&self.agent).await
    }

    pub async fn create_session_with_agent(&self, agent: &str) -> Result<SessionId, E2eError> {
        let resp = self
            .client
            .create_session(&CreateSessionRequest {
                agent: agent.to_string(),
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

    pub fn read_project_file(&self, relative: &str) -> Result<String, E2eError> {
        let path = self.backend.project.join(relative);
        Ok(std::fs::read_to_string(path)?)
    }

    pub fn project_path(&self, relative: &str) -> PathBuf {
        self.backend.project.join(relative)
    }

    pub fn fake_saw_request(&self) -> Result<bool, E2eError> {
        Ok(!self.fake.requests()?.is_empty())
    }

    /// GET JSON from the backend (Compat/native paths).
    pub async fn get_json(&self, path: &str) -> Result<Value, E2eError> {
        let url = format!("{}{path}", self.backend.url);
        let resp = self.http.get(url).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(E2eError::Http(format!("GET {path} -> {status}: {text}")));
        }
        Ok(serde_json::from_str(&text)?)
    }

    pub async fn post_json(&self, path: &str, body: &Value) -> Result<Value, E2eError> {
        let url = format!("{}{path}", self.backend.url);
        let resp = self.http.post(url).json(body).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(E2eError::Http(format!("POST {path} -> {status}: {text}")));
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(&text)?)
    }

    /// List pending permission requests (Compat).
    pub async fn list_permissions(&self) -> Result<Value, E2eError> {
        self.get_json("/permission").await
    }

    pub async fn reply_permission(&self, request_id: &str, reply: &str) -> Result<(), E2eError> {
        let body = serde_json::json!({ "reply": reply });
        let _ = self
            .post_json(&format!("/permission/{request_id}/reply"), &body)
            .await?;
        Ok(())
    }

    pub async fn list_questions(&self) -> Result<Value, E2eError> {
        self.get_json("/question").await
    }

    pub async fn reply_question(&self, request_id: &str, answers: Value) -> Result<(), E2eError> {
        let body = serde_json::json!({ "answers": answers });
        let _ = self
            .post_json(&format!("/question/{request_id}/reply"), &body)
            .await?;
        Ok(())
    }

    pub async fn list_sessions_compat(&self) -> Result<Value, E2eError> {
        self.get_json("/session").await
    }

    pub async fn list_agents(&self) -> Result<Value, E2eError> {
        self.get_json("/api/agent").await
    }

    pub async fn session_tree(&self, session: &SessionId) -> Result<Value, E2eError> {
        self.get_json(&format!("/session/{session}/tree")).await
    }

    /// Compat v2 session context (projected messages for the session).
    pub async fn session_context(&self, session: &SessionId) -> Result<Value, E2eError> {
        self.get_json(&format!("/api/session/{session}/context"))
            .await
    }

    /// Compat session todo list.
    pub async fn session_todos(&self, session: &SessionId) -> Result<Value, E2eError> {
        self.get_json(&format!("/session/{session}/todo")).await
    }

    /// Compat run statuses map (`session_id` → `{type: "busy"}` while running).
    pub async fn session_statuses(&self) -> Result<Value, E2eError> {
        self.get_json("/session/status").await
    }

    /// Wait until the session is not listed as busy under `/session/status`.
    pub async fn wait_session_idle(
        &self,
        session: &SessionId,
        timeout: Duration,
    ) -> Result<(), E2eError> {
        let key = session.to_string();
        wait_until("session idle", timeout, || async {
            let statuses = self.session_statuses().await?;
            let busy = statuses
                .get(&key)
                .and_then(|s| s.get("type"))
                .and_then(|t| t.as_str())
                == Some("busy");
            Ok(!busy)
        })
        .await
    }

    /// Create a session via Compat v2 (`/api/session`) so workdir/location is explicit.
    pub async fn compat_create_session(&self) -> Result<SessionId, E2eError> {
        let body = serde_json::json!({
            "agent": self.agent,
            "location": { "directory": self.backend.workdir_str() },
            "model": {
                "providerID": "fake",
                "id": "model"
            }
        });
        let created = self.post_json("/api/session", &body).await?;
        let id = created
            .pointer("/data/id")
            .and_then(|v| v.as_str())
            .or_else(|| created.get("id").and_then(|v| v.as_str()))
            .ok_or_else(|| E2eError::Other(format!("compat create missing id: {created}")))?;
        id.parse()
            .map_err(|e| E2eError::Other(format!("parse session id {id}: {e}")))
    }

    /// Async Compat v2 prompt (spawns turn with AGENTS/reference guidance), then wait idle.
    pub async fn compat_prompt_and_wait(
        &self,
        session: SessionId,
        text: impl Into<String>,
        timeout: Duration,
    ) -> Result<Value, E2eError> {
        let before = self.fake.requests().map(|r| r.len()).unwrap_or(0);
        let body = serde_json::json!({
            "prompt": { "text": text.into() },
            "resume": true
        });
        let admitted = self
            .post_json(&format!("/api/session/{session}/prompt"), &body)
            .await?;
        // Wait for at least one new FakeLlm hit and the run registry to clear.
        wait_until("compat turn FakeLlm", timeout, || async {
            let n = self.fake.requests().map(|r| r.len()).unwrap_or(0);
            Ok(n > before)
        })
        .await?;
        self.wait_session_idle(&session, timeout).await?;
        Ok(admitted)
    }

    /// POST `/api/session/{id}/compact` (sync summarize + inject system message).
    pub async fn compact_session(&self, session: &SessionId) -> Result<(), E2eError> {
        let url = format!("{}/api/session/{session}/compact", self.backend.url);
        let resp = self.http.post(url).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !(status.is_success() || status.as_u16() == 204) {
            return Err(E2eError::Http(format!(
                "POST compact -> {status}: {text}"
            )));
        }
        Ok(())
    }

    /// POST legacy `/session/{id}/summarize` with provider/model metadata.
    pub async fn summarize_session_legacy(&self, session: &SessionId) -> Result<Value, E2eError> {
        let body = serde_json::json!({
            "providerID": "fake",
            "modelID": "model",
            "auto": false
        });
        self.post_json(&format!("/session/{session}/summarize"), &body)
            .await
    }

    /// Wait until permission list is non-empty; return first request id.
    pub async fn wait_permission_id(&self, timeout: Duration) -> Result<String, E2eError> {
        wait_until("permission request", timeout, || async {
            let body = self.list_permissions().await?;
            Ok(extract_request_id(&body).is_some())
        })
        .await?;
        let body = self.list_permissions().await?;
        extract_request_id(&body)
            .ok_or_else(|| E2eError::Other(format!("permission id missing in {body}")))
    }

    pub async fn wait_question_id(&self, timeout: Duration) -> Result<String, E2eError> {
        wait_until("question request", timeout, || async {
            let body = self.list_questions().await?;
            Ok(extract_request_id(&body).is_some())
        })
        .await?;
        let body = self.list_questions().await?;
        extract_request_id(&body)
            .ok_or_else(|| E2eError::Other(format!("question id missing in {body}")))
    }

    /// Run `prompt` while auto-replying the first pending permission request.
    pub async fn prompt_with_permission_reply(
        &self,
        session: SessionId,
        text: impl Into<String>,
        reply: &str,
        timeout: Duration,
    ) -> Result<hya_proto::api::PromptResponse, E2eError> {
        let http = self.http.clone();
        let base = self.backend.url.clone();
        let reply = reply.to_string();
        let replier = tokio::spawn(async move {
            auto_reply_permission(http, base, &reply, timeout).await
        });
        let prompt_result = self.prompt(session, text).await;
        match replier.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                // Permission may never fire if the tool failed earlier; surface only when prompt failed.
                if prompt_result.is_err() {
                    return Err(e);
                }
            }
            Err(e) => {
                return Err(E2eError::Other(format!("permission replier join: {e}")));
            }
        }
        prompt_result
    }

    /// Run `prompt` while auto-replying the first pending question.
    pub async fn prompt_with_question_reply(
        &self,
        session: SessionId,
        text: impl Into<String>,
        answers: Value,
        timeout: Duration,
    ) -> Result<hya_proto::api::PromptResponse, E2eError> {
        let http = self.http.clone();
        let base = self.backend.url.clone();
        let replier = tokio::spawn(async move {
            auto_reply_question(http, base, answers, timeout).await
        });
        let prompt_result = self.prompt(session, text).await;
        match replier.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                if prompt_result.is_err() {
                    return Err(e);
                }
            }
            Err(e) => {
                return Err(E2eError::Other(format!("question replier join: {e}")));
            }
        }
        prompt_result
    }

    /// Compact diagnostic dump for failed assertions.
    pub fn diagnostics(&self) -> String {
        let fake_n = self.fake.requests().map(|r| r.len()).unwrap_or(0);
        let remaining = self.fake.remaining_scripts().unwrap_or(0);
        format!(
            "url={} project={} fake_requests={fake_n} remaining_scripts={remaining}",
            self.backend.url,
            self.backend.project.display()
        )
    }

    /// Wait until `/mcp` reports `name` with status `connected`.
    pub async fn wait_mcp_connected(
        &self,
        name: &str,
        timeout: Duration,
    ) -> Result<Value, E2eError> {
        wait_until(&format!("mcp {name} connected"), timeout, || async {
            let status = self.get_json("/mcp").await.unwrap_or(Value::Null);
            Ok(status
                .get(name)
                .and_then(|s| s.get("status"))
                .and_then(|s| s.as_str())
                == Some("connected"))
        })
        .await?;
        self.get_json("/mcp").await
    }
}

async fn auto_reply_permission(
    http: reqwest::Client,
    base: String,
    reply: &str,
    timeout: Duration,
) -> Result<(), E2eError> {
    let start = Instant::now();
    loop {
        if start.elapsed() > timeout {
            return Err(E2eError::Timeout("permission auto-reply".into()));
        }
        let resp = http
            .get(format!("{base}/permission"))
            .send()
            .await
            .map_err(|e| E2eError::Http(e.to_string()))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| E2eError::Http(e.to_string()))?;
        if status.is_success()
            && let Ok(body) = serde_json::from_str::<Value>(&text)
            && let Some(id) = extract_request_id(&body)
        {
            let reply_resp = http
                .post(format!("{base}/permission/{id}/reply"))
                .json(&serde_json::json!({ "reply": reply }))
                .send()
                .await
                .map_err(|e| E2eError::Http(e.to_string()))?;
            if reply_resp.status().is_success() || reply_resp.status().as_u16() == 204 {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn auto_reply_question(
    http: reqwest::Client,
    base: String,
    answers: Value,
    timeout: Duration,
) -> Result<(), E2eError> {
    let start = Instant::now();
    loop {
        if start.elapsed() > timeout {
            return Err(E2eError::Timeout("question auto-reply".into()));
        }
        let resp = http
            .get(format!("{base}/question"))
            .send()
            .await
            .map_err(|e| E2eError::Http(e.to_string()))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| E2eError::Http(e.to_string()))?;
        if status.is_success()
            && let Ok(body) = serde_json::from_str::<Value>(&text)
            && let Some(id) = extract_request_id(&body)
        {
            let reply_resp = http
                .post(format!("{base}/question/{id}/reply"))
                .json(&serde_json::json!({ "answers": answers }))
                .send()
                .await
                .map_err(|e| E2eError::Http(e.to_string()))?;
            if reply_resp.status().is_success() || reply_resp.status().as_u16() == 204 {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn extract_request_id(body: &Value) -> Option<String> {
    // Compat shapes vary: array, {data:[...]}, single object
    if let Some(arr) = body.as_array() {
        return arr.iter().find_map(|item| {
            item.get("id")
                .and_then(|id| id.as_str())
                .map(str::to_string)
        });
    }
    if let Some(arr) = body.get("data").and_then(|d| d.as_array()) {
        return arr.iter().find_map(|item| {
            item.get("id")
                .and_then(|id| id.as_str())
                .map(str::to_string)
        });
    }
    body.get("id")
        .and_then(|id| id.as_str())
        .map(str::to_string)
}

/// Immediate children of a run-tree node (`/session/{id}/tree`).
pub fn tree_children(tree: &Value) -> &[Value] {
    tree.get("children")
        .and_then(|c| c.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// Max edge depth from `tree` (0 = leaf with no children).
pub fn tree_max_depth(tree: &Value) -> usize {
    let kids = tree_children(tree);
    if kids.is_empty() {
        return 0;
    }
    1 + kids.iter().map(tree_max_depth).max().unwrap_or(0)
}

/// Collect every session id string present anywhere in the tree (root included).
pub fn tree_session_ids(tree: &Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_session_ids(tree, &mut out);
    out
}

fn collect_session_ids(node: &Value, out: &mut Vec<String>) {
    if let Some(id) = node.get("session").and_then(|s| s.as_str()) {
        out.push(id.to_string());
    }
    for child in tree_children(node) {
        collect_session_ids(child, out);
    }
}

/// Collect `member.subagent_type` values from non-root nodes.
pub fn tree_subagent_types(tree: &Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_subagent_types(tree, &mut out, true);
    out
}

fn collect_subagent_types(node: &Value, out: &mut Vec<String>, is_root: bool) {
    if !is_root
        && let Some(kind) = node
            .get("member")
            .and_then(|m| m.get("subagent_type"))
            .and_then(|s| s.as_str())
    {
        out.push(kind.to_string());
    }
    // Also accept agent field on child nodes when member is sparse.
    if !is_root
        && let Some(kind) = node.get("agent").and_then(|a| a.as_str())
        && !out.iter().any(|existing| existing == kind)
    {
        out.push(kind.to_string());
    }
    for child in tree_children(node) {
        collect_subagent_types(child, out, false);
    }
}

/// Dump a later FakeLlm request body (index `n` and beyond) as one string.
/// Use this to assert tool *results* reached the model, not the tool-call args
/// from the same turn that requested the tool.
pub fn fake_requests_from(requests: &[Value], from_index: usize) -> String {
    requests
        .iter()
        .skip(from_index)
        .map(|r| r.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}
