# yaca — Design & Implementation Roadmap (Rust-engineer lens)

> **Lens**: bottom-up, type-driven, concrete. The job here is to make this
> _buildable_. Every section answers "what crate, what type, what signature,
> what does the compiler see". Confirmed decisions D1–D4 are taken as given.
>
> **Reference systems**: compat (Bun/TS, event-sourced sessions, two-layer
> tools, Provider/Protocol/Route) — see `research/compat-architecture.md`;
> omo team mode (lead + members, file-backed mailbox/task board, 12 team_* tools,
> category→model routing) — see `research/omo-team-mode.md`; Claude Code `/goal`
> (independent cheap-model evaluator judging only the transcript) — see
> `research/goal-driven-verification.md`.
>
> **Why this lens matters**: compat and omo are TypeScript. Many of their
> patterns (`Effect`, Zod, freeform JSON shape-shifting) do not survive contact
> with the borrow checker or `dyn Trait` dyn-safety rules. This doc commits to
> Rust-idiomatic substitutes _before_ the first crate is generated, so we do not
> end up rewriting the agent loop three times.

---

## 0. Top-level architecture in one diagram

```
+------------------------------------------------------------------+
|  ratatui TUI (crate: yaca-tui)                                   |
|   - one process; renders SessionView + TeamView + GoalBar        |
|   - HTTP/SSE client; no agent logic                              |
+----------------------------^-------------------------------------+
                             | local HTTP + SSE (axum/hyper)
+----------------------------v-------------------------------------+
|  core server (crate: yaca-server)                                |
|   axum router  ->  service layer  ->  domain                     |
|     /sessions, /messages, /events (SSE), /tools, /team, /goal    |
+----------------------------+-------------------------------------+
                             |
+----------------------------v-------------------------------------+
|  domain (crate: yaca-core)                                       |
|   SessionEngine -> AgentLoop -> ProviderRouter                   |
|       |                |                |                        |
|       v                v                v                        |
|   EventBus      ToolRegistry      Provider/Protocol/Route        |
|       |                |                                         |
|       v                v                                         |
|   Projector      PermissionPlane                                 |
|       |                                                          |
|       v                                                          |
|   sqlx::Sqlite (event log + projections)                         |
|                                                                  |
|   GoalEngine (wraps AgentLoop)  --calls-->  evaluator provider   |
|   TeamOrchestrator (lead+members) -> Mailbox + TaskBoard         |
+------------------------------------------------------------------+
                             |
                             v
+------------------------------------------------------------------+
|  out-of-process side effects                                     |
|   - git worktree (shell-out to `git` binary)                     |
|   - tmux pane per member (shell-out to `tmux`)                   |
|   - provider HTTP (reqwest + eventsource-stream)                 |
+------------------------------------------------------------------+
```

One process, multiple tokio runtimes? No — **one multi-thread tokio runtime**.
Members run as supervised `tokio::spawn` tasks inside the server. Background
side effects (git, tmux) shell out via `tokio::process::Command`.

---

## 1. Cargo workspace + crate split

Workspace root `Cargo.toml` lists 8 crates. Boundaries chosen so each crate has
**one** reason to change and a small, stable dependency surface.

```
yaca/
├── Cargo.toml                  # [workspace] members = [...]
├── crates/
│   ├── yaca-proto/             # wire types: Event, ApiRequest/Response, ids
│   ├── yaca-provider/          # Provider/Protocol/Route trait + impls
│   ├── yaca-tool/              # Tool trait, schema, registry, permissions
│   ├── yaca-core/              # domain: sessions, agent loop, orchestrator, goal
│   ├── yaca-store/             # sqlx schema + event log + projector
│   ├── yaca-server/            # axum HTTP/SSE server (binary lib)
│   ├── yaca-tui/               # ratatui client binary
│   └── yaca-cli/               # `yaca` umbrella binary (spawns server + tui)
└── xtask/                      # dev tooling (migrations, fake-provider runner)
```

### Dependency graph (acyclic, narrow)

```
yaca-cli ──> yaca-server ──> yaca-core ──> yaca-store
                │              │   │
                │              │   └──> yaca-tool ──> yaca-proto
                │              └──> yaca-provider ──> yaca-proto
                │              └──> yaca-proto
                ├──> yaca-tui ──> yaca-proto
                └──> yaca-proto
```

Critical: `yaca-proto` is **dependency-free** (only `serde`, `uuid`, `time`,
`thiserror`). Every other crate depends on it but it depends on none of them.
This is what lets the TUI and the server share types without dragging in sqlx
or tokio into the TUI's compile graph.

`yaca-core` is **runtime-agnostic** of HTTP. It exposes `SessionEngine`,
`TeamOrchestrator`, `GoalEngine` as plain async types; the server only owns the
axum routes that call them. This keeps `yaca-core` testable without spinning
HTTP.

### Pinned crate choices (workspace `[workspace.dependencies]`)

```toml
tokio          = { version = "1.40", features = ["full"] }
tokio-util     = { version = "0.7",  features = ["rt"] }    # CancellationToken
tokio-stream   = { version = "0.1",  features = ["sync"] }
futures        = "0.3"
futures-util   = "0.3"
async-trait    = "0.1"                                       # dyn-safe async traits
axum           = { version = "0.7",  features = ["macros", "ws"] }
tower          = { version = "0.5",  features = ["util"] }
tower-http     = { version = "0.6",  features = ["trace", "cors"] }
hyper          = { version = "1",    features = ["server", "http1"] }
reqwest        = { version = "0.12", default-features = false, features = ["json", "rustls-tls", "stream"] }
eventsource-stream = "0.2"                                   # SSE parsing for providers
serde          = { version = "1",    features = ["derive"] }
serde_json     = "1"
schemars       = { version = "0.8",  features = ["derive", "uuid1", "chrono"] }
jsonschema     = "0.18"                                      # runtime tool-input validation
sqlx           = { version = "0.8",  features = ["runtime-tokio", "sqlite", "json", "uuid", "time", "macros", "migrate"] }
thiserror      = "1"
anyhow         = "1"                                         # only in binaries
uuid           = { version = "1",    features = ["v4", "v7", "serde"] }
time           = { version = "0.3",  features = ["serde", "macros"] }
tracing        = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
ratatui        = "0.28"
crossterm      = "0.28"
clap           = { version = "4",    features = ["derive", "env"] }
toml           = "0.8"
camino         = { version = "1",    features = ["serde1"] }
bytes          = "1"
```

**Decisions worth noting**:
- `sqlx` over `rusqlite` — async-native, compile-time checked queries, plays well with `tokio::spawn` workers and connection pool.
- `axum` over hand-rolled `hyper` — typed extractors, mature SSE, cleanly tower-stacked middleware.
- `async-trait` instead of native `async fn in Trait` for `dyn`-objects (`dyn Provider`, `dyn Tool`, `dyn MailboxBackend`). Native `async fn in Trait` (Rust 1.75+) is used for non-`dyn` paths.
- `git2` is _not_ a workspace dep — shelling out to the `git` binary is simpler, has zero libgit2 cross-compile pain, and matches how the actual repo will be cloned/used.
- `tmux`: also shell-out. `tmux` is itself external; embedding is pointless.

---

## 2. `yaca-proto` — the wire-shared core types

Everything that crosses a process or task boundary lives here. Two design rules:

1. **Tagged enums everywhere** — never `serde(untagged)`; always
   `#[serde(tag = "type", rename_all = "snake_case")]`. The TUI must be able to
   discriminate a `Part` variant without trying-and-failing.
2. **Newtype every id** — no raw `String`/`Uuid` flying around. Compiler tells
   you which kind of id you mis-passed.

### 2.1 Ids

```rust
// crates/yaca-proto/src/ids.rs
use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id {
    ($name:ident, $prefix:literal) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);
        impl $name {
            pub fn new() -> Self { Self(Uuid::now_v7()) }
            pub const PREFIX: &'static str = $prefix;
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}_{}", $prefix, self.0.simple())
            }
        }
    };
}

id!(SessionId,      "ses");
id!(MessageId,      "msg");
id!(PartId,         "part");
id!(ToolCallId,     "tc");
id!(TeamRunId,      "team");
id!(MemberId,       "mbr");
id!(MailMessageId,  "mail");
id!(TaskItemId,     "task");
id!(GoalId,         "goal");
id!(EventSeq,       "evt");   // monotonic per-session sequence is u64, not uuid
```

(In practice `EventSeq` is a `u64`; shown here for symmetry — actual code uses
`pub struct EventSeq(pub u64)`.)

### 2.2 `Message` / `Part` — tagged enums

