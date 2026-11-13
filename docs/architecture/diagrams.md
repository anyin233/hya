# Architecture & Design Diagrams

This page collects visual architecture diagrams for `hya`'s current system
structure, crate boundaries, event-sourced turn pipeline, episodic subagent
lifecycle, governed Workflows, multi-root Projects, zero-trust Relay, and
hot-reload/restart model.

For narrative rationale and crate-level invariants, pair this document with
[Design Philosophy & Architecture Overview](overview.md) and
[Project Structure](../project-structure.md).

---

## 1. High-Level System Architecture Blueprint

![hya System Architecture](../assets/hya-architecture.svg)

### Event-Sourced Turn Pipeline & Dual-Track Streaming Blueprint

![hya Turn Pipeline](../assets/hya-turn-flow.svg)

### Episodic Subagents, Vertical Channels & Secure Relay Blueprint

![hya Subagents and Relay](../assets/hya-subagent-lifecycle.svg)

---

## 2. Process Topology, Persistent Daemon & Client Surfaces

Bare `hya` never renders in Rust and never embeds an in-process server. Instead,
it discovers or auto-starts the database's detached **backend daemon**
([ADR-0022](../adr/0022-one-writer-per-database.md),
[ADR-0023](../adr/0023-persistent-backend-daemon.md)) and supervises the
Bun/OpenTUI terminal UI and the browser WebUI PTY host
([ADR-0018](../adr/0018-browser-rendered-tui-test-environment.md),
[ADR-0020](../adr/0020-bundle-tui-and-webui-in-hya.md)).

```mermaid
flowchart TB
    subgraph LocalMachine["Local Developer Machine"]
        CLI["bare hya supervisor<br/>(crates/hya-backend)"]
        TUI["Terminal TUI<br/>(packages/hya-tui · Bun/OpenTUI)"]
        WebHost["WebUI Host :3250<br/>(packages/hya-tui-web · Bun PTY)"]
        Browser["Browser Tab<br/>(xterm.js over /pty WS)"]
        Headless["Headless CLI / SDK<br/>(hya exec · sessions · hya-sdk-v1)"]

        CLI -->|spawns child| TUI
        CLI -->|spawns child| WebHost
        WebHost <-->|PtyClientFrame / PtyServerFrame| Browser
        WebHost -->|runs per-tab PTY| TabTUI["Tab TUI Process<br/>(packages/hya-tui)"]

        subgraph DaemonBox["Detached Backend Daemon (hya serve)"]
            Router["Single-Port Dual Router (hya-server)<br/>/v1 HTTP+SSE+WS  &  V1Grpc (h2c)"]
            App["Composition & Control (hya-app)<br/>WorkflowControl · CatalogScope · Auth"]
            Engine["SessionEngine (hya-core)<br/>Turns · Residents · Channels · Compaction"]
            Router --> App --> Engine
        end

        DB[("SQLite WAL Store<br/>sessions.db + .runtime-owner.lock<br/>+ .server.json + bundle_registry.db")]
        Engine <--> DB
    end

    TUI <-->|hya.v1 HTTP/SSE or gRPC| Router
    TabTUI <-->|hya.v1 HTTP/SSE| Router
    Headless <-->|hya.v1 HTTP/gRPC| Router
```

---

## 3. Workspace Crate & Package Dependency Graph

Lower crates never depend on `hya-backend` or UI packages; `hya-proto` stays free
of async/SQL/HTTP dependencies, and `hya-relay` and `hya-updater` remain isolated
from runtime crates.

