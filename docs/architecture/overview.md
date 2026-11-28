# Architecture Overview

Hya is a multi-agent coding runtime written in Rust, paired with a Bun/OpenTUI frontend. This page explains the big-picture design choices that hold the system together, so you can read any crate in the workspace without getting lost.

![Hya System Architecture](../assets/hya-architecture.svg)

---

## Six core ideas behind Hya

### 1. One event log is the single source of truth

Every durable change in a conversation—your prompt, a model's completed reply, a tool call, a permission decision, token usage, or bundle-owned domain state (`Event::Extension` for inter-agent mail, subagent lifecycle, compaction, or Workflow steps)—is appended as a typed `Event` to SQLite (`hya-store`, `format_version = 2`).

Nothing maintains a separate "shadow state" table that can drift out of sync. Whenever the server, CLI, or SDK wants the current state of a session, it folds those events through one deterministic pure function: `hya_proto::Projection` (with bundle `ProjectionReducer`s folding `Event::Extension` into `Projection::extensions`).

- **Fast live tokens (`seq = 0`):** While a model is streaming words, tiny delta chunks fly over the in-memory bus with `seq = 0` so the UI feels instant without hammering SQLite on every token.
- **Durable checkpoints (`seq > 0`):** As soon as a text block, reasoning block, or tool call finishes, Hya writes it to SQLite in one atomic batch with monotonic sequence numbers (`seq > 0`).

![Turn Flow](../assets/hya-turn-flow.svg)

### 2. A tiny three-primitive Kernel underneath Full Mode

At the bottom of the runtime sits `hya-core::kernel`—a minimal agent loop with just three context operations (**Model append**, **Tool append**, and **Rewrite**) and seven built-in kernel tools (`read`, `write`, `edit`, `bash`, `grep`, `todo`, `rewrite`). Passing the global `--bare` flag (`hya --bare`, `hya serve --bare`, `hya exec --bare`) runs the unified `SessionEngine` on top of that kernel with `BuiltinBundleProfile::Bare` (adding `skill`, built-in `code-mode` without `agent()`, `AGENTS.md`/MCP/plugin adapters, and the minimal `hya-bare` agent—while loading zero built-in bundles). Add `--pure` (`hya exec --bare --pure`) to also skip external `AGENTS.md`, disk skills, MCP, and plugins for pure kernel benchmarks.

Full mode (`hya-engine` + 15 built-in bundles under `bundles/builtin/` over Lockstep Native ABI v2, [ADR-0039](../adr/0039-hya-bare-and-v2-modular-architecture.md)) does **not** fork or replace that loop. Instead, it composes modular bundles onto the kernel via `Arc<dyn HostRuntime>`—layering hashline editing (`hya/hashline`), LSP/formatters (`hya/coding-extras`), web tools (`hya/network`), plan mode (`hya/plan`), goal/loop drivers (`hya/goal`, `hya/loop`), episodic subagents (`hya/multi-agent`), vertical channels (`hya/agent-channels`), Workflows (`hya/workflow`), and gentler context-reduction steps (`hya/compaction`).

### 3. One background daemon per database, zero server `cwd`

- **Persistent single-writer daemon:** Each SQLite session database (`~/.local/state/hya/sessions.db`) has at most one writer at a time, guarded by an OS file lock (`<db>.runtime-owner.lock`) and advertised in `<db>.server.json`. Closing your terminal or browser leaves the daemon alive so reconnects are instant.
- **Client-chosen workspaces (`Project`):** The daemon itself has **no working directory**. Every session belongs either to a named **Project** (one or more folder roots on the host) or to an isolated scratch folder. One daemon can serve ten different repositories at the same time, lazily loading each Project's `.hya/bundles`, `.hya/plugins`, skills, and `AGENTS.md` on demand.

### 4. One contract (`hya.v2`) on one port, plus blind Relay encryption

`hya-server` exposes one clean-sheet Protobuf contract (`proto/hya/v2`: 15 services, 95 RPCs) over **both** `/v2` HTTP/JSON+SSE+WebSocket and gRPC (`V2Grpc`) on the **same TCP port**. Unary gRPC methods dispatch straight through the same internal router as HTTP, so HTTP and gRPC can never drift apart. Domain endpoints for native and JS bundles mount uniformly under `BundleApi` (`/v2/bundles/{bundle}/api/{path}` and `/v2/sessions/{session}/bundles/{bundle}/{path}`), and domain events stream via generic `ExtensionEvent`.

