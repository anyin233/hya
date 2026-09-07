# Full agent live validation

## Goal

**G-01.** Prove and repair current-source Hya as a complete coding agent through its backend and TypeScript TUI. The proof must include user-defined slash commands that cause the agent to use project Skills, plugin Tools, and MCP Tools. It must use the two required live model routes, a real Herdr pane, and the official SWE-Bench Pro assets. The result must show observable behavior, durable state, recovery, and honest benchmark outcomes rather than relying on model prose.

**Open questions.** Blocking open questions are empty.

## Background / Confirmed Facts

**FACT-01. Authoritative source.** This PRD is a requirements view of the approved plan in `research/approved-plan.md`. The verified research copy is `.trellis/tasks/08-29-full-agent-live-validation/research/approved-plan.md` with SHA-256 `abbe961e90d65b71e9b7218f8cec44d61be22e414b1c0a41f8ce47c9c03b068a`. The source audit sent no live provider request and changed no product code.

**FACT-02. Audited surface.** The audit covered the TUI, backend/provider, 28-tool registry, custom command catalog, Skills, plugin/MCP registration, subagents and Workflow, the Herdr CLI, and official SWE-Bench Pro assets.

**FACT-03. Planning baseline and execution outcome.** The package version at the approved planning baseline was `0.36.0`. Product repairs advanced the current aligned release metadata to `0.36.6`. The GPT scheduling live proof ran on release `0.36.5` and remains applicable because `0.36.6` changes only TypeScript Session-cache lookup/test code and release metadata; scheduling-sensitive source is unchanged. Earlier 0.36.3/0.36.4 gate records remain historical evidence in `evidence/final-verification.json`. Confirmed planning-time local tools were Docker `28.5.1`, Bun `1.3.10`, Rust/Cargo `1.92.0`, and Herdr `0.8.1`. `/home/yanweiye/Projects/hya-playground` did not exist at planning time and was the task's execution playground. The OMP model registry is `/home/yanweiye/.omp/agent/models.yml`.

**FACT-04. Required provider route.** OMP route `12th-oai` uses OpenAI Responses at `https://api.12th.day/v1` and includes `glm-5.3` and `gpt-5.6-sol`. Both requested models advertise reasoning efforts `low`, `medium`, `high`, `xhigh`, and `max`.

**FACT-05. Credential audit.** The OMP credential is an `apiKey` string. Its value was not printed or copied during planning. CodeGraph is not initialized for this repository; planning used targeted source and test reads, and no index is authorized by this task.

**FACT-06. Artifact ownership.** This file owns requirements, constraints, out-of-scope boundaries, and observable acceptance. The companion design artifact owns boundaries, contracts, data flow, tradeoffs, and rollback. The companion implementation artifact owns ordered execution, checks and command invocations, the repair loop, and cleanup. This split keeps technical execution checklists out of the PRD while preserving the approved scope for those artifacts.

## Final scope decision (2026-08-30)

The user explicitly approved this finalization scope on 2026-08-30:

- Remaining GLM functional, coding, subagent, Workflow, and reasoning validation is waived for this task.
- Completed GPT-only validation is accepted for finalization.
- This is a user-approved scope waiver, not a GLM pass. Preserve the historical GLM successful custom-MCP Turn and later upstream HTTP 503 classification as evidence; do not rewrite or reclassify either record.
- SWE-Bench Pro remains separate diagnostic accounting: retain all 16 attempts, the 16 official evaluator Booleans, and the five setup-invalid counted-false outcomes.
- The `R-FINAL-01`/`AC-FINAL-16` GPT scheduling contract passed on 2026-08-30. Overall acceptance is `passed-with-user-approved-GLM-waiver`; final gates, cleanup, and the Trellis quality review passed on release `0.36.6`, so the worktree is ready to commit. No commit has been made by this review.

For finalization, this decision supersedes only the requirement for the remaining GLM functional/coding/subagent/Workflow/reasoning slices. It does not change deterministic coverage, credential safety, benchmark accounting, or historical evidence.

| Area | Current classification | Evidence or condition |
| --- | --- | --- |
| GPT live coverage | passed and accepted | `live-provider-summary.json`, `tui-live-matrix.json`, `coding-fixture-summary.json`, and `tool-schema-by-model.json` |
| GLM remaining live slices | waived, not passed | Historical custom-MCP success and later upstream 503 remain in `glm-live-summary.json` and related live summaries |
| SWE-Bench Pro | completed diagnostic | 16 attempts; 11/16 counted Pass@1; five setup-invalid attempts counted false |
| GPT scheduling | passed | `gpt56-model-scheduling.json`; the live proof ran on `0.36.5`, and scheduling-sensitive source is unchanged in the current `0.36.6` release |


## Requirements

### Scope, evidence, and release rules

**R-SCOPE-01. Current source only.** Build and exercise `hya`, `hya-ts`, and `hya-backend` from this worktree. Installed binaries do not satisfy acceptance.

**R-SCOPE-02. Exact dual-model routes.** Live coverage must use `12th-oai/gpt-5.6-sol` and `12th-oai/glm-5.3` through `https://api.12th.day/v1`. At least one custom-resource command must run on each model. GPT and GLM must both receive the complete functional, slash-resource, reasoning, subagent, Workflow, and coding suite unless a stated SWE-Bench allocation says otherwise. The 2026-08-30 Final scope decision explicitly waives the remaining GLM functional/coding/subagent/Workflow/reasoning slices for this task; completed GPT-only validation is accepted for finalization. This waiver is not a GLM pass.

**R-SCOPE-03. Evidence first.** Model prose is never proof. Evidence must come from canonical Events and SQLite, HTTP/SSE observations, disk diffs, process exit status, focused output, and Herdr pane captures. Evidence must retain the relationship between the request, Session, model, Tool, and resulting state.

**R-SCOPE-04. Deterministic before live.** FakeLlm and loopback fault services must cover exact tools and failure contracts before relying on organic live behavior. Real models are required for organic coding, reasoning, TUI, subagent, and Workflow proof. A real model's non-compliance is not automatically an Hya defect.

**R-SCOPE-05. Root-cause repair.** Every newly reproduced Hya defect must have a deterministic RED behavior test, the smallest root-cause fix, focused GREEN evidence, and a rerun of the original scenario. Do not add a generic retry or suppress a symptom.

**R-SCOPE-06. TDD and release rules.** Every product source fix follows failing-test-first. A product source change updates the workspace version and the single-version root changelog and archives the old changelog. Feature changes are committed and pushed only after project gates and the repository rules in `AGENTS.md` are satisfied.