```rust
// crates/yaca-proto/src/message.rs
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    User { id: MessageId, parts: Vec<Part>, time: OffsetDateTime },
    Assistant {
        id: MessageId,
        agent: AgentName,
        model: ModelRef,
        parts: Vec<Part>,
        finish: Option<FinishReason>,
        cost: Option<CostBreakdown>,
        tokens: Option<TokenUsage>,
        time: OffsetDateTime,
    },
    System { id: MessageId, content: String, time: OffsetDateTime },
    Synthetic { id: MessageId, kind: SyntheticKind, content: String, time: OffsetDateTime },
    AgentSwitched { id: MessageId, from: AgentName, to: AgentName, time: OffsetDateTime },
    ModelSwitched { id: MessageId, from: ModelRef, to: ModelRef, time: OffsetDateTime },
    Compaction { id: MessageId, summary: String, time: OffsetDateTime },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text { id: PartId, text: String },
    Reasoning { id: PartId, text: String },
    Tool {
        id: PartId,
        call_id: ToolCallId,
        name: ToolName,
        state: ToolPartState,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum ToolPartState {
    Pending  { input: serde_json::Value },
    Running  { input: serde_json::Value, started_at: OffsetDateTime },
    Completed { input: serde_json::Value, output: serde_json::Value, time_ms: u64 },
    Error    { input: serde_json::Value, message: String },
}
```

Why `serde_json::Value` for tool input/output? Because tools come from a
registry and their schemas vary. The _typed_ wrapper lives in `yaca-tool` and
project-validates input on entry / output on exit before persisting. Storing
the validated JSON shape on the part keeps `yaca-proto` schema-free.

### 2.3 `Event` — the canonical streaming event

This is the single most-replicated type in the system. Every provider stream
normalizes into this enum; the projector folds it; the SSE channel ships it;
the TUI renders it.

```rust
// crates/yaca-proto/src/event.rs
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    // -------- session lifecycle --------
    SessionCreated   { session: SessionId, parent: Option<SessionId>, agent: AgentName },
    SessionTitled    { session: SessionId, title: String },

    // -------- message lifecycle --------
    MessageStarted   { session: SessionId, message: MessageId, role: Role },
    MessageFinished  { session: SessionId, message: MessageId, finish: FinishReason },

    // -------- assistant streaming parts --------
    StepStarted      { session: SessionId, message: MessageId, step: u32 },
    StepFinished     { session: SessionId, message: MessageId, step: u32 },

    TextStart        { session: SessionId, message: MessageId, part: PartId },
    TextDelta        { session: SessionId, message: MessageId, part: PartId, delta: String },
    TextEnd          { session: SessionId, message: MessageId, part: PartId },

    ReasoningStart   { session: SessionId, message: MessageId, part: PartId },
    ReasoningDelta   { session: SessionId, message: MessageId, part: PartId, delta: String },
    ReasoningEnd     { session: SessionId, message: MessageId, part: PartId },

    // -------- tool lifecycle --------
    ToolInputStart   { session: SessionId, message: MessageId, part: PartId, call: ToolCallId, name: ToolName },
    ToolInputDelta   { session: SessionId, message: MessageId, part: PartId, call: ToolCallId, delta: String },
    ToolInputEnd     { session: SessionId, message: MessageId, part: PartId, call: ToolCallId, input: serde_json::Value },
    ToolCallRequested{ session: SessionId, message: MessageId, part: PartId, call: ToolCallId, input: serde_json::Value },
    ToolResult       { session: SessionId, message: MessageId, part: PartId, call: ToolCallId, output: serde_json::Value, time_ms: u64 },
    ToolError        { session: SessionId, message: MessageId, part: PartId, call: ToolCallId, message_text: String },

    // -------- permission plane --------
    PermissionAsked  { session: SessionId, request: PermissionRequest },
    PermissionResolved { session: SessionId, request_id: PermissionRequestId, decision: PermissionDecision },

    // -------- team mode --------
    TeamCreated      { team: TeamRunId, lead: SessionId, members: Vec<MemberId> },
    TeamMemberSpawned{ team: TeamRunId, member: MemberId, session: SessionId, model: ModelRef },
    TeamMailDelivered{ team: TeamRunId, message: MailMessageId, from: MailEndpoint, to: MailEndpoint },
    TeamTaskUpdated  { team: TeamRunId, task: TaskItemId, status: TaskStatus },
    TeamShutdownRequested { team: TeamRunId, target: MailEndpoint },
    TeamShutdownApproved  { team: TeamRunId, target: MailEndpoint },
    TeamDeleted      { team: TeamRunId, force: bool },

    // -------- goal engine --------
    GoalSet          { session: SessionId, goal: GoalId, condition: String, bound: Option<GoalBound> },
    GoalEvaluated    { session: SessionId, goal: GoalId, met: bool, reason: String, turns: u32 },
    GoalCleared      { session: SessionId, goal: GoalId, outcome: GoalOutcome },

    // -------- errors --------
    Error            { session: Option<SessionId>, code: String, message: String },
}
```

Each `Event` is also wrapped server-side as `Envelope { seq: u64, ts:
OffsetDateTime, event: Event }` for ordering across SSE reconnects and for the
event-sourcing replay path.

---

## 3. `yaca-provider` — Provider / Protocol / Route

Lifted from compat's split (see research). The point is that the agent loop
sees ONE event stream regardless of which vendor.

### 3.1 The traits

```rust
// crates/yaca-provider/src/lib.rs
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;
use yaca_proto::{Event, Message, ModelRef, ToolSchema};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderId(pub String);          // "anthropic", "openai", "openrouter", "ollama"

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProtocolKind(pub &'static str);  // "openai_chat", "openai_responses", "anthropic_messages"

#[derive(Clone, Debug)]
pub struct CompletionRequest {
    pub model: ModelRef,
    pub system: Option<String>,
    pub messages: Vec<Message>,           // canonical, not provider-shaped
    pub tools: Vec<ToolSchema>,           // canonical
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub stop: Vec<String>,
}

pub type EventStream = Pin<Box<dyn Stream<Item = Result<ProviderEvent, ProviderError>> + Send>>;

#[derive(Clone, Debug)]
pub enum ProviderEvent {
    // emitted by the protocol layer in provider-native shape
    RawDelta(serde_json::Value),
    // normalized by the protocol layer using protocol-specific knowledge:
    Canonical(Event),
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &ProviderId;
    fn list_models(&self) -> Vec<ModelDescriptor>;
    fn protocol_for(&self, model: &ModelRef) -> ProtocolKind;
    fn route_for(&self, model: &ModelRef) -> Box<dyn Route>;
}

#[async_trait]
pub trait Route: Send + Sync {
    async fn complete(&self, req: CompletionRequest, cancel: CancellationToken)
        -> Result<EventStream, ProviderError>;
}

#[async_trait]
pub trait Protocol: Send + Sync {
    fn kind(&self) -> ProtocolKind;
    /// Encode a canonical request into provider-native HTTP body JSON.
    fn encode(&self, req: &CompletionRequest) -> Result<serde_json::Value, ProviderError>;
    /// Stream provider-native SSE events and yield canonical `Event`s.
    fn decode(&self, raw: EventStream) -> EventStream;
}
```

`Provider` owns `Route` ownership; `Route` owns a `Protocol`. The actual
`AnthropicMessagesRoute` is:

```rust
pub struct AnthropicMessagesRoute {
    client: reqwest::Client,
    endpoint: Url,                       // https://api.anthropic.com/v1/messages
    auth: AnthropicAuth,                 // x-api-key
    protocol: AnthropicMessagesProtocol, // owns encode/decode
}

#[async_trait]
impl Route for AnthropicMessagesRoute {
    async fn complete(&self, req: CompletionRequest, cancel: CancellationToken)
        -> Result<EventStream, ProviderError>
    {
        let body = self.protocol.encode(&req)?;
        let resp = self.client.post(self.endpoint.clone())
            .header("x-api-key", &self.auth.key)
            .header("anthropic-version", "2023-06-01")
            .json(&body).send().await?;
        let raw = sse_stream_from(resp, cancel);
        Ok(self.protocol.decode(raw))
    }
}
```

`sse_stream_from` is a small helper over `eventsource-stream::Eventsource` that
respects a `CancellationToken`. The same pattern repeats for OpenAI Chat /
OpenAI Responses / OpenAI-compatible (Ollama, vLLM, etc.) — only the endpoint,
auth, and protocol differ.

### 3.2 `ProviderRouter` — the agent-loop-facing facade

```rust
pub struct ProviderRouter {
    providers: HashMap<ProviderId, Arc<dyn Provider>>,
}

impl ProviderRouter {
    pub fn resolve(&self, model: &ModelRef) -> Result<Arc<dyn Provider>, ProviderError> { ... }

    /// Single entry point used by AgentLoop, GoalEngine, and per-category routing.
    pub async fn stream(
        &self,
        model: &ModelRef,
        req: CompletionRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        let provider = self.resolve(model)?;
        let route = provider.route_for(model);
        route.complete(req, cancel).await
    }
}
```

Goal engine, team-mode member workers, and the lead agent **all** call
`ProviderRouter::stream(...)`. Model tiering is just "pass a different
`ModelRef`".

### 3.3 Auth & config

```rust
// loaded from $XDG_CONFIG_HOME/yaca/config.toml + env override
[providers.anthropic]
api_key = "${env:ANTHROPIC_API_KEY}"

[providers.openai]
api_key = "${env:OPENAI_API_KEY}"

[providers.local]
kind         = "openai_compatible"
base_url     = "http://127.0.0.1:11434/v1"
api_key      = "ollama"
```

