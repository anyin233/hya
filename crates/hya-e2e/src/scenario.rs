//! High-level E2E environment: FakeLlm + BackendProcess + hya-sdk + HTTP helpers.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use hya_api::v2 as pb;
use hya_proto::SessionId;
use hya_sdk::Client;
use serde_json::Value;

use crate::backend::{
    BackendProcess, BackendSpec, MCP_ECHO_SCRIPT_REL, McpFixture, default_backend_bin,
    materialize_root, mcp_echo_command, mcp_echo_script,
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
    code_mode: Option<String>,
    mcp: Vec<McpFixture>,
    skill_files: Vec<(String, String)>,
    project_files: Vec<(String, Vec<u8>)>,
    preinstall_bundles: Vec<PathBuf>,
    additional_models: Vec<String>,
    extra_env: Vec<(String, String)>,
    serve_args: Vec<String>,
}

impl Default for E2eEnvBuilder {
    fn default() -> Self {
        Self {
            scripts: Vec::new(),
            routes: Vec::new(),
            yolo: true,
            agent: "hya-main".into(),
            binary: None,
            permission_model: "allow".into(),
            code_mode: None,
            mcp: Vec::new(),
            skill_files: Vec::new(),
            project_files: Vec::new(),
            preinstall_bundles: Vec::new(),
            additional_models: Vec::new(),
            extra_env: Vec::new(),
            serve_args: Vec::new(),
        }
    }
}

impl E2eEnvBuilder {
    /// Empty builder with YOLO on, `allow` permissions, and agent `hya-main`.
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

    /// Set `tools.code.mode` in the temp config (`with_tools` | `only`).
    #[must_use]
    pub fn code_mode(mut self, mode: impl Into<String>) -> Self {
        self.code_mode = Some(mode.into());
        self
    }

    /// Default agent name used when creating sessions through this env.
    #[must_use]
    pub fn agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = agent.into();
        self
    }

    /// Override path to the `hya` binary (default: workspace/target lookup).
    #[must_use]
    pub fn binary(mut self, path: PathBuf) -> Self {
        self.binary = Some(path);
        self
    }

    /// Pass an extra environment variable to the backend process.
    #[must_use]
    pub fn backend_env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_env.push((name.into(), value.into()));
        self
    }

    /// Append an extra argument to `hya serve` (after `--bind 127.0.0.1:0`),
    /// e.g. `--relay <url>`.
    #[must_use]
    pub fn serve_arg(mut self, arg: impl Into<String>) -> Self {
        self.serve_args.push(arg.into());
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

    /// Start FakeLlm, spawn `hya`, and return a ready [`E2eEnv`].
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
        spec.code_mode = self.code_mode;
        spec.additional_models = self.additional_models;
        spec.mcp = self.mcp;
        spec.skill_files = self.skill_files;
        spec.project_files = self.project_files;
        spec.preinstall_bundles = self.preinstall_bundles;
        spec.env = self.extra_env;
        spec.serve_args = self.serve_args;
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

    /// Start FakeLlm and materialize isolated project/XDG roots WITHOUT booting
    /// `hya serve`. Intended for headless `hya exec` and CLI scenarios.
    pub async fn build_headless(self) -> Result<HeadlessEnv, E2eError> {
        let fake = FakeLlm::start(self.scripts).await?;
        for (marker, steps) in self.routes {
            fake.route(marker, steps)?;
        }
        let binary = self.binary.unwrap_or_else(default_backend_bin);
        let mut spec = BackendSpec::new(binary.clone(), fake.base_url());
        spec.yolo = self.yolo;
        spec.permission_model = self.permission_model;
        spec.code_mode = self.code_mode;
        spec.additional_models = self.additional_models;
        spec.mcp = self.mcp;
        spec.skill_files = self.skill_files;
        spec.project_files = self.project_files;
        spec.preinstall_bundles = self.preinstall_bundles;
        spec.env = self.extra_env;
        spec.serve_args = self.serve_args;
        let mat = tokio::task::spawn_blocking(move || materialize_root(&spec))
            .await
            .map_err(|e| E2eError::Other(format!("join headless fixture: {e}")))??;
        Ok(HeadlessEnv {
            fake,
            root: mat.root,
            project: mat.project,
            xdg_config_home: mat.xdg_config_home,
            xdg_data_home: mat.xdg_data_home,
            binary,
            model: mat.model_ref,
        })
    }
}

