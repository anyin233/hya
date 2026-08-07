# Batch N - getting-started.md, troubleshooting.md, development.md, process-e2e.md

You are writing documentation for the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`. This is a Rust workspace for a
terminal-first coding agent with a Bun/OpenTUI frontend.

## Your batch

You own exactly 4 file(s). Do not create or edit any other file.

- `docs/getting-started.md`
- `docs/troubleshooting.md`
- `docs/development.md`
- `docs/testing/process-e2e.md`

You have **12 gap entries** and **6 stale claims** to resolve.

Four small user-facing files with no overlap. Do NOT repoint the broken keybinding link at getting-started.md:171 -- the reconciliation pass owns cross-links.

## Non-negotiable rules

1. **Confirm every claim against the source before you write it.** Every entry
   below carries a `source` reference. Open it. If the source contradicts the
   entry, the SOURCE WINS -- write what the code does and report the discrepancy.
2. **If you cannot confirm a claim from source, do not write it.** Say you could
   not confirm it. Plausible prose that is wrong is worse than an admitted gap,
   because a reader trusts the document.
3. **Stale and contradicted entries are corrected or deleted, never merely
   supplemented.** A document that contradicts the code is a defect.
4. **Do not edit any file outside your batch.** Other writers are working in
   parallel. In particular never touch `docs/README.md`, `README.md`, `AGENTS.md`,
   `DESIGN.md`, or `docs/project-structure.md` -- a later reconciliation pass owns
   all cross-links and the docs map. Some entries below suggest edits to other
   files; ignore that part and write only your own.
5. **Match the existing documentation style.** Read the file you are editing
   before writing. Use the project's vocabulary as defined in `CONTEXT.md`.
6. **A feature counts as documented only if a reader can use it** from what you
   write: what it does, its parameters or keys, and its semantics. A name in a
   list does not count. 2 of your entries are status `thin`, meaning the
   feature IS already mentioned but unusably so -- those need real content, not a
   second mention.
7. Do not run `git commit`. Writing the files is enough.

## Work list

Each entry was produced by an agent that read the source. Treat it as a work list
and a starting point, not as verified truth -- rule 1 still applies.

### `docs/getting-started.md`

**1. install.sh flags --bin-dir, --profile, --dry-run, -h/--help (items 167,168,169,170)** — `undocumented` · severity high

- Source: `install.sh:48,53,58,62`
- Evidence: grep '--bin-dir' across all in-scope docs: ZERO hits; '--dry-run' hits only the unrelated xtask sync-compat example at docs/configuration.md:573. Only `--prefix` is shown, at README.md:35 and docs/getting-started.md:27. docs/troubleshooting.md:13 tells users to 'reinstall with ./install.sh' with no flag reference.
- Write: Add an install.sh options table under the existing install command: `--prefix DIR` installs into DIR/bin (default /usr/local); `--bin-dir DIR` installs directly into DIR and overrides --prefix, with relative paths resolved against the script directory; `--profile release|dev|debug` selects the cargo build profile and matching target dir (honouring CARGO_TARGET_DIR) and exits 2 on any other value; `--dry-run` prints every action, skips building and installing, and prints the verification commands instead of running them; `-h`/`--help` prints usage and exits 0.

**2. install.sh runtime behavior (items 172,173,174,175): Bun preflight and frozen-lockfile production install, atomic swap with automatic rollback, permission preflight, post-install verification** — `undocumented` · severity high

- Source: `install.sh:127,160,189,232`
- Evidence: docs/getting-started.md:31 says only 'The installer colocates hya, hya-ts, and hya-backend and prepares the Bun runtime under lib/hya/hya-tui-ts'; README.md:41 is equivalent. Nothing documents the Bun requirement enforced by the script, the rollback trap, the sudo/--bin-dir remedy, or the verification step — yet docs/troubleshooting.md:13 sends users straight to a reinstall.
- Write: Describe what install.sh actually does, in order, so failures are diagnosable. Bun preflight: `bun --version` must succeed or the install aborts; the script then runs `bun install --frozen-lockfile --production` in the staged runtime and prunes it with `bun packages/hya-tui-ts/scripts/prune-sdk-server.ts`. Permission preflight: it walks up to the nearest existing ancestor of the target bin and lib directories and, if that ancestor is not a writable directory, prints the sudo and `--bin-dir "$HOME/.local/bin"` remedies and exits 1 — document this as the fix for a permission-denied install. Atomic swap: it stages into .tmp.$$ paths, moves any existing install to .bak.$$, then renames into place, with an ERR/INT/TERM trap that calls restore_install to put the previous binaries and runtime back and clean up leftovers, so an interrupted install never leaves a half-installed hya. Post-install verification: it runs the hya shim against a dead server with `--bun /bin/true`, then `hya --version`, `hya-backend --help`, `hya-ts --help`, asserts every runtime file plus node_modules exists, and FAILS if `command -v hya` does not resolve to the install path (the usual cause being an older hya earlier on PATH).

**3. Getting-started key-controls table is incomplete and partly wrong** — `contradicted` · severity medium

- Source: `packages/hya-tui-ts/src/upstream/config/keybind.ts:48`
- Evidence: docs/getting-started.md:49-55 gives a five-row table. `Escape` is described as 'Interrupt the current session or dismiss a dialog' but Escape also exits shell mode, returns an observation pane to Main, clears a pending leader sequence, hides autocomplete, and only aborts on the third press within 5 s. `Ctrl-C / Ctrl-D | Exit` omits that `prompt.clear` binds ctrl+c while the prompt has text, that app.exit only fires when the prompt is unfocused or empty, and that ctrl+d is `session.delete` in the session list and `input.delete` in the prompt.
- Write: Correct and slightly expand the key-controls table, then point at the new keybindings reference for the full set. Escape row: 'Dismiss a dialog, hide autocomplete, clear a pending leader sequence, exit shell mode, return an observation pane to Main, or interrupt the running turn — press three times within 5 s to abort.' Ctrl-C row: 'Copy the selection if there is one, clear the prompt if it has text, otherwise exit.' Ctrl-D row: 'Exit when the prompt is empty and unfocused; deletes forward inside the prompt and deletes the highlighted entry in the Sessions and Stash dialogs.' Add rows for `<leader>l` sessions, `<leader>m` models, `<leader>a` agents, `<leader>o` subagent roster, and `<leader>b` sidebar so a first-run user has a usable starting set.

**STALE 1.** The document claims: 'For the full command and TUI slash-command reference, see the [CLI Reference](cli.md).'

- Reality: Same dead promise — docs/cli.md has no slash-command reference. A reader following this link finds nothing about /models, /agents, /export, /compact, etc.
- Action: correct or delete. Do not merely supplement.

**STALE 2.** The document claims: docs/configuration.md contains 'the complete `HYA_*` environment-variable reference'.

- Reality: The docs/configuration.md table lists 5 HYA_* variables. The code reads at least 17: HYA_MODEL, HYA_DB, HYA_COMPACTION_THRESHOLD, HYA_COMPACTION_KEEP_RECENT, HYA_COMPAT_ADAPTER_DIR, HYA_FRONTEND_BIN, HYA_BACKEND_BIN, HYA_TUI_TS_DIR, HYA_STARTUP_TRACE, HYA_SUBAGENT_MAX_DEPTH, HYA_SUBAGENT_MAX_CONCURRENCY, HYA_SUBAGENT_BUDGET, HYA_SUBAGENT_TURN_BUDGET, HYA_SUBAGENT_MESSAGE_BUDGET, HYA_EVENT_BUS_CAPACITY, HYA_DEFER_SIDEPLANES, HYA_VERSION, HYA_CHANNEL, HYA_ROUTE, HYA_FAST_BOOT, HYA_E2E_BACKEND_BIN.
- Action: correct or delete. Do not merely supplement.

**STALE 3.** The document claims: docs/cli.md is "the TUI slash-command reference" / "For the full command and TUI slash-command reference, see the CLI Reference (cli.md)."

- Reality: docs/cli.md contains no slash commands — grep for 'slash' in that file returns nothing. The ~23 built-in slash names derived in packages/hya-tui-ts/src/upstream/keymap.tsx:260 (/sessions, /new, /models, /agents, /mcps, /variants, /status, /themes, /help, /exit, /rename, /timeline, /fork, /compact, /undo, /redo, /timestamps, /thinking, /copy, /export, /editor, /skills, /diff plus aliases) are documented nowhere in the repository.
- Action: correct or delete. Do not merely supplement.

**STALE 4.** The document claims: "`Escape` | Interrupt the current session or dismiss a dialog." and "`Ctrl-C` / `Ctrl-D` | Exit."

- Reality: Escape has five other behaviors that take priority: exiting shell mode, returning an observation pane to Main, clearing a pending leader sequence, hiding autocomplete, and rejecting a permission prompt — and interrupt only aborts on the third press within 5 s. Ctrl+C copies the selection when one exists and otherwise binds `prompt.clear` while the prompt has text; app.exit is gated so it only fires when the prompt is unfocused or empty. Ctrl+D is `session.delete` inside the Sessions dialog, `stash.delete` inside the Stash dialog, and `input.delete` inside the prompt.
- Action: correct or delete. Do not merely supplement.

### `docs/troubleshooting.md`

**1. HYA_STARTUP_TRACE** — `undocumented` · severity medium

- Source: `crates/hya-ts/src/main.rs:267-276; crates/hya-backend/src/serve.rs:106; packages/hya-tui-ts/src/hya/startup-trace.ts:8`
- Evidence: grep for `HYA_STARTUP_TRACE`, `startup_trace`, and `startup-trace` across all in-scope docs returns zero hits.
- Write: Add a short `## Diagnosing Slow Startup` section to docs/troubleshooting.md: set `HYA_STARTUP_TRACE=1` (the only truthy values are exactly `1` or `true`, case-insensitive) to emit structured startup marks on stderr from both the Rust launcher and the TypeScript TUI (hya-ts/src/main.rs:267-276, hya-backend/src/serve.rs:106, packages/hya-tui-ts/src/hya/startup-trace.ts:8). Show a sample invocation `HYA_STARTUP_TRACE=1 hya . 2>trace.log` and add a cross-reference row to the HYA_* table in docs/configuration.md.

