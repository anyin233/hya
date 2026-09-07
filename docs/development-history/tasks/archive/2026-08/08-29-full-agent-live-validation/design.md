# Technical design: complete Hya agent live validation

## Purpose, source, and boundaries

This design is the technical contract for `.trellis/tasks/08-29-full-agent-live-validation`. It uses the approved plan as the authority. The verified source copy is `research/approved-plan.md` with SHA-256 `abbe961e90d65b71e9b7218f8cec44d61be22e414b1c0a41f8ce47c9c03b068a`. The plan was approved before this design was written.
Runtime markers in command snippets are resolved at execution and are not open work items: `<pane-id>` is parsed from the Herdr split result; `<current-source-hya-command>` is the executable built from this worktree; `<private>` is a private database path; `<prompt>` is the exact generated prompt; `<nonce>` is a per-scenario marker; `<canonical-db>` is the private SQLite path; `<n>` is a selected terminal dimension; and `<copied-secret>` is schema notation only and is never written by this document. No marker permits a different route, asset, or secret-handling rule.
In the Herdr command block, `<text>`, `<value>`, `<match>`, `<regex>`, `<arguments>`, and `<enter|esc|ctrl+p|arrows|...>` are the documented text, value, matcher, regular-expression, argument, and key-sequence inputs. `<narrow scenario-specific rules>` is the selected least-privilege permission rule set. These markers are resolved by the existing driver and never become new interfaces.



The design uses requirement IDs in source-plan order:

| ID | Design boundary |
| --- | --- |
| R1 | Current-source scope, evidence architecture, and cross-layer ownership |
| R2 | Private runtime, configuration, and secret transfer |
| R3 | Provider routes, current-source binaries, and the counted live relay |
| R4 | Backend, API, Session, persistence, and side-plane contracts |
| R5 | Herdr-owned pane and TypeScript TUI interaction/capture |
| R6 | Canonical builtins, aliases, schemas, Tool Events, and tool boundaries |
| R7 | User slash commands, Skills, plugin Tools, MCP Tools, and dynamic generations |
| R8 | ProviderError, ToolError, Session, resource, and recovery seams |
| R9 | Reasoning events, effort, rendering, and persistence |
| R10 | Subagent, team, mailbox, and bounded spawn generations |
| R11 | Event-sourced Workflow control and Workflow generations |
| R12 | Real coding capability and isolated coding fixtures |
| R13 | SWE-Bench Pro selection, execution, evaluation, and evidence |
| R14 | Deterministic-first repair loop and source-audit RED candidates |
| R15 | Version, changelog, commit, and push rules |
| R16 | Cleanup, rollback, and evidence retention |
| R17 | Final GPT-5.6-family scheduling proof (passed; final readiness gated by project gates and cleanup) |

The validation proves current worktree behavior end to end. It does not treat an installed binary as current source. It does not add a test-only product hook, generic retry layer, alternate frontend, or new E2E framework. The TypeScript frontend remains the shipped frontend. Existing SDK, store, Event, command, fixture, and Herdr seams remain the integration points.

The source audit did not send a provider request and did not change product source. The design therefore distinguishes a source-audit candidate from a live-reproduced Hya defect. A model response is evidence of model behavior only. Canonical Events, SQLite rows, HTTP/SSE observations, disk diffs, focused command output, and Herdr captures are proof.

### Final scope decision (2026-08-30)

The user explicitly approved this finalization scope on 2026-08-30. Remaining GLM functional, coding, subagent, Workflow, and reasoning validation is waived for this task, and completed GPT-only validation is accepted for finalization. This is a user-approved scope waiver, not a GLM pass.

Preserve the historical GLM successful custom-MCP Turn and the later upstream HTTP 503 classification as evidence. They remain accurate historical classifications and do not become a GLM pass or an unresolved acceptance blocker. SWE-Bench Pro remains separate diagnostic accounting, including its five setup-invalid counted-false attempts. The GPT scheduling contract in R17 passed in a live run on release `0.36.5`; scheduling-sensitive source is unchanged in current final release `0.36.6`. Overall acceptance is `passed-with-user-approved-GLM-waiver`; final gates, cleanup, and the Trellis quality review passed, so the worktree is ready to commit.

### PRD and acceptance crosswalk

The PRD also names stable domain IDs. The `R1`–`R17` headings above are the technical-design sequence requested by this task; the following crosswalk keeps every PRD requirement and acceptance ID traceable without changing its scope.

| Design section | PRD requirement IDs | PRD acceptance IDs |
| --- | --- | --- |
| R1 | `R-SCOPE-01`, `R-SCOPE-02`, `R-SCOPE-03`, `R-SCOPE-04`, `R-SCOPE-05`, `R-SCOPE-06`, `R-SCOPE-07`, `R-ART-01` | `AC-EVID-01`, `C-08` |
| R2–R3 | `R-ENV-01`, `R-ENV-02`, `R-ENV-03`, `R-ENV-04`, `R-ENV-05` | `AC-ENV-01`, `AC-ENV-02` |
| R4 | `R-BE-01`, `R-BE-02`, `R-BE-03`, `R-BE-04`, `R-BE-05`, `R-BE-06` | `AC-BE-01`, `AC-BE-02` |
| R5 | `R-TUI-01`, `R-TUI-02`, `R-TUI-03`, `R-TUI-04`, `R-TUI-05`, `R-TUI-06`, `R-TUI-07`, `R-TUI-08`, `R-TUI-09`, `R-TUI-10` | `AC-TUI-01`, `AC-TUI-02`, `AC-TUI-03`, `AC-TUI-04` |
| R6 | `R-TOOL-01`, `R-TOOL-02`, `R-TOOL-03`, `R-TOOL-04` | `AC-TOOL-01`, `AC-TOOL-02` |
| R7 | `R-RES-01`, `R-RES-02`, `R-RES-03`, `R-RES-04`, `R-RES-05`, `R-RES-06`, `R-RES-07`, `R-RES-08`, `R-RES-09`, `R-RES-10` | `AC-RES-01`, `AC-RES-02`, `AC-RES-03`, `AC-RES-04`, `AC-RES-05` |
| R8 | `R-ERR-01`, `R-ERR-02`, `R-ERR-03`, `R-ERR-04` | `AC-ERR-01` |
| R9 | `R-REASON-01`, `R-REASON-02`, `R-REASON-03`, `R-REASON-04` | `AC-REASON-01` |
| R10 | `R-SUB-01`, `R-SUB-02`, `R-SUB-03`, `R-SUB-04`, `R-SUB-05` | `AC-SUB-01` |
| R11 | `R-WF-01`, `R-WF-02`, `R-WF-03`, `R-WF-04` | `AC-WF-01` |
| R12 | `R-CODE-01`, `R-CODE-02`, `R-CODE-03` | `AC-CODE-01` |
| R13 | `R-SWE-01`, `R-SWE-02`, `R-SWE-03`, `R-SWE-04`, `R-SWE-05`, `R-SWE-06`, `R-SWE-07`, `R-SWE-08` | `AC-SWE-01`, `AC-SWE-02`, `AC-SWE-03` |
| R14 | `R-RED-01`, `R-RED-02`, `R-RED-03`, `R-RED-04`, `R-RED-05`, `R-RED-06`, `R-RED-07`, `R-RED-08` | `AC-RED-01` |
| R15–R16 | Cross-cutting finalization and safety | `AC-FINAL-01`, `AC-FINAL-02`, `AC-FINAL-03`, `AC-FINAL-04`, `AC-FINAL-05`, `AC-FINAL-06`, `AC-FINAL-07`, `AC-FINAL-08`, `AC-FINAL-09`, `AC-FINAL-10`, `AC-FINAL-11`, `AC-FINAL-12`, `AC-FINAL-13`, `AC-FINAL-14`, `AC-FINAL-15` |
| R17 | `R-FINAL-01` | `AC-FINAL-16` |