/// Isolated `FakeLlm` + project/XDG roots for headless `hya exec` / CLI runs
/// without the overhead of spawning a live `hya serve` daemon.
pub struct HeadlessEnv {
    /// Scripted OpenAI-compatible completions server.
    pub fake: FakeLlm,
    /// Temporary isolation root (removed on drop).
    pub root: PathBuf,
    /// Temp project workdir.
    pub project: PathBuf,
    /// Isolated `XDG_CONFIG_HOME`.
    pub xdg_config_home: PathBuf,
    /// Isolated `XDG_DATA_HOME`.
    pub xdg_data_home: PathBuf,
    /// Resolved `hya` binary path.
    pub binary: PathBuf,
    /// Configured default model ref (`fake/model`).
    pub model: String,
}

impl HeadlessEnv {
    /// Build a `tokio::process::Command` for `hya` with isolated HOME/XDG/cwd.
    #[must_use]
    pub fn command(&self) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.binary);
        cmd.current_dir(&self.project)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", &self.xdg_config_home)
            .env("XDG_DATA_HOME", &self.xdg_data_home)
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env_remove("HYA_MODEL")
            .kill_on_drop(true);
        cmd
    }

    /// Build a `hya --yolo --model <model> --db <root>/<db_name> exec` command.
    #[must_use]
    pub fn exec_command(&self, db_name: &str) -> tokio::process::Command {
        let mut cmd = self.command();
        cmd.args(["--yolo", "--model", &self.model, "--db"])
            .arg(self.root.join(db_name))
            .arg("exec");
        cmd
    }

    /// Read a UTF-8 file relative to the temp project workdir.
    pub fn read_project_file(&self, relative: &str) -> Result<String, E2eError> {
        Ok(std::fs::read_to_string(self.project.join(relative))?)
    }

    /// Absolute path of a file under the temp project workdir.
    #[must_use]
    pub fn project_path(&self, relative: &str) -> PathBuf {
        self.project.join(relative)
    }

    /// Compact diagnostic dump for failed assertions.
    #[must_use]
    pub fn diagnostics(&self) -> String {
        let fake_n = self.fake.requests().map(|r| r.len()).unwrap_or(0);
        let remaining = self.fake.remaining_scripts().unwrap_or(0);
        format!(
            "headless project={} fake_requests={fake_n} remaining_scripts={remaining}",
            self.project.display()
        )
    }
}

impl Drop for HeadlessEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Ready environment for scenarios.
pub struct E2eEnv {
    /// Scripted OpenAI-compatible completions server backing the backend.
    pub fake: FakeLlm,
    /// Live `hya serve` process and isolation roots.
    pub backend: BackendProcess,
    /// Typed native API client pointed at [`Self::backend`].
    pub client: Client,
    /// Raw HTTP client for assertions that inspect the v1 wire response.
    pub http: reqwest::Client,
    /// Default agent name for session create helpers.
    pub agent: String,
    /// Model id string passed to create-session (`fake/model` by default).
    pub model: String,
}