`${env:NAME}` resolved by a tiny pre-parse step (`fn expand_env(s: &str) ->
String`). Auth structs are per-provider (`AnthropicAuth { key: SecretString }`)
and never logged — `SecretString` is `secrecy::SecretString` with a
`Debug`-redacting impl. Keys live ONLY in `Provider` impls, never in
`yaca-core`.

---

## 4. `yaca-tool` — canonical schema + typed runtime wrapper

Two layers exactly as compat does it (see research §4), translated to Rust's
type system.

### 4.1 The canonical layer

```rust
// crates/yaca-tool/src/schema.rs
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: ToolName,
    pub description: String,
    pub input_schema: serde_json::Value,   // JSON Schema, generated via schemars
    pub output_schema: Option<serde_json::Value>,
}
```

Each built-in tool has typed input/output structs that derive `JsonSchema`:

```rust
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadInput {
    /// Absolute path to the file.
    pub path: Utf8PathBuf,
    pub offset: Option<u64>,
    pub limit: Option<u64>,
}
#[derive(Debug, Serialize, JsonSchema)]
pub struct ReadOutput { pub content: String, pub truncated: bool }
```

### 4.2 The typed runtime wrapper

```rust
// crates/yaca-tool/src/tool.rs
#[async_trait]
pub trait Tool: Send + Sync {
    fn schema(&self) -> &ToolSchema;
    /// Optional permission override (allows a tool to declare "I'm always read-only").
    fn permission(&self) -> Option<PermissionDescriptor> { None }
    async fn execute(&self, ctx: ToolCtx, input: serde_json::Value)
        -> Result<serde_json::Value, ToolError>;
}

pub struct ToolCtx {
    pub session: SessionId,
    pub message: MessageId,
    pub call:    ToolCallId,
    pub agent:   AgentName,
    pub permission: PermissionPlane,   // .ask(action, resource).await
    pub events:  EventSink,            // emit progress
    pub cancel:  CancellationToken,
    pub workdir: Utf8PathBuf,
}
```

Built-ins implement `Tool` directly. For example:

```rust
pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn schema(&self) -> &ToolSchema { &READ_SCHEMA }
    async fn execute(&self, ctx: ToolCtx, input: serde_json::Value)
        -> Result<serde_json::Value, ToolError>
    {
        let input: ReadInput = serde_json::from_value(input)
            .map_err(ToolError::input)?;
        ctx.permission.assert(Action::Read, Resource::Path(input.path.clone())).await?;
        let bytes = tokio::fs::read(&input.path).await?;
        let s = String::from_utf8_lossy(&bytes).into_owned();
        let output = ReadOutput { content: s, truncated: false };
        Ok(serde_json::to_value(output)?)
    }
}
```

### 4.3 Registry

```rust
pub struct ToolRegistry {
    by_name: HashMap<ToolName, Arc<dyn Tool>>,
}
impl ToolRegistry {
    pub fn builtins(/*deps*/) -> Self { ... }
    pub fn schemas(&self) -> Vec<ToolSchema> { ... }
    pub fn get(&self, name: &ToolName) -> Option<Arc<dyn Tool>> { ... }
}
```

The v0 built-in set is **deliberately small** (D1): `read`, `write`, `edit`,
`glob`, `grep`, `shell`, `team_*` (12 team tools), `goal_*` (set/status/clear),
nothing else. No MCP, no webfetch, no plugin loader in v0.

### 4.4 Permission plane

```rust
#[derive(Clone)]
pub struct PermissionPlane {
    rules: Arc<RwLock<PermissionRules>>,
    pending: Arc<Mutex<HashMap<PermissionRequestId, oneshot::Sender<PermissionDecision>>>>,
    events: EventSink,
}

#[derive(Clone, Debug)]
pub enum Action { Read, Edit, Glob, Grep, Bash, Task, ExternalDirectory }

#[derive(Clone, Debug)]
pub enum Resource {
    Path(Utf8PathBuf),
    Glob(String),
    Command(String),
    Subagent(AgentName),
    ExternalDir(Utf8PathBuf),
    Any,
}

#[derive(Clone, Debug)]
pub enum Mode { Allow, Ask, Deny }

impl PermissionPlane {
    pub async fn assert(&self, action: Action, resource: Resource) -> Result<(), ToolError> {
        match self.evaluate(&action, &resource) {
            Mode::Allow => Ok(()),
            Mode::Deny  => Err(ToolError::PermissionDenied { action, resource }),
            Mode::Ask   => self.ask(action, resource).await,
        }
    }
    async fn ask(&self, action: Action, resource: Resource) -> Result<(), ToolError> {
        let (tx, rx) = oneshot::channel();
        let id = PermissionRequestId::new();
        self.pending.lock().await.insert(id, tx);
        self.events.emit(Event::PermissionAsked { /*...*/ }).await;
        match rx.await? {
            PermissionDecision::AllowOnce | PermissionDecision::AllowAlways => Ok(()),
            PermissionDecision::Reject => Err(ToolError::PermissionDenied { action, resource }),
        }
    }
    pub fn resolve(&self, id: PermissionRequestId, dec: PermissionDecision) { ... }
}
```

Rules are merged from `config.toml` + per-session overrides (`last rule wins`,
glob keys). Members **inherit** parent rules with read/edit denied outside their
worktree.

---

## 5. `yaca-core` — sessions, agent loop, orchestrator, goal

### 5.1 SessionEngine

```rust
pub struct SessionEngine {
    store: SessionStore,                 // sqlx-backed event log + projection
    bus: EventBus,                       // tokio::sync::broadcast::Sender<Envelope>
    providers: Arc<ProviderRouter>,
    tools: Arc<ToolRegistry>,
    perms: PermissionPlane,
    agents: Arc<AgentRegistry>,
    cancel_supervisor: Arc<CancelSupervisor>,
}

impl SessionEngine {
    pub async fn create(&self, spec: CreateSession) -> Result<SessionId, CoreError>;
    pub async fn admit_user_prompt(&self, ses: SessionId, text: String) -> Result<MessageId, CoreError>;
    pub fn subscribe(&self, ses: SessionId) -> impl Stream<Item = Envelope>;
    pub async fn current_snapshot(&self, ses: SessionId) -> Result<SessionSnapshot, CoreError>;
    pub async fn resume(&self, ses: SessionId) -> Result<(), CoreError>;
    pub async fn cancel_turn(&self, ses: SessionId) -> Result<(), CoreError>;
}
```

`EventBus` is a `tokio::sync::broadcast::Sender<Envelope>` with capacity (e.g.
1024). The SSE endpoint subscribes per HTTP request; lagged consumers receive
`RecvError::Lagged(n)` and the SSE handler responds with a "resync from seq=N"
event so the TUI can refetch.

### 5.2 The agent turn loop

The single most important piece. Concrete control flow:

```rust
pub struct AgentLoop {
    providers: Arc<ProviderRouter>,
    tools:     Arc<ToolRegistry>,
    perms:     PermissionPlane,
    bus:       EventBus,
    store:     SessionStore,
}

impl AgentLoop {
    /// Run ONE assistant turn. May emit many events. Returns when the assistant
    /// reaches `finish=stop` (or `finish=tool_calls` after tool round-trip).
    pub async fn run_turn(
        &self,
        ses: &SessionView,         // projected current state
        agent: &AgentSpec,         // resolved agent (system prompt + model + tool allowlist)
        cancel: CancellationToken,
    ) -> Result<TurnOutcome, CoreError> {
        let mut step = 0u32;
        let message = MessageId::new();
        self.emit(Event::MessageStarted { session: ses.id, message, role: Role::Assistant }).await;

        loop {
            step += 1;
            self.emit(Event::StepStarted { session: ses.id, message, step }).await;

            let req = CompletionRequest {
                model: agent.model.clone(),
                system: Some(agent.system_prompt.clone()),
                messages: ses.history.clone(),
                tools: agent.allowed_tools.iter()
                    .filter_map(|n| self.tools.get(n).map(|t| t.schema().clone()))
                    .collect(),
                temperature: agent.temperature,
                max_output_tokens: agent.max_output_tokens,
                stop: vec![],
            };

            let mut stream = self.providers.stream(&agent.model, req, cancel.clone()).await?;
            let mut tool_calls: Vec<PendingToolCall> = vec![];
            let mut finish: Option<FinishReason> = None;

            while let Some(item) = stream.next().await {
                let evt = match item? {
                    ProviderEvent::Canonical(e) => e,
                    ProviderEvent::RawDelta(_) => continue,
                };
                // Persist + fan out via the bus + fold into tool-call accumulator.
                self.store.append_event(ses.id, &evt).await?;
                self.bus.send(envelope(evt.clone()));
                match &evt {
                    Event::ToolCallRequested { call, name, input, .. } => {
                        tool_calls.push(PendingToolCall { call: *call, name: name.clone(), input: input.clone() });
                    }
                    Event::MessageFinished { finish: f, .. } => { finish = Some(*f); }
                    _ => {}
                }
            }

            self.emit(Event::StepFinished { session: ses.id, message, step }).await;

            if tool_calls.is_empty() { break; }

            // Tool dispatch — fan out IN PARALLEL when the provider asks for multiple.
            let mut futs = FuturesUnordered::new();
            for tc in tool_calls {
                let tool = self.tools.get(&tc.name).ok_or(CoreError::UnknownTool(tc.name.clone()))?;
                let ctx = ToolCtx { /* ses, message, tc.call, agent, perms.clone(), bus_sink, cancel.clone(), workdir */ };
                futs.push(async move {
                    let started = Instant::now();
                    let result = tool.execute(ctx.clone(), tc.input.clone()).await;
                    (tc, started.elapsed(), result)
                });
            }
            while let Some((tc, dur, result)) = futs.next().await {
                let evt = match result {
                    Ok(out)  => Event::ToolResult { /* ... */ output: out, time_ms: dur.as_millis() as u64, .. },
                    Err(err) => Event::ToolError  { /* ... */ message_text: err.to_string(),                .. },
                };
                self.store.append_event(ses.id, &evt).await?;
                self.bus.send(envelope(evt));
            }
            // Loop again — provider gets a new turn with the tool results appended.
        }

        self.emit(Event::MessageFinished { session: ses.id, message, finish: finish.unwrap_or(FinishReason::Stop) }).await;
        Ok(TurnOutcome { message, finish: finish.unwrap_or(FinishReason::Stop) })
    }
}
```

