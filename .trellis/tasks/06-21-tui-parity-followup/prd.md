# TUI opencode parity follow-up

## Goal

Track and (when prioritized) close the **out-of-scope** surfaces identified during
the TUI opencode-parity effort — every opencode TUI feature that hya deliberately
did NOT build because it has no equivalent backend concept yet. This task is the
parent backlog; each deliverable below is independently verifiable and should
become its own child task (with its own `design.md`) when picked up.

**Explicitly excluded from this task:** provider OAuth / in-TUI provider-connect
wizard (tracked separately, per direction).

## Context & confirmed facts

- Source of truth for what shipped vs. deferred vs. out-of-scope:
  `docs/parity.md` on branch `tui/opencode-parity`.
- The TUI parity work (W0–W7: editor, dialogs, theme loader+persistence, token
  usage, rename/delete, skill picker, markdown/diff render, abort) lives on the
  unmerged branch `tui/opencode-parity` (off substrate `f5ae121`).
- **Architecture has since diverged on `main`:** hya now uses its own
  `~/.config/hya/config.yaml` (`providers: { kind: openai|anthropic|google }`)
  instead of reading opencode's config. Any backend work here targets the current
  `main` architecture, not the opencode-config substrate the branch was cut from.
- hya is currently **single-agent** (`agent_with_model` is fixed to `build`) and
  has no MCP, sharing, workspace-warp, tag, or model-variant concepts.

## Hard dependency

- **Blocked by:** `tui/opencode-parity` merged into `main`. Every deliverable here
  builds on that TUI's dialog system (`SelectDialog`/`DialogKind`, leader plane,
  `Effect`/`Msg` TEA loop) and theme registry.

## Deliverables (each → a future child task)

Each item lists: what it is, the current gap, what a real implementation needs
(backend + TUI), and acceptance. Effort is a rough t-shirt size.

### 1. Stash dialog (S) — TUI-local, no backend

- **What:** opencode's prompt-stash (set aside drafts, pop them back).
- **Gap:** `PromptStash` exists in `hya-render-tui` (push/pop/cap) but is wired to
  nothing; opencode itself ships it **unbound** by default.
- **Build:** a stash-add action + a stash list/pop dialog (`DialogKind::Stash`);
  persist to `$XDG_STATE_HOME/hya/tui/stash.jsonl` like prompt history.
- **Accept:** stashing a draft clears the editor and stores it; the stash dialog
  lists entries and pops the selected one back into the editor; survives restart.

### 2. Status dialog (S) — read-only, data already present

- **What:** a session/status overlay (id, title, model, agent, message + token
  counts, cwd, theme).
- **Gap:** all data is already in `AppState`/`Projection`; no overlay surfaces it.
- **Build:** a `<leader>`-triggered status overlay rendering current session info.
- **Accept:** the overlay shows the live session metadata and dismisses on Esc.

### 3. Session tags (M) — needs backend

- **What:** user-assigned tags on sessions; filter/group the session list by tag.
- **Gap:** no tag concept in `hya-proto`/`hya-store`.
- **Build:** an `Event::SessionTagged { tags }` (or tag table) + projection field
  + store query; a tag dialog to add/remove + a filter in the session switcher.
- **Accept:** tagging persists and the session switcher can filter by tag.

### 4. Model variants (M) — needs backend

- **What:** opencode model "variants" (e.g., reasoning-effort / config presets per
  model) selectable from a variant picker.
- **Gap:** hya has `ModelRef` only; no variant concept.
- **Build:** decide the variant model (reasoning effort? sampling preset?), add it
  to the provider/agent request, expose a `DialogKind::Variant` picker.
- **Accept:** selecting a variant changes the next turn's request parameters.
- **Risk:** product-shape decision required (see open questions).

### 5. MCP servers + MCP dialog (XL) — needs whole subsystem

- **What:** Model Context Protocol server integration + a dialog listing
  servers/tools and their status.
- **Gap:** hya has no MCP client at all.
- **Build:** an MCP client subsystem (spawn/connect servers, import their tools
  into `ToolRegistry`, surface status), then a `DialogKind::Mcp` view.
- **Accept:** a configured MCP server's tools are callable in a turn and listed in
  the dialog.
- **Risk:** large; only worth it if MCP is on hya's roadmap (see open questions).

### 6. Workspace warp / move-workspace (M) — needs backend

- **What:** opencode's "warp"/move-workspace (re-root the session to another dir).
- **Gap:** the agent workdir is fixed at startup; no re-root path.
- **Build:** an engine API to change a session's workdir (+ permission re-scope) and
  a dialog/prompt to pick the new root.
- **Accept:** warping re-roots tool resolution + permission scope to the new dir.

### 7. Console / org / share (L) — needs hosted backend

- **What:** opencode's session sharing / org console links.
- **Gap:** hya has no hosted/console concept.
- **Build:** would require a sharing backend (upload session, return a URL) — large
  and arguably outside hya's local-first design.
- **Accept:** N/A until a sharing backend exists.
- **Risk:** likely **won't-do** for a local-first tool (see open questions).

## Explicitly out of scope

- **Provider OAuth / in-TUI provider-connect wizard** — excluded by direction;
  hya resolves keys via `config.yaml` + `hya login`.

## Related, tracked elsewhere (NOT this task)

These are **deferred** (a backend/feature exists or is planned), not out-of-scope,
and should be their own tasks:

- 33 built-in opencode themes (the JSON loader already shipped; only the bundled
  theme data is missing).
- Syntax highlighting (syntect) + full-screen DiffViewer (W4.5 polish).
- Session compact / fork / timeline (need new public `hya-core` APIs;
  compaction is currently internal to the turn loop).
- `/stream?since_seq=N` SSE backfill (server-only; the embedded TUI uses the
  in-process bus).

## Acceptance criteria (for this parent task)

- [ ] Each deliverable (1–7) is either promoted to a child task with its own
      `design.md`, or explicitly marked won't-do with a one-line justification.
- [ ] Child-task dependencies on `tui/opencode-parity` (merged) are written in the
      child artifacts, not implied.
- [ ] OAuth remains excluded; the deferred list above stays out of this task.

## Open questions (product intent — resolve before promoting children)

1. **Roadmap fit:** Which of MCP (5), warp (6), and console/share (7) are actually
   wanted for hya, vs. permanently won't-do? These are the large, backend-heavy,
   possibly off-mission items. *Recommendation:* defer MCP unless on the roadmap;
   mark console/org/share won't-do for a local-first tool; treat warp as optional.
2. **Variant semantics (4):** what does a "variant" mean in hya — reasoning
   effort, sampling preset, or system-prompt preset? *Recommendation:* reasoning
   effort, since the provider request already has a slot for it.
3. **Priority order:** if building, recommended order is the cheap, fully-grounded
   wins first — Stash (1) and Status (2) — then Tags (3), then the rest by roadmap.