```mermaid
flowchart BT
    Proto["hya-proto<br/>(IDs · Event · Envelope · Projection)"]
    Ver["hya-version<br/>(versions.toml constants)"]
    Api["hya-api<br/>(generated hya.v1 prost/tonic/pbjson)"]

    Provider["hya-provider<br/>(OpenAI · Codex · Grok · Anthropic · Google · Dev)"]
    Tool["hya-tool<br/>(ToolRegistry · PermissionPlane · ProjectScope)"]
    Families["5x Native Tool Crates<br/>(base · extended · channel · network · todo)"]
    Mcp["hya-mcp<br/>(mcp__server__tool bridge)"]
    Plugin["hya-plugin<br/>(NDJSON JSON-RPC 2.0 host)"]
    PluginAdapters["hya-plugin-bun & hya-plugin-claude<br/>(JS sidecar & Claude plugin import)"]

    Store["hya-store<br/>(SQLite WAL event_log · claims · snapshots)"]
    Bundle["hya-bundle<br/>(.hyabundle format v4 · 12 presets)"]
    Workflow["hya-workflow<br/>(*.hya.md DAG compiler)"]
    Relay["hya-relay<br/>(hya.relay.v1 · Noise_NKpsk0 · proxy)"]
    Updater["hya-updater<br/>(embedded hya-install.sh)"]

    Core["hya-core<br/>(SessionEngine · ResidentSupervisor · Governor)"]
    AppCrate["hya-app<br/>(Config · OAuth · ScopeRefresh · WorkflowControl)"]
    Server["hya-server<br/>(/v1 Axum + V1Grpc + relay_host)"]
    Sdk["hya-sdk-v1<br/>(Typed v1 client + V1SessionMirror)"]
    Bin["hya-backend (bin/hya)<br/>(CLI · Daemon · Frontend launcher)"]
    Frontends["packages/hya-tui · hya-tui-web · hya-tui-sdk"]

    Provider --> Proto
    Tool --> Proto
    Families --> Tool
    Mcp --> Tool
    Plugin --> Tool
    PluginAdapters --> Plugin
    Store --> Proto
    Bundle --> Ver
    Workflow --> Proto

    Core --> Proto & Provider & Tool & Store & Bundle & Workflow & Plugin
    AppCrate --> Core & Mcp & PluginAdapters
    Server --> Core & Api & Relay
    Sdk --> Api
    Bin --> AppCrate & Server & Sdk & Relay & Updater & Ver
    Bin -.->|spawns Bun children| Frontends
```

---

## 4. Single-Turn Event-Sourced Execution & Streaming Sequence

High-frequency text deltas stream live at `seq = 0` without disk writes; part
boundaries commit durable batches (`seq > 0`), and permission `Ask`s park on the
`Interactions` plane without blocking unrelated sessions.

```mermaid
sequenceDiagram
    autonumber
    actor User
    participant Client as TUI / SDK Client
    participant Server as hya-server (/v1 | V1Grpc)
    participant Engine as SessionEngine (hya-core)
    participant Store as SessionStore (SQLite WAL)
    participant Provider as ProviderRouter (SSE)
    participant Tool as ToolRegistry + PermissionPlane

    User->>Client: Submit prompt
    Client->>Server: POST /v1/sessions/{id}/turns (CreateTurn)
    Server->>Engine: Bind CatalogScope -> TurnBinding & admit prompt
    Engine->>Store: append_events(User MessageStarted..Finished) [seq > 0]
    Store-->>Client: SSE StreamFrame (durable user message)

    loop Until round finishes with no tool calls
        Engine->>Store: read_projection() + run 5-stage Compaction if needed
        Engine->>Provider: stream(CompletionRequest)
        Provider-->>Engine: TextDelta chunks
        Engine-->>Client: publish_live(seq = 0, TextDelta)
        Provider-->>Engine: TextEnd + ToolCallRequested
        Engine->>Store: append_events([TextStart, TextReplace, TextEnd, ToolCallRequested]) [seq > 0]
        Store-->>Client: SSE StreamFrame (durable parts)

        Engine->>Tool: execute(ToolCtx { roots: ProjectScope, ... })
        alt Permission requires human approval (Ask)
            Tool->>Server: Park pending request on Interactions service
            Server-->>Client: SSE interactionRequested
            User->>Client: Approve (once / save rule) or Reject
            Client->>Server: POST /v1/interactions/{id}/respond
            Server-->>Tool: Decision
        end
        Tool-->>Engine: Shaped/capped ToolResult or ToolError
        Engine->>Store: append_event(ToolResult | ToolError) [seq > 0]
        Store-->>Client: SSE StreamFrame (tool completed)
    end

    Engine->>Store: append_events([UsageRecorded, MessageFinished]) [seq > 0]
    Store-->>Client: SSE StreamFrame (messageFinished)
```

---

## 5. Episodic Resident Subagent Lifecycle & Vertical Channel Plane

Every spawned subagent is an episodic resident actor
([ADR-0015](../adr/0015-unified-resident-subagent-lifecycle.md),
[ADR-0016](../adr/0016-channel-communication-plane.md)). Tree depth is capped at
`MAX_SUBAGENT_DEPTH = 2`, siblings never see each other, and archived actors
consume zero tokens until revived by a downward DM from their parent.

