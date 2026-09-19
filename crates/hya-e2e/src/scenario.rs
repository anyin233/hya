//! High-level E2E environment: FakeLlm + BackendProcess + hya-client + HTTP helpers.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use hya_api::v1 as pb;
use hya_client::Client;
use hya_proto::SessionId;
use serde_json::Value;

use crate::backend::{
    BackendProcess, BackendSpec, MCP_ECHO_SCRIPT_REL, McpFixture, default_backend_bin,
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
    routes: Vec<(String, Vec<ScriptStep>)>,
    yolo: bool,
    agent: String,
    binary: Option<PathBuf>,
    permission_model: String,
    mcp: Vec<McpFixture>,
    skill_files: Vec<(String, String)>,
    project_files: Vec<(String, Vec<u8>)>,
    preinstall_bundles: Vec<PathBuf>,
    additional_models: Vec<String>,
}

impl Default for E2eEnvBuilder {
    fn default() -> Self {
        Self {
            scripts: Vec::new(),
            routes: Vec::new(),
            yolo: true,
            agent: "build".into(),
            binary: None,
            permission_model: "allow".into(),
            mcp: Vec::new(),
            skill_files: Vec::new(),
            project_files: Vec::new(),
            preinstall_bundles: Vec::new(),
            additional_models: Vec::new(),
        }
    }
}

impl E2eEnvBuilder {
    /// Empty builder with YOLO on, `allow` permissions, and agent `build`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Shared (unrouted) FakeLlm script queue consumed in order by any agent.
    #[must_use]
    pub fn scripts(mut self, scripts: Vec<ScriptStep>) -> Self {
        self.scripts = scripts;
        self
    }

    /// Pin `steps` to the teammate whose system prompt contains `marker`.
    ///
    /// Multi-agent scenarios must route: residents and the main agent issue
    /// interleaved completion requests, and the shared queue has no way to tell
    /// them apart. See [`FakeLlm::route`].
    #[must_use]
    pub fn route(mut self, marker: impl Into<String>, steps: Vec<ScriptStep>) -> Self {
        self.routes.push((marker.into(), steps));
        self
    }

    /// Pass `--yolo` to the backend so tools auto-approve without a permission UI.
    #[must_use]
    pub fn yolo(mut self, yolo: bool) -> Self {
        self.yolo = yolo;
        self
    }

    /// Set `permission.model` in the temp config (`allow` | `default` | `strict`).
    #[must_use]
    pub fn permission_model(mut self, model: impl Into<String>) -> Self {
        self.permission_model = model.into();
        self
    }

    /// Default agent name used when creating sessions through this env.
    #[must_use]
    pub fn agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = agent.into();
        self
    }

    /// Override path to the `hya-backend` binary (default: workspace/target lookup).
    #[must_use]
    pub fn binary(mut self, path: PathBuf) -> Self {
        self.binary = Some(path);
        self
    }

    /// Install the fixture echo MCP server into project + config before boot.
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

    /// Write a skill file under the temp project before the backend starts.
    #[must_use]
    pub fn skill_file(mut self, relative: impl Into<String>, body: impl Into<String>) -> Self {
        self.skill_files.push((relative.into(), body.into()));
        self
    }

    /// Write an arbitrary project file (relative path + bytes) before boot.
    #[must_use]
    pub fn project_file(mut self, relative: impl Into<String>, body: impl Into<Vec<u8>>) -> Self {
        self.project_files.push((relative.into(), body.into()));
        self
    }

    /// Advertise more model ids on the same local FakeLlm provider.
    #[must_use]
    pub fn additional_models<I, S>(mut self, models: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.additional_models
            .extend(models.into_iter().map(Into::into));
        self
    }

    /// Install a hyabundle package into the isolated data home before serve.
    #[must_use]
    pub fn preinstall_bundle(mut self, package: PathBuf) -> Self {
        self.preinstall_bundles.push(package);
        self
    }

    /// Start FakeLlm, spawn `hya-backend`, and return a ready [`E2eEnv`].
    pub async fn build(self) -> Result<E2eEnv, E2eError> {
        let fake = FakeLlm::start(self.scripts).await?;
        for (marker, steps) in self.routes {
            fake.route(marker, steps)?;
        }
        let mut spec = BackendSpec::new(
            self.binary.unwrap_or_else(default_backend_bin),
            fake.base_url(),
        );
        spec.yolo = self.yolo;
        spec.permission_model = self.permission_model;
        spec.additional_models = self.additional_models;
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
    /// Scripted OpenAI-compatible completions server backing the backend.
    pub fake: FakeLlm,
    /// Live `hya-backend serve` process and isolation roots.
    pub backend: BackendProcess,
    /// Typed native API client pointed at [`Self::backend`].
    pub client: Client,
    /// Raw HTTP client for Compat paths not covered by [`Self::client`].
    pub http: reqwest::Client,
    /// Default agent name for session create helpers.
    pub agent: String,
    /// Model id string passed to create-session (`fake/model` by default).
    pub model: String,
}

