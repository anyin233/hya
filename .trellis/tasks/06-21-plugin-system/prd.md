# Plugin system for hya

## Goal

Give hya a dynamic plugin/hook system so users can extend and intercept the
Rust harness at well-defined lifecycle stages without forking the core. Two
delivery vectors are explicitly requested:

1. **OpenCode plugin compatibility** — run existing OpenCode (JS/TS) plugins
   against hya's engine, reusing OpenCode's hook surface as much as is feasible.
2. **Rust-native out-of-process plugin interface** — let a plugin ship as its own
   binary that talks to hya over IPC/RPC, so plugin authors get a flexible,
   language-agnostic, crash-isolated extension path.

> This task deliberately revisits a decision deferred in
> [pi-parity](file:///chivier-disk/yanweiye/Projects/hya/.trellis/tasks/06-20-hya-pi-parity/prd.md#L49),
> which marked a "TS plugin runtime" as out of scope ("Rust has no TS plugin
> host"). The plugin host is now in scope.

## User value

- Extend hya (new tools, observers, policy, custom providers) without patching
  core crates or waiting on upstream.
- Reuse the existing OpenCode plugin ecosystem instead of rewriting plugins.
- Author plugins in any language via a stable IPC contract, isolated from the
  harness process.

## Confirmed facts — hya current architecture (verified via exploration)

Sources: explore bg_5b66f8af (turn-loop hook surface), bg_3ae8ce64 (tool/provider/
config extension points), README, pi-parity design/FOLLOWUPS.

### Engine & event surface (`hya-core`)
- The turn loop is `SessionEngine::run_turn`
  ([engine.rs:251](file:///chivier-disk/yanweiye/Projects/hya/crates/hya-core/src/engine.rs#L251)).
  Phases per assistant turn: assistant `MessageStarted` → per-round
  (cancel-check → read projection → optional compaction → build provider request
  with tool schemas → open provider stream → decode stream/accumulate tool calls
  & finish → if no tool calls: append final `MessageFinished`; else: execute each
  tool, append `ToolResult`/`ToolError`, loop) → round/limit handling.
- **Single emit funnel**: every persisted event passes through
  `SessionEngine::emit`
  ([engine.rs:169](file:///chivier-disk/yanweiye/Projects/hya/crates/hya-core/src/engine.rs#L169))
  → appends to store, then publishes an `Envelope` on the bus. This is the single
  strongest global **observation** hook point.
- **Event bus** = `tokio::sync::broadcast::Sender<Envelope>`
  ([bus.rs](file:///chivier-disk/yanweiye/Projects/hya/crates/hya-core/src/bus.rs)).
  `Envelope { seq, ts_millis, event }`; `Event` kinds in
  [event.rs:15-130](file:///chivier-disk/yanweiye/Projects/hya/crates/hya-proto/src/event.rs#L15)
  (Session/Message/Text/Reasoning/Tool/Error lifecycles).
- `StepStarted`/`StepFinished` exist in proto but are **not emitted** today — a
  ready slot for explicit per-round boundaries if hooks need them.
- Other injectable trait seams already present: `Summarizer` (compaction),
  `GoalEvaluator`, `LoopVerifier`, `LoopPlanner`, `InteractionPlane`,
  `SpawnerPlane` — installed via `with_*` builders before engine construction.

### Tools (`hya-tool`)
- `trait Tool { name; schema() -> ToolSchema; async execute(&ToolCtx, Value) -> Result<Value, ToolError> }`
  ([tool.rs:55](file:///chivier-disk/yanweiye/Projects/hya/crates/hya-tool/src/tool.rs#L55)).
  **I/O is already `serde_json::Value`** → IPC-friendly with no reshaping.
- `ToolCtx { permission, interaction, spawner, parent_session, workdir, cancel }`.
- `ToolRegistry { tools: HashMap<String, Arc<dyn Tool>> }` is **already
  dynamically extensible**: besides `builtins()`, `get()`, `schemas()` it exposes
  **`register(Arc<dyn Tool>) -> Result<(), DuplicateName>`**
  ([tool.rs:94](file:///chivier-disk/yanweiye/Projects/hya/crates/hya-tool/src/tool.rs#L94)),
  already used by MCP bootstrap in `main.rs`. So Child B needs **no new registry
  primitive** — at most a convenience `extend(...)` wrapper; it reuses `register`
  with its existing first-wins `DuplicateName` semantics. Builtins: read, write,
  edit, ls, glob, find, grep, shell, ask_user, task.
- Tool execution loop has **no middleware**
  ([engine.rs:336-370](file:///chivier-disk/yanweiye/Projects/hya/crates/hya-core/src/engine.rs#L336));
  interception requires either wrapper tools in the registry or a new
  engine-level before/after hook.

### Permission plane (`hya-tool`)
- `Action { Read, Edit, Glob, Grep, Bash, Task, ExternalDirectory }`,
  `Resource { Path, Glob, Command, Subagent, Any }`,
  `Decision { AllowOnce, AllowAlways, Reject{feedback} }`.
- Enforced inside each tool via `ctx.permission.assert(...)`; on `Ask` it sends an
  `AskRequest { action, resource, reply: oneshot<Decision> }` over a channel to a
  responder (TUI prompt / headless policy). Natural **interception** seam.
- `AllowAlways` is action-wide (not resource-scoped) today.

### Providers (`hya-provider`)
- `trait Provider { id; capabilities(model); async stream(req, session, message) -> EventStream }`;
  response is a stream of canonical `Event`s, not text.
- `ProviderRouter { providers: Vec<Arc<dyn Provider>> }`, dynamic via `.with()`,
  **first-match** by `capabilities(model)` → wrapper/interceptor providers are
  already feasible by registering before the real one. `Protocol`/`Decoder` traits
  are a lower seam.

### Config & startup (`hya-cli`)
- Config: `~/.config/hya/config.yaml` (XDG-aware), parsed by `serde_norway` into
  `FileConfig { default_model, providers: BTreeMap<…> }`
  ([config.rs](file:///chivier-disk/yanweiye/Projects/hya/crates/hya-cli/src/config.rs)).
  **No `plugins:` section exists yet.** `config::load()` currently returns a built
  `ProviderRouter`, discarding raw declarations.
- Engine is wired in `hya-cli` (`build_session_engine`, `resolve_router`), per
  command (exec/rpc/goal/tui/serve). A plugin manager would initialize here,
  before the registry/router are frozen into `Arc`. **`hya-core` stays
  UI-agnostic**; hook insertion points live in core, plugin loading lives in CLI.
- **Existing precedents**: `hya rpc` already does stdin/stdout JSONL of
  serialized `Envelope`s (a working IPC substrate); `skills.rs` already does a
  **directory scan** for passive markdown skills.

### IPC-ready serialization boundaries
- `hya-proto` types (`Event`, `Envelope`, `Message`/`Part`, `ToolSchema`,
  `ModelRef`, projection) are all serde-serializable; reused by the HTTP/SSE
  server, the typed client, the SQLite event log, and `hya rpc`. These are the
  candidate wire types for an out-of-process plugin protocol.

## OpenCode plugin contract (verified — librarian bg_8431cee9)

Source: `sst/opencode` @ `5606d2b…`, pkg `@opencode-ai/plugin` v1.17.9.

- **Authoring**: a plugin is a JS/TS **ESM module**. Server contract is
  `Plugin(input, options?) => Promise<Hooks>`; init `input` carries a live SDK
  `client`, a Bun shell `$`, project/directory/worktree context.
- **Runtime = Bun, in-process**: plugins execute in OpenCode's JS runtime and call
  back into OpenCode via the **SDK client over HTTP**. npm plugins are
  `bun install`-ed at startup; local plugins may carry their own `package.json`.
- **Loading (two paths)**: (a) auto-scan dirs `.opencode/plugins/` and
  `~/.config/opencode/plugins/`; (b) npm/specifier entries in config
  `plugin: Array<string | [string, options]>`. Hooks run **in sequence** by load
  order.
- **Hook surface** (`Hooks`): each hook is `(input, output) => Promise<void>` and
  the plugin **mutates `output` in place** to change behavior; `tool.execute.before`
  may **throw to block**. Keys: `dispose`, `event` (observe SDK event union),
  `config`, `tool` (register new tools), `auth`, `provider`, `chat.message`,
  `chat.params`, `chat.headers`, `permission.ask` (set ask/deny/allow),
  `command.execute.before`, `tool.execute.before` (mutate args / block),
  `shell.env`, `tool.execute.after` (rewrite result), `tool.definition` (rewrite
  schema), plus `experimental.*` (messages/system transform, compaction, small
  model, text complete).
- **Tools vs hooks**: `tool: { name: ToolDefinition }` registers new tools (a
  same-named tool overrides builtin); other keys intercept existing behavior.
- **Caveats**: docs/loader drift — legacy bare plugin-function exports vs newer
  target-specific module shapes (`{ server }` / `{ tui }`); ecosystem breakage
  precedent (issue #20139). A separate **TUI** plugin surface
  (`@opencode-ai/plugin/tui`) exists — out of scope for an agent-runtime v1.
- **MCP** is a parallel, language-agnostic extension mechanism (external tool
  servers) distinct from in-process plugins.

### Implication for hya compatibility
Direct compat requires a **JS runtime**. Two host strategies: (a) **spawn Bun**
and load plugin modules there (faithful, heavy dep), or (b) **embed a JS engine**
(e.g. deno_core/boa) and shim Bun's `$`/install/resolution (lighter dep, partial
fidelity). Either way, the JS host needs an SDK-shaped client back into hya —
which hya's **`hya serve` HTTP+SSE + typed client already provides**. OpenCode's
own host↔plugin model therefore maps onto an **out-of-process** design that the
Rust-native plugin path can share.

### Convergence insight (drives sequencing)
Both requested vectors reduce to **one out-of-process hook-dispatch protocol**:
- hya-core emits named lifecycle hooks at existing boundaries (emit funnel, tool
  execute, permission ask, provider stream, chat params).
- A host process subscribes/responds over IPC (extend the `hya rpc` JSONL +
  proto substrate).
- Rust plugins speak the protocol directly; OpenCode-compat is a **bundled Bun
  adapter** that loads JS plugins and maps OpenCode `Hooks` ⇄ the protocol, with
  the SDK client pointed at hya's server.

## Resolved decisions

- **D1 — Scope & sequencing (CONFIRMED)**: Parent task with independently
  verifiable children. Sequence: (1) shared **hook-dispatch core** in `hya-core`
  (named lifecycle hooks at existing boundaries) → (2) **Rust-native
  out-of-process plugin** path over an extended `hya rpc`/proto JSONL+RPC
  protocol (MVP) → (3) **OpenCode-compat Bun adapter** on top of the same
  protocol (later child). Protocol is designed Rust-first; both vectors ship.

- **D2 — Hook power & execution model (CONFIRMED)**: Full interception. Hooks are
  **blocking request/response**: engine sends `(input, output)` to the plugin
  host, awaits a reply within a per-hook timeout, then applies the plugin's
  payload **mutations** and/or a **veto** (block tool / deny permission). Mirrors
  OpenCode's in-place output mutation + throw-to-block. Observation is the subset
  that ignores the response. Protocol is therefore **two-way request/response**,
  and requires timeout + failure-isolation machinery (resolved in D7).
  Performance note to carry into design: high-frequency observation (e.g. token
  deltas) should be async/coalesced; only low-frequency interception stages
  (tool.before/after, permission.ask, chat.params) block the turn.

- **D4 — IPC transport & framing (CONFIRMED)**: **JSONL over stdio** with a
  **JSON-RPC-style correlated envelope** — requests (`id`+`method`+`params`),
  replies (`id`+`result`/`error`), and one-way notification frames for async
  observation events. Each plugin is a **child process** hya spawns; the Bun
  OpenCode-adapter is just one such child. Extends the existing `hya rpc` JSONL +
  serde proto substrate; no heavy deps; language-agnostic. Token-level
  observation stays async/coalesced; blocking interception stays low-frequency.

- **D5 — Plugin capabilities (CONFIRMED)**: MVP = **(a) interception hooks** +
  **(b) plugin-registered tools**. A plugin declares model-callable tools (schema
  advertised to the model); on a call, hya dispatches over IPC and awaits the
  plugin's JSON result. This reuses the **already-existing**
  `ToolRegistry::register` (the registry is not static) + a plugin-tool adapter
  implementing `trait Tool` that proxies to the child process.
  Permission answering is covered by the `permission.ask` hook. **Out-of-process
  provider plugins are deferred** to a later child (high-frequency Event streaming
  + existing in-code wrapper path make them a poor MVP fit).

- **D2-detail — v1 hook surface (CONFIRMED)**: Core interception set + goal/loop
  hooks. v1 hooks, each on an existing hya boundary:
  - `event` — async observe of the `Envelope` stream (the `emit` funnel).
  - `message.user.before` — mutate user prompt at `admit_user_prompt`.
  - `chat.params` — mutate model/temperature/system before `request_from_messages`
    (covers OpenCode `chat.params` + `system.transform`).
  - `tool.execute.before` — mutate args / **veto**, in the engine tool loop.
  - `tool.execute.after` — rewrite result, in the engine tool loop.
  - `permission.ask` — answer allow/ask/deny via the permission plane.
  - session/message lifecycle — observation.
  - **goal/loop hooks** (hya differentiator): `goal.evaluate`,
    `loop.verifier`/`loop.planner` decision points.
  - plugin-tool dispatch (D5).
  - Deferred to later children: provider SSE frames, compaction, explicit
    `Step*` boundaries, the full `experimental.*` set.

## Resolved decisions (continued)

- **D6 — Loading & declaration (CONFIRMED)**: **config.yaml `plugins:` (primary)
  + native hya dir-scan + adapter.** Explicit `plugins:` entries (spawn
  command/path + `kind: rust|opencode` + options + `enabled`) are primary. hya
  **also auto-scans** `~/.config/hya/plugins/` and `.hya/plugins/` for native
  plugins (mirrors the `skills` precedent). OpenCode's own `.opencode/plugins/`
  dir-scan + npm install happen **inside** the enabled Bun `opencode` adapter, not
  in hya core. Trust mitigation (dir-scan widens the surface): a scanned plugin
  must carry a **manifest** (`plugin.toml`: id, kind, declared hooks, posture) and
  gets the same isolation + per-hook posture as config plugins; "you own these
  dirs" is the documented trust boundary. (A first-run trust prompt is a possible
  later addition.)

- **D7 — Failure posture & isolation (CONFIRMED)**: **Per-hook posture.** Guard
  hooks (`permission.ask`, `tool.execute.before` veto) **fail safe** (timeout/crash
  → fall back to hya's normal permission decision; do not apply unverified
  mutations; hard guard failure blocks rather than silently allows). Enrichment
  hooks (`chat.params`, `message.user.before`, `tool.execute.after`, `event`,
  goal/loop) **fail open** (proceed with original payload, log warning). Each
  plugin is an **isolated child process** with a **per-hook timeout** and
  **crash/restart** handling. **No OS sandbox in v1** (plugins are explicitly
  declared/trusted, like providers); seccomp/landlock/container is a later child.
  Implies a per-hook policy field (`posture: safe|open`) in the registration.

- **D8 — Crate layout**: technical; to be decided in `design.md` (likely a new
  `hya-plugin` crate for the host/manager + protocol types, hook-dispatch trait
  seams added in `hya-core`, loader/bootstrap in `hya-cli`).

## Requirements

Parent-level requirements (cross-child). Each child owns the subset it delivers.

- **R1 — Hook-dispatch core.** `hya-core` exposes named lifecycle hook points at
  existing boundaries (emit funnel, `admit_user_prompt`, `request_from_messages`,
  the tool loop, the permission plane, goal/loop gates) behind a dispatcher trait,
  so the engine stays UI/host-agnostic and hooks are no-ops when no host attached.
- **R2 — Out-of-process plugin host.** A host/manager spawns each plugin as a
  child process and speaks the **JSONL-over-stdio, JSON-RPC-style** protocol
  (requests/replies + one-way notifications). Handles handshake/registration,
  per-hook timeouts, crash detection + restart, and graceful shutdown.
- **R3 — Blocking interception.** For interception hooks the engine sends
  `(input, output)`, awaits the reply within the per-hook timeout, and applies
  payload **mutations** and/or a **veto**. Observation (`event`) is async/coalesced
  and never blocks the turn.
- **R4 — v1 hook set.** Implement: `event`, `message.user.before`, `chat.params`,
  `tool.execute.before` (mutate/veto), `tool.execute.after`, `permission.ask`,
  session/message observation, and goal/loop (`goal.evaluate`,
  `loop.verifier`/`loop.planner`) hooks.
- **R5 — Plugin-registered tools.** Reusing the **already-dynamic** `ToolRegistry`
  (its existing `register` method; no new registry primitive), a plugin declares
  model-callable tools (schema advertised to the model), and a plugin-tool proxy
  implementing `trait Tool` dispatches calls to the child and returns its JSON
  result under the normal permission plane.
- **R6 — Failure posture.** Per-hook `posture: safe|open`. Guards
  (`permission.ask`, `tool.execute.before` veto) fail safe; enrichment hooks fail
  open. Documented + enforced.
- **R7 — Declaration & loading.** `plugins:` config section + native dir-scan
  (`~/.config/hya/plugins/`, `.hya/plugins/`) with a `plugin.toml` manifest;
  per-plugin `enabled`. Central bootstrap in `hya-cli` wires the host before the
  registry/router freeze into `Arc`, across all modes (exec/rpc/goal/tui/serve).
- **R8 — OpenCode compatibility (later child).** A bundled **Bun adapter** plugin
  loads OpenCode JS/TS plugins (legacy bare-function exports + new target-module
  shapes), maps OpenCode `Hooks` ⇄ the hya protocol, points the SDK `client` at
  `hya serve`, provides Bun `$`, and runs OpenCode's `.opencode/plugins/` scan +
  npm install. Target: common server hooks (`tool.execute.*`, `chat.params`,
  `permission.ask`, `event`, plugin `tool:`). TUI plugin surface excluded.
- **R9 — Quality gate.** Every child keeps the workspace gate green:
  `cargo fmt --check`, `clippy -D warnings` (`unwrap_used`/`expect_used` denied in
  libs), `cargo test --workspace`; new behavior covered by tests (TDD).
- **R10 — No regressions.** hya's differentiators (goal/loop/team/worktrees/
  categories) and all headless modes keep working with zero plugins configured
  (hooks are inert by default; zero overhead when no host attached).

## Acceptance criteria

Parent-level (cross-child); each child restates its own testable subset.

- [ ] **AC1 (R1,R3,R4).** With a minimal native example plugin, a real turn fires
      `tool.execute.before`; the plugin mutates an arg and the mutated arg is what
      the tool receives; a separate veto blocks the tool and the engine records the
      block — proven by a test/QA transcript.
- [ ] **AC2 (R5).** An example plugin registers a tool; the model can call it; the
      call round-trips over IPC and the result appears as a normal `ToolResult`
      under permission checks.
- [ ] **AC3 (R3,R4).** A `chat.params` plugin changes temperature/model/system and
      the change is provably reflected in the built provider request.
- [ ] **AC4 (R6).** A plugin that sleeps past the timeout: an enrichment hook
      proceeds with the original payload (turn completes); a guard hook fails safe
      (action blocked / falls back to normal permission flow). Both asserted.
- [ ] **AC5 (R2,R6).** Killing the plugin mid-turn does not crash or hang hya; the
      turn completes per posture and the host reports the crash (restart on next
      use). Asserted via test/QA.
- [ ] **AC6 (R7).** A plugin declared in `config.yaml` and one dropped into the
      scanned dir (with manifest) both load and run; `enabled: false` disables one.
- [ ] **AC7 (R8, OpenCode child).** A real off-the-shelf OpenCode plugin using
      `tool.execute.before`/`after` (or `event`) runs against hya via the Bun
      adapter and its hook provably fires — live QA evidence.
- [ ] **AC8 (R9,R10).** Full quality gate green; with no plugins configured every
      existing test passes. **Overhead protocol (replaces "no measurable
      overhead"):** (1) HARD GATE — a unit test asserts default `SessionEngine::new`
      leaves `hooks == None` and the `None` branch issues zero dispatcher calls and
      zero heap allocations on the per-event (`emit`) path (verified with a counting
      allocator); (2) PERF EVIDENCE — a `--features bench` manual-timer microbench
      runs `emit` 100_000× and one `run_turn` against `FakeProvider` 1_000×,
      comparing `hooks=None` to the pre-plugin baseline commit; the `None` path must
      stay within **+3% median wall-clock** and never allocate on the hot path.
      Numbers recorded in the QA log.

## Out of scope (v1)

- Out-of-process **provider/model** plugins (deferred child) — hya keeps in-code
  provider wrappers for now.
- **OS-level sandboxing** (seccomp/landlock/container) — deferred child; v1 trust =
  explicit declaration + process isolation.
- OpenCode **TUI** plugin surface (`@opencode-ai/plugin/tui`).
- The full `experimental.*` / provider-SSE-frame / compaction / explicit `Step*`
  hook inventory beyond R4 — phased into later children.
- Embedding a JS engine in-process (v1 OpenCode-compat spawns Bun as a child).
- A plugin marketplace / registry / auto-update.

## Proposed task tree (parent + children) — for confirmation

This task `06-21-plugin-system` is the **parent** (owns requirements, the child
map, cross-child AC, final integration review; no direct implementation).

- **Child A — Plugin host & hook-dispatch core (MVP foundation).** R1–R4, R6, R7
  (hooks side), R9, R10. New `hya-plugin` crate (protocol + host/manager), hook
  seams in `hya-core`, config `plugins:` + dir-scan + manifest, `hya-cli`
  bootstrap, a native example plugin. Independently verifiable via AC1, AC3–AC6.
- **Child B — Plugin-registered tools.** R5. `PluginTool` proxy registered via the
  existing `ToolRegistry::register` (optional `extend` wrapper) + schema
  advertisement + tool-call dispatch. Depends on A's protocol/host. Verifiable via
  AC2.
- **Child C — OpenCode-compat Bun adapter.** R8. Bun host child that loads OpenCode
  plugins and bridges `Hooks` ⇄ protocol + SDK client → `hya serve`. Depends on
  A (+B for plugin `tool:`). Verifiable via AC7.
- **Child D — Deferred extensions (later, may not be planned now).** Out-of-process
  providers, OS sandbox, full hook inventory, compaction/provider-frame hooks.

Ordering A → B → C is a dependency, recorded in each child's artifacts (not implied
by tree position). Execution starts with **Child A**.

## Notes

- Keep `prd.md` focused on requirements, constraints, and acceptance criteria.
- Complex task: each implementation child needs `design.md` + `implement.md` before
  its own `task.py start`. The parent stays planning-only.
- Next: confirm the task tree, then create children and write Child A's
  `design.md` + `implement.md`, then run the cross-model plan-review gate before
  any `task.py start`.

## Plan Review (cross-model gate)

Planner: Claude (Sisyphus). Reviewer: **GPT (`gpt-5.5`, xhigh, read-only via
`codex exec`)** — cross-family per the `plan-review` skill. Scope: full set (parent
+ A + B + C, all prd/design/implement).

- Round 1 — VERDICT: FAIL (D1 overhead unfalsifiable; D2 loader phase too coarse;
  D3 stale "ToolRegistry static" fact; D4 `tool.execute.after` could mask a
  permission error; D5 hook load-order vs JoinSet untested; D6 `input_schema` vs
  locked `inputSchema`).
- Round 2 — D1/D2/D4/D5/D6 PASS; D3 FAIL (residual "becomes dynamic" wording).
- Round 3 — D1/D2/D4/D6 PASS; D3 + D5 FAIL (residual wording + drifted citations;
  Child C permission-preservation test missing).
- Round 4 — D1/D2/D3/D4/D6 PASS; D5 FAIL (forward-referenced Phase 8 E2E absent).
- **Round 5 — VERDICT: PASS (D1–D6 all PASS).** Gate cleared.

A same-family advisory pass (Claude oracle) also returned PASS but does not satisfy
the cross-family requirement; the GPT Round-5 PASS is the binding gate result.
Execution still requires explicit user approval before `task.py start` (Child A
first).
