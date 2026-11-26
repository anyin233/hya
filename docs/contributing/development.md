# Development & Coding Standards

Welcome! This guide covers what you need installed to build Hya locally and the engineering rules that keep the codebase clean and reliable.

---

## 1. Prerequisites & first build

- **Rust `1.91`+** (workspace uses Rust edition `2024`, Cargo resolver `3`)
- **Bun `1.4.2`+** (runs `packages/hya-tui`, `packages/hya-tui-web`, `packages/hya-tui-sdk`, and the Bun/Claude plugin adapters)
- **Git** (plus standard C/OpenSSL build headers on Linux: `build-essential pkg-config libssl-dev`)

Clone and build a working local `hya` binary + TUI in four commands:

```sh
git clone https://github.com/chrono-foundry/hya.git
cd hya

(cd packages/hya-tui && bun install --frozen-lockfile)
(cd packages/hya-tui-web && bun install --frozen-lockfile)
cargo build -p hya-backend --bin hya

# Launch your local debug build!
./target/debug/hya
```

---

## 2. Core codebase rules

1. **No panics in library crates (`unwrap_used` / `expect_used` denied):** Workspace Clippy lints forbid `.unwrap()` and `.expect()` in library code. Return typed errors (`StoreError`, `CoreError`, `ToolError`, `ProviderError`) instead of crashing the daemon. Binaries and tests may allow them locally where appropriate.
2. **Preserve single-source event sourcing:** Never invent a second mutable read model in the server or TUI that can drift from `event_log`. Append durable events in `hya-store` and fold them through `hya_proto::Projection`.
3. **Keep foundation crates lightweight:** `hya-proto` must stay free of async runtimes, SQLite, or HTTP libraries. `hya-updater` and `hya-relay` must not depend on runtime crates (`hya-engine`, `hya-app`, `hya-server`).
4. **Reuse existing runtime planes:** Route permissions, user prompts, subagent spawning, skills, web search, and LSP calls through `hya-tool`'s existing planes (`PermissionPlane`, `InteractionPlane`, `SpawnerPlane`, `SkillPlane`, `WebSearchPlane`, `LspPlane`) rather than threading ad-hoc globals.
5. **One interactive frontend (`packages/hya-tui`):** Never add a second terminal UI or move UI rendering into `hya-server`. Both the terminal TUI and browser WebUI share `packages/hya-tui` ([ADR-0018](../adr/0018-browser-rendered-tui-test-environment.md)–[ADR-0020](../adr/0020-bundle-tui-and-webui-in-hya.md)), and core shortcuts must stay browser-safe (never bind an action *only* to `Ctrl/Cmd+W`, `T`, `N`, `L`, or `Ctrl+Tab`, which browsers intercept).

---

## 3. Multi-step task plans

For multi-step work, run `python3 scripts/new-plan.py` from the repository root to scaffold the shared **[Plan Template](../templates/plan.md)** into the next git-ignored `.plan/pN/task_plan.md` Kanban board, and consult `.agents/spec/` before editing backend layers.

---

## 4. Shipping features with documentation

Every new or changed user- or developer-facing feature ships with updated documentation in the same change:

- **User-facing behavior & workflows:** Update the matching short guide under [`docs/guide/`](../guide/interactive-mode.md) and exact flags/fields/keys in [`docs/reference/`](../reference/cli.md).
- **Bundle, Hook, TUI SDK, or `/v1` API changes:** Update [`docs/extend/`](../extend/overview.md) or [`docs/protocol/`](../protocol/README.md) (and run `cargo run -p xtask -- gen-api` if `proto/hya/v1/` changed).
- **Internal engine, storage, or crate boundary changes:** Update [`docs/architecture/`](../architecture/overview.md) (and add an ADR under `docs/adr/` for major architectural decisions).

Keep user guides short, plain-spoken, and focused on tasks; put exhaustive schemas and flag tables in `docs/reference/`, `docs/extend/`, or `docs/protocol/`.

---

## 5. Formatting & quick local check

Before opening a PR or committing, check formatting across the Rust workspace and run Clippy + tests **only for the crates/packages you touched** (see **[Testing & Verification](testing.md)**):

```sh
cargo fmt --all --check
cargo clippy -p <changed-crate> --all-targets -- -D warnings
cargo test -p <changed-crate>
cargo build -p hya-backend --bin hya
```
