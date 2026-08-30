# Implementation Plan

This file is the executable runbook for the approved Hya full-agent validation plan. Run the steps in order. Do not mark a step complete until its check passes and its evidence is written to the sanitized ledger. A failed check is a stop-and-repair point, not permission to continue with a weaker substitute. No product source change is allowed outside the repair loop below.

The authoritative plan is `research/approved-plan.md` with SHA-256 `abbe961e90d65b71e9b7218f8cec44d61be22e414b1c0a41f8ce47c9c03b068a`. This runbook preserves its scope and uses the PRD's source-order identifiers:

| Source-plan domain | PRD requirement | Acceptance | Primary proof |
| --- | --- | --- | --- |
| Scope and source boundaries | R-SCOPE-01–07 | AC-EVID-01; AC-FINAL-01–15 | source hash, baseline, route, evidence, release rules |
| Artifact activation | R-ART-01 | AC-EVID-01 | approved artifacts, validation, and post-approval activation |
| Private runtime and credential boundary | R-ENV-01–05; C-09 | AC-ENV-01–02; AC-EVID-01; AC-FINAL-01–02 | private root modes, config, source hash, leak audit |
| Backend and API | R-BE-01–06 | AC-BE-01–02; AC-EVID-01; AC-FINAL-03 | CLI/API output, Events, SQLite, SSE/RPC records |
| Herdr-driven TUI | R-TUI-01–10 | AC-TUI-01–05; AC-FINAL-04–05, AC-FINAL-09 | pane output, ANSI frames, process and restoration records |
| Canonical Tools | R-TOOL-01–04 | AC-TOOL-01–02; AC-ERR-01; AC-FINAL-06 | tool schemas, Tool Events, result/error replay |
| Custom slash resources | R-RES-01–10 | AC-RES-01–05; AC-TUI-02–05; AC-FINAL-07–08 | catalog, command transport, Skill/plugin/MCP traces |
| Defined errors and recovery | R-ERR-01–04 | AC-ERR-01; AC-BE-02; AC-RES-05; AC-FINAL-03, AC-FINAL-06 | typed errors, bounded Events, idle and later-turn proof |
| Reasoning and CoT | R-REASON-01–04 | AC-REASON-01; AC-TUI-04–05; AC-FINAL-09 | reasoning Events, effort state, ANSI folding frames |
| Subagents and team mailbox | R-SUB-01–05 | AC-SUB-01; AC-FINAL-10 | ancestry, Member lifecycle, mailbox and route Events |
| Workflow | R-WF-01–04 | AC-WF-01; AC-BE-02; AC-FINAL-05, AC-FINAL-10 | catalog revisions, stage statuses, joins, TUI records |
| Real coding capability | R-CODE-01–03 | AC-CODE-01; AC-FINAL-11 | observed diffs, command results, focused test reports |
| SWE-Bench Pro | R-SWE-01–09 | AC-SWE-01–03; AC-FINAL-12 | frozen sample, hashes, evaluator Booleans and artifacts |
| Source-audit repairs | R-RED-01–08 | AC-RED-01; AC-EVID-01; AC-FINAL-13 | RED, minimal GREEN, original-scenario reruns |
| Release and repository gates | R-SCOPE-06 | AC-EVID-01; AC-FINAL-14–15 | version/changelog, gate output, commit/push record |
| Cleanup and rollback | R-ENV-05; C-01–07, C-09 | AC-ENV-02; AC-EVID-01; AC-FINAL-15 | cleanup registry, retained evidence, rollback record |
| Final GPT scheduling acceptance | R-FINAL-01 | AC-FINAL-16 | category chain, pre-stream fallback, Session/Event, and security evidence |

## Operating rules and evidence contract

- [ ] Treat current worktree source as the only implementation under test. Build `hya`, `hya-ts`, and `hya-backend` from this worktree; installed binaries do not satisfy acceptance.
- [ ] Use Simplified Technical English in records. Record the scenario ID, model route, source commit, Session ID, request ordinal when live, command, expected observable, actual observable, and artifact path for every check.
- [ ] Use canonical Events and SQLite as the source of truth. Model prose is not proof. Also retain HTTP/SSE observations, disk diffs, process exit status, focused command output, and Herdr pane captures where applicable.
- [ ] Use deterministic FakeLlm and loopback fault services before live provider traffic. Use real models only for organic coding, reasoning, TUI, subagent, and Workflow proof. Classify model non-compliance as model behavior unless deterministic Hya behavior also fails.
- [ ] For every source defect, follow exactly: deterministic RED -> smallest root-cause fix -> focused GREEN -> original scenario rerun -> relevant cross-layer checks. Do not add a generic retry or suppress a symptom.
- [ ] Preserve all boundaries, fixtures, commands, pinned assets, test names, request limits, and negative cases below. Do not add command-specific Agent/model/subtask routing, a test-only product hook, an alternate frontend, or a new E2E framework.
- [ ] Do not execute a later phase when its preceding phase has an unresolved stop condition. This includes no live request before the relay and private configuration gates pass, and no source fix before its RED test fails for the intended reason.

## Final scope decision (2026-08-30)

The user explicitly approved this finalization scope on 2026-08-30. Remaining GLM functional, coding, subagent, Workflow, and reasoning validation is waived for this task, and completed GPT-only validation is accepted for finalization. This is a user-approved scope waiver, not a GLM pass.

Preserve the historical GLM successful custom-MCP Turn and later upstream HTTP 503 classification as evidence. GPT coverage and the diagnostic SWE-Bench accounting remain separately classified. The R-FINAL-01/AC-FINAL-16 scheduling contract passed in a live run on release `0.36.5` and remains applicable because scheduling-sensitive source is unchanged in current final release `0.36.6`. Overall acceptance is `passed-with-user-approved-GLM-waiver`; final gates, cleanup, and the Trellis quality review passed, so the worktree is ready to commit.

## Implementation ownership anchors (R-RES-03, R-RES-05–07, R-RED-01–02; AC-RES-01–04, AC-RED-01)

- [ ] `crates/hya-server/src/compat/command_catalog.rs`: inspect `list`, `expand_prompt`, `expand_template`, and `add_skill_commands`; this owns source precedence and exact command prompt expansion.
- [ ] `packages/hya-tui-ts/src/upstream/app.tsx`, `packages/hya-tui-ts/src/upstream/component/prompt/autocomplete.tsx`, and `packages/hya-tui-ts/src/upstream/component/prompt/index.tsx`: inspect slash registration/merge/submit and `prompt.skills`; this owns local configuration dispatch, deduplication, multiline command arguments, and Skill picker insertion.
- [ ] `crates/hya-server/src/compat/command_sources.rs`: inspect `config_commands`, `disk_commands`, `command_hints`, and `parse_command_file`; this owns supported project sources, recursive names, metadata parsing, and malformed-source omission.
- [ ] `crates/hya-e2e/tests/p18_custom_slash_resources.rs`: use the new process seam for custom slash admission, Skill/plugin/MCP Tool execution, structured failure replay, recovery, and restart.
- [ ] `crates/hya-provider/src/openai/responses.rs` plus `crates/hya-server/src/compat/session_prompt.rs`: reread exact decoder constructors and spawned-run paths before any candidate edit; edit only after the corresponding deterministic RED.

## Phase 0 — Persist and activate the approved plan (R-ART-01, R-SCOPE-01–07, R-RED-03; AC-EVID-01, AC-RED-01)

### 0.1 Confirm the planning inputs

