# Testing & Verification

Hya's test suite is fast when you run the right layer. **Locally, verify only the components your change touches** (plus direct dependents when you change a public API). GitHub Actions CI (`.github/workflows/ci.yml`) runs the full workspace suite on every push and PR—do not burn 15 minutes running `cargo test --workspace` or all Playwright specs locally unless making a workspace-wide change.

---

## 1. Which tests own what?

| Tier | Where it lives | What it tests |
| --- | --- | --- |
| **Track I (In-process Rust)** | Each crate's `src/` unit tests + single `tests/it/main.rs` integration binary | Deep engine logic, `Projection` replay, SQLite locks, provider decoders, permission math, and `/v1` HTTP ↔ gRPC parity (`hya-server`). |
| **Track P (Process E2E)** | `crates/hya-e2e` (`tests/it/pNN_*.rs` + `matrix.toml`) | Real `hya` binary + scripted local `FakeLlm`: end-to-end tools, skills, MCP, subagents, `.hyabundle`, headless `exec`, and encrypted Relay tunnels. |
| **TUI In-Memory UI** | `packages/hya-tui/test/` (`testRender` + `FakeBackend`) | Layout presets, keybindings, overlays, Vim/composer, streaming cards, subagent panes, and management views (`/key`, `/project`, `/diff`, etc.) in milliseconds. |
| **TUI Real-PTY Seam (`tui-exp`)** | `packages/hya-tui-web/e2e/*.spec.ts` on Bun PTY + `@xterm/headless` | Real OS PTY, OSC 52/9/0 terminal sequences, bare `hya` launcher, daemon auto-start/restart, gRPC, Relay bridge, and confined Bun VM extensions. |

---

## 2. Running Rust component tests (Track I)

Find direct workspace dependents of a crate you changed:

```sh
cargo tree --workspace -i <crate> --depth 1 -e normal,dev --prefix none
```

Then run fmt, clippy, and tests for the touched crate(s):

```sh
cargo fmt --all --check
cargo clippy -p hya-engine -p hya-server --all-targets -- -D warnings
cargo test -p hya-engine --test it subagent_lifecycle::   # fast iteration on one module
cargo test -p hya-engine -p hya-server                   # full crate tests before finishing
```

> **Integration test binary rule:** Each crate keeps one `tests/it/main.rs` binary with submodules (`tests/it/<area>.rs`) so Cargo links one test binary per crate instead of dozens. Keep PR tests event-driven—never add `sleep`s over 1 second (heavy OS fault/soak tests carry `#[ignore = "nightly OS fault/soak"]` for `.github/workflows/nightly.yml`).

---

## 3. Running Process E2E tests (`crates/hya-e2e` — Track P)

Run Track P when touching `crates/hya-e2e` or cross-process seams (MCP, skills, subagents, bundles, Relay, headless `exec`). Build `hya` first, then run the specific `pNN_*` module:

```sh
cargo build -p hya-backend --bin hya
cargo test -p hya-e2e --test it p04_subagents_and_mailbox:: -- --test-threads=4
cargo test -p hya-e2e --test it p38_relay:: -- --test-threads=4
```

If you add or rename a scenario in `crates/hya-e2e` or `matrix.toml`, validate the registry:

```sh
cargo run -p xtask -- matrix-check
```

**Honest oracle rule:** In Track P, never assert solely that `FakeLlm` received N requests or that tool-call *arguments* contained a string. Assert real outcomes: files written on disk, session tree depth (`GET /v1/sessions`), or tool **results** appearing in the follow-up `FakeLlm` request!

---

## 4. Running Frontend & TUI tests (`Bun`)

Run checks only in the package(s) you edited:

```sh
# TUI unit + @opentui/solid testRender UI suites (default tier for UI changes!)
cd packages/hya-tui && bun run typecheck && bun test

# TUI SDK & QuickJS VM tests
cd packages/hya-tui-sdk && bun run typecheck && bun test

# Bun / Claude plugin adapters
cd crates/hya-plugin-bun/adapter && bun run typecheck && bun test
cd crates/hya-plugin-claude/adapter && bun run typecheck && bun test

# WebUI host unit tests + targeted real-PTY seam spec (build hya first)
cargo build -p hya-backend --bin hya -p xtask --bin xtask
cd packages/hya-tui-web
bun run typecheck && bun test ./test
bun run test:tui-exp:parity -- e2e/hya-tui-launch-daemon.spec.ts
```

### Live TUI preview in a browser (no `tmux` scraping!)

Never use `tmux` or `script` to eyeball TUI changes. Serve the TUI in the WebUI host and open the URL in a browser (or inspect `test-results/<test>/final-screen.png` from an optional `bun run test:e2e -- e2e/<spec>.ts` run):

```sh
HYA_BIN=target/debug/hya bun packages/hya-tui-web/src/main.ts --port 7681 -- \
  bun packages/hya-tui/src/main.ts --dir "$PWD"
```