```mermaid
stateDiagram-v2
    [*] --> Admitting: Parent calls task(tasks[])
    Admitting --> EpisodeActive: AdmissionJournal claim + Governor debit<br/>+ ActorClaim(epoch=1) + mint #DM-{8} & join #announce-{8}

    state EpisodeActive {
        [*] --> Working: Arming directive turn
        Working --> Idle: Turn completes without report()
        Idle --> Working: Coalesced inbound mail wake (#DM or #announce)
        Working --> Reporting: Subagent calls report(status, summary)
        Reporting --> Working: Rejected if inbox unread or child still live
    }

    EpisodeActive --> Archived: ARCHIVE txn<br/>SubagentReported + HandoffCommitted(n) + AgentArchived<br/>+ Claim release + Governor refund + DM report mail -> wakes Parent
    EpisodeActive --> Archived: Parent archive(target) / Shutdown drain / Budget kill<br/>(degraded handoff + refund)

    Archived --> EpisodeActive: Parent sends downward DM (#DM-{8} or handle)<br/>AgentRestarted + epoch+1 + Governor re-debit<br/>Context = system prompt + handoff(n) + new DM body
```

Vertical Team Topology (`MAX_SUBAGENT_DEPTH = 2`):

```mermaid
flowchart TD
    Main["Root Session: main<br/>(hya-main · Depth 0)"]
    Ann0(["#announce-a1b2c3d4<br/>(Main broadcasts only)"])

    Lead["Child Resident: main/hya-task-exusiai<br/>(Unit Leader · Depth 1)"]
    Scout["Child Resident: main/hya-scout-suzuran<br/>(Read-Only · Depth 1)"]
    Ann1(["#announce-e5f6g7h8<br/>(Exusiai broadcasts only)"])

    Leaf1["Grandchild: .../hya-task-amiya<br/>(Depth 2 · Orchestration tools hidden)"]
    Leaf2["Grandchild: .../hya-reviewer-texas<br/>(Depth 2 · Orchestration tools hidden)"]

    Main <-->|"#DM-1111aaaa (spawn / report / revive)"| Lead
    Main <-->|"#DM-2222bbbb (spawn / report / revive)"| Scout
    Main -.-> Ann0
    Ann0 -.-> Lead & Scout

    Lead <-->|"#DM-3333cccc"| Leaf1
    Lead <-->|"#DM-4444dddd"| Leaf2
    Lead -.-> Ann1
    Ann1 -.-> Leaf1 & Leaf2
```

---

## 6. Governed Execution Modes (Turn vs Goal vs Loop vs Workflow DAG)

Workers never decide that their own multi-step objective is complete. Stop
decisions belong to `hya-core` evaluators, verifiers, and `WorkflowControl`
([ADR-0013](../adr/0013-user-assembled-agent-workflows.md),
[ADR-0017](../adr/0017-workflow-on-unified-substrate.md)).

```mermaid
flowchart LR
    subgraph Modes["Entry Modes"]
        M1["Ordinary Turn<br/>(TUI prompt / hya exec)"]
        M2["Goal Mode<br/>(hya -p 'goal')"]
        M3["Loop Mode<br/>(hya loop --target ... --until/--while)"]
        M4["Compiled Workflow<br/>(*.hya.md / WorkflowControl)"]
    end

    subgraph Runtime["hya-core Governed Execution"]
        Turn["Single SessionEngine::run_turn<br/>(tool rounds until model stops)"]
        GoalDrv["IterationDriver + Goal Evaluator<br/>(independent model or goal.evaluate hook)"]
        LoopDrv["Loop Driver + Shell Predicate<br/>+ Independent Planner/Verifier"]
        WfExec["Workflow DAG Runner on Unified Resident Substrate<br/>(Stage -> resident spawn/actor key -> report -> retry=DM revive)"]
    end

    M1 --> Turn
    M2 --> GoalDrv --> Turn
    M3 --> LoopDrv --> Turn
    M4 --> WfExec --> Turn
```

---

## 7. Multi-Root `Project`, `ProjectScope` & Lazy `CatalogScope`

`hya serve` has no process `cwd`
([ADR-0024](../adr/0024-project-model-and-client-chosen-workspace.md),
[ADR-0026](../adr/0026-multi-root-path-permissions.md),
[ADR-0027](../adr/0027-per-project-catalog-tier.md)). Both file permissions and
bundle/plugin catalogs derive from the Session's `Project`.