The design headings retain the natural source-plan order. The PRD IDs are aliases for traceability only; they do not add a second execution plan or change any acceptance boundary.

## R1. Current-source scope and evidence architecture

### Artifact activation boundary

The approved plan is persisted as the verified research source and as the role-specific Trellis artifacts. PRD owns requirements/constraints/observable acceptance; this design owns boundaries/contracts/data flow/tradeoffs/rollback; implementation owns ordered execution, checks, commands, repair loop, and cleanup. Validate the artifacts and run `task.py start` only after approval. This design does not activate a task, edit task metadata, or claim execution results.

### Critical source anchors (C-08)

Use these existing anchors when implementation reaches the corresponding seam; do not create parallel owners:

- `crates/hya-server/src/compat/command_catalog.rs`: `list`, `expand_prompt`, `expand_template`, and `add_skill_commands` own source precedence and exact expansion.
- `packages/hya-tui-ts/src/upstream/app.tsx`, `component/prompt/autocomplete.tsx`, and `component/prompt/index.tsx` own slash registration/merge/submit and `prompt.skills`.
- `crates/hya-server/src/compat/command_sources.rs`: `config_commands`, `disk_commands`, `command_hints`, and `parse_command_file` own project sources, recursive names, metadata, and malformed-source omission.
- `crates/hya-e2e/tests/p18_custom_slash_resources.rs` is the process seam joining custom commands to Skill, plugin, and MCP Tool execution, structured errors, recovery, and restart.
- `crates/hya-provider/src/openai/responses.rs` plus `response_decoder.rs` and `crates/hya-server/src/compat/session_prompt.rs` are the initial Responses-terminal and V2-background-error RED seams. Re-read exact constructors/spawn paths before source edits.

### Design tradeoffs

- Deterministic loopback coverage comes before live traffic. This gives exact failure and recovery proof without spending production requests, while real GPT/GLM runs still cover organic behavior. A model's non-compliance is classified rather than forced into a Hya code change.
- The relay keeps only bounded metadata and fails closed at the hard cap. This sacrifices body-level relay debugging, but it prevents credential/prompt leakage and makes the 2,000-request boundary enforceable.
- Canonical Events and SQLite remain the only durable proof. SDK projections and Herdr frames are views of that source, not replacement logs. This prevents replay and UI evidence from diverging.
- A real Herdr pane is used instead of a synthetic TUI test or screenshot. This proves terminal focus, input isolation, ANSI rendering, and restoration, but requires keyboard-only interaction because mouse injection and PNG capture are unsupported.
- Command names may be bootstrap-cached while backend resources refresh by root Turn. This preserves the existing TUI sync contract: body edits for known names are visible, while added/removed names have the documented fallback until restart.
- Resource generations are immutable for an existing `TurnBinding`. New Skills/MCP/plugin declarations publish only at their documented boundary; an in-flight binding is never silently rebound. This prevents a reload from changing a request already admitted.
- SWE-Bench runs are independent diagnostic Pass@1 attempts, not leaderboard reproduction. Strict one-Turn execution and false-on-error scoring make the result honest, at the cost of no retry or patch-sharing recovery.



### Scope and current state

- Treat package version `0.36.0` as the planning baseline. Source repairs follow R15; the current aligned final release is `0.36.6`. The GPT scheduling proof ran on `0.36.5` and remains applicable because `0.36.6` changes only TypeScript Session-cache lookup/test code and release metadata, leaving scheduling-sensitive source unchanged. Earlier 0.36.3/0.36.4 gate records remain historical evidence.
- Build `hya`, `hya-ts`, and `hya-backend` from the worktree. Installed executables cannot satisfy acceptance.
- Use `/home/yanweiye/Projects/hya-playground` as the task playground. It does not exist at planning time, so execution creates it.
- Use `/home/yanweiye/.omp/agent/models.yml` only as the source of the OMP route and credential during setup. Hya runtime must not depend on this file.
- Local tool versions observed during planning are Docker `28.5.1`, Bun `1.3.10`, Rust/Cargo `1.92.0`, and Herdr `0.8.1`.
- CodeGraph is not initialized for this repository. Do not create an index for this validation.

### Evidence pipeline

All evidence has an owner and a stable correlation key. The data flow is:

```text
current source
  -> current-source executables
  -> private Hya runtime/config
  -> backend/API and provider adapter
  -> counted relay or deterministic loopback
  -> canonical Session Events + SQLite
  -> SDK/sync projection
  -> TypeScript TUI
  -> task-owned Herdr pane captures
```

A command/resource path follows the same chain:

```text
slash input or model Tool call
  -> command catalog / Tool registry
  -> Session command or Tool operation
  -> canonical Tool/Command/Session Event
  -> next provider request
  -> result/error Event
  -> projection and TUI card/status
```

Record, without secrets:

- source commit and executable hashes;
- scenario, model route, Session ID, Turn ID, ToolCall ID, operation ID, Workflow run ID, and Member IDs;
- canonical Event envelopes and SQLite state/replay output;
- HTTP status, SSE frames, request/response shape, and disconnect/reconnect observations, with credential-bearing headers and bodies redacted;
- full text-file disk diffs and command results;
- Herdr visible text, ANSI frames, process status, terminal dimensions, and input sequence;
- benchmark prompt, patch, evaluator, image, and result hashes;
- start/end timestamps and token usage where supplied.

Evidence reports never contain provider prose as the only assertion. Do not put a credential in Events, prompts, patches, logs, argv, Docker mounts, Herdr captures, or reports. Do not create a second event reducer or a second SDK client to make evidence easier. Each derived display state points to its canonical Event sequence/ID.

### Compatibility and ownership boundaries

- `crates/hya` is the canonical `exec` shim.
- `crates/hya-ts` owns CLI parsing, backend discovery, process-group handoff, and cleanup.
- `packages/hya-tui-ts` owns terminal rendering and input. Hya-specific integration stays in `src/hya`; retained Solid/OpenTUI code stays in `src/upstream`.
- `hya-backend` owns runtime composition, persistence, HTTP/SSE, provider, side planes, and command/resource registration.
- `crates/hya-tui` and `crates/hya-tui-lib` are retained compatibility crates. No shipped binary launches them.
- The frontend reaches backend state through `@opencode-ai/sdk/v2` and existing sync contexts. It does not spawn or discover `hya-backend`, maintain a second HTTP/SSE client, or read database schema directly.
- Existing `E2eEnvBuilder`, `BackendProcess`, `FakeLlm`, Skill fixtures, MCP echo fixtures, command catalog, Tool registry, Workflow control, and Herdr operations are reused. No parallel harness is introduced.

## R2. Private runtime and secret transfer

### Runtime isolation

Create one private run root outside the playground worktree. Give it a private `HOME` and private XDG roots. Hya SQLite databases and credential-bearing configuration live under a mode-0700 directory with files mode 0600. Keep the benchmark worktrees separate from this run root.

The backend receives a self-contained Hya configuration. The OMP route is read once by the controlling process. Its `apiKey` string is transferred in-process into Hya's private `config.yaml` as an inline value. The launched Hya process receives none of the following:

- the OMP config path;
- an OMP key environment variable;
- an external Hya auth token.

The semantic configuration is:

```yaml
default_model: 12th-oai/gpt-5.6-sol
providers:
  12th-oai:
    kind: openai-response
    base_url: https://api.12th.day/v1
    api_key: <copied-secret>
    models:
      - id: gpt-5.6-sol
        reasoning:
          default: high
          variants: [low, medium, high, xhigh, max]
      - id: glm-5.3
        reasoning:
          default: high
          variants: [low, medium, high, xhigh, max]
      - id: hya-nonexistent-<nonce>
permission:
  model: default
  rules: <narrow scenario-specific rules>
subagents:
  max_depth: 2
  max_concurrency: 4
  per_run_budget: 16
  per_team_turn_budget: 32
  per_team_message_budget: 32
```

`<copied-secret>` and `<nonce>` are schema notation, not unresolved values in runtime files. The secret is copied without entering any logged command. The ghost model is configured so one real request reaches `https://api.12th.day/v1` and the service rejects the nonexistent model. A separately locally unconfigured model must produce `ProviderError::UnknownModel` with zero outbound traffic.

### Secret controls

- Never print, echo, hash in a public report, or copy the credential into argv, logs, Events, prompts, patches, Docker mounts, Herdr state, or benchmark artifacts.
- Verify that the OMP source file hash is unchanged after setup and teardown.
- Keep only a redacted configuration template and the source-file hash after final cleanup. Remove the literal private test copy and all private runtime databases/configuration.
- Relay records contain no request or response body and no headers. Evidence scrubbing is applied before retaining artifacts.

## R3. Provider routes, source binaries, and counted live traffic

### Provider identities

The requested route is provider `12th-oai`, an OpenAI Responses route at `https://api.12th.day/v1`, with model IDs `gpt-5.6-sol` and `glm-5.3`. Both advertise reasoning efforts `low`, `medium`, `high`, `xhigh`, and `max`. The selected default is `12th-oai/gpt-5.6-sol`.

Live model requests occur only after deterministic coverage and the relay boundary are ready. The relay may be placed at a private localhost base URL during counted runs. It forwards to `api.12th.day`, but it does not alter model payloads or tool schemas.

### Request-counting relay

The relay is a process-scoped safety boundary:

1. Increment the ordinal before forwarding each request.
2. Record only ordinal, timestamp, path, HTTP status, Tool schema names, and reasoning effort.
3. Never store headers or request/response bodies.
4. Fail closed before forwarding request `2,001`.
5. Treat `2,000` forwarded requests as a hard global maximum, not a target.

A failed closed request is not sent to the provider. The ledger records the refusal without the secret. Do not force malformed streams, TLS faults, or throttling against production. Use deterministic loopback/fault services for those seams.

### Live and deterministic split

Deterministic `FakeLlm` and loopback fault services first cover exact Tool calls, command expansion, failures, API transitions, and recovery. Real GPT and GLM requests then cover organic coding, reasoning, TUI, subagent, and Workflow behavior. If a real model ignores an explicit custom-resource command while deterministic coverage passes, classify it as model adherence. One tighter nonce-bearing prompt is allowed within the global cap. Do not change Hya merely to force a model Tool choice.

Send one configured ghost-model request for a real remote model-not-found result and one invalid-credential request for real authentication rejection. Keep local `UnknownModel` traffic-free. Do not spend production requests trying to force malformed SSE, TLS failure, or throttling.

## R4. Backend, API, Session, persistence, and side planes

### CLI and transport matrix

Exercise current-source behavior for:

- `--help`, `--version`, `models`, `agent list`, `bundle list`, `bundle info`, `workflow list`, `workflow info`, `workflow state`, `sessions`, `tail-session`, and `auth list`;
- `exec` text mode, `exec --json`, Compat `run`, prompt/goal mode, `serve`, and JSONL `rpc`;
- both in-memory and persistent SQLite stores;
- Session create, list, replay, resume, fork, compact, summarize, abort, delete, and missing-Session paths;
- native and Compat synchronous and asynchronous prompt paths, SSE/global Event paths, permission and question paths, shell/command/file/project/VCS/MCP/PTY paths, and the frontend TUI routes;
- invalid JSON and IDs, duplicate prompt IDs, busy/conflicting Sessions, cancellation, provider errors, client disconnect, and restart recovery.

The native, legacy, and V2 surfaces must agree on typed Session/command/error behavior where their contracts overlap. A V2 command request without `text` uses the command catalog expansion. A native `/sessions/:id/command` request deliberately stores the literal slash because it does not call `command_catalog::expand_prompt`. An explicit `text` bypasses expansion on every route.

### Durable failure contract

A background provider or Workflow failure must publish one bounded durable error/status Event, return the Session to idle, and leave that Session usable for a later Turn. No `let _ = run_turn(...).await` path may silently discard the failure. The canonical Event/SQLite state is the proof; a client disconnect or process stderr line alone is not enough.

SSE and global Event observations must correlate to the same persisted envelope and sequence. On restart, recover the documented nonterminal state and verify replay/resume without executing a stale operation a second time. Permission, question, shell, command, file, project/VCS, MCP, PTY, and TUI routes must preserve the interaction identity through request, Event, replay, and answer.

## R5. Herdr-owned TUI and capture architecture

### Pane ownership

Create a real Herdr **pane** (the user calls it a panel) in `/home/yanweiye/Projects/hya-playground`. Run the current-source `hya` as an ordinary interactive pane process. Do not treat Hya as a verified `herdr agent --kind` target. Use Herdr text and key injection for final interactive evidence.

Use this control sequence, substituting only runtime values:

```text
herdr pane split --current --direction right --ratio 0.5 --cwd /home/yanweiye/Projects/hya-playground --no-focus
herdr pane run <pane-id> <current-source-hya-command>
herdr pane send-text <pane-id> <text>
herdr pane send-keys <pane-id> <enter|esc|ctrl+p|arrows|...>
herdr pane wait-output <pane-id> --match|--regex <value> --source visible|recent-unwrapped
herdr pane read <pane-id> --source visible --format ansi
herdr terminal session observe <pane-id> --cols <n> --rows <n>
herdr pane process-info --pane <pane-id>
herdr pane close <pane-id>
```

Parse the new pane ID from `.result.pane.pane_id`. The task owns only this new pane. Close only it. Never stop the Herdr server. Herdr has no PNG screenshot command; retain visible text and ANSI terminal frames. Exact rows and columns require the single-owner `terminal session control` stream. `pane resize` changes only split ratio. Mouse-click injection is unsupported, so all acceptance interactions use keyboard navigation.

### TUI data and focus boundary

The TUI uses the existing SDK and sync contexts. SSE is the primary live path. Descendant observation is read-only. On return, focus Main first, wait for observable Main-focus evidence, then hydrate pending Permission and Question rows once through `sdk.client.permission.list()` and `sdk.client.question.list()`. Filter rows to Session IDs in the current run tree. If the run tree is unavailable, return focus to Main without a hydration request. A failed hydration preserves existing state and shows a contextual error toast. Do not add a timer, poller, or second client.

### Rendering and interaction matrix

Capture startup, loading, offline/error views, composer focus, multiline submit, busy/idle, streaming text, streaming reasoning, Tool cards, permissions, questions, toasts, abort, reconnect, exit, and terminal restoration. Cover wide and narrow dimensions, scrolling, transcript stability, Tool and reasoning fold/unfold before/during/after streaming, status counters, and usage display when supplied.

Verify child observation and read-only input isolation. Escape returns ownership to the Main composer; wait for focus evidence before sending the next semantic input. A descendant Permission or Question remains pending until it renders in Main and is answered exactly once.

### Command discovery and dispatch

Ctrl+P opens the keymap-only command palette. Verify filter, navigation, cancel, execute, route-scoped omission, and hidden/disabled omission. User command files, Skill-derived rows, plugin Tools, and MCP Tools must be absent from Ctrl+P. Slash autocomplete is the discovery surface for server commands. It excludes `source="skill"`; Skills remain available through `/skills`.

