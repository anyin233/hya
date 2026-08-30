Source: `research/approved-plan.md` lines 1-218. The exact source file remains authoritative.

# Hya complete coding-agent validation and repair plan

## Context

Task: prove and repair current-source Hya as a complete coding agent through its backend and TypeScript TUI, including user-defined slash commands that make the agent invoke project Skills, plugin Tools, and MCP Tools. Required live routes: `12th-oai/gpt-5.6-sol` and `12th-oai/glm-5.3` at `https://api.12th.day/v1`. Required interactive driver: a real Herdr pane. Required coding benchmark: official SWE-Bench Pro.

The source audit covered the TUI, backend/provider, 28-tool registry, custom command catalog, Skills, plugin/MCP registration, subagents/Workflow, Herdr CLI, and official SWE-Bench Pro assets. The audit sent no live provider request and changed no product code.

The Trellis task stub is `.trellis/tasks/08-29-full-agent-live-validation`. Before source edits, persist this execution spec into its `prd.md`, `design.md`, `implement.md`, research, and curated manifests; validate it; then run `task.py start`.

## Confirmed current state

- Current package version: `0.36.0`.
- Current local tools: Docker `28.5.1`, Bun `1.3.10`, Rust/Cargo `1.92.0`, Herdr `0.8.1`.
- `/home/yanweiye/Projects/hya-playground` does not exist. Execution will create it.
- OMP model registry: `/home/yanweiye/.omp/agent/models.yml`.
- OMP route `12th-oai`: OpenAI Responses, base URL `https://api.12th.day/v1`, models include `glm-5.3` and `gpt-5.6-sol`.
- Both requested models advertise reasoning efforts `low`, `medium`, `high`, `xhigh`, and `max`.
- The OMP credential is an `apiKey` string. Its value was not printed or copied during planning.
- CodeGraph is not initialized for this repository. Planning used targeted source/test reads. Do not create an index without the user’s separate approval.

## Decisions already made

1. **Current source only.** Build `hya`, `hya-ts`, and `hya-backend` from this worktree. Installed binaries cannot satisfy acceptance.
2. **Isolated runtime.** Use a private run root outside the playground workdir with private `HOME` and all XDG roots. Hya’s SQLite databases and credential-bearing config use mode 0600 under a mode-0700 directory.
3. **Self-contained Hya config.** Transfer the OMP `apiKey` in-process into Hya’s own private `config.yaml` as an inline literal. The launched Hya process receives no OMP config path, key environment variable, or external Hya auth token. This proves runtime independence from OMP.
4. **Secret boundary.** Never print the key or put it in argv, logs, Events, prompts, patches, Docker mounts, Herdr captures, or reports. Verify the OMP source file hash is unchanged. Remove the literal test copy during final cleanup and retain only a redacted template/hash.
5. **Evidence first.** Model prose is never proof. Use canonical Events/SQLite, HTTP/SSE observations, disk diffs, process exit status, focused command output, and Herdr pane captures.
6. **Deterministic before live.** Use FakeLlm/loopback fault services to cover exact tools and failures. Use the real models for organic coding, reasoning, TUI, subagent, and Workflow proof. Model non-compliance is not automatically a Hya defect.
7. **Root-cause repair.** Each Hya defect gets a deterministic RED behavior test, the smallest fix, focused GREEN, and the original scenario rerun. No generic retry or symptom suppression.
8. **TDD/release rules.** Every source fix follows failing-test-first. Product changes update workspace version and the single-version root changelog; archive the old changelog. Feature changes commit/push only after project gates, as required by `AGENTS.md`.
9. **Benchmark honesty.** SWE-Bench results are diagnostic Pass@1, not official leaderboard reproduction. Failures count; they are not omitted.

## Self-contained configuration

Create one private Hya config with this semantic shape; `<copied-secret>` is written without entering any logged command:

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

The ghost model is configured only so Hya sends one real request that the remote service must reject as nonexistent. A separate locally unconfigured model verifies `ProviderError::UnknownModel` with zero outbound traffic.

A private localhost relay may be inserted as the configured base URL during counted live runs. It forwards to `api.12th.day`, increments before forwarding, records only ordinal/time/path/status/tool-schema names and reasoning effort, and never stores headers or request/response bodies. It fails closed before forwarded request 2,001.

## Approach

### 1. Backend and API

Exercise current-source behavior for:

- `--help`, `--version`, `models`, `agent list`, `bundle list/info`, `workflow list/info/state`, `sessions`, `tail-session`, `auth list`.
- `exec` text, `exec --json`, Compat `run`, prompt/goal mode, `serve`, and JSONL `rpc`.
- In-memory and persistent SQLite stores.
- Session create/list/replay/resume/fork/compact/summarize/abort/delete/missing.
- Native and Compat synchronous prompt, asynchronous prompt, SSE/global Event, permission, question, shell, command, file, project/VCS, MCP, PTY, and TUI routes used by the frontend.
- Invalid JSON/IDs, duplicate prompt IDs, busy/conflicting Sessions, cancellation, provider errors, client disconnect, and restart recovery.

Required invariant: background provider/Workflow failures produce a bounded durable error/status Event and leave the Session usable. No `let _ = run_turn(...).await` path may silently discard a failure.

### 2. Real Herdr TUI

Create a real Herdr **pane** (the user’s “panel”) in `/home/yanweiye/Projects/hya-playground`, run current-source `hya` as an ordinary interactive pane process, and use Herdr text/key injection rather than a synthetic TUI test for final interactive evidence. Hya is not a verified `herdr agent --kind` target.

Verified control sequence:

```text
herdr pane split --current --direction right --ratio 0.5 --cwd <playground> --no-focus
herdr pane run <pane-id> <current-source-hya-command>
herdr pane send-text <pane-id> <text>
herdr pane send-keys <pane-id> <enter|esc|ctrl+p|arrows|...>
herdr pane wait-output <pane-id> --match|--regex <value> --source visible|recent-unwrapped
herdr pane read <pane-id> --source visible --format ansi
herdr terminal session observe <pane-id> --cols <n> --rows <n>
herdr pane process-info --pane <pane-id>
herdr pane close <pane-id>
```

Parse the new pane id from `.result.pane.pane_id`. Capture visible text and ANSI terminal frames; Herdr has no PNG screenshot command. Exact rows/columns require the single-owner `terminal session control` stream; `pane resize` changes only the split ratio. Mouse-click injection is unsupported, so all acceptance interactions use keyboard navigation. Close only the pane created by this task; never stop the Herdr server.

Verify:

- startup, loading/offline/error views, composer focus, multiline submit, busy/idle, streaming text, streaming reasoning, tool cards, permissions, questions, toasts, abort, reconnect, exit, and terminal restoration;
- wide/narrow resize, scroll, transcript stability, tool and reasoning fold/unfold before/during/after streaming, status counters, and usage display when supplied;
- child observation/read-only input isolation and return to the owner composer;
- Ctrl+P opens the keymap-only command palette: verify filter/navigation/cancel/execute, route-scoped omission, hidden/disabled omission, and that user command files, Skill-derived commands, plugin Tools, and MCP Tools are absent. Slash autocomplete—not Ctrl+P—is the discovery surface for server commands; it excludes `source="skill"`, which remains available through `/skills`.
- exercise every source-defined local slash and alias: `/sessions` (`/resume`, `/continue`), `/new` (`/clear`), `/models` (`/mo`), `/agents`, `/mcps`, `/variants`, `/status`, `/themes`, `/help`, `/exit` (`/quit`, `/q`), `/editor`, `/skills`, `/diff`, `/rename`, `/timeline`, `/fork`, `/compact` (`/summarize`), `/undo`, `/redo`, `/timestamps` (`/toggle-timestamps`), `/thinking` (`/toggle-thinking`), `/copy`, and `/export`; then exercise backend-provided `/init`, `/review`, `/workflow`, `/model`, and `/think` through the discovered command catalog;
- configuration commands must act on local/server control state, not consume an ordinary model round: exact no-argument `/model` opens the existing model picker (same action as `/models`); exact no-argument `/think` opens the existing variant/effort picker (same action as `/variants`); `/agents` and `/mcps` open their pickers; `/workflow` list/info/select/run/state uses shared Workflow control. Argument-bearing backend commands keep their existing catalog path. If current source admits exact `/model` or `/think` as literal model prompt, use the RED slice below.
- user-defined slash paths: direct `/user-playbook`, the `/skills` picker, project Markdown commands under `.opencode/command{,s}/**/*.md`, and inline `command`/`commands` entries from the four project `opencode.json{,c}` locations; verify each discovered entry reaches `sdk.client.session.command`, preserves quoted and multi-line arguments, expands `$ARGUMENTS` plus `$1`…`$10` exactly once, writes `CommandExecuted`, and renders the resulting custom Tool card/final reply;
- unknown slash text reaches the prompt unchanged; TUI `!` shell mode executes only inside the playground and remains distinct from custom command template expansion.
- restart persistence for model/effort, Agent, prompt history/frequency/stash, Workflow selection, and Sessions at their documented storage boundaries.

