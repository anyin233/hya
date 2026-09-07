Source: `research/approved-plan.md` lines 219-436. The exact source file remains authoritative.

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