- [ ] Confirm the task root is `.trellis/tasks/08-29-full-agent-live-validation`.
- [ ] Confirm `research/approved-plan.md` exists and matches SHA-256 `abbe961e90d65b71e9b7218f8cec44d61be22e414b1c0a41f8ce47c9c03b068a`.
- [ ] Preserve every decision, boundary, matrix item, acceptance condition, command, pinned asset, source-audit candidate, and contingency from that file in the task artifacts. `prd.md` contains requirements/constraints/observable acceptance only; `design.md` contains boundaries/contracts/data flow/tradeoffs/rollback; this file contains ordered execution/checks/commands/repair/cleanup.
- [ ] Confirm `implement.jsonl` and `check.jsonl` contain real spec/research entries, not the seed `_example` row. Curate them from the backend, frontend, guides, and official SWE-Bench sources as assigned by the parent.
- [ ] Confirm no product source, unrelated untracked file, or task metadata is edited by this runbook. The only file this worker creates is this `implement.md`.
- [ ] Record that the source audit sent no live provider request and changed no product code. CodeGraph is not initialized for this repository; do not create an index without separate user approval (FACT-01, FACT-05, OOS-02).
- [ ] Read `skill://trellis-before-dev` and its applicable project specs before the first product-source edit (R-ART-01, AC-EVID-01).

### 0.2 Validate and activate

Run the artifact gate from the repository root:

```sh
python3 ./.trellis/scripts/task.py validate .trellis/tasks/08-29-full-agent-live-validation
```

After the user-approved artifact review, activate the task:

```sh
python3 ./.trellis/scripts/task.py start .trellis/tasks/08-29-full-agent-live-validation
```

- [ ] Record validation output and task activation/status in the ledger.
- [ ] **Go/no-go (R-ART-01, AC-EVID-01):** `prd.md`, `design.md`, `implement.md`, research, and both curated manifests are present; the plan hash matches; validation passes; activation is explicitly approved. If any condition fails, stop before baseline work.

## Phase 1 — Baseline and private runtime (R-SCOPE-01–03, R-ENV-01–05; AC-ENV-01–02, AC-EVID-01)

### 1.1 Snapshot the baseline (R-SCOPE-01, R-ENV-05, C-09; AC-ENV-01–02)

- [ ] Record repository status, current source commit, and planning-baseline package version `0.36.0`; after repairs, record the final aligned release version separately. Do not touch unrelated or untracked files.
- [ ] Record the confirmed local tool versions: Docker `28.5.1`, Bun `1.3.10`, Rust/Cargo `1.92.0`, and Herdr `0.8.1`.
- [ ] Record that `/home/yanweiye/Projects/hya-playground` does not exist before setup; execution creates it.
- [ ] Record the OMP registry path `/home/yanweiye/.omp/agent/models.yml` and hash it before any live run. Do not print, copy into evidence, or expose its credential value.
- [ ] Build current-source binaries before functional validation. Record the source commit and executable paths. Installed binaries cannot be used as proof.

### 1.2 Create the isolated runtime (R-ENV-01, C-09; AC-ENV-01)

- [ ] Create `/home/yanweiye/Projects/hya-playground` as the task-owned project/worktree root.
- [ ] Create a private run root outside the playground, with private `HOME` and all XDG roots. Record the paths in the cleanup registry but do not put secrets in command arguments.
- [ ] Store Hya SQLite databases and credential-bearing configuration below a mode-0700 private directory with files mode 0600.
- [ ] Create the sanitized evidence ledger and cleanup registry before any provider request. The ledger stores no credential, header, request body, response body, prompt secret, or copied key.

### 1.3 Write the self-contained Hya configuration (R-ENV-02–03, R-SCOPE-02; AC-ENV-01)

Create one private Hya configuration in-process. Transfer the OMP `apiKey` value directly into Hya's private `config.yaml`; do not enter the key in a logged command. Its semantic shape is:

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

- [ ] Use a generated nonce for the ghost model name. Configure the ghost model only to send one real request that the remote service rejects as nonexistent.
- [ ] Verify a separate locally unconfigured model produces `ProviderError::UnknownModel` with zero outbound traffic.
- [ ] Verify the launched Hya process receives no OMP config path, key environment variable, or external Hya auth token. Hya must resolve its own config without OMP runtime access.
- [ ] Verify both requested model rows and all advertised reasoning variants before live traffic.

### 1.4 Enforce the secret boundary (R-ENV-05; AC-ENV-02)

- [ ] Never print the key or place it in argv, logs, Events, prompts, patches, Docker mounts, Herdr captures, or reports.
- [ ] Hash the OMP source file before and after the run; the hash must be unchanged.
- [ ] Register the private config literal, private DBs, private XDG roots, and runtime logs for final deletion. Retain only a redacted template and source hash.
- [ ] If any evidence contains a credential or a request/response body, quarantine and remove that evidence before continuing; do not copy it to another location.

### 1.5 Prepare the counted relay (R-ENV-04, C-09; AC-ENV-01)

- [ ] Start a private localhost relay only if needed for counted live runs. Configure the Hya base URL to the relay for those runs.
- [ ] The relay forwards to `https://api.12th.day/v1`, increments the ordinal before forwarding, records only ordinal, time, path, status, tool-schema names, and reasoning effort, and never stores headers or request/response bodies.
- [ ] Configure fail-closed behavior before forwarded request 2,001. The global hard maximum is 2,000 forwarded requests; 2,001 is rejected before forwarding. The maximum is a ceiling, not a target.
- [ ] **Go/no-go before any live request (R-ENV-01–05, C-09, AC-ENV-01–02):** private roots and file modes pass; self-contained config resolves; both model rows/variants pass; OMP hash is recorded; leak audit is clean; relay is ready and fail-closed; the request counter is zero and the evidence ledger is writable. Do not make a live request before this gate.

## Phase 2 — Deterministic RED/GREEN candidates (R-RED-01–08, R-BE-06, R-TUI-05–07, R-RES-06–07, R-ERR-04, R-SCOPE-05–06; AC-RED-01, AC-BE-02, AC-TUI-02–04, AC-RES-03–04, AC-EVID-01)

Run each candidate independently. The test must fail for the named behavior before the smallest fix. Run focused GREEN, then rerun the original scenario and record all three outcomes before starting the next candidate. Do not treat a source-audit observation as a confirmed defect until RED reproduces it.

### 2.1 Responses terminal correctness (R-RED-01; AC-RED-01)

- [ ] Add the deterministic RED for the source-audit candidate: `OpenAiResponsesProtocol` currently constructs a permissive decoder, so EOF or `[DONE]` without typed `response.completed` or `response.incomplete` can close as success; Grok already requires a typed terminal. Prove that both OpenAI Responses and Grok reject this untyped terminal/EOF while Chat Completions remains unchanged (R-RED-01, AC-RED-01).
- [ ] Inspect the exact decoder construction before editing: `crates/hya-provider/src/openai/responses.rs`, `response_decoder.rs`, and `crates/hya-provider/tests/http_headers.rs`.
- [ ] Apply the smallest typed-terminal fix. Do not broaden a generic decoder or add a retry.
- [ ] Run the focused GREEN for both OpenAI Responses and Grok, then rerun the original stream scenarios and the unchanged Chat Completions scenario. Record typed terminal, EOF, `[DONE]`, and unchanged-route results.

### 2.2 Compat V2 background error publication (R-RED-02, R-BE-06; AC-BE-02, AC-RED-01)

- [ ] Add RED for a provider failure on `/api/session/:id/prompt`. It must emit a durable Session error/status Event, return the Session to idle, and permit a later Turn.
- [ ] Inspect `crates/hya-server/src/compat/session_prompt.rs`; do not leave a `let _ = run_turn(...).await` path that silently discards failure.
- [ ] Reuse the existing `/prompt_async` error/status publication seam. Apply the smallest fix.
- [ ] Run the focused GREEN, then rerun the original V2 background prompt scenario, canonical replay, and same-Session later Turn.