For remote access, `hya proxy` forwards encrypted byte streams between `hya serve --relay` and `hya bridge` / `hya --connect` using `Noise_NKpsk0`. The relay holds zero keys and sees only room IDs and ciphertext.

### 5. One modular UI codebase for both Terminal and Browser

Rendering never happens inside the Rust server. `packages/hya-tui` (TypeScript + OpenTUI on Bun) is the only interactive UI, owning five core panes (`conversation`, `composer`, `activity`, `status`, `layout`) while built-in `@hya/tui-sdk` v2 bundles (`hya/basic-tui-components`, `hya/multi-agent`, `hya/workflow`, `hya/plan`, `hya/agent-channels`) supply all workspace and domain panes dynamically. To put that UI in a browser, `packages/hya-tui-web` spawns the exact same TUI on a real PTY and renders it with `xterm.js` over WebSocket—meaning terminal and browser never drift out of feature parity.

### 6. Sleeping subagents & driver-owned stop decisions

- **Episodic resident subagents (`hya/multi-agent` + `hya/agent-channels`):** Every helper spawned via `task` is an actor with its own session and fenced `bundle_claim` lease. When it calls `report`, the bundle summarizes its state into a short *handoff*, releases its concurrency slot, and puts it to sleep (*archived*) at zero token cost. Sending follow-up mail down its `#DM` channel wakes the same helper into its next episode with that handoff.
- **Workers don't grade their own homework:** In Goal mode (`-p`, `hya/goal`), Loop mode (`hya loop`, `hya/loop`), and compiled Workflows (`*.hya.md`, `hya/workflow`), dedicated drivers and separate verifier/evaluator sessions decide when an objective is finished—never the worker itself.

![Subagent Lifecycle & Relay](../assets/hya-subagent-lifecycle.svg)

---

## Runtime process topology

```mermaid
flowchart TB
    subgraph ClientSide["Interactive & CLI Clients"]
        Bare["bare hya launcher<br/>(crates/hya-backend)"]
        TUI["Terminal TUI<br/>(packages/hya-tui + @hya/tui-sdk v2)"]
        Web["WebUI PTY Host :3250<br/>(packages/hya-tui-web)"]
        Browser["Browser Tab (xterm.js)"]
        CLI["Headless CLI / SDK<br/>(hya exec · workflow · hya-sdk)"]

        Bare -->|spawns| TUI
        Bare -->|spawns| Web
        Web <-->|WebSocket PTY frames| Browser
    end

    subgraph Daemon["Detached Backend Daemon (hya serve)"]
        Server["hya-server<br/>Single-Port /v2 HTTP+SSE+WS & V2Grpc"]
        App["hya-app<br/>Config v2 · Auth · CatalogScope · HostRuntime"]
        Engine["hya-engine on hya-core::kernel<br/>Single-Agent Turns · Extension Host"]
        Bundles["15 Built-in Bundles (bundles/builtin/)<br/>ABI v2 Native cdylibs · Static · TUI v2"]
        Planes["hya-provider · hya-tool · hya-mcp · hya-plugin"]
        Store[("hya-store (SQLite WAL v2)<br/>event_log · bundle_state · bundle_claim")]

        Server --> App --> Engine
        Engine <-->|Arc&lt;dyn HostRuntime&gt;| Bundles
        Engine --> Planes
        Engine <--> Store
    end

    TUI <-->|hya.v2| Server
    Web <-->|per-tab TUI over hya.v2| Server
    CLI <-->|hya.v2| Server
```

---

## Where to dive deeper

- **[Repository & Crate Map](codebase-map.md)** — Directory layout, crate dependency graph, and which crate owns which change.
- **[Kernel & Context Engine](kernel-and-context.md)** — The $(m, g, w)$ context model, three primitives, `shake` / `remote` compaction, and `Rewrite`.
- **[Events & Storage](events-and-storage.md)** — Wire events, `Projection` folding, SQLite schema, single-writer locks, and degraded-store drain.
- **[Session Engine, Subagents & Channels](engine-and-subagents.md)** — Turn lifecycle, resident actor fencing, vertical `#DM`/`#announce` channels, and Workflows.
- **[Providers, Tool Plane & Scoping](providers-and-tools.md)** — Provider stream normalization, native tool ABI, `ProjectScope`, and lazy `CatalogScope`.
- **[Server, Relay & TUI Internals](server-relay-and-tui.md)** — Dual-protocol routing, `Noise_NKpsk0` relay internals, TUI state sync, and WebUI PTY host.
- **[Domain Glossary](glossary.md)** — Shared terminology across the codebase.
