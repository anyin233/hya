# Progress

Workflow session: 6251de12-dbbb-4b7f-bfec-f10b36643467
Task directory: /Users/saber/Projects/hya/.planning/2026-09-28-tui-shift-enter
New task, isolated from existing plans. Host hook environment cannot be propagated through child shell; explicit records used.
Failing regression added in packages/hya-tui-web/e2e/hya-tui-composer.spec.ts: before the fix, Shift+Enter submitted and the multiline assertion timed out. The fixed test now covers draft preservation, second Shift+Enter, resize to a narrow viewport, plain Enter submission, transcript lines, and exit.
Implementation: the generic WebUI host exposes opt-in `--shift-enter-lf` input config; bare hya passes it to the host. The browser captures Shift+Enter before xterm.js and forwards LF, with an IME guard. TUI bindings/help/docs and generic host docs are aligned.
Passed: TUI typecheck + 597 unit tests; WebUI typecheck + 18 unit tests; targeted 21 Playwright composer/help tests; full Playwright 195/196 with one pre-existing archive/resume failure; cargo fmt, clippy, backend build, bundle tests, frontend argv tests, and isolated bridge retry.
Blocker: final workspace Rust suite hit unrelated concurrent edits in crates/hya-core (literal `***` at messages.rs:455 and compaction changes in projection.rs) plus flaky compaction assertions; these user-owned files were not changed. `git diff --check` is clean for feature files but reports the unrelated messages.rs EOF change.
