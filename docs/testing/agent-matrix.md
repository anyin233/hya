# Agent feature matrix

Process E2E lives in `crates/hya-e2e` (**Track P**): real `hya-backend` +
scripted OpenAI-compatible FakeLlm. Existing in-process tests remain the
authority for deep engine semantics (**Track I**); they are indexed, not
duplicated. TUI/SDK coverage is **Track T**.

| Resource | Path |
| --- | --- |
| Machine registry | [`crates/hya-e2e/matrix.toml`](../../crates/hya-e2e/matrix.toml) |
| Harness guide | [process-e2e.md](process-e2e.md) |
| Testing overview | [README.md](README.md) |

## How to run

```sh
# Track P — process agent suite
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e -- --test-threads=1

# Track T — non-PTY real-backend / presentation
cd packages/hya-tui-ts
bun test test/real-backend.test.ts test/task-presentation.test.ts test/real-backend-agents.test.ts
```

## Track P scenarios (implemented)

| ID | Title | Test | Strong oracle (summary) |
| --- | --- | --- | --- |
| T0.1 | Backend boots | `tests/p01_session_prompt.rs` | HTTP responds on serve URL |
| T1.2 | Prompt + FakeLlm text | `tests/p01_session_prompt.rs` | Events contain scripted assistant text |
| T1.3 | Multi-round tool loop | `tests/p02_tool_loop_fs.rs` | Sequential write → read → shell |
| T1.4 | read/write | `tests/p02_tool_loop_fs.rs` | Disk file content |
| T1.5 | shell | `tests/p02_tool_loop_fs.rs` | Disk file from shell |
| T1.7 | Permissions once/reject | `tests/p03_permissions.rs` | File created only after `once` |
| T1.8 | Questions + reply | `tests/p04_questions.rs` | Turn continues after question reply |
| T1.9 | Skills load | `tests/p05_skills.rs` | `/skill` lists skill; follow-up FakeLlm has body marker |
| T1.10 | MCP tool call | `tests/p06_mcp.rs` | `/mcp` connected; follow-up has `echo:…` result |
| T1.11 | Session list + resume | `tests/p07_session_lifecycle.rs` | Compat list shows active sessions; second prompt on same id |
| T1.12 | Session context API | `tests/p12_context_api.rs` | Multi-turn user/assistant text in `/api/session/{id}/context` |
| T1.13 | Project AGENTS.md guidance | `tests/p13_project_agents_context.rs` | Compat-guided FakeLlm request contains AGENTS body marker |
| T1.14 | Compact / summarize | `tests/p14_compact_summarize.rs` | Compact injects summary into context; follow-up turn works |
| T1.15 | todowrite + edit | `tests/p15_todo_and_edit.rs` | Todo route lists item; edit rewrites file on disk |
| T1.16 | Custom slash catalog and route expansion | `tests/p18_custom_slash_resources.rs` | Supported command sources, precedence, exact single-pass expansion, route parity, and literal fallback |
| T1.17 | Skill-backed slash expansion | `tests/p18_custom_slash_resources.rs` | Direct Skill body admission, discovery order, no redundant `skill` call, and bootstrap cache boundaries |
| T1.18 | Custom command invokes builtin Skill Tool | `tests/p18_custom_slash_resources.rs` | Real `skill` result reaches the next model request; typed failures recover in the same Session |
| T1.19 | Custom command invokes plugin Tool | `tests/p18_custom_slash_resources.rs` | `remember` RPC result, permission, process death, respawn, drift rejection, and restart boundary |
| T1.20 | Custom command invokes MCP Tool | `tests/p18_custom_slash_resources.rs` | Namespaced result, structured faults, explicit reconnect, and old-binding pinning |
| T1.21 | Custom resource conflicts fail closed | `tests/p18_custom_slash_resources.rs` | Plugin/builtin/MCP collisions publish no partial runtime generation |
| T1.22 | Dynamic resource snapshots and reload | `tests/p18_custom_slash_resources.rs` | Skill and MCP next-Turn refresh, plugin restart boundary, and immutable old bindings |
| T1.23 | Structured custom Tool errors recover | `tests/p18_custom_slash_resources.rs` | One terminal Tool Event, structured replay, no replay execution, and later same-Session success |
| T2.1 | Subagent task | `tests/p08_subagent_task.rs` | Tree children ≥ 1, `general`, distinct child session |
| T2.2 | Nested tree depth≥2 | `tests/p09_nested_subagent.rs` | Depth ≥ 2, explore+plan, ≥ 3 session ids |
| T2.3 | Agent roster / roles | `tests/p10_agent_roster.rs` | `/api/agent` lists build + spawnable roles |
| T2.4 | Swarm `roster` + `list_agents` | `tests/p16_swarm_mailbox.rs` | Caller's follow-up carries the teammate's handle, type, status and **real session id** |
| T2.5 | Swarm `send` (direct) | `tests/p16_swarm_mailbox.rs` | **Recipient's** next request contains `[mail from main/general-2] …` |
| T2.6 | Swarm `send` (`#channel`) | `tests/p16_swarm_mailbox.rs` | Subscriber's next request contains the post; receipt reads `to #squad (1 recipient)` — the channel-branch count, not the direct-send constant |
| T2.7 | Hyabundle CLI lifecycle | `tests/p11_hyabundle.rs` | install/list/info/uninstall stdout |
| T2.8 | Hyabundle spawn agent | `tests/p11_hyabundle.rs` | Roster has package agent; events have scripted text |
| T2.9 | Swarm `channels` | `tests/p16_swarm_mailbox.rs` | Result reports `members:["general-1"]` and `messages:1` the caller never supplied |
| T2.10 | Swarm `join` | `tests/p16_swarm_mailbox.rs` | Post-join message reaches the joiner; pre-join post never does |
| T2.11 | Swarm `leave` | `tests/p16_swarm_mailbox.rs` | Negative: departed member never sees the post, bounded by a still-subscribed member **and** a later direct ping it did see |
| T2.12 | Cross-unit `send` refused | `tests/p16_swarm_mailbox.rs` | Two units, two levels deep: sender's follow-up carries the scope refusal AND the payload never reaches the other unit ([ADR-0011](../adr/0011-hierarchy-scoped-mailbox.md)) |
| T2.13 | User-authored Workflow fan-out/fan-in | `tests/p17_workflow_composition.rs` | One discovered Workflow spawns four distinct stage Sessions, joins both parallel implementations into review, and returns the final report to the lead |