**2. Clipboard environment: TMUX / STY / WAYLAND_DISPLAY** — `undocumented` · severity low

- Source: `packages/hya-tui-ts/src/upstream/clipboard.ts:27, 101`
- Evidence: grep for `TMUX`, `WAYLAND`, and `STY` across all in-scope docs returns zero hits.
- Write: Add a `## Copy/Paste Does Not Work` section to docs/troubleshooting.md: when `TMUX` or `STY` is set, hya wraps the OSC-52 clipboard sequence in a tmux/screen passthrough; when `WAYLAND_DISPLAY` is set, hya prefers `wl-copy` over the X11 clipboard tools (clipboard.ts:27,101). Tell users on Wayland to install wl-clipboard, and users of nested multiplexers that a stale TMUX variable can break the passthrough.

**3. Upstream failure handling: non-2xx body truncation, no retry, and in-stream `error` frame abort** — `undocumented` · severity medium

- Source: `crates/hya-provider/src/http.rs:517, crates/hya-provider/src/http/stream.rs:23`
- Evidence: docs/architecture/providers.md:52-53 covers only the happy-path SSE pump. docs/troubleshooting.md has entries for offline mode, unknown model and API key templates but nothing about upstream HTTP errors. Grep for `500 chars`, `retry`, `non-2xx` in-scope: no hits (docs/compat-parity.md:114 only lists retry as a Compat feature hya lacks).
- Write: Add a `## Provider Call Fails with `http: <status>: ...`` section. A failed HTTP status returns `ProviderError::Http("{status}: {first 500 chars of body}")` — the body is truncated to 500 characters, so a long upstream error page is cut off (http.rs:517). State plainly that NO retry is attempted at any layer: a 429 or 5xx fails the turn immediately. Also document that once the stream is open, any SSE frame whose JSON carries an `error` object aborts the stream with `Http(message)` before the frame reaches the decoder (http/stream.rs:23), so mid-stream provider errors surface as the same error variant. Cross-link from docs/architecture/providers.md's HTTP Provider section.