Exercise every source-defined local slash and alias:

- `/sessions` and `/resume`, `/continue`;
- `/new` and `/clear`;
- `/models` and `/mo`;
- `/agents`, `/mcps`, `/variants`, `/status`, `/themes`, `/help`;
- `/exit`, `/quit`, `/q`;
- `/editor`, `/skills`, `/diff`, `/rename`, `/timeline`, `/fork`;
- `/compact`, `/summarize`, `/undo`, `/redo`;
- `/timestamps`, `/toggle-timestamps`, `/thinking`, `/toggle-thinking`;
- `/copy` and `/export`.

Exercise backend-provided `/init`, `/review`, `/workflow`, `/model`, and `/think` through the discovered command catalog. Configuration commands must use local/server control state and must not consume an ordinary model round:

- exact no-argument `/model` opens the existing `DialogModel`, equivalent to `/models`;
- exact no-argument `/think` opens the existing `DialogVariant`, equivalent to `/variants`;
- `/agents` and `/mcps` open their existing pickers;
- `/workflow` list/info/select/run/state uses shared Workflow control;
- argument-bearing backend commands keep their existing catalog path.

If the current source admits exact `/model` or `/think` as a literal model prompt, this is a deterministic RED candidate. A fixed exact command has no `CommandExecuted` Event and no provider round. `/workflow state` also has no provider round.

Commands absent from the source-defined lists are sent once as unknown slash text and must reach the ordinary prompt unchanged. Do not add a slash registration. TUI `!` shell mode runs only inside the playground and remains distinct from command-template expansion. OAuth login/logout are CLI/dialog error-path checks, not shipped slash commands.

### Persistence and restart

Restart the backend/TUI at documented boundaries and prove persistence for model and effort, Agent, prompt history/frequency/stash, Workflow selection, and Session state. Command and Skill slash names are bootstrap-cached: an existing command body change expands immediately for its known name; a newly added name is ordinary prompt text until TUI restart; a removed known name reaches command transport and then falls back to literal slash text. Dynamic Skills refresh for the next root Turn in the backend, but slash metadata refresh requires TUI restart.

## R6. Canonical builtins and Tool contracts

### Registry

The advertised builtins are exactly:

```text
read, ls, glob, find, grep, lsp, skill, webfetch, websearch, todowrite,
write, edit, apply_patch, shell, bash, question, task, list_agents, plan_exit,
invalid, ask_user, roster, send, announce, channels, join, leave, workflow
```

The hidden aliases are exactly `fetch`, `search`, `todo`, `patch`, and `plan`. Do not advertise or invent another Tool.

For every Tool, run one successful observable contract and each applicable boundary failure. The same Session must recover on a later call and a later Turn. The exact call Event sequence is:

```text
ToolInputStart
  -> zero or more Tool input deltas
  -> ToolCallRequested
  -> exactly one ToolResult OR exactly one ToolError
```

The next provider request must contain the structured failure or result. A rejected child may still produce the normal parent Turn's typed Tool-error Event; a rejected child/session/member/roster state must not be created.

### Tool coverage boundaries

- **Files and search:** pagination, truncation, BOM, binary/media, missing file, no matches, invalid regex, file-versus-directory input, and external-directory refusal.
- **Edits:** create, update, delete, move, missing/multiple match, formatter/LSP diagnostics, and denied edit with bytes unchanged.
- **Shell and bash:** permission once/always/reject, feedback, exact remembered permission subject, timeout, cancellation, output truncation, nonzero exit, and unavailable command.
- **Web:** loopback HTML, text, image, redirect, status, timeout, and oversize response; one optional public smoke.
- **LSP and formatter:** deterministic success fixture, missing server, diagnostics, and formatter failure.
- **Skill and todo:** next-provider-request Skill injection; todo create, transition, clear, and invalid input.
- **Interaction:** rich `question` selection/free text, legacy `ask_user`, and dropped reply plane.
- **Control:** `plan_exit`, the `invalid` Tool success payload, and unknown-Tool structured error.
- **Subagent/team/Workflow:** the full R10 and R11 scenarios, including admission, lifecycle, mailbox, run, and recovery evidence.

### Provider schema filtering

Inspect sanitized outbound Tool schema names. The GPT route advertises 26 builtins with `write` and `edit` absent and `apply_patch` present. The GLM route advertises all 28 builtins. The schema snapshot itself contains no credential or prompt secret.

## R7. User slash commands and dynamic Skill/plugin/MCP resources

### Fixtures and interface distinction

Custom Skills and custom Tools are different interfaces. Create these deterministic fixtures in the isolated playground and process-E2E workdir:

```text
.hya/skills/user-playbook/SKILL.md
.opencode/commands/use-skill.md
.opencode/commands/use-plugin.md
opencode.jsonc -> commands.use-mcp
.opencode/commands/nested/inspect.md
.hya/plugins/toolbox/plugin.toml + plugin.py JSON-RPC fixture declaring Tool remember
Hya config MCP server echo declaring Tool mcp__echo__ping
```

The command bodies are exact action prompts:

- `/use-skill <nonce>` tells the Agent to call builtin `skill` with `name="user-playbook"`, then return the Skill-body marker and nonce.
- `/use-plugin <nonce>` tells the Agent to call plugin Tool `remember` with `value=<nonce>`, then return the plugin result.
- `/use-mcp <nonce>` tells the Agent to call `mcp__echo__ping` with `msg=<nonce>`, then return `echo:<nonce>`.
- Direct `/user-playbook <arguments>` and `/skills` selection prove the Skill-as-command path. It expands the Skill body without a redundant `skill` call.
- `/nested/inspect` proves recursive command naming and autocomplete.

A Skill-backed slash command admits the Skill body directly as the prompt. It does not prove that builtin `skill` ran. A plugin or MCP Tool is a model Tool schema, not a static slash command. An explicit command body is used to make the model invoke that Tool.

### Catalog source precedence and expansion

At catalog and route seams, support the existing sources and this resolution order:

1. builtins are the baseline;
2. project inline `command` and `commands` entries from all four project `opencode.json{,c}` locations;
3. project `.opencode/command` and `.opencode/commands` Markdown roots, including recursive names;
4. Skill-backed commands only when no command owns the name.

Project command declarations override a same-name builtin. A later project command overrides an earlier same-name project/builtin declaration according to the existing catalog precedence. A command wins over a same-name Skill. Do not reinterpret this as last-writer-wins for Tool resources.

The catalog must list every fixture with exact source, template, and hints, with no duplicate name. Legacy and V2 Compat command POSTs without `text` store the exact expansion and one correlated `CommandExecuted` Event. Native `/sessions/:id/command` stores the literal slash because it intentionally bypasses `command_catalog::expand_prompt`. Explicit `text` bypasses expansion on every route.

Preserve quoted arguments as one position, preserve multiline text in `$ARGUMENTS`, expand empty/missing positional values to empty strings, and expand `$ARGUMENTS` plus `$1` through `$10` exactly once. Replacement text containing `$1` is not expanded a second time. Cover unclosed and empty quote behavior. Malformed JSONC, malformed frontmatter, unreadable files, and non-Markdown files are omitted without a catalog crash. The existing `disable` metadata is ignored for this test. `.hya/commands` and home/global command directories are unsupported and ignored.

Existing command frontmatter `agent`, `model`, and `subtask` are listing-only. Document this behavior in `docs/configuration.md` and `docs/FOLLOWUPS.md` as required by the approved plan, but do not add routing semantics. Resource execution uses the Session's selected Agent/model.

