# Batch B - cli.md

You are writing documentation for the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`. This is a Rust workspace for a
terminal-first coding agent with a Bun/OpenTUI frontend.

## Your batch

You own exactly 1 file(s). Do not create or edit any other file.

- `docs/cli.md`

You have **23 gap entries** and **1 stale claims** to resolve.

For slash commands write a short list plus a link to docs/tui-keybindings.md, which already contains the full table.

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
   list does not count. 12 of your entries are status `thin`, meaning the
   feature IS already mentioned but unusably so -- those need real content, not a
   second mention.
7. Do not run `git commit`. Writing the files is enough.

## Work list

Each entry was produced by an agent that read the source. Treat it as a work list
and a starting point, not as verified truth -- rule 1 still applies.

### `docs/cli.md`

**1. TUI slash-command reference (/sessions /resume /continue, /new /clear, /models /mo, /agents, /mcps, /variants, /status, /themes, /help, /exit /quit /q, /rename, /timeline, /fork, /compact /summarize, /undo, /redo, /timestamps, /thinking, /copy, /export, /editor, /skills, /diff) — covers features 100-122** — `undocumented` · severity high

- Source: `packages/hya-tui-ts/src/upstream/app.tsx:532,543,566,613,622,649,673,682,710,719; packages/hya-tui-ts/src/upstream/routes/session/index.tsx:664,675,697,719,745,783,830,847,1062,1092; packages/hya-tui-ts/src/upstream/component/prompt/index.tsx:412,504; packages/hya-tui-ts/src/upstream/feature-plugins/system/diff-viewer.tsx:1040`
- Evidence: Grepped every slash name across docs/**, README.md, CONTEXT.md, DESIGN.md, AGENTS.md, CHANGELOG.md. docs/cli.md contains ZERO slash commands. Only incidental hits exist: docs/compat-parity.md (/copy, /diff, /init as parity rows), docs/opencode-feature-inventory.md (/status, /undo, /redo, /export as gap-analysis rows), docs/architecture/runtime.md (/compact as an internal mechanism), docs/FOLLOWUPS.md (a wave-3 backlog line). /agents, /mcps, /variants, /themes, /rename, /timeline, /timestamps, /thinking, /editor have no hit anywhere. README.md:111 and docs/getting-started.md:171 both explicitly promise this reference lives in docs/cli.md.
- Write: Add a top-level '## TUI Slash Commands' section (place it after the '## `hya` frontend' section so the README.md:111 / getting-started.md:171 cross-links become true). Render one table with columns: Command | Aliases | Command id | Default keybind | Effect. Rows, verbatim from source: /sessions (aliases /resume, /continue; id session.list; <leader>l; opens the session-list dialog); /new (alias /clear; session.new; <leader>n; navigates to the home route to start a new session); /models (alias /mo; model.list; <leader>m; opens the model picker — note the /mo alias exists to bias fuzzy matching away from /move); /agents (agent.list; <leader>a; agent picker); /mcps (mcp.list; no default key; MCP enable/disable toggle dialog); /variants (variant.list; hidden when the current model has no variants and toasts 'No variants available'); /status (<leader>s; status dialog); /themes (theme.switch; <leader>t; theme list); /help (help.show; help dialog); /exit (aliases /quit, /q; app.exit; ctrl+c, ctrl+d, <leader>q); /rename (session.rename; ctrl+r); /timeline (session.timeline; <leader>g; jump-to-message dialog); /fork (session.fork; forks from a selected message); /compact (alias /summarize; session.compact; <leader>c); /undo (session.undo; <leader>u); /redo (session.redo; <leader>r); /timestamps (alias /toggle-timestamps; session.toggle.timestamps); /thinking (alias /toggle-thinking; session.toggle.thinking; cycles thinking-block visibility); /copy (session.copy; copies the whole transcript to the clipboard); /export (session.export; <leader>x; opens an options dialog, default filename session-<id8>.md); /editor (prompt.editor; <leader>e; opens $EDITOR on the prompt buffer); /skills (prompt.skills; opens the skill selector and inserts '/<skill> ' into the prompt); /diff (diff.open; category VCS; opens the git diff viewer route). State that the leader key defaults to ctrl+x and that ctrl+p opens the full command palette.

**2. Server-provided built-in slash commands (/init, /review, /help, /model, /clear, /sessions, /think) — covers features 123, 124, 125** — `undocumented` · severity medium

- Source: `crates/hya-server/src/compat/command_catalog.rs:29,36,43`
- Evidence: docs/compat-parity.md mentions '/init' only inside a parity-status sentence; '/review' and '/think' have no hit in any in-scope doc. docs/cli.md and docs/configuration.md never describe the backend-provided command catalog.
- Write: In the new '## TUI Slash Commands' section add a '### Backend-provided commands' subsection. These come from the backend catalog (crates/hya-server/src/compat/command_catalog.rs), are served over GET /api/command, and are expanded server-side. Document: /init — 'guided AGENTS.md setup'; its template supports $ARGUMENTS and the server substitutes the current workdir for ${path} before sending. /review — 'review changes [commit|branch|pr], defaults to uncommitted'; runs as a subtask (subtask: true), i.e. in a child session. Then list the five non-expandable built-ins whose template is just the literal command string: /help (show help), /model $ARGUMENTS (switch the active model), /clear (start a fresh session), /sessions (switch session), /think $ARGUMENTS (set reasoning effort). Note these merge with, and are overridden by, user-defined commands of the same name.

**3. Keybinding registry (~200 named user-overridable keybinds; leader defaults to ctrl+x; unbind with false or "none")** — `thin` · severity medium

- Source: `packages/hya-tui-ts/src/upstream/config/keybind.ts:44,56`
- Evidence: docs/getting-started.md:52-53 and docs/cli.md:76 mention only Ctrl-P and Ctrl-X. No in-scope doc names the keybind config file, the entry shape, the namespaces, or how to override/unbind. docs/opencode-feature-inventory.md:17 lists dedicated keybind config as a 'scope decision' pending; docs/compat-parity.md:117 calls the keymap/leader UX incomplete.
- Write: Add a '### Keybindings' subsection under the new TUI Slash Commands section. Explain: the frontend ships ~200 named commands with default bindings, defined in packages/hya-tui-ts/src/upstream/config/keybind.ts. Each registry entry is `{ default, description }`. The leader key is `leader` and defaults to ctrl+x; bindings written as `<leader>x` are leader chords. Namespaces covered: app, session, pane, model, agent, message, prompt, input, dialog, diff, and which-key. Any binding can be overridden by name, or unbound by setting it to `false` or the string `"none"`. Point readers at ctrl+p (command palette) for the authoritative live list of bound keys.

**4. Command palette (ctrl+p) — grouping and discoverability semantics** — `thin` · severity low

- Source: `packages/hya-tui-ts/src/upstream/component/command-palette.tsx:26`
- Evidence: docs/getting-started.md:52 says only 'Ctrl-P | List available commands.' and docs/cli.md:76 says 'press Ctrl-P for the authoritative command list'. Neither says the palette groups by category, shows a Suggested section, shows each command's bound keys, or that hidden commands are excluded.
- Write: In the Keybindings subsection, describe the palette: the `command.palette.show` command (default ctrl+p) lists every reachable non-hidden command together with its currently bound keys, grouped by category, with a 'Suggested' group first. Commands marked hidden (for example /variants when the active model has no variants) are omitted.

**5. `@` autocomplete for files, subagents, and references; `/` at column 0 for slash commands** — `undocumented` · severity medium

- Source: `packages/hya-tui-ts/src/upstream/component/prompt/autocomplete.tsx:633`
- Evidence: No in-scope doc describes the prompt autocomplete triggers. The only 'autocomplete' hit is docs/hya-pi-compat-comparison.md, which describes it as a competitor's feature. docs/opencode-feature-inventory.md:16 says richer file refs 'need coverage'.
- Write: Add a '### Prompt autocomplete' subsection: typing `@` in the prompt opens completion over workspace files, subagents (offered as `@<name>`), and reference aliases (also `@<name>`); typing `/` as the first character of the prompt opens slash-command completion. Note that the two triggers are position-sensitive — `/` only opens the command list at column 0.

**6. MCP-sourced slash commands render with a `:mcp` suffix; skill-sourced entries are hidden** — `undocumented` · severity low

- Source: `packages/hya-tui-ts/src/upstream/component/prompt/autocomplete.tsx:440`
- Evidence: Grep for ':mcp' hits only docs/architecture/agent-tool-surface.md and docs/configuration.md, both about the `mcp__<server>__<tool>` tool-naming convention, not about slash-command list rendering. Grep for 'MCP-sourced' returns nothing.
- Write: In the Prompt autocomplete subsection, note that the `/` command list also includes server-provided commands: entries whose `source` is `mcp` render with a trailing `:mcp` label so they are distinguishable from local commands, and entries whose `source` is `skill` are hidden from the list (reach those through /skills instead).

**7. Exit-code contract for all four binaries (hya shim, hya-ts, hya-backend, hya-updater) — covers features 67, 68, 69, 70** — `undocumented` · severity medium

- Source: `crates/hya/src/main.rs:9; crates/hya-ts/src/main.rs:17,192; crates/hya-backend/src/main.rs:306; crates/hya-updater/src/bin/hya-updater.rs:93`
- Evidence: Grep for 'exit code' across docs/**, README.md, CONTEXT.md, DESIGN.md, AGENTS.md, CHANGELOG.md returns no hits. docs/cli.md has no exit-code section, so nothing tells a script author what to branch on.
- Write: Add a '## Exit Codes' section near the end. hya (shim): exits 1 after printing 'hya: failed to resolve current executable: …' or 'hya: failed to launch `<path>`: …' to stderr; on success it exec()s hya-ts, replacing the process image, so hya-ts owns the final exit code. hya-ts: propagates the Bun child's exit code truncated to u8, using 1 when the child has no code (for example it died by signal); the termination-signal path returns 1 after killing the child process group; any launcher error returns 1 after printing '<invocation-name>: <error>' to stderr; forwarded backend subcommands (bundle/oauth/login/auth) propagate hya-backend's exit code verbatim. hya-backend: 0 on success, 1 with the full anyhow error chain printed to stderr on any error — CLI validation failures use the same path. hya-updater: 0 on success, 1 after printing 'hya-updater: <error>' to stderr. Also note that hya-backend tail-session deliberately exits 0 on a downstream broken pipe.

**8. hya-backend serve signal handling (SIGTERM / SIGINT / SIGHUP → graceful shutdown, exit 0)** — `undocumented` · severity medium

- Source: `crates/hya-backend/src/serve.rs:110`
- Evidence: 'SIGTERM'/'SIGHUP' appear only in CHANGELOG.md and docs/testing/coverage-baseline.md (as a test description). docs/cli.md's '## `hya-backend serve`' section says nothing about shutdown; a supervisor author reading it cannot know the process exits 0 rather than dying by signal.
- Write: Add a paragraph to '## `hya-backend serve`' after the flag table: signal handlers are installed BEFORE the listen line is printed. SIGTERM, SIGINT, and SIGHUP each trigger a graceful axum shutdown followed by spawn-supervisor teardown, so the process terminates normally with exit code 0 rather than dying by signal. This matters for supervisors (systemd, docker stop) and for test harnesses that assert a clean exit. Source crates/hya-backend/src/serve.rs.

**9. hya-backend agent list [--all]** — `thin` · severity medium

- Source: `crates/hya-backend/src/agent_cmd.rs:13`
- Evidence: docs/cli.md:197 lists the bare string `hya-backend agent list` inside the Auth-and-Catalog code block and never explains it; --all is not mentioned anywhere in the in-scope docs (the --all grep hits are unrelated cargo/test flags in AGENTS.md, docs/development.md, docs/testing/*). CONTEXT.md's 'agent list' hit is domain prose, not CLI docs.
- Write: Give `agent list` its own paragraph under 'Auth and Catalog Commands'. Default output is Compat-parity: only the built-in primary agent, printed as `build (primary)` followed by its permission rules as pretty-printed JSON. Add the `--all` flag to the code block and explain that it additionally lists ordinary agents reachable from the build-embedded catalog. State the deliberate limitation explicitly: `agent list` NEVER inspects on-disk agent files under .hya/, .claude/, or .opencode/, nor config-declared agents — it reflects the embedded catalog only. System agents (compaction/title/summary) are excluded because they are not ordinarily spawnable.

**10. hya-backend global --print-logs, --log-level, --pure are accepted no-ops (3 features)** — `thin` · severity medium

- Source: `crates/hya-backend/src/cli_args.rs:28,30,32`
- Evidence: docs/cli.md:21 collapses all three into one table row reading 'Accepted Compat-compatible global flags.' That does not tell a reader they have NO effect — a user passing --log-level DEBUG will reasonably expect debug logging and will file a bug.
- Write: Split the single row into explicit ones (or keep one row but change the wording) and state plainly that all three are parsed and then never read — they exist solely so Compat/OpenCode command lines are accepted unchanged. Specifically: --print-logs is a no-op; --log-level accepts only the four literal values DEBUG, INFO, WARN, ERROR (clap rejects anything else) but the value is discarded; --pure is a no-op. Add one sentence pointing readers at whatever mechanism actually controls logging so the row is not a dead end.

**11. hya --import + subcommand conflict error** — `undocumented` · severity low

- Source: `crates/hya-ts/src/lib.rs:157`
- Evidence: Grep for 'import cannot' returns nothing. docs/cli.md:50 documents --import but never states it is mutually exclusive with the bundle/oauth/login/auth subcommands. Its sibling rule (--fork requires --continue/--session) IS documented at docs/architecture/tui.md:51, so this is an inconsistency.
- Write: After the `hya` frontend option table, add a short 'Validation rules' note mirroring the one already present for hya-backend --resume: --import cannot be combined with any subcommand; doing so prints '<invocation>: --import cannot be used with a subcommand' to stderr and exits 1. Also restate there that --fork requires --continue or --session, so both frontend validation rules live in the CLI reference rather than only in docs/architecture/tui.md.

**12. Bare `hya-backend` with no subcommand — ephemeral in-process backend plus frontend launch, and the non-TTY version banner** — `thin` · severity medium

- Source: `crates/hya-backend/src/main.rs:318; crates/hya-backend/src/serve.rs:137`
- Evidence: docs/cli.md only says 'Bare hya-backend --resume <ID> launches hya --session <ID>' (line 24). docs/architecture/tui.md:65 says hya-backend 'may launch the current hya frontend for bare interactive startup'. Neither says it binds an ephemeral 127.0.0.1:0 backend in-process, nor documents the non-TTY branch. Grep for 'non-TTY' and 'banner' returns nothing.
- Write: Add a short '## Bare `hya-backend`' subsection before the exec section. On a TTY, bare `hya-backend` starts an in-process HTTP/SSE backend bound to an ephemeral loopback port (127.0.0.1:0) and hands the terminal to the `hya` frontend (resolved via HYA_FRONTEND_BIN, else the newest of target/release/hya and target/debug/hya, else `hya` on PATH). Note that the empty default --db is remapped to $XDG_STATE_HOME/hya/sessions.db for this path. On a non-TTY stdout it does NOT start anything: it prints 'hya <version> — a multi-agent coding agent' plus a usage hint naming `hya-backend exec "<prompt>"`, `hya-backend -p "<goal>"`, and `hya-backend --help`, then exits 0. Call out the exit-0 behavior explicitly, since scripts piping hya-backend will hit it.

**13. Global --db empty-string semantics differ per command (interactive / sessions / tail-session remap to XDG_STATE_HOME)** — `contradicted` · severity high

- Source: `crates/hya-backend/src/main.rs:45-67 (resolve_interactive_db)`
- Evidence: docs/cli.md:19 states flatly '`--db <PATH>` | SQLite database path. Empty string uses an in-memory store.' resolve_interactive_db (crates/hya-backend/src/main.rs:51) remaps an empty --db to $XDG_STATE_HOME/hya/sessions.db (or $HOME/.local/state/hya/sessions.db, or ./.local/state/hya/sessions.db) for interactive startup, `sessions`, and `tail-session`. The doc comment on that function even warns that an explicit --db "" is indistinguishable from the clap default.
- Write: Correct the --db row and add a following paragraph. Empty --db means in-memory ONLY for exec/run/serve. For bare interactive startup, `sessions`, and `tail-session`, an empty --db is remapped to $XDG_STATE_HOME/hya/sessions.db (falling back to $HOME/.local/state/hya/sessions.db) so `hya --continue` / `hya -s` can resume across restarts; the directory is created if missing. State the caveat from the source doc comment: an explicit `--db ""` is NOT distinguishable from the clap default on those commands, so to force in-memory use the `hya` frontend's `HYA_DB=` empty override instead. Also reconcile this with the existing HYA_DB paragraph at docs/cli.md:70-72.

**14. hya-backend --log-level accepted values** — `thin` · severity low

- Source: `crates/hya-backend/src/cli_args.rs:28-32`
- Evidence: docs/cli.md:21 lists `--print-logs`, `--log-level`, `--pure` in a single row as "Accepted Compat-compatible global flags" with no value vocabulary, so a user cannot know that --log-level is restricted to DEBUG|INFO|WARN|ERROR and will hit a clap error on `--log-level debug`-adjacent guesses like `trace`.
- Write: Split the combined row in the Global Options table: `--print-logs` (Compat-compatible, accepted), `--pure` (Compat-compatible, accepted), and `--log-level <LEVEL>` where LEVEL must be one of DEBUG, INFO, WARN, ERROR (cli_args.rs:28-32) — any other value is rejected by argument parsing.

**15. Codex loopback PKCE callback URL and port (`http://localhost:1455/auth/callback`)** — `thin` · severity medium