**4. HYA_STARTUP_TRACE mark vocabulary** — `undocumented` · severity medium

- Source: `packages/hya-tui-ts/src/hya/startup-trace.ts:8`
- Evidence: grep for 'startup trace', 'HYA_STARTUP_TRACE', 'bun_entry', 'shell_paint' across all in-scope docs returns nothing.
- Write: Add a 'Diagnosing slow TUI startup' section. Explain that setting HYA_STARTUP_TRACE to a truthy value makes the frontend emit one JSON line per startup mark on stderr, each carrying wall and monotonic timestamps, and list the mark vocabulary in emission order: `bun_entry`, `backend_spawn` (only when the launcher auto-spawns hya-backend), `theme_resolved`, `shell_paint` (value `immediate` unless HYA_SYNC_PLUGIN_START is set), `plugin_host_done`, `sync_partial`, `sync_complete`. Show a sample capture command and note that HYA_SHOW_TTFD adds an on-screen first-paint overlay and HYA_FAST_BOOT skips the loading overlay when comparing runs.

**5. Startup loading overlay and error boundary screen** — `undocumented` · severity medium

- Source: `packages/hya-tui-ts/src/upstream/component/startup-loading.tsx:5`
- Evidence: No in-scope doc mentions the loading overlay or the crash screen. docs/troubleshooting.md 'The TUI Does Not Start' covers only launcher failures.
- Write: Extend 'The TUI Does Not Start'. Explain the normal startup overlay: after 500 ms of not-ready a bottom-centered spinner reads 'Loading plugins...' then 'Finishing startup...', and it is held on screen for at least 3 s; `HYA_FAST_BOOT` suppresses it entirely, which is the right flag when measuring startup. Explain the error boundary: an unhandled render error replaces the UI with a full-screen crash view with a clickable reset button, themed for the detected light/dark mode — pressing reset re-mounts the app without restarting the process, so the backend session survives.