### 2.3 Coverage registry drift (R-RED-03, R-WF-04; AC-WF-01, AC-RED-01)

- [ ] Add the existing P17 Workflow test and T2.12 cross-unit refusal contract to `crates/hya-e2e/matrix.toml`.
- [ ] Update stale `docs/testing/process-e2e.md` coverage text from P01–P16 to the actual registered coverage.
- [ ] Add no new scenario for this candidate. Run the focused matrix check and verify exact registry/docs entries.

### 2.4 Custom slash-resource coverage gap (R-RED-04, R-RES-06–07; AC-RES-03–04, AC-RED-01)

- [ ] Add the registered process scenario `crates/hya-e2e/tests/p18_custom_slash_resources.rs` using existing `E2eEnvBuilder::project_file`, `FakeLlm`, the P05 Skill fixture, and the P06 MCP echo fixture.
- [ ] Reuse `BackendProcess`'s temporary project current directory. Provide `.hya/plugins/toolbox/plugin.toml` with `command = ["python3", ".hya/plugins/toolbox/plugin.py"]`. Keep P18's `HYA_DEFER_SIDEPLANES=0` setup before the first Tool schema snapshot. No new harness adapter is needed (R-RES-06, AC-RES-03).
- [ ] Add exactly the eight separately named P18 process tests T1.16 through T1.23 below, with one `matrix.toml` Track P row per test. One passing resource must not hide another failure.
- [ ] Add Track T row T3.4 for `packages/hya-tui-ts/test/pty-smoke.test.ts`.
- [ ] Run each focused process/PTY check unchanged first. If a separate deterministic runtime defect appears, return to the relevant RED/minimal GREEN/original-scenario loop; do not change behavior only to make this coverage pass.

### 2.5 Exact `/model`, `/think`, `/workflow`, and command-error REDs (R-RED-05–07, R-TUI-05–07; AC-TUI-02–03, AC-RED-01)

- [ ] Add PTY RED proving exact no-argument `/model` and `/think` open `DialogModel` and `DialogVariant`, respectively, with no `CommandExecuted` Event and no provider round. Add `model` and `think` to existing local aliases. Keep argument-bearing server command submission unchanged.
- [ ] In `packages/hya-tui-ts/src/upstream/app.tsx`, `component/prompt/autocomplete.tsx`, and `component/prompt/index.tsx`, prove local primaries/aliases claim names before server rows are merged. Colliding server rows must be omitted, not duplicated.
- [ ] Add catalog/PTY RED proving `/workflow` is discoverable and intercepted without a provider round. Publish exactly `command_info("workflow", "inspect or run workflows", "/workflow $ARGUMENTS", vec!["$ARGUMENTS"], None)` and keep `workflow::intercept_slash` as the only executor.
- [ ] Add PTY RED for a deterministic nonthrowing command-route failure. Await `sdk.client.session.command(...)` with `{ throwOnError: true }`, show `toast.show({ title: "Failed to run command", message: errorMessage(error), variant: "error" })`, preserve submitted history, and prove the next command succeeds in the same Session.
- [ ] Run focused GREEN and original scenarios for all three slices before Phase 3.

### 2.6 Phase 2 go/no-go (R-RED-08, R-SCOPE-05–06; AC-RED-01, AC-EVID-01)

- [ ] Every intended RED failed for its intended reason, and no RED was caused by setup or an unrelated test.
- [ ] Every minimal GREEN passed, every original scenario was rerun, and evidence links the RED/GREEN/original sequence.
- [ ] Version and changelog updates are made only if product source changed. Do not update release files for test-only coverage.
- [ ] Treat other audit findings as changes only after deterministic RED confirmation. Keep documented unknown YAML keys and documented offline fallback unchanged for this task (R-RED-08, AC-RED-01).
- [ ] **Go/no-go:** no unresolved deterministic defect remains from the seven source-audit candidates; P17/T2.12 registry drift is repaired; T1.16–T1.23 and T3.4 are registered; focused evidence is complete. Only then enter the backend matrix.

## Phase 3 — Backend and API matrix (R-BE-01–06, R-TOOL-04, R-RES-03, R-ERR-01–04, R-WF-01–04; AC-BE-01–02, AC-TOOL-02, AC-ERR-01, AC-WF-01)

### 3.1 CLI and transport surfaces (R-BE-01–02, R-BE-04–05; AC-BE-01)

Exercise current-source behavior and retain stdout, stderr, exit status, structured output, and relevant Events/SQLite:

- [ ] `--help`, `--version`, `models`, `agent list`, `bundle list`, `bundle info`, `workflow list`, `workflow info`, `workflow state`, `sessions`, `tail-session`, and `auth list`.
- [ ] `exec` text, `exec --json`, Compat `run`, prompt mode, goal mode, `serve`, and JSONL `rpc`.
- [ ] In-memory store and persistent SQLite store, including restart recovery.
- [ ] Native and Compat synchronous prompt, asynchronous prompt, SSE/global Event, permission, question, shell, command, file, project/VCS, MCP, PTY, and TUI routes used by the frontend.
- [ ] Invalid JSON and IDs, duplicate prompt IDs, busy/conflicting Sessions, cancellation, provider errors, client disconnect, and restart recovery.
- [ ] Confirm background provider and Workflow failures produce a bounded durable error/status Event and leave the Session usable. No discarded `run_turn` result is acceptable.
### 3.2 Session lifecycle (R-BE-03, R-BE-05–06, R-ERR-04; AC-BE-01–02)

- [ ] Create, list, replay, resume, fork, compact, summarize, abort, delete, and missing-Session behavior.
- [ ] Prove Session status transitions and Event order in canonical storage for success, failure, cancellation, conflict, disconnect, and restart.
- [ ] For each terminal Tool call, prove exactly one result or error and a later valid call and later Turn in the same Session.
### 3.3 Focused backend commands (R-BE-01–06, R-RES-06, R-RED-02–04; AC-BE-01–02, AC-RES-03, AC-RED-01)

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

- [ ] Record each command's complete focused output and exit status. A passing command does not replace canonical Event/API evidence.
- [ ] **Go/no-go:** CLI/API, Session, persistence, and background-error invariants pass on current-source binaries; focused commands pass; no live request is needed for this phase. Proceed to the real pane only after this check.

## Phase 4 — Real Herdr TUI matrix (R-TUI-01–10, R-REASON-01–04, R-RES-07–08, R-SUB-05, R-WF-02; AC-TUI-01–04, AC-TUI-05, AC-REASON-01, AC-RES-02, AC-RES-04, AC-SUB-01, AC-WF-01)

### 4.1 Create and control the task-owned pane (R-TUI-01; AC-TUI-01)

Create a real Herdr **pane** (the user may call it a panel) in `/home/yanweiye/Projects/hya-playground`. Run current-source `hya` as an ordinary interactive pane process. Hya is not a verified `herdr agent --kind` target.

Use this control sequence, substituting the captured pane ID and current-source command:

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

- [ ] Parse the new pane ID from `.result.pane.pane_id`; store it in the cleanup registry. Do not use a guessed ID.
- [ ] Capture visible text and ANSI terminal frames. Herdr has no PNG screenshot command.
- [ ] Use the single-owner `terminal session control` stream for exact rows/columns. `pane resize` changes only split ratio.
- [ ] Close only the pane created by this task. Never stop the Herdr server.

### 4.2 Startup, rendering, interaction, and lifecycle (R-TUI-02–04, R-TUI-09; AC-TUI-01)