impl E2eEnv {
    /// Create a session with the env's default agent and model.
    pub async fn create_session(&self) -> Result<SessionId, E2eError> {
        self.create_session_with_agent(&self.agent).await
    }

    /// Create a session with an explicit agent name (still uses env model/workdir).
    pub async fn create_session_with_agent(&self, agent: &str) -> Result<SessionId, E2eError> {
        let resp = self
            .client
            .create_session(&pb::CreateSessionRequest {
                agent: agent.to_string(),
                model: self.model.clone(),
                workdir: self.backend.workdir_str(),
                ..Default::default()
            })
            .await?;
        let id = resp
            .session
            .and_then(|session| session.id.parse().ok())
            .ok_or_else(|| E2eError::Other("create session response missing id".into()))?;
        Ok(id)
    }

    /// Send a user prompt through the v1 event-driven API (admit + wait)
    /// and return the terminal turn state.
    pub async fn prompt(
        &self,
        session: SessionId,
        text: impl Into<String>,
    ) -> Result<pb::TurnInfo, E2eError> {
        Ok(self.client.prompt(session, text).await?)
    }

    /// Reopen the production backend on the same durable store and project.
    pub fn reopen(&mut self) -> Result<(), E2eError> {
        self.backend.reopen()?;
        self.client = Client::new(self.backend.url.clone());
        Ok(())
    }

    /// Fetch session event envelopes, optionally after `since_seq`.
    pub async fn events(
        &self,
        session: SessionId,
        since_seq: Option<u64>,
    ) -> Result<Vec<hya_proto::Envelope>, E2eError> {
        Ok(self.client.events(session, since_seq).await?)
    }

    /// Read a UTF-8 file relative to the temp project workdir.
    pub fn read_project_file(&self, relative: &str) -> Result<String, E2eError> {
        let path = self.backend.project.join(relative);
        Ok(std::fs::read_to_string(path)?)
    }

    /// Absolute path of a file under the temp project workdir.
    pub fn project_path(&self, relative: &str) -> PathBuf {
        self.backend.project.join(relative)
    }

    /// True if FakeLlm has recorded at least one chat-completions request.
    pub fn fake_saw_request(&self) -> Result<bool, E2eError> {
        Ok(!self.fake.requests()?.is_empty())
    }