- Source: `crates/hya-app/src/oauth/openai_codex.rs:61`
- Evidence: docs/cli.md:190/205 and docs/configuration.md:211-212 mention the `--loopback` flag and "localhost PKCE" but never give the port or callback path, which a user behind a firewall or in a container needs.
- Write: In the `## Auth and Catalog Commands` prose, where `--loopback` is described, add: the loopback flow binds a local HTTP listener and uses the redirect URI `http://localhost:1455/auth/callback` with locally generated S256 PKCE plus a `state` parameter (openai_codex.rs:61). Tell the user port 1455 must be free and reachable from the browser, and to prefer the default device-code flow on headless or remote machines. Also note that `--loopback` is accepted only for `--type openai-codex` and is rejected for any other type (auth_cmd.rs:105).

**16. `hya-backend oauth status` output fields** — `thin` · severity low

- Source: `crates/hya-app/src/oauth/ensure.rs:126`
- Evidence: docs/cli.md:191 and docs/configuration.md:218 list the command but neither says what it prints. Grep for `oauth_status`, `account id`, `expires_at` near the status command: nothing describing output.
- Write: In `## Auth and Catalog Commands`, add a sentence describing `oauth status [provider]` output: it prints non-secret per-provider status only — credential kind (api vs oauth), the OAuth type, `expires_at`, an `expired` flag, and the ChatGPT/Grok account id when known (ensure.rs:126) — plus a ready-to-copy re-login command line for any expired credential (auth_cmd.rs:50). No token material is printed.