Key properties:
- **Provider events are the source of truth** — projection happens in
  `SessionStore` on `append_event`.
- **Tool execution is parallel within a step** — `FuturesUnordered`.
- **Cancellation is propagated** — the `CancellationToken` reaches both the
  HTTP body of the provider call and each tool's `ToolCtx::cancel`.
- **No `unwrap`** — every `?` returns a typed `CoreError`.

### 5.3 Agent specs

```rust
pub struct AgentSpec {
    pub name: AgentName,                       // "build", "plan", "general", "explore"
    pub mode: AgentMode,                       // Primary | Subagent | All
    pub system_prompt: String,
    pub model: ModelRef,                       // default; orchestrator may override
    pub allowed_tools: Vec<ToolName>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub permission_overlay: PermissionRules,   // member overlay: deny outside worktree
}

pub enum AgentMode { Primary, Subagent, All }
```

Loaded from `agents/*.toml` files (project + user level), merged by file name
(project wins on overlap).

### 5.4 SessionStore (event-sourced + projected)

```rust
pub struct SessionStore { pool: sqlx::SqlitePool }

impl SessionStore {
    pub async fn append_event(&self, ses: SessionId, evt: &Event) -> Result<EventSeq, CoreError> {
        let mut tx = self.pool.begin().await?;
        let seq: i64 = sqlx::query_scalar(
            "INSERT INTO event_log(session_id, payload, ts)
             VALUES (?, ?, ?)
             RETURNING seq",
        )
        .bind(ses.0).bind(serde_json::to_string(evt)?).bind(now())
        .fetch_one(&mut *tx).await?;
        // Inline projection — fold evt into projection tables in same txn.
        self.project(&mut tx, ses, evt).await?;
        tx.commit().await?;
        Ok(EventSeq(seq as u64))
    }
}
```

Projection is **synchronous & in-transaction** so a reader after
`append_event(...)` always sees a consistent state. Backfilling/rebuild is
just `SELECT * FROM event_log ORDER BY seq` and re-running `project`.

SQLite schema (one migration file):

```sql
-- migrations/0001_init.sql
CREATE TABLE session (
    id           BLOB PRIMARY KEY,        -- uuid v7
    parent_id    BLOB,
    agent        TEXT NOT NULL,
    model        TEXT NOT NULL,
    workdir      TEXT NOT NULL,
    title        TEXT,
    permission   TEXT NOT NULL,           -- snapshot JSON
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    FOREIGN KEY (parent_id) REFERENCES session(id)
);
CREATE INDEX session_parent ON session(parent_id);

CREATE TABLE message (
    id           BLOB PRIMARY KEY,
    session_id   BLOB NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    role         TEXT NOT NULL,
    agent        TEXT,
    model        TEXT,
    finish       TEXT,
    cost_json    TEXT,
    tokens_json  TEXT,
    created_at   INTEGER NOT NULL
);
CREATE INDEX message_session ON message(session_id);

CREATE TABLE part (
    id           BLOB PRIMARY KEY,
    message_id   BLOB NOT NULL REFERENCES message(id) ON DELETE CASCADE,
    seq          INTEGER NOT NULL,
    kind         TEXT NOT NULL,           -- text|reasoning|tool
    body_json    TEXT NOT NULL,
    UNIQUE(message_id, seq)
);

CREATE TABLE event_log (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id   BLOB NOT NULL,
    payload      TEXT NOT NULL,           -- canonical Event JSON
    ts           INTEGER NOT NULL
);
CREATE INDEX event_log_session ON event_log(session_id);

-- team mode
CREATE TABLE team_run (
    id           BLOB PRIMARY KEY,
    lead_session BLOB NOT NULL REFERENCES session(id),
    spec_json    TEXT NOT NULL,
    state        TEXT NOT NULL,           -- active|shutting_down|deleted
    created_at   INTEGER NOT NULL
);
CREATE TABLE team_member (
    id           BLOB PRIMARY KEY,
    team_id      BLOB NOT NULL REFERENCES team_run(id) ON DELETE CASCADE,
    session_id   BLOB NOT NULL REFERENCES session(id),
    role         TEXT NOT NULL,
    state        TEXT NOT NULL,           -- spawning|active|wrapping_up|done|failed
    created_at   INTEGER NOT NULL
);
CREATE TABLE mail (
    id           BLOB PRIMARY KEY,
    team_id      BLOB NOT NULL REFERENCES team_run(id) ON DELETE CASCADE,
    from_ep      TEXT NOT NULL,
    to_ep        TEXT NOT NULL,
    body_json    TEXT NOT NULL,
    delivered_at INTEGER,
    acked_at     INTEGER,
    created_at   INTEGER NOT NULL
);
CREATE TABLE task_board (
    id           BLOB PRIMARY KEY,
    team_id      BLOB NOT NULL REFERENCES team_run(id) ON DELETE CASCADE,
    title        TEXT NOT NULL,
    body         TEXT NOT NULL,
    status       TEXT NOT NULL,           -- open|claimed|in_progress|completed|failed
    assignee     TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);

-- goal
CREATE TABLE goal (
    id           BLOB PRIMARY KEY,
    session_id   BLOB NOT NULL REFERENCES session(id),
    condition    TEXT NOT NULL,
    bound_json   TEXT,
    state        TEXT NOT NULL,           -- active|achieved|cleared|capped
    turns_evaluated INTEGER NOT NULL,
    last_reason  TEXT,
    started_at   INTEGER NOT NULL,
    cleared_at   INTEGER
);
```

---

## 6. Team-mode orchestration (the hard part)

This is where compat buys us nothing — compat's `task` tool is a
single-subagent dispatch. The **omo team-mode** model (lead + members + mailbox
+ task board) has no equivalent in compat. We rebuild it natively in Rust.

### 6.1 Core types

```rust
// crates/yaca-core/src/team/mod.rs
pub struct TeamOrchestrator {
    runs:     Arc<DashMap<TeamRunId, Arc<TeamRunHandle>>>,
    mailbox:  Arc<dyn MailboxBackend>,
    board:    Arc<dyn TaskBoardBackend>,
    cancel:   CancellationToken,
    engine:   Arc<SessionEngine>,
    workdirs: WorktreeManager,
    panes:    Option<TmuxPaneManager>,
}

pub struct TeamRunHandle {
    pub id:           TeamRunId,
    pub lead_session: SessionId,
    pub spec:         TeamSpec,
    pub members:      RwLock<HashMap<MemberId, MemberHandle>>,
    pub state:        RwLock<TeamRunState>,
    pub cancel:       CancellationToken,
}

pub struct MemberHandle {
    pub id:           MemberId,
    pub session:      SessionId,
    pub role:         MemberRole,
    pub join_handle:  JoinHandle<MemberOutcome>,
    pub cancel:       CancellationToken,
    pub state:        watch::Receiver<MemberState>,
    pub inbox_tx:     mpsc::Sender<MailEnvelope>,    // delivered messages
}

pub enum TeamRunState { Active, ShuttingDown, Deleted }
pub enum MemberState  { Spawning, Active, ClosureReady, Done, Failed }

pub struct TeamSpec {
    pub members: Vec<MemberSpec>,           // 1..=8
    pub shared_skills: Vec<SkillRef>,       // injected into each member's system prompt
}
pub struct MemberSpec {
    pub role:       MemberRole,             // freeform name
    pub agent:      AgentName,              // direct: explicit subagent
    pub category:   Option<CategoryName>,   // category-resolved alternative
    pub model_hint: Option<ModelRef>,
    pub worktree:   WorktreePolicy,         // None | NewBranch { base: String } | Shared
    pub tmux_pane:  bool,
}
```