Capture a stable wide frame and a narrow frame for each applicable state:

- [ ] Startup, loading, offline, error, composer focus, multiline submit, busy/idle, streaming text, streaming reasoning, tool cards, permissions, questions, toasts, abort, reconnect, exit, and terminal restoration.
- [ ] Wide/narrow resize, scroll, transcript stability, tool folding/unfolding before/during/after streaming, reasoning folding/unfolding before/during/after streaming, status counters, and usage display when supplied.
- [ ] Child observation and read-only input isolation, then return to the owner composer.
- [ ] Restart persistence for model/effort, Agent, prompt history/frequency/stash, Workflow selection, and Sessions at their documented storage boundaries.

### 4.3 Palette, slash discovery, and local aliases (R-TUI-05–10, R-RED-05–06; AC-TUI-02–03, AC-RED-01)

- [ ] Open Ctrl+P. Prove it is the keymap-only command palette: filter, navigation, cancel, execute, route-scoped omission, hidden/disabled omission, and no user command files, Skill-derived commands, plugin Tools, or MCP Tools.
- [ ] Use slash autocomplete, not Ctrl+P, as the discovery surface for server commands. It must exclude `source="skill"`; Skill rows remain available through `/skills`.
- [ ] Exercise every source-defined local slash and alias: `/sessions` with `/resume` and `/continue`; `/new` with `/clear`; `/models` with `/mo`; `/agents`; `/mcps`; `/variants`; `/status`; `/themes`; `/help`; `/exit` with `/quit` and `/q`; `/editor`; `/skills`; `/diff`; `/rename`; `/timeline`; `/fork`; `/compact` with `/summarize`; `/undo`; `/redo`; `/timestamps` with `/toggle-timestamps`; `/thinking` with `/toggle-thinking`; `/copy`; and `/export`.
- [ ] Exercise backend-provided `/init`, `/review`, `/workflow`, `/model`, and `/think` through the discovered command catalog.
- [ ] Prove no-argument `/model` opens the existing model picker, exactly as `/models`; no-argument `/think` opens the existing variant/effort picker, exactly as `/variants`; `/agents` and `/mcps` open their pickers; `/workflow` list/info/select/run/state uses shared Workflow control.
- [ ] Prove configuration commands do not consume an ordinary model round. Argument-bearing backend commands retain the existing catalog path.
- [ ] Send a command absent from the source-defined lists as unknown slash text. It must reach the ordinary prompt unchanged. OAuth login/logout are CLI/dialog error-path checks, not shipped slash commands.
- [ ] Prove TUI `!` shell mode executes only inside the playground and remains distinct from custom command template expansion.

### 4.4 TUI go/no-go (R-TUI-01–04, R-TUI-09; AC-TUI-01)

- [ ] Every expected view and interaction has visible/ANSI evidence, process status, and the corresponding Session/Event record where applicable.
- [ ] The pane returns to idle after errors, reconnects, and aborts; terminal restoration is observed.
- [ ] **Go/no-go:** the real pane can execute the full control checklist without a synthetic TUI substitute. Proceed to custom resources only if owner input remains isolated and the Herdr cleanup registry contains exactly the task-owned pane.

## Phase 5 — User slash commands, Tools, and dynamic resources (R-TOOL-01–04, R-RES-01–10, R-ERR-01–04, R-SUB-01–04, R-WF-01–03; AC-TOOL-01–02, AC-RES-01–05, AC-ERR-01, AC-SUB-01, AC-WF-01)

### 5.1 Create deterministic fixtures (R-RES-01–02; AC-RES-02)

Create these fixtures in the isolated playground and process-E2E workdir:

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

- [ ] `/use-skill <nonce>` tells the agent to call builtin `skill` with `name="user-playbook"`, then return the Skill-body marker and nonce.
- [ ] `/use-plugin <nonce>` tells the agent to call plugin Tool `remember` with `value=<nonce>`, then return the plugin result.
- [ ] `/use-mcp <nonce>` tells the agent to call `mcp__echo__ping` with `msg=<nonce>`, then return `echo:<nonce>`.
- [ ] Direct `/user-playbook <arguments>` and selection through `/skills` expand the Skill body without a redundant `skill` call.
- [ ] `/nested/inspect` proves recursive command naming and autocomplete.

### 5.2 Catalog, source precedence, and expansion contract (R-RES-03–05, R-RES-09; AC-RES-01, AC-RES-05)

- [ ] Verify source precedence at the catalog and route seams: builtins first; project inline `command`/`commands`; project `.opencode/command` and `.opencode/commands` Markdown; Skill-backed commands only when no command already owns the name.
- [ ] Verify a later project command overrides a same-name builtin and a command wins over a same-name Skill.
- [ ] Omit malformed JSONC/frontmatter and unreadable/non-Markdown files without crashing the catalog.
- [ ] Empty or missing positional arguments expand to empty strings. Quoted arguments stay one position. Multi-line text remains in `$ARGUMENTS`. Replacement text containing `$1` is not expanded a second time.
- [ ] Ignore `disable` for execution semantics as currently specified; retain listing metadata.
- [ ] Ignore `.hya/commands` and home/global command directories. Adding unsupported roots is not part of this task.
- [ ] Keep command frontmatter `agent`, `model`, and `subtask` listing-only. Document or verify their metadata in `docs/configuration.md` and `docs/FOLLOWUPS.md`; do not add runtime routing. Resource execution uses the Session-selected Agent/model.
- [ ] Reuse `skill_dirs_for_workdir` for command/Skill metadata and builtin `skill` discovery, in exact first-name-wins order: project `.hya/skills`; home `.config/hya/skills`, `.claude/skills`, `.config/opencode/skills`, `.config/opencode/skill`; project `.opencode/skills`, `.opencode/skill`, `.agents/skills`; home `.codex/skills`, `.agents/skills`.
- [ ] Assert every Skill root, first duplicate wins, invalid Skill frontmatter is omitted, and the three built-in Compat Skills fill only absent names.
- [ ] Prove Plugin/AgentBundle runtime Skills are not projected into `/skill`, `/command`, or `/skills`. Use one installed-bundle control for this separation.

### 5.3 P18 process tests and Track P registration (R-RES-06–07; AC-RES-03–04)

Add one `matrix.toml` Track P row for each test. Implement the following exact test contracts in `crates/hya-e2e/tests/p18_custom_slash_resources.rs`:

1. **T1.16 `custom_slash_catalog_and_routes_expand_all_supported_sources`**
   - [ ] `/api/command` lists every fixture with exact source/template/hints and no duplicate name.
   - [ ] Legacy and V2 Compat command POSTs without `text` store exact expansion plus correlated `CommandExecuted`.
   - [ ] Native `/sessions/:id/command` deliberately stores the literal slash because it does not call `command_catalog::expand_prompt`.
   - [ ] Explicit `text` bypasses expansion on every route.
   - [ ] Cover four project JSON/JSONC locations, singular/plural inline keys, both Markdown roots, nested names, quotes, unclosed/empty quote behavior, missing positions, `$1`/`$10`, `$ARGUMENTS`, multiline arguments, nonrecursive replacement, malformed omission/plain-fence behavior, ignored `disable`, command-over-Skill, and later-command-over-builtin precedence.

2. **T1.17 `skill_backed_slash_expands_without_skill_tool_call`**
   - [ ] Direct `/user-playbook` and `/skills` selection admit the Skill body and arguments with no `ToolCallRequested { name: "skill" }`.
   - [ ] `Action::Skill` deny does not block direct template expansion.
   - [ ] Enumerate all `skill_dirs_for_workdir` roots and duplicate precedence.
   - [ ] Remove an existing Skill after TUI bootstrap: stale name still uses command transport and backend falls back to literal slash.
   - [ ] Add a new Skill after bootstrap: typed slash uses ordinary prompt until TUI restart.