    /// Every chat-completions request body FakeLlm has recorded.
    pub fn fake_requests(&self) -> Result<Vec<Value>, E2eError> {
        self.fake.requests()
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

    /// POST JSON to a backend path; empty bodies become `null`.
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

    /// List pending permission requests (v1 interaction plane), as an array.
    pub async fn list_permissions(&self) -> Result<Value, E2eError> {
        let body = self
            .get_json("/v1/interactions?type=INTERACTION_TYPE_PERMISSION")
            .await?;
        Ok(body
            .get("interactions")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())))
    }

    /// Reply to a pending permission request (`once` / `always` / other = deny).
    pub async fn reply_permission(&self, request_id: &str, reply: &str) -> Result<(), E2eError> {
        let (allowed, persist) = match reply {
            "once" => (true, false),
            "always" => (true, true),
            _ => (false, false),
        };
        let body = serde_json::json!({ "permission": { "allowed": allowed, "persist": persist } });
        let _ = self
            .post_json(&format!("/v1/interactions/{request_id}/respond"), &body)
            .await?;
        Ok(())
    }

    /// List pending interactive question requests (v1), as an array.
    pub async fn list_questions(&self) -> Result<Value, E2eError> {
        let body = self
            .get_json("/v1/interactions?type=INTERACTION_TYPE_QUESTION")
            .await?;
        Ok(body
            .get("interactions")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())))
    }

    /// Answer a pending question request; `answers` maps to the first
    /// selected option (v1 carries one answer per interaction).
    pub async fn reply_question(&self, request_id: &str, answers: Value) -> Result<(), E2eError> {
        let answer = first_answer(&answers);
        let body = serde_json::json!({ "question": { "answer": answer } });
        let _ = self
            .post_json(&format!("/v1/interactions/{request_id}/respond"), &body)
            .await?;
        Ok(())
    }

    /// List sessions via the v1 surface.
    pub async fn list_sessions_compat(&self) -> Result<Value, E2eError> {
        self.get_json("/v1/sessions").await
    }

    /// List configured agents via the v1 surface.
    pub async fn list_agents(&self) -> Result<Value, E2eError> {
        self.get_json("/v1/agents").await
    }

    /// Fetch the session tree (parent/children) for multi-agent layouts,
    /// assembled from v1 parent-filtered listings.
    pub async fn session_tree(&self, session: &SessionId) -> Result<Value, E2eError> {
        self.build_tree(session, 0).await
    }

    /// Compat-shaped session context (projected messages for the session).
    pub async fn session_context(&self, session: &SessionId) -> Result<Value, E2eError> {
        let messages = self
            .get_json(&format!("/v1/sessions/{session}/messages"))
            .await?;
        Ok(
            serde_json::json!({ "data": messages.get("messages").cloned().unwrap_or(Value::Array(Vec::new())) }),
        )
    }

    /// Session todo list (v1).
    pub async fn session_todos(&self, session: &SessionId) -> Result<Value, E2eError> {
        self.get_json(&format!("/v1/sessions/{session}/todo")).await
    }

    /// Run statuses map (`session_id` → `{type: "busy"}` while running),
    /// derived from the v1 session listing.
    pub async fn session_statuses(&self) -> Result<Value, E2eError> {
        let listed = self.get_json("/v1/sessions").await?;
        let mut map = serde_json::Map::new();
        if let Some(sessions) = listed.get("sessions").and_then(Value::as_array) {
            for session in sessions {
                let id = session
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let busy = session
                    .get("busy")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                map.insert(
                    id.to_string(),
                    if busy {
                        serde_json::json!({ "type": "busy" })
                    } else {
                        serde_json::json!({ "type": "idle" })
                    },
                );
            }
        }
        Ok(Value::Object(map))
    }

    /// Recursively build the legacy-shaped tree from v1 parent listings,
    /// enriched with team roster handles folded from raw envelopes.
    async fn build_tree(&self, session: &SessionId, depth: usize) -> Result<Value, E2eError> {
        Box::pin(self.build_tree_inner(session, depth)).await
    }

    async fn build_tree_inner(&self, session: &SessionId, depth: usize) -> Result<Value, E2eError> {
        let listed = self
            .get_json(&format!("/v1/sessions?parent={session}"))
            .await?;
        let roster = self.team_roster(session).await?;
        let mut children = Vec::new();
        if depth < 8
            && let Some(rows) = listed.get("sessions").and_then(Value::as_array)
        {
            for row in rows {
                let id = row.get("id").and_then(Value::as_str).unwrap_or_default();
                let agent = row.get("agent").and_then(Value::as_str).unwrap_or_default();
                if let Ok(child_id) = id.parse::<SessionId>() {
                    let mut node = Box::pin(self.build_tree(&child_id, depth + 1)).await?;
                    node["member"] = serde_json::json!({ "subagent_type": agent });
                    children.push(node);
                }
            }
        }
        for child in &mut children {
            let child_session = child.get("session").and_then(Value::as_str);
            if let Some(entry) = child_session.and_then(|id| roster.get(id)) {
                child["roster"] = serde_json::to_value(entry).unwrap_or(Value::Null);
            }
        }
        Ok(serde_json::json!({
            "session": session.to_string(),
            "children": children,
        }))
    }

    /// Fold the root session's raw envelopes and index team roster rows by
    /// the member's session id.
    async fn team_roster(
        &self,
        session: &SessionId,
    ) -> Result<std::collections::BTreeMap<String, hya_proto::projection::RosterEntry>, E2eError>
    {
        let envelopes = self.events(*session, None).await?;
        let projection = hya_proto::Projection::from_events(&envelopes);
        Ok(projection
            .team
            .roster
            .into_values()
            .map(|entry| (entry.session.to_string(), entry))
            .collect())
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

    /// Create a session via v1 so workdir is explicit.
    pub async fn compat_create_session(&self) -> Result<SessionId, E2eError> {
        let body = serde_json::json!({
            "agent": self.agent,
            "model": self.model,
            "workdir": self.backend.workdir_str(),
        });
        let created = self.post_json("/v1/sessions", &body).await?;
        let id = created
            .pointer("/session/id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| E2eError::Other(format!("v1 create missing id: {created}")))?;
        id.parse()
            .map_err(|e| E2eError::Other(format!("parse session id {id}: {e}")))
    }

    /// v1 event-driven prompt (admit + wait terminal) with FakeLlm pacing.
    pub async fn compat_prompt_and_wait(
        &self,
        session: SessionId,
        text: impl Into<String>,
        timeout: Duration,
    ) -> Result<Value, E2eError> {
        let before = self.fake.requests().map(|r| r.len()).unwrap_or(0);
        let body = serde_json::json!({
            "prompt": { "text": text.into() }
        });
        let admitted = self
            .post_json(&format!("/v1/sessions/{session}/turns"), &body)
            .await?;
        // Wait for at least one new FakeLlm hit and the run registry to clear.
        let wait_result = wait_until("v1 turn FakeLlm", timeout, || async {
            let n = self.fake.requests().map(|r| r.len()).unwrap_or(0);
            Ok(n > before)
        })
        .await;
        if wait_result.is_err() {
            let events = self.client.events(session, None).await;
            eprintln!("V1TURN STALL admitted={admitted} events={events:?}");
        }
        wait_result?;
        self.wait_session_idle(&session, timeout).await?;
        Ok(admitted)
    }

    /// POST `/v1/sessions/{id}/compact` (sync summarize + inject system message).
    pub async fn compact_session(&self, session: &SessionId) -> Result<(), E2eError> {
        let url = format!("{}/v1/sessions/{session}/compact", self.backend.url);
        let resp = self
            .http
            .post(url)
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(E2eError::Http(format!("POST compact -> {status}: {text}")));
        }
        Ok(())
    }

    /// POST `/v1/sessions/{id}/summarize`.
    pub async fn summarize_session_legacy(&self, session: &SessionId) -> Result<Value, E2eError> {
        self.post_json(&format!("/v1/sessions/{session}/summarize"), &Value::Null)
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

    /// Wait until a question is pending; return its request id.
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
    ) -> Result<pb::TurnInfo, E2eError> {
        let http = self.http.clone();
        let base = self.backend.url.clone();
        let reply = reply.to_string();
        let replier =
            tokio::spawn(async move { auto_reply_permission(http, base, &reply, timeout).await });
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
    ) -> Result<pb::TurnInfo, E2eError> {
        let http = self.http.clone();
        let base = self.backend.url.clone();
        let replier =
            tokio::spawn(async move { auto_reply_question(http, base, answers, timeout).await });
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

    /// Every request body attributed to `marker`'s route, joined into one string.
    ///
    /// This is the recipient-side observation channel for mailbox delivery: mail
    /// reaches a resident only by being injected as a `[mail from …] …` user
    /// prompt into its next turn, so a delivered body shows up here — and in no
    /// other agent's dump.
    pub fn route_dump(&self, marker: &str) -> Result<String, E2eError> {
        let requests = self
            .fake
            .route_requests(marker)?
            .ok_or_else(|| E2eError::Other(format!("no FakeLlm route registered for {marker}")))?;
        Ok(requests
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"))
    }

    /// Wait until `marker`'s route has been asked at least `count` times.
    ///
    /// Use this to prove a resident is actually running a turn loop rather than
    /// merely being registered in the roster.
    pub async fn wait_route_requests(
        &self,
        marker: &str,
        count: usize,
        timeout: Duration,
    ) -> Result<(), E2eError> {
        wait_until(
            &format!("route {marker} reaches {count} requests"),
            timeout,
            || async {
                Ok(self
                    .fake
                    .route_requests(marker)?
                    .is_some_and(|requests| requests.len() >= count))
            },
        )
        .await
    }

    /// Wait until `needle` appears in `marker`'s recorded request bodies.
    ///
    /// Mailbox delivery is asynchronous (send appends an event, the supervisor
    /// wakes the recipient, the recipient then asks the model), so scenarios
    /// must poll on the recipient's own observable state instead of sleeping.
    pub async fn wait_route_contains(
        &self,
        marker: &str,
        needle: &str,
        timeout: Duration,
    ) -> Result<(), E2eError> {
        wait_until(
            &format!("route {marker} sees {needle}"),
            timeout,
            || async { Ok(self.route_dump(marker)?.contains(needle)) },
        )
        .await
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
            .get(format!(
                "{base}/v1/interactions?type=INTERACTION_TYPE_PERMISSION"
            ))
            .send()
            .await
            .map_err(|e| E2eError::Http(e.to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| E2eError::Http(e.to_string()))?;
        if status.is_success()
            && let Ok(body) = serde_json::from_str::<Value>(&text)
            && let Some(id) = body
                .get("interactions")
                .and_then(Value::as_array)
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("id"))
                .and_then(Value::as_str)
        {
            let (allowed, persist) = match reply {
                "once" => (true, false),
                "always" => (true, true),
                _ => (false, false),
            };
            let reply_resp = http
                .post(format!("{base}/v1/interactions/{id}/respond"))
                .json(&serde_json::json!({ "permission": { "allowed": allowed, "persist": persist } }))
                .send()
                .await
                .map_err(|e| E2eError::Http(e.to_string()))?;
            if reply_resp.status().is_success() {
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
            .get(format!(
                "{base}/v1/interactions?type=INTERACTION_TYPE_QUESTION"
            ))
            .send()
            .await
            .map_err(|e| E2eError::Http(e.to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| E2eError::Http(e.to_string()))?;
        if status.is_success()
            && let Ok(body) = serde_json::from_str::<Value>(&text)
            && let Some(id) = body
                .get("interactions")
                .and_then(Value::as_array)
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("id"))
                .and_then(Value::as_str)
        {
            let answer = first_answer(&answers);
            let reply_resp = http
                .post(format!("{base}/v1/interactions/{id}/respond"))
                .json(&serde_json::json!({ "question": { "answer": answer } }))
                .send()
                .await
                .map_err(|e| E2eError::Http(e.to_string()))?;
            if reply_resp.status().is_success() {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Extract the first selected answer from a legacy answers payload.
fn first_answer(answers: &Value) -> String {
    if let Some(text) = answers.as_str() {
        return text.to_string();
    }
    if let Some(rows) = answers.as_array()
        && let Some(first) = rows.first()
    {
        if let Some(text) = first.as_str() {
            return text.to_string();
        }
        if let Some(inner) = first.as_array()
            && let Some(text) = inner.first().and_then(Value::as_str)
        {
            return text.to_string();
        }
    }
    String::new()
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