### 6.2 Mailbox + TaskBoard behind a trait

The trait choice matters because the v0 lives in-process (in-memory) but D4 +
the omo research call out the future need for a file-backed/multi-process
backend. We make this swap **invisible** to the team code.

```rust
#[async_trait]
pub trait MailboxBackend: Send + Sync {
    async fn send(&self, team: TeamRunId, env: MailEnvelope) -> Result<MailMessageId, TeamError>;
    async fn poll(&self, team: TeamRunId, recipient: MailEndpoint, after: Option<MailMessageId>)
        -> Result<Vec<MailEnvelope>, TeamError>;
    async fn ack(&self, team: TeamRunId, ids: &[MailMessageId]) -> Result<(), TeamError>;
    async fn close(&self, team: TeamRunId) -> Result<(), TeamError>;
}

#[async_trait]
pub trait TaskBoardBackend: Send + Sync {
    async fn create(&self, team: TeamRunId, item: TaskItemNew)   -> Result<TaskItemId, TeamError>;
    async fn list(&self,   team: TeamRunId, filter: TaskFilter)  -> Result<Vec<TaskItem>, TeamError>;
    async fn get(&self,    team: TeamRunId, id: TaskItemId)      -> Result<TaskItem, TeamError>;
    async fn update(&self, team: TeamRunId, id: TaskItemId, upd: TaskItemUpdate)
                                                                  -> Result<TaskItem, TeamError>;
}
```

**v0 concrete impls**: `InMemoryMailbox` and `InMemoryTaskBoard`, built on
`tokio::sync::RwLock<HashMap<...>>` plus a `tokio::sync::broadcast` per team
for new-mail notifications. Notification fan-out lets members `await` next mail
without polling.

The **actor vs Arc<Mutex>** question for the mailbox: we pick **shared state
behind RwLock plus a broadcast channel for change notifications**, not a full
actor. Justification:
- Mailbox is conceptually a write-heavy log + read-heavy poll, both of which
  RwLock handles fine at our scale (≤8 members per team).
- An actor would force every operation through a single mpsc, which serializes
  reads needlessly and complicates testing.
- Atomicity per-operation is preserved by holding the write guard for the
  duration of a single op (insert or status flip), which is the only
  granularity the trait API exposes.

The `InMemoryMailbox` change-notification path uses `tokio::sync::broadcast`
because each team has at most one watcher per member, and broadcasting
"something changed, repoll" beats sending the actual envelope through N
channels.

### 6.3 Spawning a member

```rust
impl TeamOrchestrator {
    pub async fn create(&self, lead: SessionId, spec: TeamSpec) -> Result<TeamRunId, TeamError> {
        let run_id = TeamRunId::new();
        let cancel = self.cancel.child_token();
        let handle = Arc::new(TeamRunHandle { id: run_id, lead_session: lead, spec: spec.clone(),
            members: Default::default(), state: RwLock::new(TeamRunState::Active), cancel });
        self.runs.insert(run_id, handle.clone());

        // Spawn members in PARALLEL (D4 — token efficiency).
        let mut spawn_futs = FuturesUnordered::new();
        for m_spec in spec.members.iter().cloned() {
            let this = self.clone_ref();
            let handle = handle.clone();
            spawn_futs.push(async move { this.spawn_member(handle, m_spec).await });
        }
        let members: Vec<MemberHandle> = spawn_futs
            .map(|r| r.map_err(TeamError::SpawnFailed))
            .try_collect().await?;

        {
            let mut guard = handle.members.write().await;
            for m in members { guard.insert(m.id, m); }
        }
        self.engine.emit_team_created(run_id, lead, &handle.members.read().await).await;
        Ok(run_id)
    }

    async fn spawn_member(self: Arc<Self>, run: Arc<TeamRunHandle>, spec: MemberSpec)
        -> Result<MemberHandle, TeamError>
    {
        // 1. Resolve model (category routing or explicit hint).
        let model = self.resolve_model(&spec)?;
        // 2. Allocate worktree if requested.
        let workdir = self.workdirs.allocate(&run.id, &spec.role, &spec.worktree).await?;
        // 3. Optional tmux pane.
        let pane = if spec.tmux_pane { Some(self.panes.as_ref().unwrap().open(&run.id, &spec.role, &workdir).await?) } else { None };
        // 4. Create the child session under the lead.
        let child = self.engine.create(CreateSession {
            parent: Some(run.lead_session),
            agent: spec.agent.clone(),
            model: model.clone(),
            workdir: workdir.clone(),
            permission_overlay: member_overlay(&workdir),  // deny outside worktree
            kind: SessionKind::TeamMember { team: run.id },
        }).await?;
        // 5. Build the per-member inbox channel.
        let (inbox_tx, inbox_rx) = mpsc::channel(64);
        let (state_tx, state_rx) = watch::channel(MemberState::Spawning);
        let cancel = run.cancel.child_token();
        let member_id = MemberId::new();
        // 6. Spawn the supervised member task.
        let supervisor = MemberSupervisor {
            engine: self.engine.clone(),
            mailbox: self.mailbox.clone(),
            board:   self.board.clone(),
            run_id:  run.id,
            member_id,
            child_session: child,
            inbox_rx,
            state_tx,
            cancel: cancel.clone(),
            pane,
        };
        let join = tokio::spawn(async move {
            let outcome = supervisor.run().await;
            outcome
        });
        Ok(MemberHandle { id: member_id, session: child, role: spec.role, join_handle: join, cancel, state: state_rx, inbox_tx })
    }
}
```

### 6.4 Panic isolation & cancellation

Two failure modes to handle explicitly:

1. **Member task panics** — `tokio::spawn` returns `Err(JoinError::Panic(_))`.
   The supervisor that owns the `JoinHandle` is the lead-side
   `MemberHandle`. The orchestrator runs a small watchdog loop that
   `join_handle.await`s each member's exit:
   ```rust
   tokio::spawn(async move {
       match member.join_handle.await {
           Ok(MemberOutcome::Done)   => emit Done,
           Ok(MemberOutcome::Failed(e)) => emit Failed,
           Err(join_err) if join_err.is_panic() => {
               emit Failed("panicked");
               // panic is isolated — orchestrator and other members keep running.
           }
           Err(join_err) => emit Failed("cancelled"),
       }
   });
   ```
   No `catch_unwind` inside the supervisor; we rely on `tokio::spawn` panic
   capture and the watchdog.

2. **Lead cancellation / team delete** — `TeamRunHandle.cancel` is a parent
   `CancellationToken`. Every member supervisor's task takes a child token. On
   `team_delete(force=true)` we just call `run.cancel.cancel()`. Each member's
   in-flight provider call and tool execution observes the cancel (we pass it
   into `ProviderRouter::stream` and `Tool::execute`) and unwinds.

### 6.5 Result flow (message/task-oriented, not transcript)

Critical token-efficiency invariant from omo (research §3): the lead's context
window must NOT absorb the member's full transcript.

Concrete realization:
- A member's assistant text is persisted into its child session but **not
  copied into the lead session**.
- `team_send_message(from=member, to=lead|*)` writes one envelope into the
  mailbox.
- The lead's next turn includes a system-injected pseudo-message summarizing
  unread envelopes (titles + 200-char snippet + ids); the lead can fetch full
  bodies with `team_status` / explicit `team_task_get`.
- `team_status` is a tool the lead actively calls; it does not auto-inject.

This puts the lead in control of when it pulls full member output into its
context — exactly the omo design.

### 6.6 The 12 `team_*` tools

Each is a thin `Tool` impl over `TeamOrchestrator`. Signatures (input shape):

```
team_create              { spec: TeamSpec }                              -> { team: TeamRunId, members: Vec<MemberId> }
team_delete              { team: TeamRunId, force: bool }                -> ()
team_shutdown_request    { team: TeamRunId, target: MailEndpoint }       -> ()      [lead-only]
team_approve_shutdown    { team: TeamRunId, target: MailEndpoint }       -> ()
team_reject_shutdown     { team: TeamRunId, target: MailEndpoint, reason: String } -> ()
team_send_message        { team: TeamRunId, to: MailEndpoint, kind: MailKind, body: String, refs: Vec<TaskItemId> } -> { id: MailMessageId }
team_task_create         { team: TeamRunId, title, body, assignee?: MailEndpoint } -> { id: TaskItemId }
team_task_list           { team: TeamRunId, filter: TaskFilter }         -> Vec<TaskItemSummary>
team_task_update         { team: TeamRunId, id: TaskItemId, status?, assignee?, body? } -> TaskItem
team_task_get            { team: TeamRunId, id: TaskItemId }             -> TaskItem
team_status              { team: TeamRunId }                              -> TeamStatusSnapshot
team_list                {}                                               -> Vec<TeamRunSummary>
```