/// Replay the opaque raw envelope lines and decode them for Track P assertions.
///
/// Returns a harness error with the one-based record position if decoding fails.
pub async fn load_raw_events(
    client: &Client,
    session: SessionId,
    since_seq: Option<u64>,
) -> Result<Vec<hya_proto::Envelope>, E2eError> {
    let response = client
        .list_events_request(&pb::ListEventsRequest {
            session: session.to_string(),
            since_seq: since_seq.unwrap_or_default(),
            include_raw: true,
            ..Default::default()
        })
        .await?;
    response
        .raw_envelopes
        .iter()
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|error| {
                E2eError::Other(format!(
                    "decode raw event {} for {session}: {error}",
                    index + 1
                ))
            })
        })
        .collect()
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
                workdir: Some(self.backend.workdir_str()),
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
        Ok(self.client.prompt_text(&session.to_string(), text).await?)
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
        load_raw_events(&self.client, session, since_seq).await
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

    /// PUT JSON to a backend path; empty bodies become `null`.
    pub async fn put_json(&self, path: &str, body: &Value) -> Result<Value, E2eError> {
        let url = format!("{}{path}", self.backend.url);
        let resp = self.http.put(url).json(body).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(E2eError::Http(format!("PUT {path} -> {status}: {text}")));
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(&text)?)
    }

    /// PATCH JSON to a backend path; empty bodies become `null`.
    pub async fn patch_json(&self, path: &str, body: &Value) -> Result<Value, E2eError> {
        let url = format!("{}{path}", self.backend.url);
        let resp = self.http.patch(url).json(body).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(E2eError::Http(format!("PATCH {path} -> {status}: {text}")));
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(&text)?)
    }

    /// List pending permission requests (v1 interaction plane), as an array.
    pub async fn list_permissions(&self) -> Result<Value, E2eError> {
        let body = self
            .get_json("/v2/interactions?type=INTERACTION_TYPE_PERMISSION")
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
            .post_json(&format!("/v2/interactions/{request_id}/respond"), &body)
            .await?;
        Ok(())
    }

    /// List sessions using the server’s default filters and page size.
    pub async fn list_sessions(&self) -> Result<pb::ListSessionsResponse, E2eError> {
        Ok(self.client.list_sessions().await?)
    }

    /// List configured agents via the v1 surface.
    pub async fn list_agents(&self) -> Result<Value, E2eError> {
        self.get_json("/v2/agents").await
    }

    /// Compat-shaped session context (projected messages for the session).
    pub async fn session_context(&self, session: &SessionId) -> Result<Value, E2eError> {
        let messages = self
            .get_json(&format!("/v2/sessions/{session}/messages"))
            .await?;
        Ok(
            serde_json::json!({ "data": messages.get("messages").cloned().unwrap_or(Value::Array(Vec::new())) }),
        )
    }

    /// Session todo list (v1).
    pub async fn session_todos(&self, session: &SessionId) -> Result<Value, E2eError> {
        self.get_json(&format!("/v2/sessions/{session}/todo")).await
    }

    /// Fetch the nested tree from spawning-session edges and roster rows in the event log.
    pub async fn session_tree(&self, session: &SessionId) -> Result<Value, E2eError> {
        const DEPTH_LIMIT: usize = 8;
        // session -> [(child, subagent_type)]
        let mut nodes: std::collections::BTreeMap<String, Vec<(String, String)>> =
            std::collections::BTreeMap::new();
        let mut roster: std::collections::BTreeMap<String, hya_tool::extension::RosterEntry> =
            std::collections::BTreeMap::new();
        let mut queue: Vec<(hya_proto::SessionId, usize)> = vec![(*session, 0)];
        let mut seen: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::from([session.to_string()]);
        let reducers = hya_tool::extension::projection_reducers();
        while let Some((current, depth)) = queue.pop() {
            let envelopes = self.events(current, None).await?;
            let projection = hya_proto::Projection::from_events_with(&envelopes, &reducers);
            if depth == 0 {
                let team = hya_tool::extension::team_projection(&projection);
                for entry in team.roster.into_values() {
                    roster.insert(entry.session.to_string(), entry);
                }
            }
            if depth >= DEPTH_LIMIT {
                continue;
            }
            for member in &hya_tool::extension::members_projection(&projection) {
                let Some(child) = member.child else {
                    continue;
                };
                let child_key = child.to_string();
                nodes
                    .entry(current.to_string())
                    .or_default()
                    .push((child_key.clone(), member.subagent_type.to_string()));
                if seen.insert(child_key) {
                    queue.push((child, depth + 1));
                }
            }
        }
        fn assemble(
            key: &str,
            nodes: &std::collections::BTreeMap<String, Vec<(String, String)>>,
            roster: &mut std::collections::BTreeMap<String, hya_tool::extension::RosterEntry>,
        ) -> Value {
            let mut children = Vec::new();
            if let Some(spawns) = nodes.get(key) {
                for (child, subagent_type) in spawns {
                    let mut node = assemble(child, nodes, roster);
                    node["member"] = serde_json::json!({ "subagent_type": subagent_type });
                    if let Some(entry) = roster.remove(child) {
                        node["roster"] = serde_json::to_value(entry).unwrap_or(Value::Null);
                    }
                    children.push(node);
                }
            }
            serde_json::json!({ "session": key, "children": children })
        }
        Ok(assemble(&session.to_string(), &nodes, &mut roster))
    }
    /// Wait until the session is not listed as busy (v1 session listing).
    pub async fn wait_session_idle(
        &self,
        session: &SessionId,
        timeout: Duration,
    ) -> Result<(), E2eError> {
        let key = session.to_string();
        wait_until("session idle", timeout, || async {
            let listed = self.list_sessions().await?;
            Ok(!listed.sessions.iter().any(|row| row.id == key && row.busy))
        })
        .await
    }

    /// Admit a typed prompt and wait for both a new FakeLlm request and the turn.
    pub async fn prompt_and_wait_for_fake_request(
        &self,
        session: SessionId,
        text: impl Into<String>,
        timeout: Duration,
    ) -> Result<pb::TurnInfo, E2eError> {
        let before = self.fake.requests()?.len();
        let admitted = self
            .client
            .create_turn_request(&pb::CreateTurnRequest {
                session: session.to_string(),
                kind: Some(pb::create_turn_request::Kind::Prompt(pb::PromptTurn {
                    text: text.into(),
                    ..Default::default()
                })),
            })
            .await?;
        let turn = admitted
            .turn
            .filter(|turn| !turn.id.is_empty())
            .ok_or_else(|| E2eError::Other("create turn response missing a turn ID".into()))?;
        let observed = wait_until("turn FakeLlm request", timeout, || async {
            Ok(self.fake.requests()?.len() > before)
        })
        .await;
        if observed.is_err() {
            let events = self.events(session, None).await;
            eprintln!("turn {} stalled: events={events:?}", turn.id);
        }
        observed?;
        let info = self
            .client
            .wait_turn_request(&pb::WaitTurnRequest {
                session: session.to_string(),
                turn: turn.id,
                timeout_ms: timeout.as_millis().min(600_000) as u64,
            })
            .await?;
        Ok(info)
    }

    /// POST `/v2/sessions/{id}/compact` (sync summarize + inject system message).
    pub async fn compact_session(&self, session: &SessionId) -> Result<(), E2eError> {
        let url = format!("{}/v2/sessions/{session}/compact", self.backend.url);
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

    /// Wait until `/v2/mcp` reports `name` with status `connected`.
    pub async fn wait_mcp_connected(
        &self,
        name: &str,
        timeout: Duration,
    ) -> Result<Value, E2eError> {
        wait_until(&format!("mcp {name} connected"), timeout, || async {
            let status = self.get_json("/v2/mcp").await.unwrap_or(Value::Null);
            Ok(status
                .get("servers")
                .and_then(Value::as_array)
                .is_some_and(|rows| {
                    rows.iter().any(|row| {
                        row.get("name").and_then(Value::as_str) == Some(name)
                            && row.get("state").and_then(Value::as_str)
                                == Some("MCP_SERVER_STATE_CONNECTED")
                    })
                }))
        })
        .await?;
        self.get_json("/v2/mcp").await
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
                "{base}/v2/interactions?type=INTERACTION_TYPE_PERMISSION"
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
                .post(format!("{base}/v2/interactions/{id}/respond"))
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
                "{base}/v2/interactions?type=INTERACTION_TYPE_QUESTION"
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
                .post(format!("{base}/v2/interactions/{id}/respond"))
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