**17. OAuth type aliases accepted by `--type` (`codex`, `grok`, `xai-oauth`)** — `thin` · severity low

- Source: `crates/hya-app/src/auth.rs:13`
- Evidence: docs/cli.md:190 documents `--type <openai-codex|grok-build>` only. The parser also accepts `codex`, `grok` and `xai-oauth` (auth.rs:13); no doc mentions them.
- Write: In the `oauth login` description, note that `--type` accepts aliases: `openai-codex` or `codex`, and `grok-build`, `grok` or `xai-oauth`. State that these two are the ONLY interactive OAuth provider implementations — every other provider must use `hya-backend login <provider> <token>` or an inline `api_key`.

**18. `hya-backend models` unknown-provider error and synthesized fallback entry** — `undocumented` · severity low

- Source: `crates/hya-backend/src/models_cmd.rs:51`
- Evidence: docs/cli.md:196 lists the command and its flags; docs/configuration.md:338 shows an example. Neither documents the `Provider not found: <id>` error or the `hya/<fallback_model>` synthesis when no models are configured.
- Write: In `## Auth and Catalog Commands`, add: `models <provider>` filtered by a provider that has no configured models exits with `Provider not found: <id>`; with no configured models at all (offline), `models` synthesizes and prints a single `hya/<fallback_model>` entry rather than printing nothing (models_cmd.rs:51). Also state that `--verbose` appends a JSON line per model in addition to the sorted `provider/model` ids.