`MailKind` rejects `Shutdown*` — shutdown is its own typed tool path
(matches omo's "no shutdown smuggled through message kinds").

`MailEndpoint = Lead | Member(MemberId) | Broadcast` (and `Broadcast` is
lead-only at the tool layer).

### 6.7 Worktree + tmux managers

Both are thin shell-out wrappers; they own zero in-memory state beyond a
per-team registry of allocations.

```rust
pub struct WorktreeManager { root: Utf8PathBuf, runs: DashMap<TeamRunId, Vec<Utf8PathBuf>> }
impl WorktreeManager {
    pub async fn allocate(&self, team: &TeamRunId, role: &MemberRole, policy: &WorktreePolicy)
        -> Result<Utf8PathBuf, TeamError>
    {
        match policy {
            WorktreePolicy::None    => Ok(self.root.clone()),
            WorktreePolicy::Shared  => Ok(shared_dir(team)),
            WorktreePolicy::NewBranch { base } => {
                let dir = self.root.join(".yaca/worktrees").join(format!("{}-{}", team, role));
                let branch = format!("yaca/{}/{}", team, role);
                run_git(&["worktree", "add", "-b", &branch, dir.as_str(), base]).await?;
                self.runs.entry(*team).or_default().push(dir.clone());
                Ok(dir)
            }
        }
    }
    pub async fn release_all(&self, team: &TeamRunId) -> Result<(), TeamError> {
        if let Some((_, dirs)) = self.runs.remove(team) {
            for d in dirs { run_git(&["worktree", "remove", "--force", d.as_str()]).await.ok(); }
        }
        Ok(())
    }
}
```

```rust
pub struct TmuxPaneManager { session_name: String }
impl TmuxPaneManager {
    pub async fn open(&self, team: &TeamRunId, role: &MemberRole, cwd: &Utf8Path)
        -> Result<TmuxPane, TeamError>
    {
        // tmux new-window | split-window -h -c <cwd> -P -F "#{pane_id}"
        let out = Command::new("tmux").args(["split-window", "-h", "-c", cwd.as_str(),
            "-P", "-F", "#{pane_id}", "-t", &self.session_name]).output().await?;
        let pane_id = String::from_utf8(out.stdout)?.trim().to_string();
        // Pipe the member session's event stream into the pane via `tmux send-keys "yaca tail <ses>" Enter`.
        Command::new("tmux").args(["send-keys", "-t", &pane_id,
            &format!("yaca-cli tail-session {}", child_session_id), "Enter"]).status().await?;
        Ok(TmuxPane { id: pane_id })
    }
}
```

Note: tmux is **observability**, not control. The member's actual driver is
the tokio task in the server; the pane is a human-readable trail.

---

## 7. Goal engine

Wraps the agent loop. The goal engine is a thin supervisor that owns _no_
agent state of its own; it just decides "run another turn or stop?".

### 7.1 Types

```rust
pub struct GoalEngine {
    engine: Arc<SessionEngine>,
    providers: Arc<ProviderRouter>,
    store:  SessionStore,
}

pub struct GoalSpec {
    pub condition: String,                      // <= 4000 chars, validated on intake
    pub bound: Option<GoalBound>,               // user-stated; engine also enforces hard caps
    pub evaluator_model: ModelRef,              // cheap tier
}
pub enum GoalBound { Turns(u32), DurationSecs(u64) }

pub struct GoalState {
    pub id: GoalId,
    pub session: SessionId,
    pub spec: GoalSpec,
    pub turns: u32,
    pub started_at: OffsetDateTime,
    pub last_reason: Option<String>,
    pub status: GoalStatus,                     // Active | Achieved | Capped | Cleared
}
```

### 7.2 Hard caps (independent of user-stated bound)

```rust
pub struct GoalSafety {
    pub max_turns_default: u32,         // 50
    pub max_duration_default_secs: u64, // 1800
    pub max_token_default: u64,         // 2_000_000
}
```

These apply EVEN if the user did not specify a bound clause. The bound clause
is a hint to the evaluator; safety is a hard kill from the engine.

### 7.3 The loop

```rust
impl GoalEngine {
    pub async fn run(&self, ses: SessionId, spec: GoalSpec, cancel: CancellationToken)
        -> Result<GoalOutcome, CoreError>
    {
        let state = self.create_goal(ses, spec.clone()).await?;
        loop {
            if cancel.is_cancelled() { return Ok(GoalOutcome::Cleared); }
            self.check_safety(&state)?;

            // 1. Run one turn of the lead agent with the goal as the directive
            //    (or with the evaluator's last reason for guidance).
            let directive = self.compose_directive(&state);
            self.engine.admit_user_prompt(ses, directive).await?;
            let outcome = self.engine.run_turn_until_idle(ses, cancel.clone()).await?;

            // 2. Pull the transcript-since-last-eval (NOT the whole history;
            //    keep the evaluator window small + reproducible).
            let window = self.engine.transcript_window(ses, outcome.window).await?;

            // 3. Independent eval on a separate provider call. NO tools.
            let verdict = self.evaluate(&spec, &window, cancel.clone()).await?;

            self.engine.emit(Event::GoalEvaluated {
                session: ses, goal: state.id, met: verdict.met,
                reason: verdict.reason.clone(), turns: state.turns,
            }).await;

            if verdict.met {
                self.engine.emit(Event::GoalCleared { session: ses, goal: state.id, outcome: GoalOutcome::Achieved }).await;
                return Ok(GoalOutcome::Achieved);
            }
            self.update_last_reason(&state, verdict.reason).await?;
            // loop continues — next iteration will inject reason into the directive
        }
    }

    async fn evaluate(&self, spec: &GoalSpec, window: &TranscriptWindow, cancel: CancellationToken)
        -> Result<Verdict, CoreError>
    {
        let system = include_str!("goal_evaluator_system.md");  // strict: yes/no + reason; no tool-call
        let prompt = format!(
            "## CONDITION\n{}\n\n## TRANSCRIPT (most recent turn)\n{}\n\n\
             Reply with ONLY a single JSON object: {{\"met\": true|false, \"reason\": \"...\"}}.",
            spec.condition, window.render_plain_text());
        let req = CompletionRequest {
            model: spec.evaluator_model.clone(),
            system: Some(system.to_string()),
            messages: vec![Message::User { id: MessageId::new(), parts: vec![Part::Text { id: PartId::new(), text: prompt }], time: now() }],
            tools: vec![],                  // <-- INTENTIONAL: zero tools
            temperature: Some(0.0),
            max_output_tokens: Some(256),
            stop: vec![],
        };
        let mut stream = self.providers.stream(&spec.evaluator_model, req, cancel).await?;
        // Drain to a single text part and parse the JSON.
        let text = drain_text(&mut stream).await?;
        let verdict: Verdict = serde_json::from_str(text.trim())
            .map_err(|e| CoreError::GoalEvaluatorMalformed(e.to_string()))?;
        Ok(verdict)
    }
}
```

### 7.4 Goal + Team interaction (open question Q7 in PRD, decided here)

When a goal-driven turn delegates to a team, the lead must surface the team's
output INTO the lead transcript before the evaluator runs; otherwise the
evaluator (which only reads the transcript) cannot judge work the team did.

Concrete rule:
- After every team turn, the lead is forced to call `team_status`. The tool's
  output becomes a `Part::Tool { Completed }` in the lead's assistant message.
- That tool output is therefore in the transcript window the evaluator sees.
- If a goal turn requires running tests, the recommended pattern is:
  lead → `shell { cmd: "cargo test" }` (in the lead's transcript) — NOT a team
  member running it silently — so the evaluator has the exit code.

This is documented in the goal evaluator system prompt: "Judge ONLY from the
transcript. If you cannot see evidence the agent did the work, answer 'not
met'."

### 7.5 `goal_*` tools

```
goal_set       { condition: String, bound?: GoalBound, evaluator_model?: ModelRef } -> GoalId
goal_status    {}                                                                    -> GoalState
goal_clear     {}                                                                    -> ()
```

Non-interactive use: `yaca-cli -p "/goal cargo test exits 0"` runs `goal_set`
and blocks on the engine until `GoalOutcome::Achieved | Capped | Cleared`,
streaming events to stdout/stderr.

---

## 8. `yaca-server` — transport layer

Axum router. Five route groups; SSE for the streaming firehose.

```rust
pub fn router(state: AppState) -> axum::Router {
    Router::new()
        .nest("/sessions", session_routes())
        .nest("/messages", message_routes())
        .nest("/events",   event_routes())          // SSE
        .nest("/team",     team_routes())
        .nest("/goal",     goal_routes())
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())             // localhost-only by default
        .with_state(state)
}

// /events/:session  -- SSE firehose, replay-capable
async fn sse_handler(
    State(st): State<AppState>,
    Path(session): Path<SessionId>,
    Query(q): Query<EventsQuery>,                   // { since_seq?: u64 }
) -> Sse<impl Stream<Item = Result<sse::Event, Infallible>>> {
    let live = st.engine.subscribe(session);
    // Replay first, then live; live consumer handles broadcast lag with a resync event.
    let stream = async_stream::stream! {
        if let Some(since) = q.since_seq {
            for env in st.store.replay(session, since).await? {
                yield Ok(sse::Event::default().json_data(&env).unwrap());
            }
        }
        let mut live = live;
        loop {
            match live.recv().await {
                Ok(env) => yield Ok(sse::Event::default().json_data(&env).unwrap()),
                Err(RecvError::Lagged(n)) => {
                    yield Ok(sse::Event::default().event("resync").json_data(&LagInfo { dropped: n }).unwrap());
                }
                Err(RecvError::Closed) => break,
            }
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::new())
}
```

Bind to `127.0.0.1:<port>` by default with `port = 0` ("ask OS"); the actual
port is written to `$XDG_RUNTIME_DIR/yaca/port` so the TUI client can discover
it. No external network exposure unless the user passes `--bind 0.0.0.0:N`.

---

## 9. `yaca-tui` — ratatui client

State management: a single `AppState` updated from a background task that
consumes the SSE stream. The render loop is plain `crossterm` event polling.

```rust
pub struct AppState {
    pub session: Option<SessionView>,
    pub team:    Option<TeamView>,
    pub goal:    Option<GoalView>,
    pub command_palette: Palette,
    pub focus:   Focus,
    pub pending_perm: Option<PermissionRequest>,
}

pub async fn run(api: ApiClient) -> Result<()> {
    let mut term = init_terminal()?;
    let (tx, mut rx) = mpsc::channel::<UiUpdate>(256);
    tokio::spawn({
        let api = api.clone();
        async move {
            let mut sse = api.subscribe_events(None).await?;
            while let Some(env) = sse.next().await {
                let env = env?;
                tx.send(UiUpdate::Event(env)).await?;
            }
            Ok::<_, anyhow::Error>(())
        }
    });

    let mut state = AppState::default();
    loop {
        while let Ok(upd) = rx.try_recv() {
            state.apply(upd);
        }
        term.draw(|f| ui::draw(f, &state))?;
        if event::poll(Duration::from_millis(16))? {
            match event::read()? {
                Event::Key(k) => {
                    if let Some(cmd) = state.on_key(k) {
                        api.dispatch(cmd).await?;
                    }
                }
                _ => {}
            }
        }
    }
}
```

Render layout (three-pane): left = session/message tree; center = current
message stream with tool-call cards (Pending/Running/Completed/Error); right =
team panel (member statuses + last mail snippets + task board summary) and a
sticky GoalBar on top (condition + turns + last reason). Rendering is driven
**purely by the canonical `Event` stream** — no provider-specific paths in the
TUI.

---

## 10. Error handling

Every crate has a typed error. No `anyhow` in libraries (only binaries).

```rust
// yaca-provider
#[derive(thiserror::Error, Debug)]
pub enum ProviderError {
    #[error("http: {0}")] Http(#[from] reqwest::Error),
    #[error("sse parse: {0}")] Sse(String),
    #[error("encode: {0}")]   Encode(String),
    #[error("decode: {0}")]   Decode(String),
    #[error("auth missing for provider {0}")] Auth(ProviderId),
    #[error("cancelled")]     Cancelled,
}

// yaca-tool
#[derive(thiserror::Error, Debug)]
pub enum ToolError {
    #[error("input invalid: {0}")] InputInvalid(String),
    #[error("permission denied: {action:?} on {resource:?}")]
    PermissionDenied { action: Action, resource: Resource },
    #[error("io: {0}")]   Io(#[from] std::io::Error),
    #[error("json: {0}")] Json(#[from] serde_json::Error),
    #[error("cancelled")] Cancelled,
    #[error("other: {0}")] Other(String),
}

// yaca-core
#[derive(thiserror::Error, Debug)]
pub enum CoreError {
    #[error(transparent)] Provider(#[from] ProviderError),
    #[error(transparent)] Tool(#[from] ToolError),
    #[error(transparent)] Store(#[from] sqlx::Error),
    #[error("unknown agent: {0}")] UnknownAgent(AgentName),
    #[error("unknown tool: {0}")]  UnknownTool(ToolName),
    #[error("goal evaluator malformed: {0}")] GoalEvaluatorMalformed(String),
    #[error("team error: {0}")] Team(#[from] TeamError),
    #[error("cancelled")] Cancelled,
}
```

`Cancelled` is preserved end-to-end (provider → tool → loop → engine) so the
caller can distinguish "user hit ctrl-c" from "real error".

---

## 11. Test strategy

### 11.1 Unit tests

Per-crate. Heaviest in `yaca-provider` (protocol encode/decode against
recorded provider SSE fixtures) and `yaca-tool` (each tool's permission +
schema + happy path).

### 11.2 Integration: the **fake provider** is the keystone

`yaca-provider` ships `FakeProvider` (behind `cfg(any(test, feature =
"test-utils"))`) that replays a scripted sequence of canonical `Event`s. The
agent loop, goal engine, and team orchestrator are all driven against
`FakeProvider` in `tests/`:

```rust
// crates/yaca-core/tests/turn_loop.rs
#[tokio::test]
async fn tool_call_loop_round_trips() {
    let fake = FakeProvider::scripted(vec![
        scripted::text("I will read foo.rs"),
        scripted::tool_call("read", json!({"path": "/tmp/foo.rs"})),
        scripted::wait_for_tool_result(),
        scripted::text("File has 42 lines."),
        scripted::finish(FinishReason::Stop),
    ]);
    let engine = test_engine_with(fake).await;
    let ses = engine.create(simple_build_session()).await?;
    engine.admit_user_prompt(ses, "tell me about /tmp/foo.rs".into()).await?;
    let events = engine.collect_events(ses).await;
    assert_text_contains(&events, "File has 42 lines.");
    assert_tool_completed(&events, "read");
}
```

### 11.3 Goal engine deterministic tests

The evaluator is also `FakeProvider`-driven: script `met=false` 3x then
`met=true` once and assert the agent loop ran exactly 4 turns and emitted the
right `GoalEvaluated`/`GoalCleared` events.

### 11.4 Team orchestration tests

Same approach; `FakeProvider` per member returns a scripted "I claim task X /
I post message Y to lead / I shut down" trajectory. Assert mailbox + task
board converge to the expected final state, including the shutdown-request →
approve → delete sequence.

### 11.5 The HTTP layer

`axum::extract::connect_info` plus `tower::ServiceExt::oneshot` lets us drive
the router with synthetic requests; for SSE we use `eventsource-stream` on the
client side against an in-memory hyper.

---

## 12. Configuration

```toml
# $XDG_CONFIG_HOME/yaca/config.toml
[server]
bind = "127.0.0.1:0"                   # 0 -> os-assigned

[providers.anthropic]
api_key = "${env:ANTHROPIC_API_KEY}"
[providers.openai]
api_key = "${env:OPENAI_API_KEY}"
[providers.local]
kind     = "openai_compatible"
base_url = "http://127.0.0.1:11434/v1"
api_key  = "ollama"

[agents.build]      # primary
system_prompt_file = "agents/build.md"
model              = "anthropic:claude-sonnet-4"
allowed_tools      = ["read","write","edit","glob","grep","shell","team_*","goal_*"]

[agents.general]    # subagent
system_prompt_file = "agents/general.md"
model              = "openai:gpt-5-mini"
allowed_tools      = ["read","write","edit","glob","grep","shell"]

[categories]                              # team-mode category -> model
ultrabrain = "anthropic:claude-opus-4"
deep       = "anthropic:claude-sonnet-4"
quick      = "openai:gpt-5-mini"
writing    = "openai:gpt-5"

[goal]
evaluator_model        = "openai:gpt-5-nano"
max_turns_default      = 50
max_duration_secs      = 1800
max_tokens             = 2_000_000

[permissions]
read   = "allow"
edit   = "ask"
glob   = "allow"
grep   = "allow"
bash   = "ask"
task   = "ask"
external_directory = "deny"
```

---

## 13. Implementation roadmap

Phases are sized so each is **shippable on its own** (cargo build + clippy +
test pass at every phase boundary). The validation command for every phase
ends in `cargo clippy --workspace --all-targets -- -D warnings && cargo test
--workspace --all-features`.

### Phase 0 — workspace bootstrap (≈ 0.5 day)
**Deliverables**
- `cargo new --lib` for each crate; workspace `Cargo.toml` with pinned deps.
- `rustfmt.toml`, `clippy.toml` (deny `unwrap_used`, `expect_used` in libs).
- `xtask` skeleton for `cargo xtask migrate`.
- CI: `cargo build --workspace`, `cargo clippy ... -D warnings`, `cargo test`.
**Validate**: `cargo build --workspace`.
**Rollback point**: tagged commit `phase0`.

### Phase 1 — `yaca-proto` types + `yaca-store` migrations (≈ 1.5 days)
**Deliverables**
- All ids, `Message`, `Part`, `Event`, `ToolPartState`, `ModelRef`, `Envelope`.
- Migrations 0001 (schema above), 0002 (PRAGMAs).
- `SessionStore::append_event` + `replay` + `project` + property tests.
**Validate**: `cargo test -p yaca-store -- --include-ignored` (runs migrations against tempdir SQLite).
**Rollback**: tag `phase1`. Deleting `yaca-store` and `yaca-proto` reverts cleanly.

### Phase 2 — `yaca-provider` skeleton + Fake + OpenAI Chat (≈ 2 days)
**Deliverables**
- `Provider`/`Protocol`/`Route` traits.
- `FakeProvider` (scripted Events; the keystone for all future tests).
- `OpenAIChatRoute` + `OpenAIChatProtocol` (encode + SSE decode → canonical Events).
- `ProviderRouter`.
**Validate**: `cargo test -p yaca-provider` (fake roundtrip + recorded OpenAI fixtures).
**Rollback**: tag `phase2`. `OpenAIChat` is isolated; reverting only removes that provider.

### Phase 3 — `yaca-tool` minimal set + permissions (≈ 2 days)
**Deliverables**
- `Tool` trait, `ToolCtx`, `ToolRegistry`.
- Built-ins: `read`, `write`, `edit`, `glob`, `grep`, `shell`.
- `PermissionPlane` with rules + ask-pending channel.
**Validate**: `cargo test -p yaca-tool` (each tool happy + permission denied + cancelled).
**Rollback**: tag `phase3`.

### Phase 4 — `yaca-core` SessionEngine + AgentLoop (single agent) (≈ 3 days)
**Deliverables**
- `SessionEngine::{create, admit_user_prompt, subscribe, cancel_turn}`.
- `AgentLoop::run_turn` with parallel tool dispatch.
- `EventBus` (broadcast) + projection via `SessionStore`.
**Validate**:
- `cargo test -p yaca-core --test turn_loop` (the tool-call round-trip above).
- Manual: drive end-to-end against `FakeProvider` with a small binary in `xtask`.
**Rollback**: tag `phase4`. From here on, server/TUI revertable independently.

### Phase 5 — `yaca-server` HTTP + SSE + first wire-through (≈ 2 days)
**Deliverables**
- Axum router with `/sessions`, `/messages`, `/events`.
- SSE handler with replay + lag-resync.
- Port discovery file under `$XDG_RUNTIME_DIR/yaca`.
**Validate**:
- Integration test: `tower::ServiceExt::oneshot` create-session + admit-prompt + scrape SSE; assert events.
- Manual: `yaca-cli serve --bind 127.0.0.1:0` + `curl /events/{ses}`.
**Rollback**: tag `phase5`.

### Phase 6 — Anthropic + OpenAI-compatible providers (≈ 2 days)
**Deliverables**
- `AnthropicMessagesRoute` + protocol (encode + SSE decode incl. tool_use).
- `OpenAICompatibleRoute` (reuses `OpenAIChatProtocol`).
- Recorded SSE fixtures + golden-file tests for each.
**Validate**: `cargo test -p yaca-provider` covers all 3 providers including a parametrized "same canonical event sequence across providers" test (drives the round-trip).
**Rollback**: tag `phase6`. Per-provider isolation means dropping any is local.

### Phase 7 — `yaca-tui` ratatui client (≈ 3 days)
**Deliverables**
- `AppState` + apply(`UiUpdate`).
- Three-pane layout, message rendering with streamed text + tool cards.
- Permission ask modal (the pending-request channel surfaces here).
- Keybindings: send/cancel/exit/scroll/focus.
**Validate**:
- TUI snapshot test using `ratatui::backend::TestBackend`.
- Manual QA: drive end-to-end with `FakeProvider` (deterministic + reproducible).
**Rollback**: tag `phase7`. TUI is the only crate touching ratatui/crossterm.

### Phase 8 — Goal engine (≈ 2 days)
**Deliverables**
- `GoalEngine::run` + `evaluate` + safety caps + composing directives.
- `goal_*` tools.
- Non-interactive runner in `yaca-cli`.
- Goal evaluator system prompt file with the "no tools, judge transcript only" contract.
**Validate**:
- `cargo test -p yaca-core --test goal_loop`: scripted Fake evaluator, asserts exact turn count and final outcome on (`met=false`×3 → `met=true`).
- Manual: `yaca-cli -p "/goal cargo test exits 0"` against a tiny fixture crate.
**Rollback**: tag `phase8`. The goal engine has zero callers outside `goal_*` tools and CLI; can be feature-gated off.

### Phase 9 — Team orchestrator: in-memory mailbox + task board (≈ 3 days)
**Deliverables**
- `MailboxBackend`/`TaskBoardBackend` traits + `InMemoryMailbox`/`InMemoryTaskBoard`.
- `TeamOrchestrator::create/spawn_member/route/shutdown_request/approve/delete`.
- All 12 `team_*` tools.
- Member supervisor with panic isolation + cancellation propagation.
- Watchdog over each member's `JoinHandle`.
**Validate**:
- `cargo test -p yaca-core --test team_orchestration`: 3-member scripted run with task claim, mail roundtrip, graceful shutdown, force shutdown.
- Lead transcript NEVER contains member assistant text (assert).
**Rollback**: tag `phase9`. Team feature is gated behind `team` crate feature.

### Phase 10 — Worktrees + tmux panes (≈ 1.5 days)
**Deliverables**
- `WorktreeManager` (shell-out to `git worktree`).
- `TmuxPaneManager` (shell-out to `tmux split-window` + `send-keys`).
- Cleanup on `team_delete` (also on force).
**Validate**:
- `cargo test -p yaca-core --test worktree_lifecycle -- --ignored` (requires git + tmux in PATH).
- Manual: spin a 2-member team in a real repo, observe two worktrees + two tmux panes; `team_delete` cleans both.
**Rollback**: tag `phase10`. Worktree/tmux features gated; system still works without them.

### Phase 11 — Categories + skill injection + multi-provider routing in team mode (≈ 1.5 days)
**Deliverables**
- `categories` config block parsing.
- `resolveCategoryExecution`-equivalent: category → model + fallback chain + prompt append.
- Skill injection: load named skill content; append to member system prompt.
**Validate**:
- `cargo test -p yaca-core --test category_routing`: 4 members with 4 categories → 4 distinct provider/model calls observed via `FakeProvider`.
**Rollback**: tag `phase11`.

### Phase 12 — End-to-end manual QA + polish (≈ 1 day)
**Deliverables**
- `yaca-cli` umbrella: `yaca` (interactive TUI), `yaca serve`, `yaca -p "..."`, `yaca tail-session <id>`.
- README + sample agent + sample category config.
**Validate**:
- Manual: start `yaca`, set a goal that requires a 3-member team, watch goal achieve.
- `cargo test --workspace --all-features` clean.

### Total estimate
~22–25 engineering days at a single-developer pace, single-track. Phases 0–4
are non-negotiable critical path; 8 & 9 are the two D4 deliverables and can be
parallelized after Phase 7 if two developers are available.

---

## 14. Risk register & decisions worth re-validating

| # | Risk | Decision / mitigation |
|---|------|-----------------------|
| 1 | SQLite write contention with N members appending events | Use a single connection pool with `journal_mode=WAL`; each member append is one txn; pool size = N+2. |
| 2 | broadcast channel lag drops events | SSE handler emits `resync` event with last seen seq; TUI refetches via `/events?since_seq=N`. |
| 3 | dyn-trait async fn ergonomics | `async-trait` for `dyn Provider/Tool/MailboxBackend`; native `async fn in Trait` everywhere else. |
| 4 | `git2` cross-compile + libgit2 size | Shell out to the `git` binary; document `git >= 2.30` requirement. |
| 5 | tmux not installed | `tmux_pane: bool` in `MemberSpec` defaults to false; pane mgr returns `Err(NotAvailable)` which surfaces as a non-fatal warning. |
| 6 | Provider tool-call format drift | All providers normalize via `Protocol::decode` → canonical `Event`; agent loop sees only canonical events; fixture-based protocol tests catch drift. |
| 7 | Goal evaluator answers garbage JSON | Strict parse; on malformed, emit `GoalEvaluatorMalformed` and treat as `met=false` (do not infinite-loop on malformed eval — count toward `max_turns_default`). |
| 8 | Team member context bleed into lead | Asserted in `team_orchestration` test: lead history must not contain member-only message ids; CI fails if it does. |
| 9 | Panic in tool execution crashes the runtime | `tokio::spawn` captures; supervisor turns into `ToolError::Other("panic: ...")`; loop continues. |
| 10 | Permission ask hangs the agent forever | `PermissionPlane::ask` honors cancellation; TUI shows a modal; non-interactive mode auto-denies after a configurable timeout. |

---

## 15. What deliberately is NOT in v0 (D1 reminder)

- No LSP. No MCP. No webfetch. No plugin loader. No theming engine beyond a
  basic palette. No web docs site. No desktop app. No multi-user. No billing.
- No file-backed mailbox/task board — but the trait is there for v1.
- No remote SDK — but the HTTP+SSE server is there so wiring an SDK is later
  packaging, not new code.
- No fancy summary/compaction policies — basic per-turn summarization in
  `Message::Compaction` is the v0 surface.

The slice we ARE shipping is deep (full team-mode lifecycle, full goal
lifecycle, multi-provider, client/server) precisely so v1 can layer plugins,
MCP, LSP, and a polished UI on a stable substrate without rewriting the loop.
