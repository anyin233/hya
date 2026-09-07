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

Also cover Session not-found/busy/conflict, invalid command/request, disconnected/reconnected TUI, MCP/plugin unavailable, Workflow stale revision/invalid graph/missing Agent, and exhausted subagent budgets.

### 6. Reasoning/CoT

- GPT live request: require summary-style reasoning events and visible TUI “Thinking” content.
- GLM live request: require full reasoning-text events and visible TUI content.
- Switch effort through `/model`; prove UI state and sanitized outbound effort; restart and prove the documented persistence behavior.
- Fold/unfold reasoning before, during, and after streaming. Preserve ordering and stable part keys.
- Test reasoning disabled/off deterministically and live if accepted by the route.
- If the model returns no reasoning at an enabled effort, classify it as provider/model behavior unless Hya received and lost a reasoning event.

### 7. Subagents/team

Cover with canonical ancestry, model route, Member lifecycle, and mailbox evidence:

- discovery;
- foreground result;
- same-child resume;
- nested spawn to depth two;
- two-member parallel batch;
- category/model override and inline Agent identity;
- nonblocking background terminal ordering;
- resident registration, stable handle, idle/wake/idle, recovery, stop, quiescence;
- roster, direct mail, `announce`, channel join/send/list/leave, pre-join/post-leave exclusion, cross-unit refusal;
- depth, concurrency, run, turn, and message budget failures;
- real TUI child observation and input isolation.

At least one slice uses each real model. Mixed-model children must show their actual route in Events.

### 8. Workflow

Use playground `.hya/workflows` fixtures to verify:

- catalog precedence/revision, info, select/state persistence, stale revision rejection, Session binding;
- fan-out/fan-in with parallel Stages, child links, deterministic join sections, durable statuses, TUI presentation;
- fail-fast and collect-all;
- cancellation through Session abort;
- idempotent replay/retry and changed-input operation conflict;
- invalid graph, missing Agent, and failed Stage.

Repair process-coverage tracking: add the existing P17 Workflow test and T2.12 mailbox contract to `crates/hya-e2e/matrix.toml`; update stale `docs/testing/process-e2e.md` coverage text.

### 9. Real coding capability

Prepare small isolated Rust/TypeScript fixtures. Run each real model in a fresh Session through inspect → diagnose → edit → focused test → report.

- GPT must use `apply_patch` because its route filters `write`/`edit`.
- GLM must exercise `write`, `edit`, and one patch path.
- Include a compile/type error, failing test, lint/format issue, merge-conflict-like file, large search, binary file, network-dependent request, denied action, human question, aborted Turn, resume, and post-error recovery.
- Accept only observed disk diffs and command results.

### 10. SWE-Bench Pro

Pinned authoritative assets:

- Evaluator: `scaleapi/SWE-bench_Pro-os@ca10a60a5fcae51e6948ffe1485d4153d421e6c5`.
- Dataset: `ScaleAI/SWE-bench_Pro@7ab5114912baf22bb098818e604c02fe7ad2c11f`.
- Dataset is public/ungated, 731 rows. Evaluator is MIT; dataset metadata has no declared license. Do not redistribute rows, repo snapshots, or images.

Fixed deterministic sample:

- Seed `hya-swebench-pro-live-8x2-v1`.
- Two rows each from Go, JavaScript, Python, and TypeScript.
- Sort by `sha256(seed + NUL + instance_id)` per language; prefer a second repository.
- Eligibility uses only nonempty identity/base/image fields and evaluator asset existence.
- Freeze IDs, row hashes, prompt hashes, base commits, and Docker image digests before the first API request.

Prompt bytes are exactly publisher `problem_statement`, then `Requirements:`, then `requirements`, then `New interfaces introduced:`, then `interface`. Gold patch, test patch, test names, scripts, and evaluator-only fields stay outside Hya worktrees and mounts.

For each attempt:

1. Create a fresh detached worktree at `base_commit`.
2. Run exactly one Hya task Turn; no retry, resume, hints, or shared patch.
3. Backend surface: `hya-backend --model 12th-oai/gpt-5.6-sol --yolo --db <private> exec --json <prompt>`.
4. TUI surface: launch current-source `hya` in a fresh Herdr pane; approve requested actions without task hints; exit only at idle.
5. Capture all text-file changes as a full-index patch. Reject binary patches and verify application against a separate clean base.
6. Run backend and TUI predictions in separate official local-Docker evaluator invocations because results key only by `instance_id`.
7. Read Booleans from `eval_results.json`; process exit alone is not a score.

