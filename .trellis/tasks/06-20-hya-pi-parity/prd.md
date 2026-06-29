# hya: broad feature parity with pi

## Goal

Bring **hya** (Rust multi-agent coding agent, ~5.7K LOC) to broad (Tier 3)
*functional* parity with **pi** (`earendil-works/pi`, ~200K LOC TS coding agent),
while preserving hya's existing differentiators. "Parity" = a user can do the
same core coding-agent work in hya that they can in pi; it is NOT a line-by-line
port of pi's TypeScript.

## User value

Today hya is a clean foundation that **cannot actually code**: edit/write/shell
are blocked in every mode. After this work hya is a usable coding agent with
project awareness, the standard tool set, slash-command UX, context survival,
multiple providers, and scripting/integration modes.

## Confirmed facts (from inspection)

### hya current state
- Crates: proto, provider, tool, store, core, server, client, tui, cli.
- Tools (`hya-tool/src/tool.rs` `ToolRegistry::builtins`): `read, write, edit, glob, grep, shell` (6).
- **CRITICAL BUG**: `hya-cli/src/main.rs::build_session_engine` drops the permission
  `asks` receiver (`let (permission, _asks) = …`) and only allow-lists Read/Glob/Grep.
  Every Edit/Write/Bash → `Mode::Ask` → send on dead channel → `PermissionError::Unavailable`
  → tool fails. Verified live: `hya exec "write hello to test.txt"` returns a permission error.
  This affects ALL modes (exec, goal, serve, **and TUI** — all use `build_session_engine`).
- Agent loop `hya-core/src/engine.rs::run_turn` is real & correct: multi-round tool calls
  (MAX_TOOL_ROUNDS=25), event-sourced, streams provider events.
- Providers: OpenAI Chat Completions + Anthropic Messages + Dev/echo + Fake. API key only.
- Config: reuses opencode's `opencode.json` (no separate config).
- System prompt: hardcoded `"You are hya, a coding agent."` — no AGENTS.md/context loading.
- Store: SQLite event log + projection + token ledger (no JSONL, no branching/tree).
- CLI: bare→TUI, `exec`, `serve` (HTTP+SSE), `tail-session`, `-p` goal mode.
- Differentiators pi lacks: engine-owned **goal mode** + **loop mode** (verifier-gated),
  **team/subagent** plane, per-agent **worktrees**, **categories**, a real **permission plane**.
- Quality gate: `cargo fmt --check` + `clippy -D warnings` (`unwrap_used`/`expect_used` denied in libs) + `cargo test`.

### pi reference (the parity target)
- "Minimal at the core, extended via TS extensions/skills/prompts/themes/packages."
- Built-in tools (authoritative, `packages/coding-agent/src/core/tools/`): `bash, edit, find, grep, ls, read, write` (7) + edit-diff + file-mutation-queue.
- Multi-provider AI (`packages/ai`): OpenAI, Anthropic, Google, Bedrock, Azure, Mistral, Cloudflare, OpenRouter, GitHub Copilot; **OAuth** (`/login`) + API key; prompt caching; images.
- Context: AGENTS.md / context files; system-prompt builder.
- Sessions: JSONL persistence, **branching + tree navigation**, session picker.
- **Compaction**: context compaction + branch summarization.
- **Skills**: SKILL.md on-demand capabilities.
- **Slash commands** + **prompt templates**.
- Interactive TUI: themes, components, selectors, keybindings, trust.
- **Extensions**: TS plugin runtime (N/A to Rust hya — out of scope as a port).
- Modes: interactive, **print/JSON event stream**, **RPC (stdin/stdout JSONL)**.
- pi has **no** built-in permission system (runs with user perms; sandbox externally).

## Scope — Tier 3 (broad parity), phased into child tasks

Each wave is an independently verifiable child task. Ordering is a hard dependency
chain only where noted; otherwise waves are independent.

- **Wave 1 — Agent can code (P0 foundation).** Fix the permission blocker; make
  edit/write/shell actually work (interactive approval in TUI + safe headless policy);
  add `ls` + real `find` tools; edit-diff safety. *Blocks all later waves' usefulness.*
