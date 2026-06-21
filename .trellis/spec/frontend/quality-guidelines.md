# Quality Guidelines

> Code quality standards for frontend development.

---

## Overview

TUI changes must be covered by render tests using `ratatui::backend::TestBackend`.
Tests should assert stable semantics and important layout behavior across terminal
sizes rather than brittle full-frame snapshots.

The normal project gate applies:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

---

## Forbidden Patterns

- Non-Rust TUI renderers in this crate.
- Terminal I/O, async streaming, or crossterm event loops inside `yaca-tui`.
- Raw color literals inside widgets when a semantic theme field can express the role.
- Layout code that indexes optional sidebar columns eagerly; use explicit `if`
  branches when a rectangle may not exist.

---

## Required Patterns

- Write failing render tests before changing TUI behavior.
- Use saturating geometry math for terminal dimensions.
- Keep `AppState` application idempotent and projection-driven.
- Preserve prompt visibility on narrow terminals.

---

## Testing Requirements

Every TUI layout change should include at least one focused render test. Responsive
changes should cover narrow and wide widths, currently represented by 80-column
and 120-column tests.

---

## Code Review Checklist

- Does `crates/yaca-cli/src/tui.rs` still own terminal/event-loop behavior?
- Does the TUI remain readable at 80 columns?
- Are status labels understandable without color?
- Do new tests fail on the old behavior and pass on the new behavior?