Retain prompt/binary/patch hashes, source commit, Session DB/Event JSONL, TUI export, stderr/status, permission decisions, base HEAD/status, Docker tag/digest/image/platform, evaluator patch/entry script/stdout/stderr/output, token usage, and start/end timestamps.

A crash, empty/invalid patch, unanswered content question, dirty base, evaluator exception, or missing required test is false, not omitted.

### 11. Source-audit RED slices

These are source-grounded candidates, not yet live-run failures:

1. **Responses false success.** `OpenAiResponsesProtocol` constructs a permissive decoder. EOF or `[DONE]` without `response.completed`/`response.incomplete` closes as success. Grok already requires a typed terminal. RED: both OpenAI Responses and Grok reject untyped terminal/EOF; Chat Completions remains unchanged. Likely files: `crates/hya-provider/src/openai/responses.rs`, `response_decoder.rs`, `crates/hya-provider/tests/http_headers.rs`.
2. **Custom V2 background error loss.** `crates/hya-server/src/compat/session_prompt.rs` spawns `run_turn...` and discards its `Result`. The older `/prompt_async` path publishes error/status Events. RED: provider failure on `/api/session/:id/prompt` emits a Session error, returns idle, and permits a later Turn. Reuse the existing async publication seam.
3. **Coverage registry drift.** P17 Workflow has no matrix row; T2.12 cross-unit refusal has no contract row; process docs still say P01–P16. Add exact registry/docs entries and verify with matrix-check/focused tests.
4. **Custom slash-resource coverage gap.** Catalog/route tests prove metadata/Skill expansion, P05 proves direct `skill`, P06 proves direct MCP, and plugin tests prove direct `remember`; no process/real-TUI test proves slash admission → expanded prompt → user Skill/plugin/MCP Tool → replay → recovery. Add registered T1.16–T1.24 cases above. This is missing verification, not a known runtime defect.
5. **Exact `/model` and `/think` misroute.** The frontend registers `/models` and `/variants`; the server advertises `/model` and `/think`. Add a PTY RED proving exact no-argument `/model`/`/think` open `DialogModel`/`DialogVariant` with no `CommandExecuted` or provider round. Add `model` and `think` to the existing local aliases. In `autocomplete.tsx::commands`, compute names claimed by local primaries/aliases and omit colliding server rows. Argument-bearing server command submission remains unchanged.
6. **`/workflow` is not TUI-discoverable.** Routes intercept `command="workflow"`, but the catalog does not publish it; TUI submission therefore falls through to a prompt. Add catalog/PTY RED. Publish `command_info("workflow", "inspect or run workflows", "/workflow $ARGUMENTS", vec!["$ARGUMENTS"], None)`; keep `workflow::intercept_slash` as the only executor.
7. **Command request errors are silent.** `submitInner` discards `sdk.client.session.command(...)` and never inspects a nonthrowing error response. Add a PTY RED for a deterministic command-route failure. Await with `{ throwOnError: true }` and show `toast.show({ title: "Failed to run command", message: errorMessage(error), variant: "error" })`; preserve submitted history and prove the next command succeeds in the same Session.

Other findings become changes only after deterministic RED confirmation. Unknown YAML keys and offline fallback are documented behavior; do not change them for this task.

### 12. Execution order

### Phase 0 — Persist and activate the approved plan

- Copy this plan into Trellis `prd.md`, `design.md`, and `implement.md` with a source/research record.
- Curate `implement.jsonl` and `check.jsonl` from backend/frontend/guides specs and official SWE-Bench sources.
- Validate task artifacts and run `task.py start` only after user approval.
- Read `trellis-before-dev` before the first edit.

### Phase 1 — Baseline and private runtime

- Snapshot relevant workspace state without touching unrelated/untracked files.
- Build current-source binaries.
- Create playground, private XDG roots, databases, sanitized evidence ledger, fixtures, and cleanup registry.
- Securely create Hya config from OMP; verify both model catalog rows/variants and source hash.
- Start counting relay with the approved boundary. Make no live call before it is ready.