### Built-in tool coverage

`ToolRegistry::builtins()` advertises exactly **28** canonical Tool names. The
dispatch-only aliases `fetch`, `search`, `todo`, `patch`, and `plan` are hidden
from provider schemas and are not counted as canonical Tools. Track P directly
exercises **15** canonical Tools:

| Covered (15) | Not directly covered by Track P (13) |
| --- | --- |
| `read`, `write`, `edit`, `shell`, `question`, `skill`, `task`, `todowrite`, `send`, `roster`, `channels`, `join`, `leave`, `list_agents`, `workflow` | `bash`, `ls`, `glob`, `find`, `grep`, `lsp`, `ask_user`, `apply_patch`, `webfetch`, `websearch`, `plan_exit`, `invalid`, `announce` |

### Multi-agent scenarios need per-agent FakeLlm routing

`FakeLlm` holds one shared `VecDeque<ScriptStep>`, which is nondeterministic the
moment two agents are live: either can pop the other's step. `FakeLlm::route`
pins a queue to the agent whose **system prompt** contains a marker, and records
only that agent's request bodies. Attribution deliberately looks at `system`-role
content alone — a marker anywhere else in the transcript (tool-call arguments,
mail bodies) is echoed back into the *caller's* history too. With no routes
registered, dispatch is unchanged, so single-agent scenarios are unaffected.

Mail delivery has no HTTP surface, and only **resident** agents receive it
(`hya-core::resident` injects a handle's unread inbox as `[mail from …]` user
prompts). The recipient's own next model request is therefore the only honest
delivery oracle — see the module docs of `tests/p16_swarm_mailbox.rs` for the
ordering rules that keep those scenarios deterministic.

## Track T scenarios (implemented)

| ID | Title | Test |
| --- | --- | --- |
| T3.1 | Real-backend permission reply | `packages/hya-tui-ts/test/real-backend.test.ts` |
| T3.2 | Multi-agent task presentation | `packages/hya-tui-ts/test/task-presentation.test.ts` |
| T3.3 | Real-backend agent roster | `packages/hya-tui-ts/test/real-backend-agents.test.ts` |
| T3.4 | Custom resource slash command transport | `packages/hya-tui-ts/test/pty-smoke.test.ts` |

The first three scenarios are the **enforced** Track T gate in
`.github/workflows/ci.yml`; the workflow names them explicitly rather than
running a blanket `bun test`. T3.4 is matrix-registered PTY coverage in the
non-gating TUI smoke step.

Full PTY matrix for every feature ID is **not** required for the PR gate; PTY
remains presentation smoke (`pty-smoke.test.ts`). The rest of the Bun suite,
PTY included, runs in a **non-gating** step (`continue-on-error: true`) so it
still reports without being able to block the Rust gate. That is not a
hypothetical concern: on 2026-08-05, run `31053432077` failed on a
`pty-smoke.test.ts` timeout and skipped `fmt`, `clippy`, `build`, the entire
workspace test suite, and `verify-no-http`. The identical commit passed the
same step in two other runs.

### `pty-smoke.test.ts` is a known flake, and stays non-gating

Status as of `fee38938`: `continue-on-error: true`. Observed once as
`timed out waiting for root draft` (`test/pty-smoke.test.ts:589`, ~65s) on run
`31053432077`; byte-identical code passed the same step in two other CI runs,
and it passes 3/3 locally. No root cause established.

