# hya — Follow-ups & Deferred Work

Reference for work intentionally left for a future pass. The pi-parity waves
(1–7) and their follow-ups are merged into `main`.

## Deferred (not yet implemented)

- _(none currently tracked here — interactive OAuth login shipped; see Implemented.)_

## Implemented (merged)

- Wave 1 — permission responder (Scoped / ReadOnly / Yolo) + interactive TUI
  approval; `ls` / `find` tools; `edit` ambiguity guard.
- Wave 2 — system-prompt builder + AGENTS.md / context-file discovery.
- Wave 3 — slash commands (`/help` `/model` `/clear` `/new` `/exit` `/sessions`)
  + prompt templates.
- Wave 4 — context compaction (`ModelSummarizer` auto-trigger, env-tunable
  threshold) + SKILL.md skills.
- Wave 5 — native Google (Gemini) provider + auth token store + `hya-backend login`.
- Wave 6 — session list / branch / resume (`list_sessions`, `hya-backend sessions`,
  `--db` / `--resume`, TUI session picker).
- Wave 7 — `exec --json` and `hya-backend rpc` (stdin/stdout JSONL) integration modes.
- Hardening — path-containment resolves symlinks on existing ancestors.
- TUI typed-deny feedback — the permission overlay captures optional rejection
  text and sends it through `Decision::Reject { feedback }`.
- **OAuth interactive login** — full Rust flow in `crates/hya-app/src/oauth/`
  (device-code and loopback/PKCE for `openai-codex`, Grok Build, browser open,
  poll/refresh). CLI: `hya oauth login --provider … --type openai-codex|grok-build`
  with `--device` / `--loopback` / `--browser` flags; see `docs/cli.md` and
  `docs/configuration.md`.

## Notes

- This work was developed on `feat/hya-w1-agent-can-code` (branched from the
  pre-permission baseline) and merged with the concurrent `tui-compat-parity`
  permission commit; on overlap the broader implementation won, while that
  commit's `Decision::Reject { feedback }` plane and tool-output truncation were
  preserved.

## Defects found during the documentation-coverage pass (2026-08-06)

Surfaced while documenting the code against its behaviour. None are fixed here —
that pass changed documentation only. Each entry says whether the claim was
confirmed against the source directly.

### Confirmed

- **`find` does not scope paths to the workdir.** `GlobTool` resolves its input
  through `resolve_file(&ctx.workdir, path)`, but `FindTool` uses
  `PathBuf::from(path)` directly
  ([`crates/hya-tool/src/tool.rs:1010`](../crates/hya-tool/src/tool.rs)). A
  relative path is therefore not resolved against the workdir, and an absolute
  path outside it is not rejected, unlike every neighbouring file tool.
- **The `task` tool description contradicts the implementation.** The advertised
  description at `crates/hya-tool/src/task.rs:113` says background launches
  "currently require foreground execution in hya", while `task.rs:261` rejects only
  *multi-member* background and a `spawn_background` path exists. The model is
  being told the feature does not work. Corrected text was drafted during this
  pass and deliberately reverted, because changing what the model reads is a
  product change, not a documentation change.
- **`lsp` returns a hard error when no server is available.**
  `crates/hya-tool/src/lsp.rs:85` returns `Err(ToolError::Other(...))`, which
  reaches the wire as an unknown error. If a soft "no server for this file type"
  payload was intended, clients cannot distinguish it from a real failure.
- **`codes::VETO` is dead.** `crates/hya-plugin/src/protocol.rs:14` defines
  `VETO: i64 = 1` and nothing else in the tree references it, so a plugin that
  returns it gets no special handling.
- **The plugin event channel drops silently under load.**
  `EVENT_CHANNEL_CAP = 256` with drop-on-overflow
  ([`crates/hya-plugin/src/host.rs:27`](../crates/hya-plugin/src/host.rs)) and no
  backpressure to the engine, so a slow plugin loses events rather than slowing
  the producer.
- **The which-key panel ships disabled.**
  `packages/hya-tui-ts/src/upstream/feature-plugins/system/which-key.tsx:604` sets
  `enabled: false` and `src/hya/static-host.ts:21` filters on
  `plugin.enabled !== false`, yet the full keybinding defaults and a Home hint
  still refer to it. It reads as an available feature.
