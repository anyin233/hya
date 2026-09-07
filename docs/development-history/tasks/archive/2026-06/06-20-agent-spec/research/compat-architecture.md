# Research: compat Architecture

Source: librarian deep-dive of the active `anomalyco/compat` tree
(SHA `bd84c32860a7650965419716214196adbbb00e2f`; this is what `compat.ai`
currently ships; `sst/compat` resolves to the same tree). All paths below are
in that repo.

## TL;DR for our Rust design

compat is a **Bun/TypeScript monorepo** with a **client/server** split, an
**event-sourced session model** projected into SQLite, a **two-layer tool
system** with a rich **allow/ask/deny permission** plane, a **session-tree
subagent** model (the `task` tool), and a **clean Provider / Protocol / Route**
LLM abstraction. The TUI is **TypeScript + Solid + OpenTUI** (NOT Go/bubbletea —
that earlier assumption is outdated).

## 1. Language / package layout

Bun/TS monorepo. Key packages:
- `packages/compat` — CLI, runtime, **server**, tool registry, provider orchestration
- `packages/core` — durable session / database / domain logic
- `packages/tui` — TUI client (`@opentui/core`, `@opentui/solid`, `solid-js`)
- `packages/sdk/js` — generated typed HTTP client
- `packages/llm` — provider/protocol/tool-call normalization

> Lesson: the split is **logical layering** (core domain ⟂ runtime/server ⟂
> client ⟂ provider abstraction). We can keep the same *logical* boundaries in
> Rust crates without necessarily copying the *physical* process split.

## 2. Process model — client/server

- Running `compat` starts a **TUI client + a server**; the TUI talks to the
  server. The server publishes an OpenAPI spec and backs the SDK.
- **Two transport modes**:
  - **A. Default/internal**: TUI command spawns a **Worker**; RPC `fetch(...)`
    calls are forwarded to `Server.Default().app.fetch(request)` **in-process**;
    global events forwarded over RPC as `"global.event"`. (No real socket.)
  - **B. External**: if host/port/mDNS requested, the worker starts a **real
    local HTTP server** and the SDK points at that URL (enables remote/multi-client).
- Server is an Effect HTTP app: typed route groups, WebSocket tracking, OpenAPI
  gen, SSE/event routes, UI fallback.

> Lesson for yaca (lean v0): we can ship a **single process** with a clean
> internal "core API" trait boundary, and add a real server later (mode B) for
> remote/SDK. Don't pay the socket/IPC cost on day 1.

## 3. Session & message model — event-sourced + projected

- SQLite tables: `session`, `message`, `part`, `todo`, `session_message`,
  `session_input`, `session_context_epoch`.
- Flow: prompts **admitted** via `session_input` → durable **events** published
  (`session.next.prompt.admitted`) → a **projector** consumes events and writes
  the SQLite projection tables (`SessionTable`, `SessionMessageTable`).
- `session` row stores: identity + parent/child link, project/workspace/dir,
  title, agent, model, cost/token accounting, **permission snapshot**,
  revert/share metadata, timestamps. (Parent/child link = session tree.)
- Messages are a **tagged union**: `user | assistant | system | shell |
  synthetic | agent-switched | model-switched | compaction`.
- Assistant message: `agent, model, content[], snapshot, finish, cost, tokens,
  error, time`. Content is **part-based**: `text | reasoning | tool`.
- Tool parts have lifecycle: `pending → running → completed | error`.
- `SessionMessageUpdater.update(...)` folds streamed events (`step.started`,
  `text.delta`, `reasoning.delta`, `tool.*`) into message state.

> Lesson: persist a **normalized conversation/event model**, not raw provider
> transcripts. Tool-call lifecycle states drive TUI rendering. The parent/child
> session link is the substrate that makes subagents a *session tree*.

## 4. Tool system — two layers + permissions

- **Canonical LLM tool** (`packages/llm`): description + param schema + success
  schema + optional execute handler + conversion to `ToolDefinition[]`.
- **Core typed wrapper** (`packages/core/src/tool/tool.ts`): adds session/agent/
  message/call context, input+output validation, optional permission override,
  model-facing content projection.
- **Registry** (`packages/compat/src/tool/registry.ts`) assembles built-ins
  (`shell, read, glob, grep, edit, write, task, webfetch, todowrite, websearch,
  skill, apply_patch`) + file-based custom tools (`{tool,tools}/*.{js,ts}`) +
  plugin tools + MCP tools.
- Execution: decode input → execute handler → encode/validate result → emit
  `toolResult` / `toolError` events.

### Permission model (the real control plane)
- Rules are `allow | ask | deny`, granular wildcard matching, **last rule wins**.
- Keys: `read, edit, glob, grep, bash, task, skill, webfetch, websearch,
  external_directory, doom_loop`.
- Runtime evaluates `action + resource` against merged rulesets; default `ask`.
- `assert(...)` → allow / deny / create a **pending request** and wait for reply.
- Replies: `once | always | reject`; `always` persists future allowances.
- Tools call `ctx.ask(...)` with **semantic scopes** (e.g. `git *`, path globs,
  subagent names, external dirs) — not just blanket gates.

> Lesson: model permissions as `(action, resource)` over a merged, last-wins
> ruleset, with per-tool semantic approval scopes + a pending-request channel
> the TUI surfaces.

## 5. Agent / subagent model — session tree via `task`

- Agent modes: `primary | subagent | all`.
- Built-ins: `build` (primary), `plan` (primary), `general` (subagent),
  `explore` (subagent); hidden system agents `compaction, title, summary`.
- The **`task` tool** launches subagents: resolve subagent type → create/reuse
  a **child session** → derive child permissions (parent denies + external-dir
  rules) → prompt the child.
- Results: **foreground** waits and returns a `<task ...>` block into the parent
  turn; **background** starts a job and later injects a **synthetic** result
  message back into the parent session.

> Lesson: subagents are **child sessions**, not opaque RPC workers. Child
> permissions are **derived** from the parent. This is exactly the substrate our
> team-mode + goal features build on.

## 6. Provider / LLM abstraction — Provider / Protocol / Route

- **Provider facade**: provider id + model factory.
- **Protocol**: semantic API family (`OpenAIChat`, `OpenAIResponses`,
  `AnthropicMessages`, `Gemini`, ...).
- **Route**: deployment = protocol + endpoint + auth + framing/transport.
- e.g. OpenRouter reuses OpenAI-chat semantics, swaps endpoint/body; OpenAI
  exposes multiple routes (`responses`, `responsesWebSocket`, `chat`).
- AI SDK streams are normalized into canonical `LLMEvent`s: text start/delta/end,
  reasoning start/delta/end, tool input start/delta/end, tool call/result/error,
  step start/finish, finish.

> Lesson (directly supports D2 multi-provider): split **Provider** (who) from
> **Protocol** (wire family) from **Route** (endpoint+auth+transport), and
> normalize all providers into ONE internal event stream so the agent loop and
> TUI are provider-agnostic. This is the single most reusable idea for our
> provider-normalization layer.

## 7. TUI specifics

- `createCliRenderer(...)` from `@opentui/core`; Solid components via `render`.
- Rich component model (Solid reactivity), event-driven updates, theme palette.
- Streaming render is driven by the tool-part / message lifecycle states above.

> Lesson for our Rust ratatui TUI: drive rendering off the **normalized
> message/part lifecycle** (pending/running/completed) and a streaming event bus,
> not direct provider callbacks.