OAuth login/logout are CLI/dialog error-path checks rather than shipped slash commands. Commands absent from the source-defined lists above are tested once as unknown slash text and must reach the ordinary prompt unchanged; do not invent a slash registration for them.

### 3. Canonical builtins

The exact 28 advertised tools are:

`read`, `ls`, `glob`, `find`, `grep`, `lsp`, `skill`, `webfetch`, `websearch`, `todowrite`, `write`, `edit`, `apply_patch`, `shell`, `bash`, `question`, `task`, `list_agents`, `plan_exit`, `invalid`, `ask_user`, `roster`, `send`, `announce`, `channels`, `join`, `leave`, `workflow`.

Hidden aliases: `fetch`, `search`, `todo`, `patch`, `plan`.

For every tool, run one successful observable contract and each applicable boundary failure. Specific coverage:

- files/search: pagination, truncation, BOM, binary/media, missing file, no matches, invalid regex, file-vs-directory input, external-directory refusal;
- edits: create/update/delete/move, missing/multiple match, formatter/LSP diagnostics, denied edit leaves bytes unchanged;
- shell/bash: permission once/always/reject, feedback, exact remembered subject, timeout, cancellation, output truncation, nonzero exit, unavailable command;
- web: loopback HTML/text/image/redirect/status/timeout/oversize plus one optional public smoke;
- LSP/formatter: deterministic success fixture, missing server, diagnostics, formatter failure;
- skill/todo: next-provider-request Skill injection, todo create/transition/clear/invalid;
- interaction: `question` rich selection/free-text and legacy `ask_user`; dropped reply plane;
- control: plan exit, invalid-tool success payload, unknown-tool structured error;
- subagent/team/Workflow tools: full scenarios below.

Inspect sanitized outbound schema names:

- GPT route: 26 builtins; `write` and `edit` absent; `apply_patch` present.
- GLM route: all 28 builtins present.

Each Tool call must follow `ToolInputStart` → optional deltas → `ToolCallRequested` → exactly one `ToolResult` or `ToolError`. The next provider request must contain the structured failure. The same Session must recover on a later call and later Turn.

### 4. User-defined slash commands and dynamic resources

Custom Skills and custom Tools are distinct interfaces. A Skill-backed slash command expands the Skill body directly as the admitted prompt; it does not itself prove the builtin `skill` Tool ran. Plugin and MCP Tools are model Tool schemas, not static slash commands. Prove the full composition by adding explicit user commands whose expanded prompts require those resources.

Create these deterministic project fixtures in the isolated playground and process-E2E workdir:

```text
.hya/skills/user-playbook/SKILL.md
.opencode/commands/use-skill.md
.opencode/commands/use-plugin.md
opencode.jsonc -> commands.use-mcp
.opencode/commands/nested/inspect.md
.hya/plugins/toolbox/plugin.toml + `plugin.py` JSON-RPC fixture declaring Tool `remember`
Hya config MCP server `echo` declaring Tool `mcp__echo__ping`
```

The command bodies are exact action prompts:

- `/use-skill <nonce>` tells the agent to call builtin `skill` with `name="user-playbook"`, then return the Skill-body marker and nonce;
- `/use-plugin <nonce>` tells the agent to call plugin Tool `remember` with `value=<nonce>`, then return the plugin result;
- `/use-mcp <nonce>` tells the agent to call `mcp__echo__ping` with `msg=<nonce>`, then return `echo:<nonce>`;
- direct `/user-playbook <arguments>` and selection through `/skills` prove the Skill-as-command path expands the Skill body without a redundant `skill` call;
- `/nested/inspect` proves recursive command naming and autocomplete.

At the command catalog and route seams, verify all currently supported sources and precedence: builtins first; project inline `command`/`commands`; project `.opencode/command` and `.opencode/commands` Markdown; then Skill-backed commands only when no command already owns the name. A later project command overrides a same-name builtin; a command wins over a same-name Skill. Malformed JSONC/frontmatter and unreadable/non-Markdown files are omitted without crashing the catalog. Empty/missing positional arguments expand to empty strings; quoted arguments stay one position; multi-line text remains in `$ARGUMENTS`; replacement text containing `$1` is not expanded a second time. Unknown slash names remain ordinary prompt text.