**R-SCOPE-07. Benchmark honesty.** SWE-Bench results are diagnostic Pass@1, not official leaderboard reproduction. Failed attempts count and are never omitted.
**R-ART-01. Artifact activation.** Persist this approved execution specification in the Trellis PRD, design, implementation, research record, and curated manifests. Validate the task artifacts and run `task.py start` only after user approval. The `trellis-before-dev` guidance is read before the first source edit. The implementation artifact owns the order of these activities.
**R-FINAL-01. GPT-5.6-family scheduling proof.** Before finalization, run one current-source live multi-model scheduling test with a category chain containing two GPT-5.6 Sol provider/model refs with distinct reasoning variants (planned refs: `12th-oai/gpt-5.6-sol#low` preferred and `12th-oai/gpt-5.6-sol#high` fallback). The preferred ref must use a deterministic local route that returns HTTP 503 before streaming; ordered fallback must use the live GPT-5.6 Sol route at `https://api.12th.day/v1`. Observe every preferred-route attempt and prove all attempts precede fallback, the engine advances before any Event stream, fallback returns the exact expected response, one Session persists ordered terminal Events, counted relay metadata is credential-safe, and no mid-stream replay occurs. This contract passed in the current-source release `0.36.5`; see `evidence/gpt56-model-scheduling.json`.

### Private runtime and credential boundary

**R-ENV-01. Isolated runtime.** Run the validation in a private run root outside the playground workdir, with private `HOME` and all XDG roots. Hya SQLite databases and credential-bearing configuration must be mode `0600` under a mode `0700` directory.

**R-ENV-02. Self-contained Hya configuration.** Transfer the OMP `apiKey` in-process into one private Hya `config.yaml` as an inline literal. The launched Hya process must receive no OMP config path, key environment variable, or external Hya auth token. This proves runtime independence from OMP.

**R-ENV-03. Configuration contract.** The private configuration has this semantic shape; `<copied-secret>` is written without entering any logged command:

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

The configured ghost model exists only to send one real request that the remote service must reject as nonexistent. A separately locally unconfigured model must exercise `ProviderError::UnknownModel` with zero outbound traffic.

**R-ENV-04. Counted relay boundary.** A private localhost relay may be the configured base URL during counted live runs. It forwards to `api.12th.day`, increments the request count before forwarding, and records only ordinal, time, path, status, Tool-schema names, and reasoning effort. It must never store headers or request/response bodies and must fail closed before forwarded request `2,001`.

**R-ENV-05. Secret handling.** Never print the credential or place it in argv, logs, Events, prompts, patches, Docker mounts, Herdr captures, or reports. Verify that the OMP source file hash is unchanged. During final cleanup remove the literal test copy and retain only a redacted template and hash. Preserve benchmark evidence without credentials.

### Backend and API behavior

**R-BE-01. CLI surfaces.** Exercise current-source behavior for `--help`, `--version`, `models`, `agent list`, `bundle list/info`, `workflow list/info/state`, `sessions`, `tail-session`, and `auth list`.

**R-BE-02. Execution surfaces.** Exercise `exec` text, `exec --json`, Compat `run`, prompt/goal mode, `serve`, and JSONL `rpc`.

**R-BE-03. Storage and Session lifecycle.** Exercise in-memory and persistent SQLite stores. Cover Session create, list, replay, resume, fork, compact, summarize, abort, delete, and missing-Session behavior.

**R-BE-04. API route matrix.** Cover native and Compat synchronous prompt, asynchronous prompt, SSE/global Event, permission, question, shell, command, file, project/VCS, MCP, PTY, and every TUI route used by the frontend.

**R-BE-05. Invalid and concurrent requests.** Cover invalid JSON and IDs, duplicate prompt IDs, busy and conflicting Sessions, cancellation, provider errors, client disconnect, and restart recovery.

**R-BE-06. Durable background failure invariant.** A background provider or Workflow failure must produce a bounded durable error/status Event, return the Session to a usable state, and permit a later Turn. No `let _ = run_turn(...).await` path may silently discard a failure.

### Real Herdr TUI

