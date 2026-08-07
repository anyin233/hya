# Grok Build Provider Plan

## Goal

Verify the implemented native `grok-build` provider, complete live acceptance for
`grok-4.5` at `low`, `medium`, and `high`, then commit and push only task-owned
changes.

## Phases

- [x] Design and implementation
- [ ] Quality review and deterministic verification (task scope passes;
  workspace gate blocked by concurrent TUI work)
- [ ] Live endpoint acceptance (blocked: all three efforts return HTTP 503)
- [ ] Spec/release consistency, atomic commit, and push

## Constraints

- Never print or persist the supplied API key.
- Preserve concurrent work, especially shared config and release files.
- Do not commit or push until all required checks and live acceptance pass.

## Known Errors

- Live inference returned HTTP 503 for Responses and Chat controls; the exact
  implemented request also returned 503 for `low`, `medium`, and `high` on
  2026-07-22.
- `cargo fmt --all --check` is blocked by unrelated concurrent edits in
  `hya-tui`.
- `cargo test --workspace` is blocked by the unrelated concurrent TUI test
  `authoritative_idle_suppresses_interrupt_indicator_with_pending_prompt_history`.
- The exact test-first RED transcript was not returned by the implement agent.