3. **T1.18 `custom_command_invokes_builtin_skill_tool`**
   - [ ] `/use-skill` causes a real `skill` Tool call, body-bearing `ToolResult`, next-request replay, and final answer.
   - [ ] Unknown/missing `name`, denied permission, and unavailable permission use separate scripts; each emits a structured error and a later valid command succeeds in the same Session.
   - [ ] A stale/deleted command name falls back to literal slash text, not a catalog error.

4. **T1.19 `custom_command_invokes_plugin_tool`**
   - [ ] `/use-plugin` exposes and calls `remember`, preserves exact RPC input/result in Tool Events and the next request, and renders the Tool card.
   - [ ] Test `Action::Write` rejection, fixture-RPC validation of malformed input, process death during call, lazy same-declaration respawn, and fail-closed declaration drift as separate cases.
   - [ ] Editing `plugin.toml` becomes visible only after backend restart.

5. **T1.20 `custom_command_invokes_mcp_tool`**
   - [ ] `/use-mcp` exposes and calls `mcp__echo__ping`, asks once, replays `echo:<nonce>`, and renders the Tool card.
   - [ ] Separately cover disconnected/unknown server, `isError`, malformed result, timeout, malformed/oversized frame, and post-publication process death.
   - [ ] Death returns a structured closed error and does not auto-respawn.
   - [ ] Recovery is explicit disconnect/connect followed by a fresh root Turn; the old in-flight binding stays pinned.

6. **T1.21 `resource_name_conflicts_fail_closed`**
   - [ ] Duplicate plugin Tool, plugin-vs-builtin Tool, and crafted MCP namespace collisions reject the candidate runtime generation without partial publication.
   - [ ] Command/Skill name collision follows command precedence instead of runtime rejection.

7. **T1.22 `dynamic_resource_snapshots_and_reload`**
   - [ ] A Skill edit appears on the next root Turn.
   - [ ] MCP reconnect publishes for the next root Turn.
   - [ ] Plugin declaration changes require backend restart.
   - [ ] Every old `TurnBinding` remains unchanged.
   - [ ] Newly added post-bootstrap commands/Skill slash metadata are not promised before TUI restart because `sync.data.command` is bootstrap-cached.

8. **T1.23 `structured_custom_tool_errors_replay_and_session_recovers`**
   - [ ] One error per scripted resource call yields exactly one terminal Tool Event.
   - [ ] Structured `value.error.type/message` survives canonical/API replay.
   - [ ] TUI shows bounded error text, the Session returns idle, replay does not execute the Tool, and a later valid custom slash succeeds.

- [ ] Add Track T row **T3.4 `custom_resource_tui_command_transport`** for `packages/hya-tui-ts/test/pty-smoke.test.ts`.
- [ ] T3.4 proves Ctrl+P excludes user resources; slash autocomplete includes custom commands but excludes Skill rows; `/skills` includes Skills; local/server primary-or-alias collisions render one row after the dedupe fix; first-line command parsing preserves later lines in `arguments`; and command-request HTTP failures show a contextual toast with same-Session recovery.
- [ ] While TUI runs, change a known command body, add a command, and remove a command. A changed body expands immediately for the known name; an added name is ordinary prompt; a removed name reaches command transport and then falls back to literal slash; TUI restart refreshes names.

### 5.4 Builtin, dynamic-plane, and tool-contract matrix (R-TOOL-01–04, R-RES-10; AC-TOOL-01–02, AC-RES-05)

#### 5.4.1 Dynamic resource contract controls (R-RES-05, R-RES-10; AC-RES-05)

- [ ] MCP: prove local echo connected status, namespaced call, one permission decision, remote/error/closed cases, explicit disconnect/connect recovery, next-root generation publication, old-binding pinning, and TUI visibility.
- [ ] Plugin: prove startup discovery, Tool registration/invocation, hook allow/block, permission, RPC failure, lazy same-declaration crash respawn, drift rejection, and declaration-change visibility only after backend restart.
- [ ] AgentBundle: prove valid local package install/list/info, Agent execution, local resource call, uninstall, malformed package rejection, and invalid-capability rejection. Bundle resources remain model Tools rather than static slash commands.
- [ ] Skills and Workflow: prove next-root Skill catalog refresh with old-binding pinning; command/Skill slash metadata and plugin declarations refresh in TUI only after restart; Workflow uses its existing catalog revision contract.
- [ ] Use a local loopback web service and deterministic LSP/formatter fixtures for the remaining dynamic-plane contracts.

The exact 28 advertised tools are:

`read`, `ls`, `glob`, `find`, `grep`, `lsp`, `skill`, `webfetch`, `websearch`, `todowrite`, `write`, `edit`, `apply_patch`, `shell`, `bash`, `question`, `task`, `list_agents`, `plan_exit`, `invalid`, `ask_user`, `roster`, `send`, `announce`, `channels`, `join`, `leave`, `workflow`.

Hidden aliases are `fetch`, `search`, `todo`, `patch`, and `plan`.

- [ ] For every builtin, run one successful observable contract and every applicable boundary failure.
- [ ] Files/search: pagination, truncation, BOM, binary/media, missing file, no matches, invalid regex, file-vs-directory input, and external-directory refusal.
- [ ] Edits: create/update/delete/move, missing/multiple match, formatter/LSP diagnostics, and denied edit leaves bytes unchanged.
- [ ] Shell/bash: permission once/always/reject, feedback, exact remembered subject, timeout, cancellation, output truncation, nonzero exit, and unavailable command.
- [ ] Web: loopback HTML/text/image/redirect/status/timeout/oversize plus one optional public smoke.
- [ ] LSP/formatter: deterministic success fixture, missing server, diagnostics, and formatter failure.
- [ ] Skill/todo: next-provider-request Skill injection, todo create/transition/clear/invalid.
- [ ] Interaction: `question` rich selection/free-text, legacy `ask_user`, and dropped reply plane.
- [ ] Control: plan exit, invalid-tool success payload, and unknown-tool structured error.
- [ ] Subagent/team/Workflow tools: complete scenarios in Sections 5.5, 5.6, and 5.7.
- [ ] Inspect sanitized outbound schema names. GPT route has 26 builtins with `write` and `edit` absent and `apply_patch` present. GLM route has all 28 builtins.
- [ ] For each Tool call, prove `ToolInputStart` -> optional deltas -> `ToolCallRequested` -> exactly one `ToolResult` or `ToolError`.
- [ ] Prove the next provider request contains the structured failure and the same Session recovers on a later call and later Turn.

### 5.5 Defined error matrix (R-ERR-01–04; AC-ERR-01, AC-BE-02)

Cover every `ProviderError` variant:

- [ ] `Json`: malformed JSON frame/body.
- [ ] `Http`: typed provider `error`, `response.failed`, and bad header/client construction seam.
- [ ] `Transport`: connection refusal and deterministic transport failure.
- [ ] `HttpStatus`: bounded representative 4xx, 429 with `Retry-After`, and 5xx.
- [ ] `UnknownModel`: locally unconfigured model with zero traffic.
- [ ] `Incompatible`: route without streaming tool-call capability and unsupported payload.
- [ ] `Decode`: malformed/truncated SSE, zero frames, EOF, and `[DONE]` without typed terminal.
- [ ] `AuthExpired`: expired/revoked OAuth seam with recovery hint.
- [ ] Send one configured ghost model request to `api.12th.day` and prove the real remote model-not-found error.
- [ ] Send one invalid credential request in the isolated config and prove real auth rejection.
- [ ] Do not force malformed streams, TLS faults, or throttling against production.