Command/Skill slash metadata and builtin `skill` Tool discovery both reuse `skill_dirs_for_workdir`, in exact first-name-wins order: project `.hya/skills`; home `.config/hya/skills`, `.claude/skills`, `.config/opencode/skills`, `.config/opencode/skill`; project `.opencode/skills`, `.opencode/skill`, `.agents/skills`; home `.codex/skills`, `.agents/skills`. Assert every root, first duplicate wins, invalid Skill frontmatter is omitted, and the three built-in Compat Skills fill only absent names. Plugin/AgentBundle runtime Skills are not projected into `/skill`, `/command`, or `/skills`; use one installed-bundle control to prove that separation.

Add one registered process scenario `crates/hya-e2e/tests/p18_custom_slash_resources.rs`. Reuse `E2eEnvBuilder::project_file`, `FakeLlm`, the P05 Skill fixture, and the P06 MCP echo fixture. No new harness adapter is needed: `BackendProcess` already starts with the temporary project as its current directory, so project files can provide `.hya/plugins/toolbox/plugin.toml` with `command = ["python3", ".hya/plugins/toolbox/plugin.py"]`; P18’s MCP fixture already sets `HYA_DEFER_SIDEPLANES=0` before the first Tool schema snapshot.

P18 contains eight separately named process tests, each with its own `matrix.toml` Track P row so one passing resource cannot hide another failure:

1. **T1.16 `custom_slash_catalog_and_routes_expand_all_supported_sources`:** `/api/command` lists every fixture with exact source/template/hints and no duplicate name. Legacy and V2 Compat command POSTs without `text` store exact expansion plus correlated `CommandExecuted`; native `/sessions/:id/command` deliberately stores the literal slash because it does not call `command_catalog::expand_prompt`, and explicit `text` bypasses expansion on every route. Cover four project JSON/JSONC locations, singular/plural inline keys, both Markdown roots, nested names, quotes, unclosed/empty quote behavior, missing positions, `$1`/`$10`, `$ARGUMENTS`, multi-line arguments, nonrecursive replacement, malformed omission/plain-fence behavior, ignored `disable`, command-over-Skill, and later-command-over-builtin precedence. Assert `.hya/commands` and home/global command directories are ignored; adding those unsupported roots is not part of this task.
2. **T1.17 `skill_backed_slash_expands_without_skill_tool_call`:** direct `/user-playbook` and `/skills` selection admit the Skill body and arguments with no `ToolCallRequested { name: "skill" }`; `Action::Skill` deny does not block direct template expansion. Enumerate all `skill_dirs_for_workdir` roots and duplicate precedence. Remove an existing Skill after TUI bootstrap: stale name still uses command transport, backend falls back to literal slash; add a new Skill after bootstrap: typed slash uses ordinary prompt until TUI restart.
3. **T1.18 `custom_command_invokes_builtin_skill_tool`:** `/use-skill` causes a real `skill` Tool call, body-bearing `ToolResult`, next-request replay, and final answer. Unknown/missing `name`, denied permission, and unavailable permission use separate scripts; each emits structured error and a later valid command succeeds in the same Session. A stale/deleted command name falls back to literal slash text rather than a catalog error.
4. **T1.19 `custom_command_invokes_plugin_tool`:** `/use-plugin` exposes and calls `remember`, preserves exact RPC input/result in Tool Events and the next request, and renders the Tool card. Test `Action::Write` rejection, fixture-RPC validation of malformed input, process death during call, lazy same-declaration respawn, and fail-closed declaration drift as separate cases. Editing `plugin.toml` is visible only after backend restart.
5. **T1.20 `custom_command_invokes_mcp_tool`:** `/use-mcp` exposes and calls `mcp__echo__ping`, asks once, replays `echo:<nonce>`, and renders the Tool card. Separately cover disconnected/unknown server, `isError`, malformed result, timeout, malformed/oversized frame, and post-publication process death. Death returns a structured closed error and does not auto-respawn; recovery is explicit disconnect/connect followed by a fresh root Turn, while the old in-flight binding stays pinned.
6. **T1.21 `resource_name_conflicts_fail_closed`:** duplicate plugin Tool, plugin-vs-builtin Tool, and crafted MCP namespace collisions reject the candidate runtime generation without partial publication. Command/Skill name collision follows command precedence instead of runtime rejection.
7. **T1.22 `dynamic_resource_snapshots_and_reload`:** a Skill edit appears on the next root Turn, MCP reconnect publishes for the next root Turn, plugin declaration changes require backend restart, and every old `TurnBinding` remains unchanged. Newly added post-bootstrap commands/Skill slash metadata are not promised before TUI restart because `sync.data.command` is bootstrap-cached.
8. **T1.23 `structured_custom_tool_errors_replay_and_session_recovers`:** one error per scripted resource call yields exactly one terminal Tool Event; structured `value.error.type/message` survives canonical/API replay, TUI shows bounded error text, Session returns idle, replay does not execute the Tool, and a later valid custom slash succeeds.
Add Track T row **T3.4 `custom_resource_tui_command_transport`** for `packages/hya-tui-ts/test/pty-smoke.test.ts`: PTY/Herdr proves Ctrl+P excludes user resources, slash autocomplete includes custom commands but excludes Skill rows, `/skills` includes Skills, local/server primary-or-alias collisions render one row after the dedupe fix, first-line command parsing preserves later lines in `arguments`, and command-request HTTP failures show a contextual toast with same-Session recovery. Add/change/remove command files while TUI runs to pin startup name caching: changed body expands immediately for a known name, added name is ordinary prompt, removed name reaches command transport then falls back to literal slash, and TUI restart refreshes names.