### Skill directory order and cache

Both command/Skill slash metadata and builtin `skill` discovery reuse `skill_dirs_for_workdir` in this exact first-name-wins order:

1. project `.hya/skills`;
2. home `.config/hya/skills`;
3. home `.claude/skills`;
4. home `.config/opencode/skills`;
5. home `.config/opencode/skill`;
6. project `.opencode/skills`;
7. project `.opencode/skill`;
8. project `.agents/skills`;
9. home `.codex/skills`;
10. home `.agents/skills`.

Assert every root, duplicate first-name wins, invalid Skill frontmatter is omitted, and the three built-in Compat Skills fill only absent names. Plugin/AgentBundle runtime Skills are not projected into `/skill`, `/command`, or `/skills`; use one installed-bundle control to prove the separation.

A deleted Skill after TUI bootstrap keeps its stale name in command transport: the backend then falls back to literal slash text. A new Skill after bootstrap is ordinary prompt text until TUI restart. A Skill edit is visible on the next root Turn. Existing `TurnBinding` objects remain unchanged across a refresh.

### P18 process scenario and Track T

Register `crates/hya-e2e/tests/p18_custom_slash_resources.rs`. Reuse `E2eEnvBuilder::project_file`, `FakeLlm`, the P05 Skill fixture, and the P06 MCP echo fixture. `BackendProcess` already starts with the temporary project as current directory, so project files can supply `.hya/plugins/toolbox/plugin.toml` with `command = ["python3", ".hya/plugins/toolbox/plugin.py"]`. P18's MCP fixture sets `HYA_DEFER_SIDEPLANES=0` before the first Tool schema snapshot. No new harness adapter is needed.

The eight separately named process cases are:

1. **T1.16 `custom_slash_catalog_and_routes_expand_all_supported_sources`:** `/api/command` exposes every fixture with exact source/template/hints and no duplicate name. Cover legacy/V2 POST without `text`, correlated `CommandExecuted`, native literal-slash behavior, explicit-text bypass, four project JSON/JSONC locations, singular/plural keys, both Markdown roots, nested names, quotes, unclosed/empty quotes, missing positions, `$1`/`$10`, `$ARGUMENTS`, multiline arguments, nonrecursive replacement, malformed omission/plain-fence behavior, ignored `disable`, command-over-Skill, later-project-over-builtin precedence, and ignored `.hya/commands`/home/global roots.
2. **T1.17 `skill_backed_slash_expands_without_skill_tool_call`:** direct `/user-playbook` and `/skills` selection admit the Skill body and arguments without `ToolCallRequested { name: "skill" }`; `Action::Skill` denial does not block direct expansion. Enumerate all Skill roots and duplicate precedence. Cover deleted and newly added post-bootstrap Skills and restart behavior.
3. **T1.18 `custom_command_invokes_builtin_skill_tool`:** `/use-skill` produces a real `skill` Tool call, body-bearing `ToolResult`, next-request replay, and final answer. Unknown/missing `name`, denied permission, and unavailable permission use separate scripts. Each emits a structured error, and a later valid command succeeds in the same Session. A stale/deleted command falls back to literal slash text.
4. **T1.19 `custom_command_invokes_plugin_tool`:** `/use-plugin` exposes and calls `remember`, preserves exact RPC input/result in Tool Events and the next request, and renders the Tool card. Cover `Action::Write` rejection, malformed RPC input, process death, lazy same-declaration respawn, fail-closed declaration drift, and declaration-change visibility only after backend restart.
5. **T1.20 `custom_command_invokes_mcp_tool`:** `/use-mcp` exposes/calls `mcp__echo__ping`, asks once, replays `echo:<nonce>`, and renders the Tool card. Cover disconnected/unknown server, `isError`, malformed result, timeout, malformed/oversized frame, and post-publication process death. Death returns a structured closed error and does not auto-respawn. Recovery is explicit disconnect/connect followed by a fresh root Turn; the old in-flight binding remains pinned.
6. **T1.21 `resource_name_conflicts_fail_closed`:** duplicate plugin Tool, plugin-versus-builtin Tool, and crafted MCP namespace collisions reject the candidate runtime generation without partial publication. Command/Skill collision follows command precedence instead of runtime rejection.
7. **T1.22 `dynamic_resource_snapshots_and_reload`:** a Skill edit appears on the next root Turn, MCP reconnect publishes for the next root Turn, plugin declaration changes require backend restart, and every old `TurnBinding` remains unchanged. Newly added post-bootstrap command/Skill slash metadata is not promised before TUI restart because `sync.data.command` is bootstrap-cached.
8. **T1.23 `structured_custom_tool_errors_replay_and_session_recovers`:** one scripted resource error produces exactly one terminal Tool Event. Structured `value.error.type/message` survives canonical/API replay, TUI shows bounded error text, the Session returns idle, replay does not execute the Tool, and a later valid custom slash succeeds.

The source plan also uses the registry range notation `T1.16–T1.24` when describing the full custom-resource contract. The eight named P18 cases above define the executable cases; do not invent an unlisted ninth behavior. Reconcile any matrix label without changing these eight contracts.

Add Track T row **T3.4 `custom_resource_tui_command_transport`** to `packages/hya-tui-ts/test/pty-smoke.test.ts`. Herdr/PTY evidence must prove Ctrl+P excludes user resources, slash autocomplete includes custom commands but excludes Skill rows, `/skills` includes Skills, local/server primary-or-alias collisions render one row after deduplication, first-line command parsing preserves later lines in `arguments`, and command-request HTTP failures show a contextual toast with same-Session recovery.

While the TUI runs, add/change/remove command files to pin name caching. A changed known body expands immediately. An added name is ordinary prompt text. A removed name reaches command transport and then falls back to literal slash. TUI restart refreshes names. In the real pane, use autocomplete for `/use-skill`, `/use-plugin`, and `/use-mcp`; use `/skills` for `user-playbook`; submit quoted and multiline arguments; capture expanded user rows, permission interactions, custom Tool cards, result, recovery toast/state, and restart behavior. Run at least one custom-resource command on GPT and one on GLM.

### Dynamic resource contracts

- **MCP:** local echo connected state, namespaced call, one permission decision, remote/error/closed cases, explicit disconnect/connect recovery, next-root generation publication, old-binding pinning, and TUI visibility.
- **Plugin:** startup discovery, Tool registration/invocation, hook allow/block, permission, RPC failure, lazy same-declaration crash respawn, drift rejection, and declaration-change visibility only after backend restart.
- **AgentBundle:** valid local package install/list/info/Agent execution/local resource call/uninstall; malformed and invalid-capability rejection; bundle resources remain model Tools rather than static slash commands.
- **Skills and Workflow:** next-root Skill catalog refresh with old-binding pinning; command/Skill slash metadata and plugin declaration refresh in the TUI only after restart; Workflow uses its existing catalog revision contract.
- **Fixtures:** local loopback web service and deterministic LSP/formatter fixtures.

## R8. Errors, classifications, and recovery seams

### Provider errors

Cover every `ProviderError` variant:

- `Json`: malformed JSON frame/body;
- `Http`: typed provider `error`, `response.failed`, and bad header/client construction seam;
- `Transport`: connection refusal and deterministic transport failure;
- `HttpStatus`: bounded representative 4xx, 429 with `Retry-After`, and 5xx;
- `UnknownModel`: locally unconfigured model with zero traffic;
- `Incompatible`: route without streaming Tool-call capability and unsupported payload;
- `Decode`: malformed/truncated SSE, zero frames, and EOF/`[DONE]` without typed terminal;
- `AuthExpired`: expired/revoked OAuth seam with recovery hint.