### Phase 2 — Deterministic RED/GREEN candidates

- Add focused RED tests for typed Responses terminal, custom V2 background errors, exact `/model`/`/think` dispatch plus deduplication, `/workflow` discovery/zero-provider execution, and visible command-request errors/recovery.
- Apply one minimal fix per RED slice and run focused GREEN before the next slice.
- Add T1.16–T1.23 custom slash-resource process contracts and T3.4 TUI transport coverage; they must pass unchanged behavior unless a separate deterministic defect is reproduced.
- Repair matrix/docs drift and run matrix-check.
- Update version/changelog only for product source changes.

### Phase 3 — Backend/API matrix

- Run CLI, persistence, HTTP/SSE, Session, permission/question, and fault matrix.
- Record canonical Events and status transitions.
- Rerun any repaired original backend scenario.

### Phase 4 — Herdr/TUI matrix
- Create the task-owned Herdr pane, launch backend/TUI, and execute the complete UI command/control checklist.
- Capture stable wide/narrow/fold/stream/error/reconnect/read-only states.
- Restart to prove persistence.

### Phase 5 — User slash commands, Tools, and dynamic resources

- Add P18 with existing `project_file`, `skill_file`, and `with_mcp_echo` builder seams; register it in `matrix.toml`.
- Run catalog/route cases for Markdown, inline JSON/JSONC, nested, overriding, malformed, quoted, positional, and multi-line custom commands plus direct Skill commands.
- Run P18’s slash → `skill` / plugin `remember` / `mcp__echo__ping` success, structured failure, next-request replay, same-Session recovery, and restart sequence.
- Run all 28 builtin success/error contracts plus aliases, then the remaining bundle/LSP/formatter/Workflow/loopback web fixtures.
- In Herdr, exercise `/skills` and all three custom resource commands through actual autocomplete/submit/tool-card/error-recovery UI on both requested models.
- Assert schema filtering, correlated `CommandExecuted`/Tool Events, model-visible results, and no duplicate execution.

### Phase 6 — Live dual-model behavior

- Run health, reasoning, remote missing-model, invalid-auth, organic tool, coding, subagent, resident/team, and Workflow scenarios across both models.
- Classify external/model failures without speculative source changes.

### Phase 7 — SWE-Bench Pro

- Acquire/pin dataset/evaluator via `uv` and public Docker images.
- Freeze selected control rows and image digests.
- Run the user-selected independent Pass@1 matrix.
- Generate/apply-check/evaluate patches and preserve official artifacts.

### Phase 8 — Repair loop

For every newly reproduced Hya defect: classify → deterministic RED → minimal fix → focused GREEN → original scenario → relevant cross-layer checks → version/changelog. Do not rerun already-passing expensive slices.

### Phase 9 — Verification and cleanup

- Rust: `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace --exclude hya-e2e`.
- Process: `cargo build -p hya-backend --bin hya-backend`; `cargo test -p hya-e2e -- --test-threads=1`.
- TUI: `bun run typecheck`; `bun test`; focused real-backend Track T files; real Herdr smoke.
- Run `cargo xtask matrix-check` and build local `hya`, `hya-ts`, and `hya-backend` executables.
- Stop backends, panes, relays, fixtures, Docker containers, MCP/plugins, and watchdogs. Remove only secret/runtime artifacts. Preserve benchmark evidence without credentials.
- Run Trellis quality/spec review. Finish/archive task after evidence is written and repository rules for any feature commit/push are satisfied.

## Critical files & anchors

- `crates/hya-server/src/compat/command_catalog.rs` — `list`, `expand_prompt`, `expand_template`, `add_skill_commands`; owns source precedence and exact command prompt expansion.
- `packages/hya-tui-ts/src/upstream/app.tsx`, `component/prompt/autocomplete.tsx`, and `component/prompt/index.tsx` — slash registration/merge/submit and `prompt.skills`; owns exact local configuration dispatch, deduplication, multi-line command arguments, and Skill picker insertion.
- `crates/hya-server/src/compat/command_sources.rs` — `config_commands`, `disk_commands`, `command_hints`, `parse_command_file`; owns the supported project source set, recursive names, metadata parsing, and malformed-source omission.
- `crates/hya-e2e/tests/p18_custom_slash_resources.rs` — new process seam joining custom commands to Skill/plugin/MCP Tool execution, structured failure replay, recovery, and restart.
- `crates/hya-provider/src/openai/responses.rs` plus `crates/hya-server/src/compat/session_prompt.rs` — the two independent initial RED candidates; reread exact constructors/spawned-run paths before edits.