**19. OAuthLoginOptions fields and the 600s login timeout** — `thin` · severity low

- Source: `crates/hya-app/src/oauth/mod.rs:69`
- Evidence: docs/cli.md:190 lists the CLI flags but does not state the overall login timeout. Grep for `600`, `timeout` near oauth in-scope docs: no hits.
- Write: In the `oauth login` description, add that the whole interactive flow has a 600-second (10 minute) default timeout — if the user does not complete the device or loopback approval in that window the command fails and must be rerun (oauth/mod.rs:69). Mention the option set backing the flags: provider, oauth_type, device, loopback, no_browser, model, base_url (the auth_dir/config_path fields are test-only overrides).

**20. `--loopback` is rejected for non-`openai-codex` types** — `thin` · severity low

- Source: `crates/hya-backend/src/auth_cmd.rs:105`
- Evidence: docs/cli.md:190 lists `[--loopback]` in the generic flag list with no restriction; docs/configuration.md:211-212 shows it only under the Codex example but never states it is an error elsewhere.
- Write: In the flag list for `oauth login`, annotate `--loopback` as openai-codex only — passing it with `--type grok-build` (or any other type) is rejected with an error rather than silently ignored (auth_cmd.rs:105). Also note `--browser` and `--no-browser` are mutually exclusive (auth_cmd.rs:108).