It is deliberately not chased further: it can no longer block the Rust gate, and
the cost of diagnosing a single-observation PTY timing flake is out of proportion
to that. A red pty-smoke step is therefore **reported, not ignored** — check
whether the failure is this timeout before reading it as a PTY regression.

### Open flakes recorded but not fixed (2026-08-06)

Three tests are known to fail intermittently and are deliberately **not** fixed.
Each is recorded with its observation count so a red run is recognised rather
than re-investigated from scratch. None is a regression from the
2026-08-06 hardening round.

| Test | Symptom | Evidence |
| --- | --- | --- |
| `transient_sidecar_loss_interrupts_running_member_before_provider_release` (`crates/hya-core/tests/subagent.rs:3904`) | assertion failure | **1 CI observation** (run `31061204771`, on `573924f4`); 3 later CI runs on `main` green |
| `foreign_promotion_is_wake_only` (`crates/hya-app/src/runtime.rs`) | `Elapsed(())` on a store-state poll | 4/30 runs, **only under artificial 24× CPU saturation**; never observed on CI |
| `oauth::callback::tests::captures_code_and_state_from_callback` (`crates/hya-app/src/oauth/callback.rs:160`) | `ConnectionRefused` | 3/60 runs under the same artificial load; never observed on CI |

Notes that matter for whoever picks these up:

- The **first** is the only one with real CI evidence. It fired on the commit
  *preceding* the hardening round and has not recurred in 3 subsequent runs, so
  its rate on CI is at most ~1 in 4. Root cause **not established** — do not
  assume the hardening round fixed it; nothing in that round touched
  `hya-core`'s subagent tests.
- The **second** busy-spins on `tokio::task::yield_now()` while polling the
  store, which starves the very promotion task it waits for. Under 24×
  oversubscription that is largely self-inflicted by the probe, not evidence of a
  CI-relevant defect.
- The **third** is a genuine race by inspection, independent of load: the test
  binds a port, drops the listener, spawns a thread to re-bind, and then sleeps
  50 ms before connecting. The correct fix is to remove the rebind window by
  having `wait_for_callback` accept an already-bound `TcpListener` — **not** a
  longer sleep and **not** a connect retry, either of which would convert an
  intermittent failure into a slow one.

## Track I (index-only)

Deep engine / API coverage owned by in-process suites. Process E2E does not
replace these.

| ID | Title | Path |
| --- | --- | --- |
| I.nested | Nested spawn tree | `crates/hya-app/tests/nested_spawn_tree.rs` |
| I.subagent | Subagent/resident core | `crates/hya-core/tests/subagent.rs` |
| I.permission_api | Compat permission/question | `crates/hya-server/tests/compat_permission_question_api.rs` |
| I.mcp_api | Compat MCP | `crates/hya-server/tests/compat_mcp_api.rs` |
| I.bundle_cli | Bundle CLI | `crates/hya-backend/tests/bundle_cli.rs` |
| I.context_api | Compat session context | `crates/hya-server/tests/compat_session_v2_context_api.rs` |
| I.compact_api | Compat compact | `crates/hya-server/tests/compat_session_v2_compact_api.rs` |
| I.compact_engine | Engine compact_context | `crates/hya-core/tests/compact_context.rs` |

## The registry is enforced

`crates/hya-e2e/matrix.toml` is validated by
`cargo run -p xtask -- matrix-check` (there is **no** Cargo alias named
`xtask` in this workspace), which runs as a CI gate step. It fails on:

- a registered `path` that does not exist;
- a duplicate or malformed id;
- a Track P file that is registered but holds no test function;
- a Track P test file that **no** entry references (reverse drift — an
  unregistered scenario is as much a registry failure as a phantom row);
- a numbering hole in a `T<major>` series that is neither used nor retired.

Bidirectional drift is enforced for Track P only. Track T is TypeScript, and
Track I rows are index pointers into other crates that are deliberately not
one-to-one with registry rows; checking those would generate false failures.

Correspondence is **file-level**: `p01` carries two ids in one function, `p02`
carries three, `p03` has one id and two functions.

### ID allocation rule

- A new scenario takes the next free number in its series.
- Retiring an id requires a `[[retired]]` entry with a real reason — not
  "unused". If the original intent is unrecoverable, say that.
- Both rules are enforced by `matrix-check`, so a gap cannot reappear silently.
  `T1.1` and `T1.6` are retired on exactly those grounds: nothing in this
  repository's history records what they were meant to cover.

## Adding a scenario

1. Prefer extending Track I if the regression is pure engine/API shape.
2. For product-path regressions (config, real binary, FakeLlm tool loop), add
   `crates/hya-e2e/tests/pNN_*.rs` using `E2eEnvBuilder`.
3. Register the ID in `matrix.toml` and this page.
4. Keep oracles honest — see [process-e2e.md](process-e2e.md#oracle-rules-do-not-weaken).
5. Run `cargo test -p hya-e2e --test pNN_… -- --test-threads=1` then the full
   crate suite before landing.