Cover every `ToolError` variant:

`Input`, `Permission::Denied`, `Permission::Unavailable`, `Io`, `Json`, `Cancelled`, `Overloaded`, `OperationIdConflict`, `OperationAlreadyHandled`, `WorkflowControl`, `UnknownAgentId`, `AgentSpawnNotAllowed`, `UnsupportedInlineAgentField`, `Other`.

- [ ] Also cover Session not-found/busy/conflict, invalid command/request, disconnected/reconnected TUI, MCP/plugin unavailable, Workflow stale revision/invalid graph/missing Agent, and exhausted subagent budgets.

### 5.6 Subagent and team matrix (R-SUB-01–04; AC-SUB-01)

Use canonical ancestry, model route, Member lifecycle, and mailbox evidence:

- [ ] Discovery and foreground result.
- [ ] Same-child resume.
- [ ] Nested spawn to depth two.
- [ ] Two-member parallel batch.
- [ ] Category/model override and inline Agent identity.
- [ ] Nonblocking background terminal ordering.
- [ ] Resident registration, stable handle, idle/wake/idle, recovery, stop, and quiescence.
- [ ] `roster`, direct mail, `announce`, channel join/send/list/leave, pre-join/post-leave exclusion, and cross-unit refusal.
- [ ] Depth, concurrency, run, turn, and message budget failures.
- [ ] Real TUI child observation and input isolation.
- [ ] At least one slice uses each real model. Mixed-model children must show their actual route in Events.

### 5.7 Workflow matrix (R-WF-01–03; AC-WF-01)

Use playground `.hya/workflows` fixtures:

- [ ] Catalog precedence/revision, info, select/state persistence, stale revision rejection, and Session binding.
- [ ] Fan-out/fan-in with parallel Stages, child links, deterministic join sections, durable statuses, and TUI presentation.
- [ ] Fail-fast and collect-all.
- [ ] Cancellation through Session abort.
- [ ] Idempotent replay/retry and changed-input operation conflict.
- [ ] Invalid graph, missing Agent, and failed Stage.
- [ ] Verify P17 Workflow registration and T2.12 mailbox registration from Phase 2.

### 5.8 Real-pane custom-resource evidence (R-RES-08–09, R-TUI-06–08; AC-RES-02, AC-TUI-04–05)

In the real Herdr pane, use the actual slash/command transport:

- [ ] Use slash autocomplete for `/use-skill`, `/use-plugin`, and `/use-mcp`.
- [ ] Use `/skills` to select `user-playbook`.
- [ ] Submit `/use-skill SKILL_<nonce>`, `/use-plugin "PLUGIN <nonce>"`, and the multiline command `/use-mcp MCP_<nonce>\nsecond-line`.
- [ ] Capture the expanded user row rather than the literal template, permission interaction, one correlated custom Tool card per command, terminal result, recovery toast/state, canonical Event records, and Event/model IDs for each GPT and GLM slice (AC-TUI-05).
- [ ] Run at least one custom-resource command on GPT and one on GLM.
- [ ] Expected observables include `SKILL_BODY_<nonce>`, plugin `remember` output, `echo:MCP_<nonce>`, a structured error card on forced failure, a later success in the same Session, and the same commands after backend/TUI restart (AC-TUI-05).
- [ ] Assert no duplicate execution, exact `CommandExecuted` correlation, exact Tool Event correlation, and model-visible results.

### 5.9 Phase 5 go/no-go (R-RES-01–10, R-TOOL-01–04, R-ERR-01–04; AC-TOOL-01–02, AC-RES-01–05, AC-ERR-01)

- [ ] P18 T1.16–T1.23 and T3.4 each have separate registered rows and evidence.
- [ ] All 28 tools, five aliases, dynamic planes, every ProviderError, every ToolError, and recovery behavior have observable proof.
- [ ] Catalog precedence, expansion, Skill roots, plugin/MCP generation rules, old-binding pinning, and restart boundaries pass.
- [ ] **Go/no-go:** deterministic resource and tool matrix is complete before live organic traffic. Any real-model deviation is classified, not used as an excuse to weaken deterministic coverage.

## Phase 6 — Live dual-model behavior (R-SCOPE-02–05, R-ENV-02–05, R-TUI-01–04, R-RES-08, R-REASON-01–04, R-SUB-01–05, R-WF-01–03, R-CODE-01–03; AC-ENV-01–02, AC-EVID-01, AC-TUI-01, AC-TUI-04, AC-RES-02, AC-TUI-05, AC-REASON-01, AC-SUB-01, AC-WF-01, AC-CODE-01)

### 6.1 Live traffic controls

- [ ] Recheck the relay, private config, OMP hash, secret audit, source commit, and request counter immediately before the first live request.
- [ ] Use only configured routes `12th-oai/gpt-5.6-sol` and `12th-oai/glm-5.3` at `https://api.12th.day/v1` (or the approved private localhost relay forwarding to that URL).
- [ ] Keep the relay ordinal in every live evidence record. Stop before ordinal 2,001; never target the 2,000-request ceiling.
- [ ] If a real model ignores an explicit custom-resource command but deterministic P18 passes, classify it as model adherence and allow one tighter nonce-bearing prompt within the global cap. Do not change Hya solely to force a Tool choice.

### 6.2 Health, reasoning, organic behavior, and persistence

Across both models, run and retain health, reasoning, remote missing-model, invalid-auth, organic tool, coding, subagent, resident/team, and Workflow scenarios.
For this task's finalization, the 2026-08-30 user decision waives the remaining GLM functional, coding, subagent, Workflow, and reasoning slices. Completed GPT-only validation is accepted. Historical GLM custom-MCP success and later upstream HTTP 503 classification remain retained evidence, not a GLM pass and not an unresolved blocker.

- [ ] GPT live request produces summary-style reasoning Events and visible TUI “Thinking” content.
- [waived — user decision 2026-08-30] GLM live request produces full reasoning-text Events and visible TUI reasoning content.
- [ ] Switch effort through `/model`; prove UI state and sanitized outbound effort. Restart and prove documented persistence.
- [ ] Fold and unfold reasoning before, during, and after streaming. Preserve ordering and stable part keys.
- [ ] Test reasoning disabled/off deterministically and live if the route accepts it.
- [ ] If a model returns no reasoning at an enabled effort, classify it as provider/model behavior unless Hya received and lost a reasoning Event.
- [ ] Record real model IDs/routes for mixed-model child Events and Workflow Events.

### 6.3 Live coding and capability proof

Prepare small isolated Rust/TypeScript fixtures. Run each real model in a fresh Session through inspect -> diagnose -> edit -> focused test -> report.

- [ ] GPT uses `apply_patch`; its route filters `write` and `edit`.
- [waived — user decision 2026-08-30] GLM exercises `write`, `edit`, and one patch path.
- [ ] Include compile/type error, failing test, lint/format issue, merge-conflict-like file, large search, binary file, network-dependent request, denied action, human question, aborted Turn, resume, and post-error recovery.
- [ ] Accept only observed disk diffs and command results. Model claims without disk/command evidence do not pass.

### 6.4 Live phase gate

- [x] Both models resolve through the self-contained config without OMP runtime access.
- [x] Request ordinals remain below the hard cap and all live evidence is redacted (R-ENV-04–05, C-09, AC-ENV-01–02).
- [x] **Go/no-go:** GPT live coverage is complete and every live failure is classified as Hya defect, provider behavior, model adherence, external outage, or the documented GLM scope waiver. R-FINAL-01/AC-FINAL-16 passed in a live run on release `0.36.5`; scheduling-sensitive source is unchanged in current final release `0.36.6`. Final gates, cleanup, and the Trellis quality review passed.