```mermaid
flowchart TB
    Session["Session (project_id = prj_..., workdir = /work/app)"]
    Roots["Ordered Project Roots<br/>1. /work/app (primary)<br/>2. /work/shared-lib"]

    Session --> Roots

    subgraph FileGuard["File Boundary (hya-tool::ProjectScope)"]
        Canon["Canonicalize target & existing ancestors<br/>(symlink-escape safe)"]
        Inside["Inside Root 1 or Root 2<br/>-> Allowed without prompt"]
        Outside["Outside all Project Roots<br/>-> ExternalDirectory Ask / Saved Project Rule"]
        Canon --> Inside
        Canon --> Outside
    end

    subgraph CatalogOverlay["Lazy LRU/TTL ScopeOverlay (hya-core::CatalogScope)"]
        Tier1["Project Tier (Root 1 wins over Root 2)<br/>&lt;root&gt;/.hya/bundles · &lt;root&gt;/.hya/plugins<br/>&lt;root&gt;/.hya/skills · commands · AGENTS.md"]
        Tier2["User Installed Tier<br/>bundle_registry.db + config.yaml mcp/plugins"]
        Tier3["Trusted First-Party Tier<br/>12 Preset Bundles (core-agents, 5x *-tools, ...)"]
        Tier1 -->|shadows| Tier2 -->|shadows non-reserved| Tier3
    end

    Roots --> Canon
    Roots --> Tier1
```

---

## 8. Zero-Trust End-to-End Encrypted Relay (`hya.relay.v1` + `Noise_NKpsk0`)

The `hya proxy` relay holds no keys and sees only Room IDs, stream IDs, and
ciphertext ([ADR-0025](../adr/0025-secure-relay.md)). Loopback-only RPCs
(`RelayControl`) are rejected on `Origin::Relay`.

```mermaid
sequenceDiagram
    autonumber
    participant TUI as Local TUI / WebUI
    participant Bridge as Local hya bridge<br/>(127.0.0.1:ephemeral)
    participant Proxy as Untrusted hya proxy<br/>(Stateless Room Rendezvous)
    participant Host as Backend relay_host<br/>(inside hya serve)
    participant V1 as hya.v1 Router<br/>(Origin::Relay)

    Host->>Proxy: RegisterRoom (Ed25519 challenge-response signature)
    Proxy-->>Host: Room registered (room_id = base32(sha256(ed25519_pub))[..26])
    Note over TUI,Bridge: User supplies secret hya://host/room?t=auto#x25519_pub.psk

    TUI->>Bridge: Plain loopback HTTP / SSE / PTY WS (+ bridge token)
    Bridge->>Proxy: OpenStream (HMAC-SHA256 open token · gRPC or WS via t=auto)
    Proxy->>Host: Splice encrypted stream frames
    Bridge<-->Host: Noise_NKpsk0_25519_ChaChaPoly_BLAKE2s handshake
    Bridge<-->Host: End-to-end encrypted raw HTTP/1.1 stream
    Host->>V1: hyper::serve_connection (tagged Origin::Relay)
    V1-->>TUI: hya.v1 responses & live SSE / WS frames
```

---

## 9. Extension Hot-Reload vs Binary Stop/Start Recovery

Per [ADR-0031](../adr/0031-hot-reload-and-stop-start-restart.md), extension
changes never restart the daemon, while binary upgrades perform a clean
`stop` → `start` with durable `pending_resume` turn restoration.

```mermaid
flowchart TD
    subgraph HotReload["Path A: Extension Change (Zero Daemon Restart)"]
        E1["hya bundle install/remove, MCP connect,<br/>or edit &lt;root&gt;/.hya/{bundles,plugins}"]
        E2["Publish new ConfigGeneration / ScopeOverlay<br/>(PID & open sockets unchanged)"]
        E3["In-flight round keeps captured TurnBinding;<br/>next turn/round binds updated catalog + emits catalogUpdated"]
        E1 --> E2 --> E3
    end

    subgraph BinaryRestart["Path B: Binary Upgrade (hya update + hya serve restart)"]
        B1["hya serve stop: fence new runs & cancel active turns"]
        B2["Commit MessageFinished(Cancelled, cause: Shutdown)<br/>+ upsert pending_resume row in same SQLite txn"]
        B3["Start new hya serve binary on sessions.db"]
        B4["take_restart_resume injects structured User restore prompt<br/>(unfinished tool calls + interrupted subagents)"]
        B5["Attached TUIs reconnect & hot-reload frontend code"]
        B1 --> B2 --> B3 --> B4 --> B5
    end
```