The configured ghost model additionally proves a real remote model-not-found result. The invalid isolated credential proves real authentication rejection. Do not force production-only malformed streams or throttling.

### Tool and Session errors

Cover every `ToolError` variant:

```text
Input
Permission::Denied
Permission::Unavailable
Io
Json
Cancelled
Overloaded
OperationIdConflict
OperationAlreadyHandled
WorkflowControl
UnknownAgentId
AgentSpawnNotAllowed
UnsupportedInlineAgentField
Other
```

Cover Session not-found, busy, and conflict; invalid command/request; disconnected and reconnected TUI; MCP/plugin unavailable; Workflow stale revision, invalid graph, and missing Agent; and exhausted subagent budgets. Every error that reaches a model is structured. Every background failure has one bounded durable status/error Event, leaves the Session idle and usable, and is replayable without executing an external Tool again.

Workflow public/persisted error text is bounded to 2,048 Unicode scalar values. Syntax errors do not echo raw `key=value` assignments. Resource and Tool failures use the existing typed error surface; do not replace a typed error with a generic retry.

## R9. Reasoning, effort, and rendering

- GPT live requests must produce summary-style reasoning Events and visible TUI Thinking content.
- GLM live requests must produce full reasoning-text Events and visible TUI reasoning content.
- Switch effort through `/model`; prove UI state and sanitized outbound effort. Restart and prove documented persistence.
- Fold and unfold reasoning before, during, and after streaming. Preserve Event order and stable part keys.
- Test reasoning disabled/off deterministically and live when the route accepts it.
- If an enabled effort returns no reasoning, classify it as provider/model behavior unless Hya received a reasoning Event and lost it.

The relay records only reasoning effort, not reasoning text. Reasoning text belongs in redacted canonical/TUI evidence only when it contains no credential or private fixture secret.

## R10. Subagent, team, mailbox, and bounded spawn generations

### Scenarios

Use canonical ancestry, model route, Member lifecycle, and mailbox evidence for:

- discovery;
- foreground result;
- same-child resume;
- nested spawn to depth two;
- two-member parallel batch;
- category/model override and inline Agent identity;
- nonblocking background terminal ordering;
- resident registration, stable handle, idle/wake/idle, recovery, stop, and quiescence;
- roster, direct mail, `announce`, channel join/send/list/leave, pre-join/post-leave exclusion, and cross-unit refusal;
- depth, concurrency, run, turn, and message budget failures;
- real TUI child observation and input isolation.

At least one slice uses each real model. Mixed-model children must show the actual route in canonical Events.

### Admission and generation contract

Use the existing durable spawn admission contract. Every persisted `ToolCallId` deterministically derives one domain-separated `OperationId`. `ToolCtx`, `SpawnRequest`, and `ToolOperation` carry this immutable pair. `SpawnerPlane::with_capacity(capacity)` is a bounded Tokio channel. Foreground/background dispatch uses non-blocking `try_send`; a full queue is `SpawnError::Overloaded`, and a closed queue is `SpawnError::Unavailable`.

Runtime queue capacity derives from resolved `SubagentLimits::per_run_budget`, clamped to `1..=tokio::sync::Semaphore::MAX_PERMITS`. Do not add queue defaults `100`, `128`, or `256`. A rejected queue request never reaches the supervisor and creates no request-owned task, child Session, resident registration, or child Event.

The first durable write records operation ID, source ToolCall ID, root Session, SHA-256 request fingerprint, admission units, and `accepted` before governor debit, child/session creation, resident registration, or dispatch. `accepted` charges no capacity. Only the operation that atomically debits the governor transitions to `started`; all continuations use the pre-admitted path and do not reserve again. The store journal owns `accepted -> started -> completed|cancelled|aborted`; the in-memory governor owns only the current process debit and cancellation token.

The same operation and immutable request are idempotent. A changed immutable request fails closed with `OPERATION_ID_CONFLICT`; an identical started or terminal request returns `OperationAlreadyHandled` and never dispatches again. Terminal states are irreversible. Finalization records a logical release only for `started`, and governor release removes/credits at most one operation debit. An `accepted` overload/cancel never credits capacity. Explicit cancellation, completion, spawn failure, and root-turn cleanup use the same store-first finalizer. Root cleanup cancels operation tokens before finalization.

Before any supervisor starts, startup atomically moves all `accepted` and `started` rows to `aborted`. A recovered `started` row records a logical release for audit only and never credits a fresh governor. Recovery is repeatable and creates no child. The admission journal emits no public Event; replay remains Event-log based. A rejected child can still have the parent Turn's typed Tool-error Event.

For tests that reach `prepare_spawn_admission`, every member must be non-resident with `spawn_lifecycle: transient`. Foreground batches use durable admission at any member count. Background batches use it only with exactly one member. Multi-member background batches and any resident member stay on the legacy route. Verified bundle provenance and complete provider identities are required; missing either fails closed as `SpawnError::Unavailable`. Use existing verified catalog and identity fixture constructors. Do not add identity to the bare `FakeProvider`.

## R11. Event-sourced Workflow control and generations

### Shared control seam

The Session Event log and `hya_proto::Projection` are the only durable Workflow read model. Catalog availability and live Agent activity are derived. Do not add a Workflow table, a second reducer, or a surface-specific executor.

The shared app operation is:

```rust
pub async fn WorkflowControl::execute(
    &self,
    session: SessionId,
    invocation: WorkflowInvocation,
    command: WorkflowCommand,
    cancel: CancellationToken,
) -> Result<WorkflowCommandResult, WorkflowControlError>;
```

`WorkflowCommand` has exactly five operations:

```text
List
Info { name }
Select { name, expected_revision }
State
Run { name, expected_revision, inputs, run }
```

Use the existing store writer transactions for selection, run admission, runtime ownership, and recovery. All five commands share one catalog. CLI and Agent Tool use `WorkflowDelivery::Finished`. HTTP and slash commands use `Started`; progress arrives through Events and state reads. Server Select/Run and parent-model admission reserve the same process-local `RunRegistry`. List/Info/State remain readable while a run is active.

### Event and projection contract

Append these Events to the owning root Session:

```text
WorkflowSelected
WorkflowRunStarted
WorkflowStageStarted
WorkflowStageMemberLinked
WorkflowStageFinished
WorkflowRunFinished
```

Events store typed identity, revision, plan metadata, status, Member references, and request hash. They never store directives, input values, Stage output, or child transcripts. Projection applies Stage/member/terminal Events only to the active run ID. Member links deduplicate `(member, role, iteration)`. Terminal Stage and run states are sticky. Selection changes only `SessionProjection.workflow.selection` and preserves the complete message vector and canonical Member state.

A model Tool invocation carries the original `ToolOperation`, actor claim, `TurnBinding`, and caller. `WorkflowRunId` derives from the operation unless a stable ID is supplied. Admission fences the actor claim, compares prior run ID/hash, rejects another active run, and appends `WorkflowRunStarted` in one `BEGIN IMMEDIATE` transaction. Publish only after commit. Every lifecycle Event first emits normal `session.updated` invalidation and then the raw envelope.

### Runtime ownership and recovery

Call `claim_runtime_owner` before Workflow recovery. A file-backed store holds an exclusive mode-0600 `<canonical-db>.runtime-owner.lock` until the final clone drops it. Reject symbolic-link lock paths. Do not use heartbeat or TTL. Recovery requires the matching held owner claim, appends one `Interrupted` terminal Event for each prior nonterminal run, and never replays a Stage.

