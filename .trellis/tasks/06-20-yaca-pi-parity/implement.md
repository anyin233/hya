# Implement — yaca pi-parity (phased)

Execution is per child task. This parent file holds the ordered checklist for the
**first executable wave (Wave 1)** in detail; later waves get their own
`implement.md` in their child tasks when reached. Each box is TDD: failing test first,
then the smallest code to pass, then full gate.

## Validation commands (every wave)
```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Live QA (Wave 1): real-model `yaca exec` + tmux TUI approval overlay.

## Wave 1 — Agent can code  (child: 06-20-yaca-w1-agent-can-code)

Order matters: tools first (pure, easy TDD), then headless responder, then TUI wiring.

1. [ ] `yaca-tool`: RED test for `LsTool` (lists entries w/ type+size under workdir) → impl `LsTool` → GREEN. Register in `builtins`.
2. [ ] `yaca-tool`: RED test for `FindTool` (name/glob + metadata, path root) → impl `FindTool` → GREEN. Register in `builtins`.
3. [ ] `yaca-tool`: RED test for edit ambiguity guard (>1 match errors unless `replace_all`) → extend `EditTool` → GREEN. (Keep existing single-match behavior.)
4. [ ] `yaca-tool` tests: confirm permission asserts still fire for new tools.
5. [ ] `yaca-cli/src/permission.rs` (new): RED unit tests for `path_in_workdir(workdir, p)` (inside / nested / `..` escape / absolute-outside / absolute-inside) → impl lexical normalization → GREEN.
6. [ ] `yaca-cli/src/permission.rs`: RED tests for responder decisions — `WorkdirScoped` allows in-dir Edit + any Bash, rejects out-of-dir Edit; `Yolo` allows all → impl `respond(policy, &AskRequest) -> Decision` → GREEN.
7. [ ] `yaca-cli/src/main.rs`: change `build_session_engine` to return `(engine, asks_rx)`; add `--yolo/--allow-all` global flag; for exec/-p/serve spawn the auto-responder task (policy from flag, workdir from agent).
8. [ ] `yaca-cli/src/main.rs`: regression — `yaca exec` no longer errors on write within workdir (integration-style test or scripted live check).
9. [ ] `yaca-tui/src/lib.rs`: add `pending_permission: Option<PermissionPrompt>` to `AppState`; RED `tui_render` test that an overlay renders when set → impl overlay → GREEN.
10. [ ] `yaca-cli/src/tui.rs`: `select!` on `asks_rx`; set pending prompt; key handler a/s/d → `reply.send(Decision)`; suppress normal input while pending.
11. [ ] Gate: fmt + clippy + `cargo test --workspace` green.
12. [ ] Live QA (real model): `yaca exec "write 'hello' to ./qa_w1.txt"` creates file; `--yolo` allows an out-of-workdir path; without it, rejected. Capture transcripts.
13. [ ] Live QA (tmux TUI): send a prompt that triggers a write; overlay appears; press `a`; file written. Capture `tmux capture-pane`.
14. [ ] Cleanup: remove `qa_w1.txt` and any temp files/tmux sessions.

## Wave 2 — Project context  (child: w2-project-context)
- [ ] system-prompt builder (pure fn) + tests; AGENTS.md discovery; wire into AgentSpec. Gate + AC2.

## Wave 3 — Slash commands + prompt templates  (child: w3-slash-commands)
- [ ] slash registry + TUI parse + /help /model /clear /new /exit; prompt templates. Gate + AC4.

## Wave 4 — Context survival  (child: w4-context-survival)
- [ ] token-threshold compaction in engine + summarizer; SKILL.md discovery/injection. Gate + AC5.

## Wave 5 — Providers + auth  (child: w5-providers-auth)
- [ ] Google provider decoder + router wiring; OAuth /login (Anthropic + OpenAI-class). Gate + AC6.

## Wave 6 — Session tree  (child: w6-session-tree)
- [ ] list/branch/resume + tree picker in TUI + CLI flags. Gate + AC7.

## Wave 7 — Integration modes  (child: w7-integration-modes)
- [ ] exec --json (JSONL events) + `yaca rpc` stdin/stdout JSONL. Gate + AC8.

## Risky points / rollback
- `build_session_engine` signature change touches all CLI modes — compile-check each.
- TUI `select!` arm + overlay must not deadlock the turn task; reply channel must always
  be answered (drop = `Reject` via responder default) so a tool never hangs.
- Each wave is its own child task + commit; rollback = revert that child's commits.