**STALE 1.** The document claims: "`kind` is `openai`, `openai-compatible`, `anthropic`, or `google`" — presented as the set of valid provider kinds to check.

- Reality: crates/hya-app/src/config.rs:203-220 accepts `openai` (aliases `openai-compatible`, `openai-completion`), `openai-response`, `openai-codex`, `grok-build`, `anthropic`, and `google`. The troubleshooting list omits openai-completion, openai-response, openai-codex, and grok-build, so a user with a valid `kind: grok-build` route is told their config is wrong. docs/configuration.md:182-191 already has the correct table; troubleshooting.md should link to it instead of repeating a subset.
- Action: correct or delete. Do not merely supplement.

**STALE 2.** The document claims: "make sure that exact model id appears as a key under a supported provider's `models` object."

- Reality: `providers.<id>.models` is a YAML sequence, not a mapping (crates/hya-app/src/config.rs:178; entries are either a bare model-id string or a mapping with `id`). There are no "keys" under `models`. The wording should say the model id must appear as an item in the provider's `models` list (either as a bare string or as the `id` of a detailed entry).
- Action: correct or delete. Do not merely supplement.

### `docs/development.md`

**1. cargo xtask dev commands (sync-compat, migrate, startup-bench, matrix-check)** — `thin` · severity medium

- Source: `crates/xtask/src/main.rs:12`
- Evidence: Only `sync-compat` is documented (docs/configuration.md:548-600). `matrix-check` gets one incidental mention (docs/testing/agent-matrix.md:163). `startup-bench` has zero hits and `migrate` is nowhere described as an alias. Worse, docs/project-structure.md:28 and AGENTS.md:86 both describe crates/xtask as a 'scaffold', which is now false.
- Write: Add a '## Dev tasks (`cargo xtask`)' section listing all four dispatch targets from crates/xtask/src/main.rs. Note that xtask uses a hand-rolled positional dispatcher (not clap): the first positional argument selects the task and every remaining argument is forwarded verbatim; an unrecognized task prints 'usage: cargo xtask <sync-compat|migrate|startup-bench|matrix-check>' and exits 0. Document: sync-compat — imports providers/models/MCP/skills from an OpenCode/Compat config (cross-link the existing recipe in docs/configuration.md); migrate — an alias that dispatches to the same sync-compat implementation; startup-bench — startup latency benchmark, honors HYA_BACKEND_BIN to select the binary under test; matrix-check — validates crates/hya-e2e/matrix.toml (cross-link docs/testing/agent-matrix.md). State that xtask is dev-only tooling and is not part of any shipped binary. In the same change, fix the 'scaffold' descriptions in docs/project-structure.md:28 and AGENTS.md:86.