**R-TUI-01. Real pane and driver.** Create a real Herdr **pane** (the user's “panel”) in `/home/yanweiye/Projects/hya-playground`. Run current-source `hya` as an ordinary interactive pane process. Use Herdr text and key injection, not a synthetic TUI test, for final interactive evidence. Hya is not a verified `herdr agent --kind` target.

The pane contract includes Herdr pane split/run/send-text/send-keys/wait-output/read, terminal session observe, pane process-info, and pane close operations. Parse the new pane id from `.result.pane.pane_id`; capture visible text and ANSI terminal frames. Herdr has no PNG screenshot command. Exact rows and columns require the single-owner `terminal session control` stream; `pane resize` changes only the split ratio. Mouse-click injection is unsupported, so acceptance interactions use keyboard navigation. Close only the pane created by this task and never stop the Herdr server.

**R-TUI-02. Runtime views and interaction.** Prove startup, loading, offline, and error views; composer focus; multiline submit; busy and idle; streaming text; streaming reasoning; Tool cards; permissions; questions; toasts; abort; reconnect; exit; and terminal restoration.

**R-TUI-03. Layout and transcript behavior.** Prove wide and narrow resize, scroll, transcript stability, Tool and reasoning fold/unfold before, during, and after streaming, status counters, and usage display when supplied.

**R-TUI-04. Child observation and input isolation.** Prove read-only child observation, read-only input isolation, and return to the owner composer.

**R-TUI-05. Keymap-only command palette.** Ctrl+P must open the keymap-only command palette. Prove filter, navigation, cancel, execute, route-scoped omission, hidden/disabled omission, and absence of user command files, Skill-derived commands, plugin Tools, and MCP Tools. Slash autocomplete, not Ctrl+P, is the discovery surface for server commands. Slash autocomplete excludes `source="skill"`; those entries remain available through `/skills`.

**R-TUI-06. Local slash commands and aliases.** Exercise every source-defined local slash and alias:

- `/sessions` with `/resume` and `/continue`;
- `/new` with `/clear`;
- `/models` with `/mo`;
- `/agents`, `/mcps`, `/variants`, `/status`, `/themes`, `/help`;
- `/exit` with `/quit` and `/q`;
- `/editor`, `/skills`, `/diff`, `/rename`, `/timeline`, `/fork`;
- `/compact` with `/summarize`;
- `/undo`, `/redo`;
- `/timestamps` with `/toggle-timestamps`;
- `/thinking` with `/toggle-thinking`;
- `/copy` and `/export`.

Then exercise backend-provided `/init`, `/review`, `/workflow`, `/model`, and `/think` through the discovered command catalog.

**R-TUI-07. Configuration command dispatch.** Configuration commands act on local or server control state and do not consume an ordinary model round. Exact no-argument `/model` opens the existing model picker, the same action as `/models`. Exact no-argument `/think` opens the existing variant/effort picker, the same action as `/variants`. `/agents` and `/mcps` open their pickers. `/workflow` list/info/select/run/state uses shared Workflow control. Argument-bearing backend commands retain their existing catalog path. If current source admits exact `/model` or `/think` as a literal model prompt, the behavior is handled by the deterministic RED slice, not by an unapproved workaround.

**R-TUI-08. Unknown and non-command input.** Unknown slash text reaches the ordinary prompt unchanged. TUI `!` shell mode executes only inside the playground and remains distinct from custom command template expansion. Commands absent from the source-defined lists are tested once as unknown slash text and no slash registration is invented for them.

**R-TUI-09. TUI persistence.** At the documented storage boundaries, restart must preserve model and effort, Agent, prompt history/frequency/stash, Workflow selection, and Sessions.

**R-TUI-10. OAuth boundary.** OAuth login/logout are CLI/dialog error-path checks, not shipped slash commands.

### Canonical built-in Tools

**R-TOOL-01. Exact advertised registry.** The advertised Tool registry is exactly these 28 names:

`read`, `ls`, `glob`, `find`, `grep`, `lsp`, `skill`, `webfetch`, `websearch`, `todowrite`, `write`, `edit`, `apply_patch`, `shell`, `bash`, `question`, `task`, `list_agents`, `plan_exit`, `invalid`, `ask_user`, `roster`, `send`, `announce`, `channels`, `join`, `leave`, `workflow`.

The hidden aliases are exactly `fetch`, `search`, `todo`, `patch`, and `plan`.

**R-TOOL-02. Success and boundary coverage.** For every Tool, prove one successful observable contract and each applicable boundary failure. Coverage includes:

- files and search: pagination, truncation, BOM, binary/media, missing file, no matches, invalid regex, file-versus-directory input, and external-directory refusal;
- edits: create, update, delete, move, missing/multiple match, formatter/LSP diagnostics, and denied edit leaves bytes unchanged;
- shell/bash: permission once/always/reject, feedback, exact remembered subject, timeout, cancellation, output truncation, nonzero exit, and unavailable command;
- web: loopback HTML/text/image/redirect/status/timeout/oversize and one optional public smoke;
- LSP/formatter: deterministic success fixture, missing server, diagnostics, and formatter failure;
- Skill/todo: next-provider-request Skill injection and todo create/transition/clear/invalid;
- interaction: rich-selection/free-text `question`, legacy `ask_user`, and dropped reply plane;
- control: plan exit, invalid-Tool success payload, and unknown-Tool structured error;
- subagent/team/Workflow Tools: the complete scenarios in the corresponding requirements below.

**R-TOOL-03. Provider schema filtering.** Inspect sanitized outbound Tool schema names. The GPT route advertises 26 builtins, with `write` and `edit` absent and `apply_patch` present. The GLM route advertises all 28 builtins.

**R-TOOL-04. Tool event and recovery contract.** Every Tool call follows `ToolInputStart`, optional input deltas, `ToolCallRequested`, and exactly one `ToolResult` or `ToolError`. The next provider request contains the structured failure when a failure occurs. The same Session recovers on a later Tool call and later Turn.

### User-defined slash commands and dynamic resources

**R-RES-01. Distinct interfaces.** Custom Skills and custom Tools remain distinct. A Skill-backed slash command expands the Skill body directly as the admitted prompt; it does not by itself prove that the builtin `skill` Tool ran. Plugin and MCP Tools are model Tool schemas, not static slash commands. Explicit user commands must compose slash admission with the required Skill, plugin Tool, or MCP Tool action.

**R-RES-02. Deterministic fixtures.** Create these project fixtures in the isolated playground and process-E2E workdir:

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

- `/use-skill <nonce>` instructs the agent to call builtin `skill` with `name="user-playbook"`, then return the Skill-body marker and nonce;
- `/use-plugin <nonce>` instructs the agent to call plugin Tool `remember` with `value=<nonce>`, then return the plugin result;
- `/use-mcp <nonce>` instructs the agent to call `mcp__echo__ping` with `msg=<nonce>`, then return `echo:<nonce>`;
- direct `/user-playbook <arguments>` and selection through `/skills` prove Skill-as-command expansion without a redundant `skill` call;
- `/nested/inspect` proves recursive command naming and autocomplete.

**R-RES-03. Command catalog precedence and expansion.** At command catalog and route seams, verify this exact precedence: builtins first; project inline `command`/`commands`; project `.opencode/command` and `.opencode/commands` Markdown; then Skill-backed commands only when no command owns the name. A later project command overrides a same-name builtin, and a command wins over a same-name Skill. Malformed JSONC/frontmatter and unreadable or non-Markdown files are omitted without a catalog crash. Empty or missing positional arguments expand to empty strings. Quoted arguments stay one position. Multiline text remains in `$ARGUMENTS`. Replacement text containing `$1` is not expanded a second time. Unknown slash names remain ordinary prompt text.

**R-RES-04. Supported sources and unsupported roots.** Cover all four project `opencode.json{,c}` locations, singular and plural inline keys, both Markdown roots (`.opencode/command` and `.opencode/commands`), nested names, quotes, unclosed and empty quotes, missing positions, `$1` through `$10`, `$ARGUMENTS`, multiline arguments, nonrecursive replacement, malformed omission/plain-fence behavior, and ignored `disable`. `.hya/commands` and home/global command directories are ignored; adding unsupported roots is not part of this task.

**R-RES-05. Skill discovery order.** Command/Skill slash metadata and builtin `skill` Tool discovery both reuse `skill_dirs_for_workdir`, in this exact first-name-wins order:

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

Assert every root, first duplicate wins, invalid Skill frontmatter is omitted, and the three built-in Compat Skills fill only absent names. Plugin/AgentBundle runtime Skills are not projected into `/skill`, `/command`, or `/skills`; one installed-bundle control proves that separation.

**R-RES-06. Registered P18 process matrix.** Add one registered process scenario at `crates/hya-e2e/tests/p18_custom_slash_resources.rs`. Reuse `E2eEnvBuilder::project_file`, `FakeLlm`, the P05 Skill fixture, and the P06 MCP echo fixture. No new harness adapter is needed. `BackendProcess` already starts in the temporary project directory, so the project may provide `.hya/plugins/toolbox/plugin.toml` with `command = ["python3", ".hya/plugins/toolbox/plugin.py"]`. P18's MCP fixture sets `HYA_DEFER_SIDEPLANES=0` before the first Tool schema snapshot.

P18 has eight separately named process tests, each with its own `matrix.toml` Track P row:

1. **T1.16 `custom_slash_catalog_and_routes_expand_all_supported_sources`:** `/api/command` lists every fixture with exact source, template, and hints and no duplicate name. Legacy and V2 Compat command POSTs without `text` store exact expansion plus correlated `CommandExecuted`; native `/sessions/:id/command` deliberately stores the literal slash because it does not call `command_catalog::expand_prompt`; explicit `text` bypasses expansion on every route. Cover four project JSON/JSONC locations, singular/plural inline keys, both Markdown roots, nested names, quotes, unclosed/empty quote behavior, missing positions, `$1`/`$10`, `$ARGUMENTS`, multiline arguments, nonrecursive replacement, malformed omission/plain-fence behavior, ignored `disable`, command-over-Skill, and later-command-over-builtin precedence.
2. **T1.17 `skill_backed_slash_expands_without_skill_tool_call`:** direct `/user-playbook` and `/skills` selection admit the Skill body and arguments with no `ToolCallRequested { name: "skill" }`; `Action::Skill` deny does not block direct template expansion. Enumerate all `skill_dirs_for_workdir` roots and duplicate precedence. Removing an existing Skill after TUI bootstrap leaves the stale name on command transport and makes the backend fall back to literal slash; adding a new Skill after bootstrap makes typed slash ordinary prompt text until TUI restart.
3. **T1.18 `custom_command_invokes_builtin_skill_tool`:** `/use-skill` causes a real `skill` Tool call, body-bearing `ToolResult`, next-request replay, and final answer. Unknown/missing `name`, denied permission, and unavailable permission use separate scripts; each emits a structured error and a later valid command succeeds in the same Session. A stale/deleted command name falls back to literal slash text rather than a catalog error.
4. **T1.19 `custom_command_invokes_plugin_tool`:** `/use-plugin` exposes and calls `remember`, preserves exact RPC input/result in Tool Events and the next request, and renders the Tool card. Separate cases cover `Action::Write` rejection, fixture-RPC malformed-input validation, process death during a call, lazy same-declaration respawn, and fail-closed declaration drift. Editing `plugin.toml` is visible only after backend restart.
5. **T1.20 `custom_command_invokes_mcp_tool`:** `/use-mcp` exposes and calls `mcp__echo__ping`, asks once, replays `echo:<nonce>`, and renders the Tool card. Separately cover disconnected/unknown server, `isError`, malformed result, timeout, malformed/oversized frame, and post-publication process death. Death returns a structured closed error and does not auto-respawn. Recovery is explicit disconnect/connect followed by a fresh root Turn, while the old in-flight binding remains pinned.
6. **T1.21 `resource_name_conflicts_fail_closed`:** duplicate plugin Tool, plugin-versus-builtin Tool, and crafted MCP namespace collisions reject the candidate runtime generation without partial publication. Command/Skill name collision follows command precedence instead of runtime rejection.
7. **T1.22 `dynamic_resource_snapshots_and_reload`:** a Skill edit appears on the next root Turn, MCP reconnect publishes for the next root Turn, plugin declaration changes require backend restart, and every old `TurnBinding` remains unchanged. Newly added post-bootstrap commands/Skill slash metadata are not promised before TUI restart because `sync.data.command` is bootstrap-cached.
8. **T1.23 `structured_custom_tool_errors_replay_and_session_recovers`:** one error per scripted resource call yields exactly one terminal Tool Event. Structured `value.error.type/message` survives canonical/API replay, the TUI shows bounded error text, the Session returns idle, replay does not execute the Tool, and a later valid custom slash succeeds.

**R-RES-07. Track T transport matrix.** Add Track T row **T3.4 `custom_resource_tui_command_transport`** for `packages/hya-tui-ts/test/pty-smoke.test.ts`. The PTY/Herdr contract proves Ctrl+P excludes user resources, slash autocomplete includes custom commands but excludes Skill rows, `/skills` includes Skills, local/server primary-or-alias collisions render one row after the dedupe fix, first-line command parsing preserves later lines in `arguments`, and command-request HTTP failures show a contextual toast with same-Session recovery. Adding, changing, and removing command files while TUI runs pins startup name caching: a changed body expands immediately for a known name, an added name is ordinary prompt, a removed name reaches command transport then falls back to literal slash, and TUI restart refreshes names.

**R-RES-08. Real custom-resource TUI proof.** In the real Herdr pane, use slash autocomplete for `/use-skill`, `/use-plugin`, and `/use-mcp`; use `/skills` for `user-playbook`; submit quoted and multiline arguments; and capture the expanded user row, permission interaction, custom Tool cards, terminal result, and recovery toast/state. Run at least one custom-resource command on GPT and one on GLM.

**R-RES-09. Command metadata semantics.** Existing command frontmatter fields `agent`, `model`, and `subtask` are listing-only and have no runtime consumer, as documented in `docs/configuration.md` and `docs/FOLLOWUPS.md`. Their metadata remains present on `/api/command`, while resource execution uses the Session's selected Agent/model. Do not add command-routing behavior for these fields.

**R-RES-10. Dynamic resource contracts.** Preserve these contracts:

- MCP: local echo connected status, namespaced call, one permission decision, remote/error/closed cases, explicit disconnect/connect recovery, next-root generation publication, old-binding pinning, and TUI visibility;
- plugin: startup discovery, Tool registration/invocation, hook allow/block, permission, RPC failure, lazy same-declaration crash respawn, drift rejection, and declaration-change visibility only after backend restart;
- AgentBundle: valid local package install/list/info/Agent execution/local resource call/uninstall, malformed and invalid-capability rejection, and bundle resources remain model Tools rather than static slash commands;
- Skills and Workflow: next-root Skill catalog refresh with old-binding pinning, command/Skill slash metadata and plugin declarations refresh in TUI only after restart, and Workflow uses its existing catalog revision contract;
- local loopback web service and deterministic LSP/formatter fixtures.

### Defined errors and recovery

**R-ERR-01. ProviderError coverage.** Cover every `ProviderError` variant:

- `Json`: malformed JSON frame/body;
- `Http`: typed provider `error`, `response.failed`, and bad header/client construction seam;
- `Transport`: connection refusal and deterministic transport failure;
- `HttpStatus`: bounded representative 4xx, 429 with `Retry-After`, and 5xx;
- `UnknownModel`: locally unconfigured model with zero traffic;
- `Incompatible`: route without streaming Tool-call capability and unsupported payload;
- `Decode`: malformed/truncated SSE, zero frames, and EOF/`[DONE]` without typed terminal;
- `AuthExpired`: expired/revoked OAuth seam with recovery hint.

Also send one configured ghost model to `api.12th.day` to prove a real remote model-not-found error. Send one invalid credential request in the isolated configuration to prove real auth rejection. Do not force malformed streams, TLS faults, or throttling against production.

**R-ERR-02. ToolError coverage.** Cover every `ToolError` variant: `Input`, `Permission::Denied`, `Permission::Unavailable`, `Io`, `Json`, `Cancelled`, `Overloaded`, `OperationIdConflict`, `OperationAlreadyHandled`, `WorkflowControl`, `UnknownAgentId`, `AgentSpawnNotAllowed`, `UnsupportedInlineAgentField`, and `Other`.

**R-ERR-03. Cross-surface failures.** Also cover Session not-found, busy, and conflict; invalid command/request; disconnected and reconnected TUI; MCP/plugin unavailable; Workflow stale revision, invalid graph, and missing Agent; and exhausted subagent budgets.

**R-ERR-04. Structured recovery.** For every applicable Tool/provider/resource failure, preserve one bounded terminal error/status Event and structured failure in replay and the next provider request. The Session must return idle and accept a later valid command or Turn. Replay must not execute a Tool again.

### Reasoning and Chain of Thought presentation

**R-REASON-01. Model-specific reasoning.** A GPT live request must produce summary-style reasoning Events and visible TUI `Thinking` content. A GLM live request must produce full reasoning-text Events and visible TUI content.

**R-REASON-02. Effort state.** Switch effort through `/model`; prove UI state and sanitized outbound effort; restart and prove documented persistence behavior.

**R-REASON-03. Ordering and controls.** Fold and unfold reasoning before, during, and after streaming. Preserve ordering and stable part keys. Test reasoning disabled/off deterministically and live if the route accepts it.

**R-REASON-04. Provider classification.** If a model returns no reasoning at an enabled effort, classify it as provider/model behavior unless Hya received and lost a reasoning Event.

### Subagents and teams

**R-SUB-01. Lifecycle and ancestry.** With canonical ancestry, model route, Member lifecycle, and mailbox evidence, cover discovery, foreground result, same-child resume, nested spawn to depth two, a two-member parallel batch, category/model override, and inline Agent identity.

**R-SUB-02. Resident and background behavior.** Cover nonblocking background terminal ordering; resident registration; stable handle; idle/wake/idle; recovery; stop; and quiescence.

**R-SUB-03. Mailbox and channels.** Cover roster, direct mail, `announce`, channel join/send/list/leave, pre-join and post-leave exclusion, and cross-unit refusal.

**R-SUB-04. Limits and models.** Cover depth, concurrency, run, turn, and message budget failures. At least one slice uses each real model. Mixed-model children must show their actual route in Events.

**R-SUB-05. Real TUI children.** Cover real TUI child observation and input isolation as part of the Herdr evidence.

### Workflow

**R-WF-01. Catalog and binding.** Use playground `.hya/workflows` fixtures to verify catalog precedence/revision, info, select/state persistence, stale revision rejection, and Session binding.

**R-WF-02. Execution and presentation.** Verify fan-out/fan-in with parallel Stages, child links, deterministic join sections, durable statuses, and TUI presentation.

**R-WF-03. Outcomes and recovery.** Verify fail-fast and collect-all, cancellation through Session abort, idempotent replay/retry, changed-input operation conflict, invalid graph, missing Agent, and failed Stage.

**R-WF-04. Coverage registry.** Add the existing P17 Workflow test and T2.12 mailbox contract to `crates/hya-e2e/matrix.toml`, and update stale `docs/testing/process-e2e.md` coverage text.

### Real coding capability

**R-CODE-01. Fresh isolated tasks.** Prepare small isolated Rust and TypeScript fixtures. Run each real model in a fresh Session through inspect, diagnose, edit, focused test, and report. Accept only observed disk diffs and command results.

**R-CODE-02. Route-specific editing.** GPT must use `apply_patch` because its route filters `write` and `edit`. GLM must exercise `write`, `edit`, and one patch path.

**R-CODE-03. Coding failure and recovery matrix.** Include a compile/type error, failing test, lint/format issue, merge-conflict-like file, large search, binary file, network-dependent request, denied action, human question, aborted Turn, resume, and post-error recovery.

### SWE-Bench Pro

**R-SWE-01. Pinned authoritative assets.** Use evaluator `scaleapi/SWE-bench_Pro-os@ca10a60a5fcae51e6948ffe1485d4153d421e6c5` and dataset `ScaleAI/SWE-bench_Pro@7ab5114912baf22bb098818e604c02fe7ad2c11f`. The dataset is public/ungated and has 731 rows. The evaluator is MIT; dataset metadata declares no license. Do not redistribute dataset rows, repository snapshots, or images.

**R-SWE-02. Fixed deterministic sample.** Use seed `hya-swebench-pro-live-8x2-v1`. Select two rows each from Go, JavaScript, Python, and TypeScript. Sort by `sha256(seed + NUL + instance_id)` per language and prefer a second repository. Eligibility uses only nonempty identity/base/image fields and evaluator asset existence. Freeze IDs, row hashes, prompt hashes, base commits, and Docker image digests before the first API request.

**R-SWE-03. Exact prompt bytes.** Prompt bytes are exactly publisher `problem_statement`, then `Requirements:`, then `requirements`, then `New interfaces introduced:`, then `interface`. Gold patch, test patch, test names, scripts, and evaluator-only fields stay outside Hya worktrees and mounts.

**R-SWE-04. Independent one-Turn attempts.** Each attempt uses a fresh detached worktree at `base_commit` and exactly one Hya task Turn with no retry, resume, hints, or shared patch. The backend surface is `hya-backend --model 12th-oai/gpt-5.6-sol --yolo --db <private> exec --json <prompt>`. The TUI surface launches current-source `hya` in a fresh Herdr pane, approves requested actions without task hints, and exits only at idle.

**R-SWE-05. Patch and evaluator integrity.** Capture all text-file changes as a full-index patch. Reject binary patches and verify application against a separate clean base. Run backend and TUI predictions in separate official local-Docker evaluator invocations because results key only by `instance_id`. Read Booleans from `eval_results.json`; process exit alone is not a score.

**R-SWE-06. Retained evidence.** Retain prompt, binary, and patch hashes; source commit; Session DB/Event JSONL; TUI export; stderr/status; permission decisions; base HEAD/status; Docker tag/digest/image/platform; evaluator patch, entry script, stdout, stderr, and output; token usage; and start/end timestamps.

**R-SWE-07. False outcomes.** A crash, empty or invalid patch, unanswered content question, dirty base, evaluator exception, or missing required test is false, not omitted.

**R-SWE-08. Fixed allocation and relay cap.** The SWE-Bench scope is fixed at eight blind-selected instances and 16 independent GPT runs: eight backend and eight Herdr/TUI. GLM still receives the complete functional, slash-resource, CoT, subagent, Workflow, and coding suite. The live-provider relay has a global hard maximum of 2,000 forwarded requests; this maximum is not a target and request 2,001 is rejected before forwarding.
**R-SWE-09. Frozen-row blocking.** If a frozen SWE-Bench row lacks a required pinned evaluator asset or Docker image during preflight, mark that fixed row blocked. Do not substitute another row after model execution begins.

### Source-audit RED candidates

**R-RED-01. Responses typed terminal.** The source audit found that `OpenAiResponsesProtocol` constructs a permissive decoder and that EOF or `[DONE]` without `response.completed` or `response.incomplete` can close as success, while Grok already requires a typed terminal. Treat this as a candidate only. A deterministic RED must prove that both OpenAI Responses and Grok reject an untyped terminal/EOF while Chat Completions remains unchanged. Likely files are `crates/hya-provider/src/openai/responses.rs`, `response_decoder.rs`, and `crates/hya-provider/tests/http_headers.rs`.

**R-RED-02. Compat V2 background errors.** The audit found a spawned `run_turn...` in `crates/hya-server/src/compat/session_prompt.rs` that discards its `Result`, while the older `/prompt_async` path publishes error/status Events. A deterministic RED must prove that provider failure on `/api/session/:id/prompt` emits a Session error, returns idle, and permits a later Turn, reusing the existing async publication seam.

**R-RED-03. Coverage registry drift.** The audit found no matrix row for P17 Workflow, no contract row for T2.12 cross-unit refusal, and stale P01–P16 wording in process docs. The exact registry/docs entries and matrix-check/focused evidence are required.

**R-RED-04. Custom slash-resource coverage gap.** Existing catalog/route tests prove metadata and Skill expansion, P05 proves direct `skill`, and P06 plus plugin tests prove direct resources, but no process/real-TUI test proves slash admission to expanded prompt to user Skill/plugin/MCP Tool to replay and recovery. This is a missing verification candidate, not a known runtime defect; the registered P18 and T3.4 contracts are required.

**R-RED-05. Exact `/model` and `/think` dispatch.** The frontend registers `/models` and `/variants`, while the server advertises `/model` and `/think`. A deterministic PTY RED must prove that exact no-argument `/model` and `/think` open `DialogModel` and `DialogVariant` with no `CommandExecuted` and no provider round. Add `model` and `think` to existing local aliases only after RED. In `autocomplete.tsx::commands`, compute names claimed by local primaries and aliases and omit colliding server rows. Argument-bearing server command submission remains unchanged.

**R-RED-06. Workflow discovery.** Routes intercept `command="workflow"`, but the catalog does not publish it, so TUI submission can fall through to a prompt. A catalog/PTY RED is required. The candidate publication is `command_info("workflow", "inspect or run workflows", "/workflow $ARGUMENTS", vec!["$ARGUMENTS"], None)`, with `workflow::intercept_slash` as the only executor.

**R-RED-07. Command request errors.** `submitInner` discards `sdk.client.session.command(...)` and does not inspect a nonthrowing error response. A deterministic PTY RED must prove a command-route failure. The candidate behavior is to await with `{ throwOnError: true }`, show `toast.show({ title: "Failed to run command", message: errorMessage(error), variant: "error" })`, preserve submitted history, and prove that the next command succeeds in the same Session.

**R-RED-08. Conditional findings.** Other audit findings become changes only after deterministic RED confirmation. Unknown YAML keys and offline fallback are documented behavior and are not changed for this task.

## Constraints

**C-01. No silent scope changes.** Preserve the exact routes, boundaries, fixtures, matrices, pinned assets, request cap, error variants, persistence promises, and completion outcomes in this PRD. Do not reduce the approved scope or add a behavior that is not stated here or in the verified research copy.

**C-02. Source and artifact boundaries.** Product source is changed only when a source-audit candidate has a deterministic RED. The PRD worker edits only this PRD. The design and implementation artifacts retain their assigned roles; the implementation artifact carries execution sequence, focused checks, exact validation command invocations, repair-loop mechanics, and cleanup mechanics.

**C-03. Production safety.** Do not try to force malformed streams, TLS faults, or throttling against production. Use deterministic local faults for those contracts. A real remote ghost-model rejection and one isolated invalid-credential rejection are the only required real provider failures of those types.

**C-04. Resource collision safety.** If a custom plugin Tool name collides with an existing Tool, the fixture must fail registration with the existing duplicate-name error. Rename only the test fixture before any evidence run. Do not add alias or last-writer-wins behavior. Command/Skill collision follows the specified command precedence and is not a runtime resource collision.

**C-05. Dynamic snapshot boundaries.** Preserve old `TurnBinding` values. Skill edits publish on the next root Turn; MCP reconnect publishes on the next root Turn; plugin declaration changes require backend restart. TUI command/Skill slash metadata and plugin declarations refresh only after TUI restart because `sync.data.command` is bootstrap-cached. Workflow uses its existing catalog revision contract.

**C-06. Model adherence contingency.** If a real model ignores an explicit custom-resource command while deterministic P18 passes, classify the result as model adherence. One tighter nonce-bearing prompt is allowed within the global cap. Do not change Hya solely to force a model Tool choice.

**C-07. Evidence and retention boundary.** Retain only the approved evidence classes. Never copy secrets into any retained artifact. Keep benchmark evaluator-only material outside Hya worktrees and mounts, and do not redistribute restricted or unlicensed benchmark material.
**C-08. Critical source anchors.** The approved audit anchors the following symbols and files for any later deterministic investigation; they are not permission to edit product source without the RED rule:

- `crates/hya-server/src/compat/command_catalog.rs`: `list`, `expand_prompt`, `expand_template`, and `add_skill_commands`; owns source precedence and exact command prompt expansion;
- `packages/hya-tui-ts/src/upstream/app.tsx`, `packages/hya-tui-ts/src/component/prompt/autocomplete.tsx`, and `packages/hya-tui-ts/src/component/prompt/index.tsx`; own slash registration/merge/submit and `prompt.skills`, exact local configuration dispatch, deduplication, multiline command arguments, and Skill picker insertion;
- `crates/hya-server/src/compat/command_sources.rs`: `config_commands`, `disk_commands`, `command_hints`, and `parse_command_file`; owns supported project sources, recursive names, metadata parsing, and malformed-source omission;
- `crates/hya-e2e/tests/p18_custom_slash_resources.rs`; owns the new process seam joining custom commands to Skill/plugin/MCP Tool execution, structured failure replay, recovery, and restart;
- `crates/hya-provider/src/openai/responses.rs` and `crates/hya-server/src/compat/session_prompt.rs`; contain the two independent initial RED candidates and require exact constructor/spawn-path review before edits.

Do not add command-specific Agent/model/subtask routing, a test-only product hook, a generic retry layer, an alternate frontend, or a new E2E framework.
**C-09. Live-start boundary.** Snapshot relevant workspace state without touching unrelated or untracked files. Create the playground, private XDG roots, databases, sanitized evidence ledger, fixtures, and cleanup registry before live execution. Start the counting relay and make no live provider call before its approved boundary is ready.

## Out of Scope

**OOS-01.** Installed binaries, a second frontend, or a replacement UI are not accepted or added.

**OOS-02.** CodeGraph initialization is not part of this task and requires separate user approval.

**OOS-03.** No test-only product hook, new E2E framework, generic retry layer, symptom suppression, or speculative source change is allowed.

**OOS-04.** Do not add command-specific Agent/model/subtask routing. The existing `agent`, `model`, and `subtask` command metadata remains listing-only.

**OOS-05.** OAuth login/logout slash commands are not added. Only their CLI/dialog error paths are in scope. Do not invent registrations for commands absent from the source-defined local/backend lists.

**OOS-06.** `.hya/commands` and home/global command directories are unsupported roots for this task. Do not add discovery for them.

**OOS-07.** Unknown YAML keys and documented offline fallback are not changed unless a separate deterministic RED proves a defect in the required behavior.

**OOS-08.** SWE-Bench work is diagnostic Pass@1, not leaderboard reproduction. Do not redistribute dataset rows, repository snapshots, or images. Do not substitute a frozen row after model execution starts because of a missing asset.

**OOS-09.** Do not force malformed production streams, TLS faults, or throttling, and do not alter duplicate Tool collision semantics. Do not omit a failed benchmark or error case to improve a score.

## Acceptance Criteria

The following criteria are observable and map to the requirements above. The implementation artifact owns the execution order and exact command invocations; these criteria define only the required outcomes and evidence.

### Runtime, security, and evidence

**AC-ENV-01 (R-SCOPE-01, R-SCOPE-02, R-ENV-01–04).** Current-source `hya`, `hya-ts`, and `hya-backend` run in the private isolated runtime; the self-contained configuration resolves both exact `12th-oai` routes; the required reasoning variants are visible; the private relay never forwards request 2,001; and no installed binary or OMP runtime access is used.

**AC-ENV-02 (R-ENV-05).** The OMP source hash is unchanged. No credential appears in argv, environment paths, logs, Events, prompts, patches, Docker mounts, Herdr captures, reports, or retained benchmark evidence. The literal private copy is removed at cleanup and only a redacted template/hash remains.

**AC-EVID-01 (R-ART-01, R-SCOPE-03–07).** The evidence ledger contains canonical Event/SQLite, HTTP/SSE, disk-diff, process, Herdr, and evaluator evidence; each reproduced defect has RED, minimal fix, focused GREEN, and original-scenario evidence; product fixes obey version/changelog rules; benchmark failures remain counted; model/provider non-compliance is classified without speculative source changes; and the approved artifacts are validated and activated only after user approval.

### Backend, API, Tools, and errors

**AC-BE-01 (R-BE-01–05).** The CLI, `exec`, Compat, prompt/goal, serve, JSONL RPC, storage, Session lifecycle, native/Compat routes, and invalid/concurrent/restart cases all have observed success or defined failure evidence.

**AC-BE-02 (R-BE-06, R-ERR-04).** Background provider and Workflow failures produce bounded durable error/status Events, return idle, preserve structured failure in replay and the next provider request, do not execute again during replay, and allow a later valid Turn in the same Session.

**AC-TOOL-01 (R-TOOL-01–03).** The advertised registry is exactly the 28 specified Tools, the five hidden aliases resolve as specified, GPT emits exactly 26 filtered builtins with `write`/`edit` absent and `apply_patch` present, and GLM emits all 28.

**AC-TOOL-02 (R-TOOL-02, R-TOOL-04).** Every builtin has its required success and applicable boundary evidence. Each Tool call has one `ToolInputStart`, optional deltas, one `ToolCallRequested`, and exactly one terminal `ToolResult` or `ToolError`; structured failures appear in the next request; and later Tool calls and Turns recover.

**AC-ERR-01 (R-ERR-01–03).** Every listed `ProviderError` and `ToolError` variant has an observable deterministic or permitted live case. Session not-found/busy/conflict, invalid command/request, TUI disconnect/reconnect, MCP/plugin unavailable, Workflow stale/invalid/missing-Agent, and exhausted subagent budgets are represented with bounded structured errors and recovery outcomes.

### Herdr and command surfaces

**AC-TUI-01 (R-TUI-01–04, R-TUI-09).** A task-owned real Herdr pane runs current-source Hya in the playground. ANSI and visible captures prove all startup/loading/offline/error, composer, multiline, busy/idle, streaming text/reasoning, Tool, permission, question, toast, abort, reconnect, exit/restoration, resize, scroll, fold/unfold, status/usage, child observation/input-isolation, and restart-persistence outcomes.

**AC-TUI-02 (R-TUI-05).** Ctrl+P is keymap-only and proves filter/navigation/cancel/execute plus route, hidden, and disabled omission. User command files, Skill-derived commands, plugin Tools, and MCP Tools do not appear there. Slash autocomplete exposes server commands while excluding `source="skill"`, and `/skills` exposes Skills.

**AC-TUI-03 (R-TUI-06–08, R-TUI-10).** Every listed local slash and alias and every listed backend slash has been exercised. Exact no-argument `/model`, `/think`, `/agents`, `/mcps`, and Workflow controls act locally/server-side without an ordinary model round where required; unknown slash text is unchanged ordinary prompt input; `!` is playground-only shell mode; OAuth remains CLI/dialog-only; and no unlisted registration exists.

**AC-TUI-04 (R-REASON-02, R-RES-08).** In the real pane, `/model` switches effort/model state without an unintended provider round for the no-argument picker path, GPT and GLM show their required reasoning presentation, and the custom slash/resource interactions produce captured expanded rows, permission decisions, Tool cards, terminal results, and recovery state on both models.
**AC-TUI-05 (R-RES-08, R-REASON-02).** In the task-owned pane, select `user-playbook` through `/skills`, submit `/use-skill SKILL_<nonce>`, `/use-plugin "PLUGIN <nonce>"`, and the multiline `/use-mcp MCP_<nonce>\nsecond-line` command. Captures show expanded user text rather than a literal template, `SKILL_BODY_<nonce>`, the plugin `remember` output, `echo:MCP_<nonce>`, one correlated custom Tool card per command, a structured error card for the forced failure, a later success in the same Session, and the same commands after backend/TUI restart. Switching `/model` between GPT and GLM retains Event/model ids with the ANSI frames.

### Custom commands, Skills, plugins, MCP, and dynamic resources

**AC-RES-01 (R-RES-02–05).** `/api/command` and route evidence cover all supported JSON/JSONC and Markdown sources, exact metadata/templates/hints, recursive names, quoted/empty/missing/multiline/positional expansion, `$ARGUMENTS` and `$1`–`$10` exactly once, malformed omission, ignored `disable`, no duplicates, specified precedence, unsupported-root omission, and literal fallback for unknown names. Existing listing-only metadata remains present.

**AC-RES-02 (R-RES-01, R-RES-05, R-RES-06).** Direct `/user-playbook` and `/skills` selection admit the Skill body and arguments without a redundant builtin `skill` call and are not blocked by `Action::Skill` deny. `/use-skill`, `/use-plugin`, and `/use-mcp` each admit the exact expanded prompt, invoke the intended builtin/plugin/MCP Tool, replay its real output to the model, render one correlated Tool card, and return the final marker/result.

**AC-RES-03 (R-RES-06).** Each of T1.16 through T1.23 is separately named and separately registered in Track P. Catalog/route, Skill, builtin Skill, plugin, MCP, collision, reload, structured-error, restart, permission, process-death, malformed-frame, drift, and same-Session recovery outcomes match their stated contracts; no passing resource masks another failure.

**AC-RES-04 (R-RES-07).** Track T row T3.4 exists and proves PTY/Herdr command transport, autocomplete/picker separation, deduplication, multiline argument preservation, contextual command failure toast, same-Session recovery, and known/added/removed command name caching across live edits and TUI restart.

**AC-RES-05 (R-RES-10, C-04–06).** MCP, plugin, AgentBundle, Skill, Workflow, loopback web, and LSP/formatter dynamic contracts hold. Runtime generation collisions fail closed without partial publication; plugin declaration drift and process death use the specified no-auto-respawn/restart boundaries; old bindings stay pinned; and a model-adherence classification uses only the allowed tighter prompt contingency.

### Reasoning, subagents, Workflow, and coding

**AC-REASON-01 (R-REASON-01–04).** GPT provides summary-style reasoning Events and visible Thinking content; GLM provides full reasoning-text Events and visible content; effort state is sanitized and persisted as documented; fold/unfold preserves order and part keys; disabled/off is covered; and absent reasoning is classified correctly.

**AC-SUB-01 (R-SUB-01–05).** Subagent/team evidence proves canonical ancestry, actual model routes, all lifecycle, resident/background, mailbox/channel, input-isolation, parallel, nested, override, and budget outcomes. Mixed-model child Events identify their actual route.

**AC-WF-01 (R-WF-01–04).** Workflow fixtures prove catalog/revision/select/state/Session binding, fan-out/fan-in and deterministic joins, durable status/TUI presentation, fail-fast/collect-all, abort cancellation, idempotent replay/retry, operation conflict, invalid graph, missing Agent, failed Stage, and the P17/T2.12 matrix/docs coverage repairs.

**AC-CODE-01 (R-CODE-01–03).** Fresh Rust and TypeScript coding Sessions for both models show inspect, diagnose, edit, focused test, and report with observed diffs/results. GPT uses `apply_patch`; GLM uses `write`, `edit`, and a patch path; and every listed coding error, denied/question/abort/resume, search/binary/network, and recovery case is observed.

### SWE-Bench Pro

**AC-SWE-01 (R-SWE-01–03, R-SWE-09).** The evaluator and dataset resolve to the pinned commits, the eight rows are selected and frozen by the exact seed/order/eligibility rule, all hashes and image digests are frozen before live requests, prompt bytes use the exact publisher-field sequence, evaluator-only assets stay outside Hya worktrees/mounts without redistribution, and a frozen row missing a required asset or image is marked blocked without post-execution substitution.

**AC-SWE-02 (R-SWE-04–07).** Each backend and Herdr attempt uses a fresh base worktree and exactly one Turn with no retry/resume/hint/shared patch, captures a full-index text patch, rejects binary/invalid application, runs separate official local-Docker evaluations, reads Boolean scores from `eval_results.json`, retains the required evidence, and records crashes/empty patches/questions/dirty bases/evaluator exceptions/missing tests as false.

**AC-SWE-03 (R-SWE-08).** The fixed scope contains eight blind-selected instances and 16 independent GPT runs (eight backend and eight Herdr/TUI). GLM receives the complete non-benchmark suite. No run forwards beyond the global 2,000-request cap.

### Source-audit repairs and final completion

**AC-RED-01 (R-RED-01–08).** Each source-audit candidate is classified as confirmed defect, verified behavior, or missing coverage using deterministic RED evidence. Confirmed defects have the smallest fix, focused GREEN, original rerun, and relevant cross-layer evidence. Unconfirmed candidates cause no speculative change; documented unknown-YAML/offline behavior remains unchanged.

**AC-FINAL-01.** The self-contained private Hya configuration resolves and calls both models without OMP runtime access.

**AC-FINAL-02.** No credential leaks and the OMP source hash is unchanged.

**AC-FINAL-03.** The backend/API success and failure matrix has Event/SQLite proof.

**AC-FINAL-04.** Real Herdr pane ANSI captures cover the complete TUI command/control, wide/narrow rendering, folding, interaction, reconnect, and read-only child matrix.

**AC-FINAL-05.** Every source-defined local/backend slash command and alias is exercised; exact `/model` and `/think` open configuration dialogs without provider traffic; `/workflow state` reaches shared Workflow control with zero provider traffic; and autocomplete has no duplicate exact names.

**AC-FINAL-06.** All 28 builtins, five aliases, dynamic planes, every `ProviderError`, every `ToolError`, and recovery behavior are covered.

**AC-FINAL-07.** Every supported user-defined slash source is discovered with deterministic precedence. Direct Skill, `/skills`, Markdown, inline JSON/JSONC, nested, quoted, positional, and multiline forms admit the exact expected prompt.

**AC-FINAL-08.** P18 and real Herdr runs prove that custom slash commands invoke `skill`, plugin `remember`, and `mcp__echo__ping`, replay real outputs to the model, surface permission/resource failures, recover in the same Session, and remain available after restart.

**AC-FINAL-09.** GPT summary CoT rendering, folding, and state proof are retained. The remaining GLM full-CoT live slice is waived by the 2026-08-30 user decision; historical GLM reasoning behavior is retained as evidence but is not a pass claim.

**AC-FINAL-10.** GPT subagent/team and Workflow lifecycle proof is retained and accepted for finalization. Remaining GLM-driven subagent and Workflow validation is waived by the user decision and is not represented as passed.

**AC-FINAL-11.** Independent GPT real coding-task diffs and observed tests are retained and accepted for finalization. Remaining GLM coding validation is waived by the user decision and is not represented as passed.

**AC-FINAL-12.** User-selected SWE-Bench official evaluator Booleans are retained, including failures.

**AC-FINAL-13.** RED/GREEN/original-scenario evidence exists for every repair.

**AC-FINAL-14.** All applicable Rust, process E2E, TUI, Track T, matrix, and executable-build gates pass; the exact gate commands are implementation-owned and are not repeated as a PRD execution checklist.

**AC-FINAL-15.** All processes, containers, panes, relays, fixtures, Docker containers, MCP/plugins, and watchdogs are stopped. Only secret/runtime artifacts are removed, benchmark evidence without credentials is preserved, and Trellis quality/spec review and task finish/archive occur only after evidence and repository feature-commit/push rules are satisfied.
**AC-FINAL-16 (R-FINAL-01).** The live scheduling proof passed on release `0.36.5`: preferred `gpt56-primary/gpt-5.6-sol#low` produced three deterministic pre-stream HTTP 503 attempts, then ordered fallback `gpt56-fallback/gpt-5.6-sol#high` used `reasoning_effort=high` and returned HTTP 200 at relay ordinal 2. The fallback response was exactly `GPT56_SCHEDULED_GREEN_A30`; one Session (`hysec_3xMXopbQV0NsMGHScP0J`) persisted 14 Events with matching stdout/SQLite sequence identity and one assistant delivery, with no mid-stream replay. The focused regression was RED before the fix (Low versus High, `artifact://1810`), then GREEN at 1/1; the complete `model_fallback` suite passed 7/7. The mode-0600 private `provider.key` was the only credential-scan hit and Git evidence had zero secret hits. This proof remains applicable to the current final release `0.36.6` because only TypeScript Session-cache lookup/test code and release metadata changed; scheduling-sensitive source is unchanged. Overall status is `passed-with-user-approved-GLM-waiver`, and final gates, cleanup, and the Trellis quality review passed. The worktree is ready to commit, but this review did not commit it.