- **`session.page.up` / `.down` scroll half a page.** The keybinding descriptions
  say "one page"; the handlers use `height / 2`
  ([`packages/hya-tui-ts/src/upstream/routes/session/index.tsx:889`](../packages/hya-tui-ts/src/upstream/routes/session/index.tsx)).
- **`NO_MODELS_TIP` is unreachable.** Defined at
  `packages/hya-tui-ts/src/upstream/feature-plugins/home/tips-view.tsx:31` but only
  used as an array-index fallback that a random index into a non-empty `TIPS`
  never triggers, so the "Configure a model to start coding" path never shows.
- **ADR-0006 described `/new` behaviour that was never implemented.** It claimed
  `/new` aborts the active turn and clears prompt bookkeeping; the handler at
  `packages/hya-tui-ts/src/upstream/app.tsx:545` only navigates home and clears
  dialogs. The ADR now records this as a historical note.

### Reported by a writer, not independently confirmed

- Three hooks (`goal.evaluate`, `loop.verifier`, `loop.planner`) parse and can
  register but appear to have no dispatcher arms.
- Plugin tools whose `inputSchema` is not an object are dropped silently.
- The compat adapter never emits `allow_always`, so OpenCode's "always allow"
  cannot be expressed through it.
- `contextMaxCharacters` advertises a default of 10000 in the schema, but the Exa
  client only forwards the field when explicitly supplied.
- Output truncation is asymmetric: `shell` keeps the first 16 KiB while the global
  `cap_tool_output` keeps the last 5000 characters.
- Two leader-key collisions: `<leader>q` is both `app.exit` and
  `session.queued_prompts`; `<leader>h` is both `session.toggle.conceal` and
  `tips.toggle`.
- `packages/hya-tui-ts/src/upstream/routes/session/footer.tsx` appears unused.

## Documentation coverage: residual and known limits (2026-08-07)

The coverage pass closed a 324-item work list derived from source. Three
independent re-audits, each run by agents that did not write the documents,
measured progress against that list:

| Round | Gaps closed | Still open |
| --- | ---: | ---: |
| 1 | 306 / 324 | 18 |
| 2 | 312 / 324 | 12 |
| 3 | 316 / 324 | 8 |

A fourth correction pass targeted the remaining 8 and was verified against source
by hand, but **no fourth audit was run**, so 316/324 is the last independently
measured figure. Treat the final 8 as fixed-but-unverified.

### The audit found more than the work list contained

Each round also reported claims outside the original 324 entries. That count did
not converge (31 → 14 → 21), and the reason matters: **8 of the 13 documents
flagged in round 3 had never been touched by an earlier fix pass**. The adversarial
readers were sampling different regions of a ~15k-line corpus each time, so the
stream is *discovery*, not regression. It will not reach zero by iterating.

Anyone continuing this work should treat "zero findings from a fresh adversarial
read" as unreachable for a corpus this size, and instead:

- keep the 324-item list as the regression surface (it is stable and re-runnable),
- add a documentation-contract test when a sentence encodes a real contract, the
  way `crates/hya-bundle/tests/docs_example.rs` pins the AgentBundle wording — that
  test is the only reason a rewrite that silently weakened 12 of its 22 pinned
  sentences was caught.

### Recurring defect class worth fixing in code, not docs

The audits repeatedly found configuration fields that are parsed, serialized onto
the wire, and then **silently dropped**. Documenting them accurately is a
workaround; the fields either need consumers or removal:

- Per-command `agent` and `model` (disk frontmatter and config maps) are listed on
  `/api/command` but `CommandRequest` carries only `command`, `arguments`, `text`.
- Per-command `subtask` has no consumer anywhere in `crates/` or `packages/`.
- `PreparedAgent::workdir` has no reader.
- `resolve_default_reasoning`'s `last_used` branch is unreachable — its one
  production caller passes `None`.
- Skill `allowed-tools` and `model` feed only a semantic-identity digest; neither
  restricts tools nor routes models.

Each of these reads as a working feature in configuration. A user setting them
gets silence, not an error.