**2. HYA_BACKEND_DIR (native-spike example)** — `undocumented` · severity low

- Source: `crates/hya-sdk/examples/native_spike.rs:9`
- Evidence: grep for `HYA_BACKEND_DIR` across all in-scope docs returns zero hits.
- Write: In docs/development.md, under the crate/example notes, add one line: `HYA_BACKEND_DIR` is read only by the SDK native-bridge example (crates/hya-sdk/examples/native_spike.rs:9) and names the package directory for that bridge. It is an example-only variable and has no effect on `hya`, `hya-backend`, or the TUI — do not add it to the user-facing configuration reference.

**3. Frontend build and tooling commands (bun run build, bunfig preload, scripts/prune-sdk-server.ts, scripts/generate-logo-art.py)** — `thin` · severity medium

- Source: `packages/hya-tui-ts/package.json:11`
- Evidence: AGENTS.md:141 and docs/development.md:48-54 document only `bun test` and `bun run typecheck`. No doc mentions `bun run build`, the bunfig preload requirement, or when to re-run either script. docs/architecture/tui.md:81 alludes to the SDK pruning ('removes SDK server code that the frontend does not use') without naming the script. docs/research/terminal-icon-rendering.md links generate-logo-art.py but gives no invocation.
- Write: Extend the 'TUI / SDK real-backend (Track T)' section into a fuller 'TypeScript frontend' section. Document `bun run build` = `bun build src/main.tsx --outdir dist --target bun --packages external`, producing `dist/main.js` plus the copied audio assets. Document `bun run typecheck` = `tsgo --noEmit` over `src` and `test` with jsx=preserve and jsxImportSource=@opentui/solid. Document `bun test`, naming the suites so a contributor knows what to run after a change: boundary, branding-pruning, sdk-spine, runtime-boundary, startup-trace, agent-visibility, task-presentation, subagent-workspace, pty-smoke, real-backend, real-backend-agents. State the hard prerequisite that both the runtime and the test runner preload `@opentui/solid/preload` via `bunfig.toml` — without it the TUI JSX does not resolve, which is the first thing to check when a fresh checkout fails to render or a test fails to compile. Document `scripts/prune-sdk-server.ts` as the post-install step that rewrites the installed `@opencode-ai/sdk` export map down to only the v2 client, deletes the server/process bundles, and probes that `createOpencodeClient` still imports — say when it must be re-run (after any SDK dependency bump). Document `scripts/generate-logo-art.py` as the regenerator for `component/logo-art.data.ts` and `util/epilogue-art.data.ts` from the 8-bit Hya wordmark PNG, and say it only needs re-running when the wordmark asset changes.

### `docs/testing/process-e2e.md`

**1. env HYA_ROUTE and env HYA_FAST_BOOT (2 features)** — `undocumented` · severity low

- Source: `packages/hya-tui-ts/src/upstream/app.tsx:249,250`
- Evidence: Zero hits for either name in any in-scope doc, including docs/testing/*.md where automation hooks are otherwise described.
- Write: Add a short 'TUI automation hooks' subsection. HYA_ROUTE — a JSON value parsed at TUI startup that overrides the initial route; malformed JSON throws during boot, so quote it carefully in test harnesses. HYA_FAST_BOOT — any truthy value skips the TUI's initial loading screen, which makes deterministic screen assertions possible. Mark both explicitly as test/automation-only hooks that are not part of the supported user configuration surface. Source packages/hya-tui-ts/src/upstream/app.tsx.

## When you are done

Report, in this order:

1. Each file you wrote and its approximate line count.
2. How many of the 12 gap entries you resolved. If any remain, name them.
3. Any entry where the source CONTRADICTED the work list, with the `file:line`
   you checked and what the code actually does.
4. Any claim you could NOT confirm from source and therefore omitted.
5. Any code defect you noticed. Do not fix it; just name it.