## Phase 7 — SWE-Bench Pro diagnostic run (R-SCOPE-01–02, R-SCOPE-07, R-SWE-01–09, R-CODE-01–03; C-09; AC-SWE-01–03, AC-CODE-01, AC-FINAL-11–12)

### 7.1 Pin authoritative assets

- [ ] Use evaluator `scaleapi/SWE-bench_Pro-os@ca10a60a5fcae51e6948ffe1485d4153d421e6c5`.
- [ ] Use dataset `ScaleAI/SWE-bench_Pro@7ab5114912baf22bb098818e604c02fe7ad2c11f`.
- [ ] Record that the dataset is public/ungated with 731 rows, evaluator license is MIT, and dataset metadata has no declared license. Do not redistribute dataset rows, repository snapshots, or images.
- [ ] Use seed `hya-swebench-pro-live-8x2-v1`.
- [ ] Select two rows each from Go, JavaScript, Python, and TypeScript. Sort by `sha256(seed + NUL + instance_id)` per language and prefer a second repository.
- [ ] Apply eligibility using only nonempty identity/base/image fields and evaluator asset existence.
- [ ] Before the first benchmark API request, freeze instance IDs, row hashes, prompt hashes, base commits, and Docker image digests (R-SWE-02, R-SWE-09, C-09). A row blocked by a missing pinned evaluator asset or image stays blocked; do not substitute after model execution starts.

### 7.2 Construct prompts and protect gold data

- [ ] Prompt bytes are exactly publisher `problem_statement`, then `Requirements:`, then `requirements`, then `New interfaces introduced:`, then `interface`.
- [ ] Keep gold patch, test patch, test names, scripts, and evaluator-only fields outside Hya worktrees and mounts.
- [ ] Record prompt and binary hashes without exposing secrets or gold patches in Hya evidence.

### 7.3 Run exactly 16 independent GPT attempts

The fixed sample has eight rows. Run 16 independent GPT attempts: eight backend and eight Herdr/TUI, one attempt per surface for each frozen row. GLM does not enter this benchmark count. The original plan assigned GLM the complete functional, slash-resource, CoT, subagent, Workflow, and coding suite; the 2026-08-30 user decision waives those remaining GLM slices for task finalization. This is not a GLM pass.

For each attempt:

1. Create a fresh detached worktree at the frozen `base_commit`.
2. Run exactly one Hya task Turn. Do not retry, resume, provide hints, or share a patch.
3. For the backend surface, run `hya-backend --model 12th-oai/gpt-5.6-sol --yolo --db <private> exec --json <prompt>` using private paths and the exact prompt bytes.
4. For the TUI surface, launch current-source `hya` in a fresh Herdr pane, approve requested actions without task hints, and exit only at idle.
5. Capture all text-file changes as a full-index patch. Reject binary patches and verify application against a separate clean base.
6. Run backend and TUI predictions in separate official local-Docker evaluator invocations because results key only by `instance_id`.
7. Read Booleans from `eval_results.json`; process exit status alone is not a score.

- [ ] Record attempt ID, surface, frozen instance ID, source commit, relay ordinal range, base HEAD/status, and clean-base application result.
- [ ] A crash, empty/invalid patch, unanswered content question, dirty base, evaluator exception, or missing required test is false, not omitted.
- [ ] Record prompt/binary/patch hashes, Session DB/Event JSONL, TUI export, stderr/status, permission decisions, Docker tag/digest/image/platform, evaluator patch/entry script/stdout/stderr/output, token usage, and start/end timestamps.
- [ ] **Go/no-go:** all 16 attempts have a Boolean result or an explicitly false result with the required artifact. Never omit a failed attempt and never substitute a row after execution begins.

## Phase 8 — Repair loop (R-RED-01–08, R-SCOPE-05–06; AC-RED-01, AC-EVID-01, AC-FINAL-13–14)

For every newly reproduced Hya defect, run the following loop before resuming the affected matrix:

1. **Classify:** identify the exact Hya layer and contract boundary. Distinguish Hya defect from provider/model adherence, external service, fixture, or evaluator failure.
2. **RED:** add or use a deterministic behavior test that fails for the reported defect and proves the failure is not setup noise.
3. **Minimal GREEN:** implement the smallest root-cause fix. Reuse existing seams. Do not add generic retry, symptom suppression, alternate frontend, test-only product hook, or unrelated cleanup.
4. **Focused GREEN:** run only the affected focused test/check first and record output.
5. **Original scenario:** rerun the original backend, TUI, live, or benchmark scenario that exposed the defect. Record Event/SQLite/ANSI/disk/evaluator evidence.
6. **Cross-layer checks:** rerun the relevant API, Session, frontend, matrix, or resource contract checks so the fix does not lose data at a boundary.
7. **Release record:** if product source changed, update workspace version and the single-version root changelog, and archive the old changelog. Test-only changes do not receive a product release bump.
8. **Resume:** continue only after the original scenario and relevant cross-layer checks pass.

- [ ] Do not rerun already-passing expensive slices without a reason tied to a changed contract. Keep the expensive live and SWE-Bench matrix stable.
- [ ] For Responses, V2 background errors, registry drift, slash dispatch, Workflow discovery, and command errors, retain explicit RED/GREEN/original-scenario artifacts.

## Phase 9 — Final verification, release gates, and cleanup (R-ART-01, R-SCOPE-01–07, R-ENV-01–05, R-BE-01–06, R-TUI-01–10, R-TOOL-01–04, R-RES-01–10, R-ERR-01–04, R-REASON-01–04, R-SUB-01–05, R-WF-01–04, R-CODE-01–03, R-SWE-01–09, R-RED-01–08, R-FINAL-01; AC-ENV-01–02, AC-EVID-01, AC-BE-01–02, AC-TOOL-01–02, AC-ERR-01, AC-TUI-01–05, AC-RES-01–05, AC-REASON-01, AC-SUB-01, AC-WF-01, AC-CODE-01, AC-SWE-01–03, AC-RED-01, AC-FINAL-01–16)

### 9.1 Full quality gates

Run the complete gates only after all phases and repair loops finish:

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

Also run focused real-backend Track T files and a real Herdr smoke. Build local `hya`, `hya-ts`, and `hya-backend` executables from the current worktree. The required focused commands are repeated here as the final confirmation:

```sh
cargo test -p hya-server --test compat_command_metadata_api
cargo test -p hya-server --test compat_session_api compat_session_command_without_text_uses_skill_template_body
cargo test -p hya-server --test compat_session_v2_api compat_v2_session_command_without_text_uses_skill_template_body
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e --test p18_custom_slash_resources -- --test-threads=1
cargo xtask matrix-check
(cd packages/hya-tui-ts && bun test test/pty-smoke.test.ts)
```

- [x] Record command, source commit, environment, complete output, and exit status for every gate.
- [x] A gate failure blocks commit/push. Use Phase 8 if it is a newly reproduced Hya defect; do not hide or narrow the failure.

### 9.2 Final evidence completeness