In the real Herdr pane, use slash autocomplete for `/use-skill`, `/use-plugin`, `/use-mcp`, use `/skills` for `user-playbook`, submit quoted and multi-line arguments, and capture the expanded user row, permission interaction, custom Tool cards, terminal result, and recovery toast/state. Run at least one custom-resource command on GPT and one on GLM.

Existing command frontmatter fields `agent`, `model`, and `subtask` are explicitly documented as listing-only with no runtime consumer in `docs/configuration.md` and `docs/FOLLOWUPS.md`. This added test does not silently redefine their execution semantics; assert their metadata remains present on `/api/command`, while resource execution uses the Session’s selected Agent/model. Do not add command-routing behavior for those fields as part of this test-only requirement.

Dynamic resource contracts remain:

- MCP: local echo connected status, namespaced call, one permission decision, remote/error/closed cases, explicit disconnect/connect recovery, next-root generation publication, old-binding pinning, and TUI visibility.
- Plugin: startup discovery, Tool registration/invocation, hook allow/block, permission, RPC failure, lazy same-declaration crash respawn, drift rejection, and declaration-change visibility only after backend restart.
- AgentBundle: valid local package install/list/info/Agent execution/local resource call/uninstall; malformed and invalid-capability rejection; bundle resources remain model Tools rather than static slash commands.
- Skills and Workflow: next-root Skill catalog refresh with old-binding pinning; command/Skill slash metadata and plugin declarations refresh in the TUI only after restart; Workflow uses its existing catalog revision contract.
- Local loopback web service and deterministic LSP/formatter fixtures.

### 5. Defined errors

Cover every `ProviderError` variant:

- `Json`: malformed JSON frame/body.
- `Http`: typed provider `error`, `response.failed`, bad header/client construction seam.
- `Transport`: connection refusal and deterministic transport failure.
- `HttpStatus`: bounded representative 4xx; 429 with `Retry-After`; 5xx.
- `UnknownModel`: locally unconfigured model with zero traffic.
- `Incompatible`: route without streaming tool-call capability and unsupported payload.
- `Decode`: malformed/truncated SSE, zero frames, EOF/`[DONE]` without typed terminal.
- `AuthExpired`: expired/revoked OAuth seam with recovery hint.

Also send one configured ghost model to `api.12th.day` to prove a real remote model-not-found error. Send one invalid credential request in the isolated config to prove real auth rejection. Do not try to force malformed streams, TLS faults, or throttling against production.

Cover every `ToolError` variant:

`Input`, `Permission::Denied`, `Permission::Unavailable`, `Io`, `Json`, `Cancelled`, `Overloaded`, `OperationIdConflict`, `OperationAlreadyHandled`, `WorkflowControl`, `UnknownAgentId`, `AgentSpawnNotAllowed`, `UnsupportedInlineAgentField`, `Other`.