Do not add command-specific Agent/model/subtask routing, a test-only product hook, generic retry layer, alternate frontend, or new E2E framework.

## Verification

Completion requires all of the following:

- self-contained private Hya config resolves and calls both models without OMP runtime access;
- no credential leakage and unchanged OMP source hash;
- backend/API success/failure matrix with Event/SQLite proof;
- real Herdr pane ANSI captures cover the complete TUI command/control, wide/narrow rendering, folding, interaction, reconnect, and read-only child matrix;
- every source-defined local/backend slash command and alias is exercised; exact `/model` and `/think` open configuration dialogs without provider traffic, `/workflow state` reaches shared Workflow control with zero provider traffic, and autocomplete has no duplicate exact names;
- all 28 builtins, five aliases, dynamic planes, every `ProviderError`, every `ToolError`, and recovery behavior;
- every supported user-defined slash source is discovered with deterministic precedence; direct Skill, `/skills`, Markdown, inline JSON/JSONC, nested, quoted, positional, and multi-line forms admit the exact expected prompt;
- P18 and real Herdr runs prove custom slash commands invoke `skill`, plugin `remember`, and `mcp__echo__ping`, replay real outputs to the model, surface permission/resource failures, recover in the same Session, and remain available after restart;
- GPT summary CoT and GLM full CoT rendering/folding/state proof;
- subagent/team and Workflow lifecycle proof across both models;
- independent real coding-task diffs and observed tests for both models;
- user-selected SWE-Bench official evaluator Booleans, including failures;
- RED/GREEN/original-scenario evidence for every repair;
- all applicable Rust, process E2E, TUI, Track T, matrix, and executable-build gates;
- complete process/container/pane cleanup.

Run from the repository root after building `target/debug/hya-backend`:

```sh
cargo test -p hya-server --test compat_command_metadata_api
cargo test -p hya-server --test compat_session_api compat_session_command_without_text_uses_skill_template_body
cargo test -p hya-server --test compat_session_v2_api compat_v2_session_command_without_text_uses_skill_template_body
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e --test p18_custom_slash_resources -- --test-threads=1
cargo xtask matrix-check
(cd packages/hya-tui-ts && bun test test/pty-smoke.test.ts)
```

For the live UI proof, start the private backend/config first, then in the task-owned Herdr pane select `user-playbook` through `/skills`, submit `/use-skill SKILL_<nonce>`, `/use-plugin "PLUGIN <nonce>"`, and a multi-line `/use-mcp MCP_<nonce>\nsecond-line` command. Expected observables: expanded user text rather than the literal template, one correlated custom Tool card per command, `SKILL_BODY_<nonce>`, plugin `remember` output, MCP `echo:MCP_<nonce>`, structured error card on the forced failure, a later success in the same Session, and the same commands after backend/TUI restart. Switch `/model` between the GPT and GLM slices and retain Event/model ids with the ANSI frames.

## Assumptions & contingencies

- SWE-Bench scope is fixed at eight blind-selected instances and 16 independent GPT runs: eight backend plus eight Herdr/TUI. GLM still receives the complete functional, slash-resource, CoT, subagent, Workflow, and coding suite.
- The live-provider relay has a global hard maximum of 2,000 forwarded requests. The maximum is not a target; request 2,001 is rejected before forwarding.
- If one frozen SWE-Bench row lacks a required pinned evaluator asset or Docker image during preflight, mark that fixed row blocked and do not substitute another after model execution begins.
- If a custom plugin Tool name collides with an existing Tool, the fixture must fail registration with the existing duplicate-name error; rename only the test fixture before any evidence run, never add alias or last-writer-wins behavior.
- If a real model ignores an explicit custom-resource command but deterministic P18 passes, classify the attempt as model adherence and allow one tighter nonce-bearing prompt within the global cap; do not change Hya solely to force a model Tool choice.
