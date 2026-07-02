# Design — yaca pi-parity (Tier 3, phased)

## Guiding principles

- **Keep yaca's architecture.** Event-sourced engine, permission plane, goal/loop/team
  stay. Add pi's coding essentials as new crates/modules, not a rewrite.
- **Engine stays UI-agnostic.** New behavior (permission responders, compaction,
  context loading) lives behind traits the engine already uses or in the CLI layer.
- **TDD + quality gate every wave.** `cargo fmt --check`, `clippy -D warnings`
  (`unwrap_used`/`expect_used` denied in libs), `cargo test --workspace`.

---

## Wave 1 — Agent can code (P0 foundation)  [execute first]

### Problem
`build_session_engine` does `let (permission, _asks) = PermissionPlane::new(rules)` —
the `asks` receiver is **dropped**. Only Read/Glob/Grep are allow-listed, so Edit
(write/edit) and Bash (shell) hit `Mode::Ask` → `assert()` sends on a dead channel →
`PermissionError::Unavailable` → tool fails in every mode (verified live).

### Solution: route the Ask channel to a "permission responder"

The `PermissionPlane` Ask mechanism already exists (`AskRequest { action, resource,
reply: oneshot<Decision> }`). The fix is to **consume** the receiver instead of
dropping it, with a responder per mode.

1. **`build_session_engine` keeps the receiver** and returns it (or takes a responder).
   Signature change: return `(Arc<SessionEngine>, mpsc::UnboundedReceiver<AskRequest>)`,
   or accept a `PermissionPolicy`. Chosen: return the receiver so each mode wires its own.

2. **Headless auto-responder** (`yaca-cli`, new `permission.rs`): a spawned task that
   drains `AskRequest`s and replies by policy:
   - `PermissionPolicy::WorkdirScoped { workdir }` (default): `Resource::Path(p)` →
     `AllowOnce` if `p` resolves inside `workdir`, else `Reject`. `Resource::Command(_)`
     (Bash) → `AllowOnce` (it executes in `workdir`). Read/Glob/Grep already Allow via rules.
   - `PermissionPolicy::Yolo` (from `--yolo`/`--allow-all`): always `AllowOnce`.
   - Path containment: normalize `workdir.join(p)` lexically (resolve `.`/`..` without
     requiring existence); inside iff it stays under canonical workdir. Pure fn → unit-testable.

3. **Interactive TUI responder** (`yaca-cli/src/tui.rs` + `yaca-tui`): the event loop
   `select!`s on the Ask receiver. On a request, set `AppState.pending_permission =
   Some(PermissionPrompt { action, resource, reply })` and render an overlay
   ("Allow once [a] / Allow always [s] / Deny [d]"). Key handler, when a prompt is
   pending, routes a/s/d to `reply.send(Decision::…)` and clears the prompt. While a
   prompt is pending, normal input is suppressed.

4. **Allow-list stays minimal**: keep auto-allow for Read/Glob/Grep; Edit/Bash go
   through Ask → responder. (TUI = human approves; headless = policy decides.)

### New tools: `ls` and `find` (tool parity)
In `yaca-tool/src/tool.rs`, add to `ToolRegistry::builtins`:
- **`ls`**: list a directory's immediate entries with type (file/dir) + size.
  Input `{path?: string}` (default workdir). Permission: `Action::Read` on path.
- **`find`**: find files by name/glob recursively with metadata (path, size).
  Input `{pattern: string, path?: string}`. Permission: `Action::Glob`. (Distinct from
  existing `glob` which only returns paths; `find` adds metadata + path root.)

### Edit safety (diff)
`edit` already errors when `old` not found. Add: error if `old` occurs >1 time
(ambiguous) unless a `replace_all: bool` is set — mirrors pi's safer edit semantics.
Return a minimal unified-diff-ish summary `{replaced: n}`.