`WorkflowProjection.availability` is runtime-only and absent after replay. For the exact source ID, name, and revision: unchanged valid source is `available`; changed or invalid exact source is `stale`; missing exact source is `unavailable`. Compat Session hydration calls the app decoration port once. SDK activity joins Workflow Member references to canonical Member projections and excludes unrelated run-tree Members. Old `TurnBinding` snapshots remain pinned when a catalog/resource generation changes.

### Workflow matrix and errors

Use playground `.hya/workflows` fixtures for catalog precedence/revision, info, select/state persistence, stale revision rejection, Session binding, fan-out/fan-in with parallel Stages, child links, deterministic join sections, durable statuses, TUI presentation, fail-fast, collect-all, Session-abort cancellation, idempotent replay/retry, changed-input operation conflict, invalid graph, missing Agent, and failed Stage.

Use these stable error mappings:

| Condition | Stable code | HTTP status |
| --- | --- | ---: |
| Invalid slash syntax | `WORKFLOW_SYNTAX` | 400 |
| Invalid source or inputs | `WORKFLOW_INVALID_SOURCE` / `WORKFLOW_INVALID_INPUT` | 422 |
| Missing Session/source/selection | `SESSION_NOT_FOUND` / `WORKFLOW_NOT_FOUND` / `WORKFLOW_NOT_SELECTED` | 404 |
| Unauthorized Stage or verifier Agent | `WORKFLOW_UNAUTHORIZED` | 403 |
| Busy, stale revision, or changed immutable request | `WORKFLOW_BUSY` / `WORKFLOW_STALE_REVISION` / `WORKFLOW_OPERATION_CONFLICT` | 409 |
| Runtime fingerprint unavailable | `WORKFLOW_RUNTIME_UNAVAILABLE` | 503 |
| Store/internal failure | `WORKFLOW_INTERNAL` | 500 |
| Another process owns database | `RUNTIME_OWNER_BUSY` | startup failure |
| Recovery lacks matching claim | `RUNTIME_OWNER_CLAIM_REQUIRED` | startup failure |

A governed Stage failure is a successful transport response with a terminal failed run. Public/persisted error text is bounded to 2,048 Unicode scalar values.

Repair process coverage by adding the existing P17 Workflow test and T2.12 mailbox contract to `crates/hya-e2e/matrix.toml`, and update stale `docs/testing/process-e2e.md` coverage text. This is registry/documentation repair, not a new execution framework.

## R12. Real coding capability and fixtures

Prepare small isolated Rust and TypeScript fixtures. For each real model, use a fresh Session and the sequence inspect -> diagnose -> edit -> focused test -> report. Accept only observed disk diffs and command results.

- GPT must use `apply_patch`, because its route filters out `write` and `edit`.
- GLM must exercise `write`, `edit`, and one patch path.
- Include a compile/type error, failing test, lint/format issue, merge-conflict-like file, large search, binary file, network-dependent request, denied action, human question, aborted Turn, resume, and post-error recovery.

The fixture remains isolated from the private credential runtime. A successful model answer without the corresponding disk bytes, Tool Event sequence, and command result is not coding proof.

## R13. SWE-Bench Pro diagnostic pipeline

### Pinned assets and sample

Use the authoritative evaluator and dataset:

- Evaluator: `scaleapi/SWE-bench_Pro-os@ca10a60a5fcae51e6948ffe1485d4153d421e6c5`.
- Dataset: `ScaleAI/SWE-bench_Pro@7ab5114912baf22bb098818e604c02fe7ad2c11f`.

The dataset is public/ungated with 731 rows. The evaluator is MIT. Dataset metadata has no declared license. Do not redistribute dataset rows, repository snapshots, or images.

Use seed `hya-swebench-pro-live-8x2-v1`. Select two rows each from Go, JavaScript, Python, and TypeScript. Within each language, sort by `sha256(seed + NUL + instance_id)` and prefer a second repository. Eligibility requires only nonempty identity/base/image fields and evaluator asset existence. Before any model request, freeze selected IDs, row hashes, prompt hashes, base commits, and Docker image digests.

Prompt bytes are exactly publisher `problem_statement`, then `Requirements:`, then `requirements`, then `New interfaces introduced:`, then `interface`. Keep the gold patch, test patch, test names, scripts, and evaluator-only fields outside Hya worktrees and mounts.

### Attempt data flow

For each selected instance:

1. Create a fresh detached worktree at `base_commit`.
2. Run exactly one Hya task Turn. Do not retry, resume, hint, or share a patch.
3. Backend surface is `hya-backend --model 12th-oai/gpt-5.6-sol --yolo --db <private> exec --json <prompt>`.
4. TUI surface starts current-source `hya` in a fresh Herdr pane. Approve requested actions without task hints. Exit only at idle.
5. Capture all text-file changes as a full-index patch. Reject binary patches. Verify application against a separate clean base.
6. Run backend and TUI predictions in separate official local-Docker evaluator invocations because results key only by `instance_id`.
7. Read Booleans from `eval_results.json`. Process exit status alone is not a score.

Retain prompt, binary, patch, and test hashes; source commit; Session DB/Event JSONL; TUI export; stderr/status; permission decisions; base HEAD/status; Docker tag/digest/image/platform; evaluator patch/entry script/stdout/stderr/output; token usage; and start/end timestamps.

A crash, empty/invalid patch, unanswered content question, dirty base, evaluator exception, or missing required test is false, not omitted. Results are diagnostic Pass@1, not official leaderboard reproduction. The fixed scope is eight blind-selected instances and 16 independent GPT runs: eight backend and eight Herdr/TUI. GLM receives the complete functional, slash-resource, reasoning, subagent, Workflow, and coding suite but is not added to the fixed SWE-Bench count.
The 2026-08-30 user-approved scope decision supersedes the remaining GLM functional/coding/subagent/Workflow/reasoning execution obligation for this task. Completed GPT-only validation is accepted for finalization; this is not a GLM pass. The historical successful GLM custom-MCP Turn and later upstream HTTP 503 classification remain preserved evidence.

If a frozen row lacks a required pinned evaluator asset or Docker image during preflight, mark that row blocked and do not substitute another after model execution begins.

## R14. Deterministic-first repair loop and source-audit candidates

### Repair loop

For every newly reproduced Hya defect:

1. classify the boundary and the observed failure;
2. add a deterministic RED behavior test at the existing seam;
3. apply the smallest source fix;
4. run focused GREEN evidence;
5. rerun the original scenario;
6. run relevant cross-layer checks;
7. update version/changelog under R15.

Do not add a generic retry, suppress a symptom, special-case an input, or force model behavior. Do not rerun an expensive slice that already passed unless the changed boundary requires it. Unknown YAML keys and documented offline fallback are existing behavior; do not change them without a deterministic RED.

### Seven source-audit candidates

These candidates are source-grounded and are not live defects until RED behavior proves them:

1. **Responses false success.** `OpenAiResponsesProtocol` constructs a permissive decoder. EOF or `[DONE]` without `response.completed` or `response.incomplete` may close as success. Add RED coverage so OpenAI Responses and Grok reject an untyped terminal/EOF while Chat Completions remains unchanged. Likely files: `crates/hya-provider/src/openai/responses.rs`, `response_decoder.rs`, and `crates/hya-provider/tests/http_headers.rs`.
2. **Custom V2 background error loss.** `crates/hya-server/src/compat/session_prompt.rs` spawns `run_turn...` and discards its `Result`, while the older `/prompt_async` path publishes error/status Events. RED: provider failure on `/api/session/:id/prompt` emits a Session error, returns idle, and permits a later Turn. Reuse the existing async publication seam.
3. **Coverage registry drift.** P17 Workflow lacks a matrix row, T2.12 cross-unit refusal lacks a contract row, and process docs still say P01–P16. Add exact registry/docs entries and verify matrix consistency.
4. **Custom slash-resource coverage gap.** Existing metadata/Skill expansion, P05 direct Skill, P06 direct MCP, and plugin direct `remember` coverage do not prove slash admission -> expanded prompt -> user Skill/plugin/MCP Tool -> replay -> recovery. P18 and Track T3.4 close this verification gap. This is missing verification, not a known runtime defect.
5. **Exact `/model` and `/think` misroute.** Frontend registers `/models` and `/variants`; server advertises `/model` and `/think`. Add PTY RED for exact no-argument dialogs with no `CommandExecuted` and no provider round. Add `model` and `think` to existing local aliases. In `autocomplete.tsx::commands`, compute names claimed by local primaries/aliases and omit colliding server rows. Argument-bearing server submission remains unchanged.
6. **`/workflow` not TUI-discoverable.** Routes intercept `command="workflow"`, but the catalog does not publish it, so TUI submission falls through to a prompt. Add catalog/PTY RED. Publish `command_info("workflow", "inspect or run workflows", "/workflow $ARGUMENTS", vec!["$ARGUMENTS"], None)` and keep `workflow::intercept_slash` as the only executor.
7. **Command request errors silent.** `submitInner` discards `sdk.client.session.command(...)` and does not inspect a nonthrowing error response. Add PTY RED using a deterministic command-route failure. Await with `{ throwOnError: true }` and show `toast.show({ title: "Failed to run command", message: errorMessage(error), variant: "error" })`. Preserve submitted history and prove same-Session recovery.

The source-audit fixes must preserve command/Skill precedence, old binding pinning, and all unsupported behaviors in this design.

## R15. Version, changelog, and source-change policy

This design file does not edit product source. During later repair phases, every source fix follows failing-test-first. A product source change updates the workspace version and the single-version root changelog, and archives the old changelog. Feature changes commit and push only after project gates and the repository rules in `AGENTS.md` permit them. Planning artifacts and evidence do not trigger a product version bump.

Use existing backend/frontend ownership boundaries, command catalog, SDK/sync contexts, Event reducers, process fixtures, and Workflow control. Do not add command-specific Agent/model/subtask routing; those fields remain listing-only. Do not add a test-only hook, alternate frontend, generic retry, second client, or new E2E framework.

## R16. Cleanup, rollback, and operational safety

### Cleanup ownership

After evidence capture and final checks, stop backends, task-owned Herdr panes, relay, loopback fixtures, Docker containers, MCP/plugin processes, and watchdogs. Close only the pane created by this task. Never stop the Herdr server. Remove only secret and private runtime artifacts. Preserve benchmark evidence and redacted hashes. Verify the OMP source hash is unchanged.

The private run root, literal test credential, SQLite databases, Hya config, relay body buffers, and temporary fixtures are cleanup targets. Public benchmark metadata, source/evaluator/image digests, Event JSONL, ANSI captures, full-index patches, and redacted reports are evidence targets. Do not delete unrelated user work, pre-existing untracked files, or another Herdr pane.

### Rollback boundaries

- The relay fails closed at request 2,001; it never retries or forwards over the cap.
- A deterministic RED change that fails focused GREEN is reverted at its own source boundary before any live rerun. Do not hide the failure with a retry or fallback.
- A live model/provider failure is classified first. Do not make a source change unless a deterministic RED reproduces an Hya defect.
- A frozen SWE-Bench row blocked during preflight stays blocked; do not substitute it after model execution starts.
- A crash, unanswered question, invalid patch, evaluator exception, or missing test is a false result, not a reason to rerun the same attempt.
- MCP process death returns a structured closed error and does not auto-respawn. Recovery requires explicit disconnect/connect and a fresh root Turn. The old in-flight binding remains pinned.
- Workflow recovery uses the held runtime-owner lock and appends one `Interrupted` Event without replaying a Stage. Repeated recovery changes no rows.
- Durable spawn recovery marks accepted/started rows `aborted` before supervisor startup and never resumes an external effect.
- If a command or Skill name is stale, use the documented literal-slash fallback. Do not add a catalog error or hidden registration.

## R17. Final GPT-5.6-family scheduling acceptance

R17 passed in a live run on release `0.36.5` after the 2026-08-30 user-approved GLM scope waiver. It remains applicable to current final release `0.36.6` because only TypeScript Session-cache lookup/test code and release metadata changed; scheduling-sensitive source is unchanged. The waiver is not a GLM pass: the successful historical custom-MCP Turn and later upstream HTTP 503 classification remain evidence, while completed GPT-only validation is accepted for finalization.

The category chain used preferred `gpt56-primary/gpt-5.6-sol#low` and ordered fallback `gpt56-fallback/gpt-5.6-sol#high`. The preferred deterministic local route produced three pre-stream HTTP 503 attempts before the fallback. The fallback used `reasoning_effort=high`, returned HTTP 200 at relay ordinal 2, and delivered the exact response `GPT56_SCHEDULED_GREEN_A30`.

The proof shows:

1. All three preferred attempts occurred before stream creation and before the ordered fallback.
2. The engine advanced before any Event stream; no mid-stream replay occurred.
3. One Session (`hysec_3xMXopbQV0NsMGHScP0J`) persisted 14 Events with matching stdout/SQLite sequence identity and one assistant delivery.
4. The focused regression was RED before the fix for Low versus High (`artifact://1810`), then GREEN at 1/1; the complete `model_fallback` suite passed 7/7.
5. The mode-0600 private `provider.key` was the only credential-scan hit, Git evidence had zero secret hits, and retained runtime evidence is local-only.

Evidence is retained in `evidence/gpt56-model-scheduling.json`. R17 and AC-FINAL-16 are passed. Overall acceptance is `passed-with-user-approved-GLM-waiver`; final project gates, cleanup, and the Trellis quality review passed on current release `0.36.6`, so the worktree is ready to commit.

## Execution-facing command contracts

The following exact commands are part of the approved implementation/evidence contract. They are recorded here for the implementation phase; they are not run while creating this design:

```sh
cargo test -p hya-server --test compat_command_metadata_api
cargo test -p hya-server --test compat_session_api compat_session_command_without_text_uses_skill_template_body
cargo test -p hya-server --test compat_session_v2_api compat_v2_session_command_without_text_uses_skill_template_body
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e --test p18_custom_slash_resources -- --test-threads=1
cargo xtask matrix-check
(cd packages/hya-tui-ts && bun test test/pty-smoke.test.ts)
```

For the live UI proof, start the private backend/config first. In the task-owned Herdr pane, select `user-playbook` through `/skills`; submit `/use-skill SKILL_<nonce>`, `/use-plugin "PLUGIN <nonce>"`, and the multiline command `/use-mcp MCP_<nonce>\nsecond-line`. Expected observables are expanded user text rather than the literal template, one correlated custom Tool card per command, `SKILL_BODY_<nonce>`, plugin `remember` output, MCP `echo:MCP_<nonce>`, a structured error card for the forced failure, a later success in the same Session, and the same commands after backend/TUI restart. Switch `/model` between GPT and GLM slices and retain Event/model IDs with ANSI frames.

The full release gate, after all work is complete, is:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude hya-e2e
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e -- --test-threads=1
(cd packages/hya-tui-ts && bun run typecheck)
(cd packages/hya-tui-ts && bun test)
cargo xtask matrix-check
```

The implementation built local `hya`, `hya-ts`, and `hya-backend` executables, completed the real Herdr proof, and completed R16 cleanup. Current gate and cleanup results are recorded in `evidence/final-verification.json`; the commands above remain the design-time contract.