- **Wave 2 — Project context.** Load AGENTS.md / context files into a real
  system-prompt builder; environment/preamble (cwd, platform, date).
- **Wave 3 — Slash commands + prompt templates.** `/help /model /clear /new /exit`
  (+ extensible registry) in the TUI; markdown prompt templates expand to prompts.
- **Wave 4 — Context survival.** Token-threshold compaction/summarization in the
  engine; SKILL.md discovery + on-demand injection.
- **Wave 5 — Providers + auth.** Add Google provider; OAuth `/login` (Anthropic +
  one OpenAI-class) alongside API keys.
- **Wave 6 — Session tree.** Session branching + tree navigation + picker over the
  event store; resume.
- **Wave 7 — Integration modes.** `print`/JSON event-stream mode and RPC
  (stdin/stdout JSONL) mode for scripting/embedding.

## Requirements

- R1. hya must execute edit/write/shell successfully in TUI (with approval UX) and
  in headless modes (with a documented default policy + override flag), without
  weakening the permission plane's safety guarantees.
- R2. The agent's system prompt must incorporate project context (AGENTS.md and
  equivalent) and runtime environment, not a hardcoded string.
- R3. Tool set reaches pi parity: add `ls` (directory listing) and `find` (file find
  by name/glob with metadata); keep existing tools.
- R4. TUI supports a slash-command registry with the core commands; prompt templates
  expand from markdown.
- R5. Long sessions survive via compaction; skills load from SKILL.md.
- R6. At least one new provider (Google) and OAuth login work end-to-end.
- R7. Sessions can branch and be navigated as a tree; resume a prior session.
- R8. print/JSON and RPC modes emit/consume structured events.
- R9. Every wave preserves the quality gate: `cargo fmt --check`, `clippy -D warnings`,
  `cargo test --workspace` all green; new behavior covered by tests (TDD).
- R10. hya's existing differentiators (goal/loop/team/worktrees/categories) keep
  working (regression-free) throughout.

## Acceptance criteria

- [ ] AC1. `hya exec "create file X with content Y"` against a real model actually
      creates the file; `hya` TUI prompts for approval on a mutating tool and
      proceeds on Allow. (Wave 1)
- [ ] AC2. With an `AGENTS.md` in cwd, the system prompt provably includes its
      content (unit test on the prompt builder + live transcript). (Wave 2)
- [ ] AC3. `ls` and `find` tools are registered, schema-valid, and return correct
      results under permission checks. (Wave 1/3)
- [ ] AC4. In the TUI, `/help` lists commands and `/model` switches model. (Wave 3)
- [ ] AC5. A session exceeding the compaction threshold is summarized and continues
      coherently; a SKILL.md is discoverable and injectable. (Wave 4)
- [ ] AC6. A Google model and an OAuth-authenticated provider each complete a turn. (Wave 5)
- [ ] AC7. A session can be branched and the tree navigated/resumed. (Wave 6)
- [ ] AC8. print/JSON mode emits parseable events; RPC mode answers a JSONL request. (Wave 7)
- [ ] AC9. Full quality gate green after every wave; existing tests still pass. (all)

## Out of scope

- Porting pi's TypeScript **extension runtime** (Rust has no TS plugin host) — hya's
  equivalent extensibility is its team/subagent plane.
- Themes beyond minimal support, doom/snake/games, image input pipeline, Windows-self-update,
  termux, clipboard image, HTML export — cosmetic/peripheral.
- Becoming pi: hya keeps its event-sourced, permissioned, multi-agent architecture.

## Resolved decisions

- D1 (Wave 1): **Keep & fix hya's permission plane.** TUI shows interactive
  Allow / Deny / Allow-Always prompts for mutating tools (edit/write/shell). Headless
  modes (exec/-p/serve) auto-allow within the session workdir and require approval/deny
  outside it, with a `--yolo`/`--allow-all` override flag. Safety differentiator preserved.
- D2: Keep hya's architecture & differentiators (event-sourced, permissioned,
  goal/loop/team/worktrees); add pi's coding essentials on top. Not a pi port.

## Open questions (block planning)

- None blocking. Wave-specific design captured in design.md.