### Files touched (Wave 1)
- `crates/yaca-cli/src/main.rs` — keep receiver; add `--yolo`; spawn responder per mode.
- `crates/yaca-cli/src/permission.rs` (new) — `PermissionPolicy`, auto-responder, path containment.
- `crates/yaca-cli/src/tui.rs` — select on asks; approval keybinds.
- `crates/yaca-tui/src/lib.rs` — `AppState.pending_permission`, overlay render.
- `crates/yaca-tool/src/tool.rs` — `LsTool`, `FindTool`, edit ambiguity guard.

### Acceptance (Wave 1)
- AC1: `yaca exec "create file /workdir/x.txt …"` (real model) writes the file; outside-workdir write is rejected without `--yolo`; `--yolo` allows it.
- AC3: `ls`/`find` registered, schema-valid, correct results, permission-checked.
- Unit tests: path-containment fn (inside/outside/.. escape/abs), auto-responder decisions, edit ambiguity guard, ls/find output.
- Live QA: tmux TUI shows the approval overlay and proceeds on `a`.

---

## Wave 2 — Project context
- New `yaca-core` system-prompt builder: base persona + environment preamble
  (cwd, platform, date) + discovered context files (`AGENTS.md`, then nearest up-tree).
- `AgentSpec.system_prompt` becomes built, not hardcoded; builder is a pure fn
  (`build_system_prompt(base, env, context_files) -> String`) → unit-testable.
- CLI discovers `AGENTS.md` from workdir upward; passes contents into the builder.
- AC2: with `AGENTS.md` present, prompt provably contains it.

## Wave 3 — Slash commands + prompt templates
- `yaca-cli` slash registry: `/help /model /clear /new /exit` (extensible map).
  Parsed in the TUI before submit; `/model` updates active model live; `/clear` starts a
  new session; non-commands fall through to the agent.
- Prompt templates: markdown files in a known dir expand `/name args` → prompt text.
- AC4: `/help` lists commands; `/model X` switches model.

## Wave 4 — Context survival (compaction + skills)
- Compaction in engine: when projected token estimate crosses a threshold, summarize
  older turns into a System summary message (branch-summarization style), preserving
  recent turns. Verifier/summarizer uses the provider (like `ModelGoalEvaluator`).
- Skills: discover `SKILL.md` (frontmatter name/description) under a skills dir; expose
  a `skill` tool or inject available-skill list into the system prompt; load on demand.
- AC5: over-threshold session summarized + coherent; SKILL.md discoverable/injectable.

## Wave 5 — Providers + auth
- Add Google provider decoder in `yaca-provider` (Gemini generateContent/SSE),
  wired into router + compat-config mapping.
- OAuth `/login`: device/PKCE flow for Anthropic + one OpenAI-class; store token
  (SecretString) in config dir; router prefers OAuth token when present.
- AC6: a Google model and an OAuth provider each complete a turn.

## Wave 6 — Session tree
- Event store already has `parent` on sessions. Add: list sessions, branch from a
  message (new session with parent + replayed prefix), tree view + picker in TUI,
  `--resume`/`--session` CLI.
- AC7: branch + navigate/resume.

## Wave 7 — Integration modes
- `print`/JSON mode: `yaca exec --json` emits the `Envelope` event stream as JSONL
  (already serializable) instead of rendered transcript.
- RPC mode: `yaca rpc` reads JSONL requests on stdin (prompt/create/replay), writes
  event JSONL on stdout. Reuses engine + proto DTOs.
- AC8: print mode parseable; RPC answers a JSONL request.

---

## Cross-cutting
- **Regression safety (R10):** existing tests for goal/loop/team/worktree/category must
  stay green every wave; run full `cargo test --workspace` as the gate.
- **No new heavy deps** without need; reuse existing (reqwest, serde, tokio, ratatui).
- **Dependency order:** Wave 1 → (2,3 independent) → 4 → (5,6,7 independent). Wave 1 is
  the only hard prerequisite for end-to-end usefulness.