- [x] Self-contained private Hya config resolves and calls both models without OMP runtime access.
- [x] No credential leaked; OMP source hash is unchanged.
- [x] Backend/API success and failure matrix has Event/SQLite proof.
- [x] Real Herdr pane ANSI captures cover complete TUI command/control, wide/narrow rendering, folding, interaction, reconnect, and read-only child matrix.
- [x] Every source-defined local/backend slash command and alias is exercised. Exact `/model` and `/think` open configuration dialogs without provider traffic. `/workflow state` reaches shared Workflow control with zero provider traffic. Autocomplete has no duplicate exact names.
- [x] All 28 builtins, five aliases, dynamic planes, every `ProviderError`, every `ToolError`, and recovery behavior are covered.
- [x] Every supported user-defined slash source is discovered with deterministic precedence. Direct Skill, `/skills`, Markdown, inline JSON/JSONC, nested, quoted, positional, and multiline forms admit the exact expected prompt.
- [x] P18 and real Herdr runs prove custom slash commands invoke `skill`, plugin `remember`, and `mcp__echo__ping`, replay real outputs to the model, surface permission/resource failures, recover in the same Session, and remain available after restart.
- [x] GPT summary CoT rendering/folding/state proof is retained and accepted. Remaining GLM full-CoT live validation is waived by the 2026-08-30 user decision; historical GLM reasoning evidence is retained but is not a pass claim.
- [x] GPT subagent/team and Workflow lifecycle proof is retained and accepted. Remaining GLM-driven subagent and Workflow validation is waived by the 2026-08-30 user decision and is not a pass claim.
- [x] Independent GPT real coding-task diffs and observed tests are retained and accepted. Remaining GLM coding validation is waived by the user decision and is not a pass claim.
- [x] User-selected SWE-Bench official evaluator Booleans are retained, with five setup-invalid attempts still counted false despite official evaluator true.
- [x] RED/GREEN/original-scenario evidence exists for every repair.
- [x] All applicable Rust, process E2E, TUI, Track T, matrix, and executable-build gates pass.
- [x] Complete process/container/pane cleanup is recorded.

### 9.3 Version, changelog, commit, and push gate

- [x] Product source changed, so workspace version, the single-version root changelog, and the archived prior changelog are aligned at `0.36.6`.
- [x] The project's required repository gates passed, and the final worktree file list was inspected without including unrelated `.argus_subagents/` output.
- [x] Do not commit or push if any quality gate, live evidence gate, benchmark artifact gate, or cleanup prerequisite fails. Do not tag or publish.
- [ ] Use the commit skill for the outstanding commit. Record commit and push results only after all gates pass.
- [x] Run the Trellis quality/spec review only after evidence and project gates pass.
- [ ] Finish and archive the task only after repository feature commit/push rules are satisfied (AC-FINAL-15).

### 9.4 Cleanup registry and shutdown

Use the cleanup registry created in Phase 1. Stop only task-owned resources:

- [x] Stop Hya backends and other task-owned processes.
- [x] Close only the Herdr panes created by this task. Never stop the Herdr server.
- [x] Stop the private relay, loopback fixtures, watchdogs, Docker containers, MCP servers, and plugin processes.
- [x] Remove only private secret/runtime artifacts: literal config copy, private HOME/XDG roots, SQLite databases, runtime logs, temporary worktrees, and fixture processes that are not part of retained evidence.
- [x] Remove the copied secret literal and retain only a redacted config template and OMP source hash.
- [x] Preserve benchmark evidence, evaluator Booleans, hashes, redacted Event/Session records, ANSI/TUI exports, and focused gate output without credentials.
- [x] Recheck that no key appears in argv, process output, logs, Events, prompts, patches, Docker mounts, Herdr captures, reports, or retained evidence.
- [x] Record final process/container/pane/relay status and cleanup exit results.
### 9.5 Final GPT-5.6-family scheduling acceptance (R-FINAL-01, AC-FINAL-16)

- [x] The live scheduling proof ran on release `0.36.5` with one category chain containing preferred `gpt56-primary/gpt-5.6-sol#low` and ordered fallback `gpt56-fallback/gpt-5.6-sol#high`; the preferred deterministic local route produced three pre-stream HTTP 503 attempts. Scheduling-sensitive source is unchanged in current final release `0.36.6`.
- [x] The engine advanced before any Event stream. The fallback used `reasoning_effort=high`, returned HTTP 200 at counted relay ordinal 2, and no mid-stream replay occurred.
- [x] The fallback response was exactly `GPT56_SCHEDULED_GREEN_A30`. One Session (`hysec_3xMXopbQV0NsMGHScP0J`) persisted 14 Events with matching stdout/SQLite sequence identity and one assistant delivery.
- [x] The focused regression was RED before the fix for Low versus High (`artifact://1810`), then GREEN at 1/1; the complete `model_fallback` suite passed 7/7.
- [x] Credential safety passed: only the mode-0600 private `provider.key` matched the credential scan, Git evidence had zero secret hits, and retained runtime evidence is local-only. Source evidence is `evidence/gpt56-model-scheduling.json`.
- [x] R-FINAL-01/AC-FINAL-16 and Phase 9.5 are complete. Overall acceptance is `passed-with-user-approved-GLM-waiver`; final gates, cleanup, and the Trellis quality review passed, so the worktree is ready to commit. This review did not commit, push, archive, or change task status.


## Rollback points and contingencies

- **Before Phase 1:** If artifact validation, approval, or baseline ownership fails, do not activate or touch product source. Preserve the user's unrelated/untracked files.
- **Before first live request:** If private roots, modes, source hash, config isolation, leak audit, or relay cap is not proven, make no live request. Repair setup only; do not bypass the relay.
- **Snapshot/playground/relay boundary (C-09, R-ENV-01–04):** Keep the baseline snapshot, created playground, private runtime, and counted relay as one ordered boundary. Do not begin live traffic until snapshot and isolated-runtime evidence is complete.
- **Phase 2 source candidates:** If a RED fails for a setup or unrelated reason, fix the test setup and rerun RED. If minimal GREEN or original-scenario rerun fails, revert only that candidate change and keep later candidates blocked. Do not ship a speculative fix.
- **Registry and coverage:** If a matrix/docs update is wrong, restore the previous registry text and correct the exact row. Do not hide an unregistered test behind a broad suite.
- **Custom plugin collision:** A duplicate plugin Tool, plugin-vs-builtin collision, or crafted MCP namespace collision must fail registration with the existing duplicate-name error. Rename only the test fixture before any evidence run; never add alias or last-writer-wins behavior.
- **Live provider/model behavior:** If deterministic tests pass but a real model ignores an explicit custom-resource command, classify model adherence and allow one tighter nonce-bearing prompt within the global cap. Do not change Hya solely to force a model Tool choice. Do not force malformed streams, TLS faults, or throttling against production.
- **SWE-Bench preflight (R-SWE-09, AC-SWE-01–03):** If a frozen row lacks a required evaluator asset or Docker image, mark that fixed row blocked and do not substitute another after model execution begins. Keep the 16-attempt accounting honest; crashes, invalid patches, unanswered questions, dirty bases, evaluator exceptions, and missing tests are false, not omitted.
- **Release:** If any final gate fails, do not commit, push, tag, or publish. Keep evidence and the failed status for repair. Product version/changelog changes are required only for actual product source changes.
- **Cleanup:** If cleanup encounters an error, stop safely, retain the cleanup registry and redacted evidence, and never stop a shared Herdr server or delete unrelated user files.

Completion is the conjunction of AC-ENV-01–02, AC-EVID-01, AC-BE-01–02, AC-TOOL-01–02, AC-ERR-01, AC-TUI-01–05, AC-RES-01–05, AC-REASON-01, AC-SUB-01, AC-WF-01, AC-CODE-01, AC-SWE-01–03, AC-RED-01, AC-FINAL-01–16. The remaining GLM slices are waived under the 2026-08-30 user decision and are not a pass claim. Finish/archive the Trellis task only after overall acceptance is recorded with the user-approved GLM waiver, final project gates pass, cleanup is complete, Trellis quality/spec review is clean, and repository commit/push rules are satisfied.