**21. Package staging with exclusive lock and orphan cleanup (item 114)** — `undocumented` · severity low

- Source: `crates/hya-bundle/src/package.rs:133`
- Evidence: docs/cli.md:80-118 documents install/list/info/uninstall and the registry path, but never mentions a staging directory, its permissions, or crash cleanup. grep 'stage_package', 'staging' in scope: only docs/self-update.md's unrelated updater staging.
- Write: Document what install does on disk before it touches the registry: stage_package copies the package into <staging_root>/hya-bundle-stage-<pid>-<n>/package with mode 0600 inside a 0700 directory, built first under a `hya-bundle-building-` prefix and then atomically renamed, holding an flock for the staging lifetime. Note that cleanup_orphaned_staging reclaims unlocked leftovers from crashed installs, so stale hya-bundle-stage-* directories are self-healing and should not be deleted by hand while an install is running.

**22. hya-backend serve — signal handlers installed before the listen line** — `undocumented` · severity low

- Source: `crates/hya-backend/src/serve.rs:8`
- Evidence: docs/cli.md:164-181 documents serve's flags but not its shutdown behaviour. No in-scope doc mentions SIGTERM/SIGINT/SIGHUP handling or graceful shutdown ordering. docs/testing/process-e2e.md does not cover it either.
- Write: Add a paragraph under `hya-backend serve`: SIGTERM, SIGINT and SIGHUP handlers are installed BEFORE the listen line is printed (an e2e-harness ordering requirement — a harness that sees the URL may signal immediately), and the server then runs with graceful shutdown so the teardown path actually executes rather than being killed mid-flight.

**23. `hya server listening on <url>` readiness contract and HYA_STARTUP_TRACE** — `thin` · severity medium

- Source: `crates/hya-backend/src/serve.rs:58`
- Evidence: docs/getting-started.md:108 shows the line as sample output only. No doc states it is a CONTRACT string that hya-sdk's ServerHandle parses to discover the base URL (crates/hya-sdk/src/server.rs:1-4 documents this on the SDK side only). grep for 'HYA_STARTUP_TRACE' across all in-scope docs returns zero hits, and docs/configuration.md's env-var table (which does list HYA_FRONTEND_BIN at :422) omits it.
- Write: Under `hya-backend serve`, mark `hya server listening on <url>` as a stability contract: hya-sdk's ServerHandle parses this exact line from merged stdout/stderr to discover the base URL, so its wording must not change. Also document HYA_STARTUP_TRACE=1|true, which additionally emits a JSON `backend_listen` startup mark on stderr, and add it to the env-var table in docs/configuration.md.

**STALE 1.** The document claims: '`--db <PATH>` | SQLite database path. Empty string uses an in-memory store.'

- Reality: resolve_interactive_db (crates/hya-backend/src/main.rs:45-67) remaps an empty --db to $XDG_STATE_HOME/hya/sessions.db (or $HOME/.local/state/hya/sessions.db) for bare interactive startup, `sessions`, and `tail-session`. Empty means in-memory only for exec/run/serve. The source doc comment further notes an explicit `--db ""` is indistinguishable from the clap default on those commands.
- Action: correct or delete. Do not merely supplement.

## When you are done

Report, in this order:

1. Each file you wrote and its approximate line count.
2. How many of the 23 gap entries you resolved. If any remain, name them.
3. Any entry where the source CONTRADICTED the work list, with the `file:line`
   you checked and what the code actually does.
4. Any claim you could NOT confirm from source and therefore omitted.
5. Any code defect you noticed. Do not fix it; just name it.
