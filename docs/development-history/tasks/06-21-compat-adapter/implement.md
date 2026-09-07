# Implement - Compat-compat Bun adapter (Child C)

> Execute only after Child A (plugin host/core protocol) and Child B
> (plugin-registered tools) are complete and this plan has passed review.
> Production implementation must not change the yaca stdio plugin protocol.

## 0. Preconditions

- Child A protocol/host is merged and exposes `kind: compat` plugin process
  spawning, or this child adds only the config-resolution branch needed to spawn
  the bundled adapter as an ordinary child command.
- Child B dynamic tool registration is merged, so Compat `tool:` definitions can
  become yaca model-callable tools through `initialize.tools` and `tool/call`.
- The yaca HTTP server still exposes at least the current endpoints documented in
  README and implemented in `yaca-server`: `POST /sessions`,
  `POST /sessions/:id/prompt`, `GET /sessions/:id/events`, and
  `GET /sessions/:id/stream`. Source: [README](/chivier-disk/yanweiye/Projects/yaca/README.md:68),
  [router](/chivier-disk/yanweiye/Projects/yaca/crates/yaca-server/src/lib.rs:30).
- Adapter package dependencies are pinned to `@opencode-ai/plugin@1.17.9` and
  `@opencode-ai/sdk@1.17.9` for the first compatibility target. Source:
  [plugin package](/tmp/compat-src/packages/plugin/package.json:3).

## 1. Phase 1 - Adapter package skeleton and Bun isolation

Acceptance mapping: C-AC5.

Tasks:

1. Add `crates/yaca-plugin-compat/Cargo.toml` as a tiny Rust package with no
   Bun dependency and no dependency from yaca core.
2. Add `crates/yaca-plugin-compat/adapter/package.json`, `tsconfig.json`, and
   `bun.lock` with pinned Compat packages.
3. Add a minimal `src/main.ts` that can start, print a version/health message to
   stderr, and exit cleanly when invoked with `--version` or `--help`.
4. Add a Rust-side command resolution function for `kind: compat` that checks
   for `bun` only when this plugin is enabled.
5. If `bun` is missing, mark the adapter plugin disabled with a clear warning and
   continue startup with no hooks.

Validation:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

```sh
if command -v bun >/dev/null; then
  cd crates/yaca-plugin-compat/adapter
  bun install --frozen-lockfile
  bun test
else
  echo "Bun absent: adapter tests skipped; C-AC5 graceful-degrade path required"
fi
```

Rollback point: remove `crates/yaca-plugin-compat/` and the `kind: compat`
command-resolution branch. yaca core remains unchanged.

## 2. Phase 2 - Stdio JSON-RPC adapter runtime

Acceptance mapping: C-R1.

Tasks:

1. Implement `adapter/src/protocol.ts` for JSON-RPC 2.0 line framing over
   stdin/stdout using the parent protocol's method names.
2. Implement `initialize` handling that reads env/options, loads Compat plugins,
   and replies with:
   - `protocol_version: 1`
   - `plugin: { id: "compat", version: <adapter version>, kind: "compat" }`
   - declared yaca hooks derived from loaded Compat hooks
   - declared yaca tools derived from Compat `tool:` definitions
3. Implement `shutdown` to call every loaded Compat hook `dispose()` in order,
   then exit after flushing stdout/stderr.
4. Add unit tests for request/response correlation, malformed JSON lines,
   unknown methods, initialize response shape, and shutdown disposal.

Validation:

```sh
cd crates/yaca-plugin-compat/adapter
bun test protocol initialize shutdown
```

Rollback point: keep the package skeleton but disable `kind: compat` bootstrap;
no yaca runtime behavior depends on this phase until the child command is enabled.

## 3. Phase 3 - Compat loader fidelity (split into atomic TDD sub-steps — D2)

Acceptance mapping: C-R2. Each sub-step is RED test(s) → GREEN impl → `bun test`,
and is an independent rollback unit (revert only that sub-step's file/section).

### Phase 3a — Config parsing + directory discovery
- RED: `bun test loader.discovery` — adapter options parse Compat's
  `plugin?: Array<string | [string, options]>`; discovery returns candidates from
  `${directory}/.opencode/{plugin,plugins}/*.{ts,js}` and
  `${XDG_CONFIG_HOME:-~/.config}/compat/{plugin,plugins}/*.{ts,js}`.
- GREEN: implement config parse + `Glob.scan` discovery only. Rollback: revert
  `loader/discovery.ts`.

### Phase 3b — Local path normalization + module-shape detection
- RED: `bun test loader.shape` — local specs normalize relative to their declaring
  config source; module detection returns "v1 `{id,server}`", "legacy bare fn",
  "legacy object-with-`server`", "tui-only → skip+warn", or "error".
- GREEN: implement `resolvePluginSpec`-equivalent + shape detector
  (`readV1Plugin`/`getServerPlugin` analogs). Rollback: revert `loader/shape.ts`.

### Phase 3c — npm spec resolution + Bun install (cache dir)
- RED: `bun test loader.npm` (Bun-only, opt-in) — an npm specifier installs into
  `$XDG_CACHE_HOME/yaca/compat-adapter/`, preserves package order, and never
  writes `node_modules` into the user's repo unless the plugin is already local.
- GREEN: implement `bun add`/`bun install` shell-out + resolution. Rollback: revert
  `loader/npm.ts`.

### Phase 3d — Entrypoint resolution (v1.17.9 semantics)
- RED: `bun test loader.entry` — resolution prefers package `exports["./server"]`,
  then package `main` for server plugins, then file/dir index for local plugins;
  rejects a server entry resolving outside the plugin dir.
- GREEN: implement `resolvePackageEntrypoint` analog. Rollback: revert
  `loader/entry.ts`.

### Phase 3e — Plugin init + fault isolation + order
- RED: `bun test loader.init` — load + init each plugin sequentially in config
  order; one import/init error skips only the bad plugin and preserves later
  plugins; `server(input, options)` receives tuple options.
- GREEN: implement the `applyPlugin` loop with per-plugin try/catch. Rollback:
  revert `loader/init.ts`.

Phase-3 exit gate: `cd crates/yaca-plugin-compat/adapter && bun test loader`
(all sub-suites green).

## 4. Phase 4 - Compat init context and SDK-shaped yaca client

Acceptance mapping: C-R4, C-AC4.

Tasks:

1. Implement `init-context.ts` to synthesize the exact Compat `PluginInput`:
   `client`, `project`, `directory`, `worktree`, `experimental_workspace`,
   `serverUrl`, and `$`.
2. Implement `sdk-client.ts` with supported methods:
   - `session.create` -> `POST /sessions`
   - `session.prompt` -> `POST /sessions/:id/prompt` for text-only parts
   - `session.messages` -> adapter replay/reduce only if a yaca projection endpoint
     is not yet available; otherwise call `GET /sessions/:id/messages`
   - `event.subscribe` / `global.event` -> adapter-local stream of incoming yaca
     protocol `event` notifications
   - `app.log` -> stderr/tracing now, `POST /log` when the server follow-up lands
   - `project.current`, `path.get`, `vcs.get` -> local context/git shims
   - `file.read/list/status`, `find.text/files` -> local read-only filesystem shims
3. Every unsupported SDK method must throw `UnsupportedSdkMethodError` with the
   method name and a pointer to Child C compatibility docs.
3b. Implement the re-entrancy guard (design §4b): a `currentHookContext` set/cleared
   around every hook handler; `session.prompt`/`session.create` throw
   `YacaReentrantWriteError` while a hook is in flight. Read methods
   (`session.messages`, `event.subscribe`) are allowed mid-hook. Add a test that a
   write SDK call from inside a hook throws and a read call succeeds.
4. Add tests that a plugin can call at least one SDK method against a live yaca
   server. Minimum C-AC4 target: `client.session.create` calls yaca
   `POST /sessions` successfully and receives a session id.
5. Coordinate yaca-server follow-ups only if selected target plugins need them:
   - `GET /sessions/:id`
   - `GET /sessions/:id/messages`
   - `GET /events/stream` or `GET /stream`
   - `POST /log`
   - `GET /tools`

Validation:

```sh
cd crates/yaca-plugin-compat/adapter
bun test sdk-client init-context
```

```sh
tmux new-session -d -s yaca-compat-sdk 'cargo run -p yaca-cli -- serve --bind 127.0.0.1:0 --db "" 2>&1 | tee /tmp/yaca-compat-sdk.log'
# Capture the selected server URL from the log, then run the Bun SDK integration
# test with YACA_SERVER_URL set to that URL.
```

Rollback point: adapter remains loadable, but SDK methods can be disabled by
returning `UnsupportedSdkMethodError`; no protocol rollback is needed.

## 5. Phase 5 - Hook translation

Acceptance mapping: C-R3, C-AC3.

Tasks:

1. Implement `event` notification translation:
   - `SessionCreated` -> `session.created`
   - `Error` -> `session.error`
   - `TextDelta` -> `message.part.delta`
   - `ToolResult`/`ToolError` -> `message.part.updated` for a tool part
   - unknown yaca events -> logged and skipped by default
2. Implement `message.user.before` -> Compat `chat.message` text-only mapping.
3. Implement `chat.params` -> `experimental.chat.system.transform` then
   `chat.params`, applying supported fields to yaca `CompletionRequest`.
4. Implement `tool.execute.before`:
   - seed output `{ args }` from yaca input
   - normal return -> yaca `continue { input: output.args }`
   - throw -> yaca `veto { reason }`
5. Implement `tool.execute.after`:
   - seed Compat output `{ title, output, metadata }` from yaca JSON result/error
   - normal return -> yaca `continue { result }`
   - never synthesize permission errors AND never rewrite an original permission
     `Err` into a success — when the seeded input was a permission denial, the
     adapter returns the original `Err` unchanged (parent design §2.6; the yaca host
     also enforces this defensively).
6. Implement `permission.ask`:
   - Compat `allow` -> yaca `allow_once`
   - Compat `deny` -> yaca `reject`
   - Compat `ask` -> yaca `defer`
7. Add Bun tests for:
   - in-place arg mutation changes yaca outcome input
   - **[D5] permission-preservation:** a `tool.execute.after` whose seeded input is a
     permission denial and whose Compat plugin rewrites `output.output` to a
     success string — assert the adapter still returns the original permission `Err`
     outcome (does not mask the denial), per parent design §2.6. (A yaca-side
     integration test in Phase 8 confirms the host preserves it end-to-end.)
   - `tool.execute.before` throw becomes veto
   - `chat.params` mutates temperature/max output tokens/system where supported
   - `permission.ask` allow/deny/ask mapping
   - hook chain order with two plugins mutating the same output
   - thrown non-guard hook returns a hook error and leaves posture to the host

Validation:

```sh
cd crates/yaca-plugin-compat/adapter
bun test hooks
```

Rollback point: disable individual hook mappings in the `initialize.hooks` result
while keeping the adapter process and loader intact.

## 6. Phase 6 - Compat `tool:` registration and `tool/call`

Acceptance mapping: C-AC2.

Tasks:

1. Convert each Compat `ToolDefinition` to a yaca tool declaration in the locked
   `initialize.tools[]` wire shape (parent design §2.2 — the wire key is
   **`inputSchema`** camelCase, NOT `input_schema`):
   - `name` from the `tool:` object key
   - `description` from `ToolDefinition.description`
   - **`inputSchema`** from the Zod args object via `zod-to-json-schema`
   Add an adapter wire-shape test asserting the emitted `initialize` result uses the
   key `inputSchema` (matching yaca-mcp `ToolInfo` and the protocol codec).
2. Implement `tool/call` dispatch to the corresponding Compat
   `ToolDefinition.execute(args, context)`.
3. Build Compat `ToolContext` with `sessionID`, `messageID`, `agent`,
   `directory`, `worktree`, `abort`, `metadata`, and `ask`.
4. Implement `metadata()` by accumulating the latest title/metadata and merging it
   into the final yaca tool result.
5. For `context.ask()`, return a documented unsupported error until the yaca
   stdio protocol grows a reverse request or the SDK/server exposes permission
   answering. This is a compatibility gap, not a Child C protocol change.
6. Add tests for string result, object result, metadata accumulation, thrown tool
   error, and unsupported `ask()`.

Validation:

```sh
cd crates/yaca-plugin-compat/adapter
bun test tools
```

End-to-end C-AC2 validation after Rust integration:

```sh
# Start yaca with a fixture Compat plugin that returns { tool: { mytool: tool(...) } }.
# Prompt a model/fake provider to call the tool or drive the tool/call path through
# the plugin host test harness.
cargo test --workspace compat_tool_registration
```

Rollback point: return an empty `tools` array in `initialize` while keeping hook
translation active.

## 7. Phase 7 - Rust integration for `kind: compat`

Acceptance mapping: C-R1, C-R6, C-AC5.

Tasks:

1. Extend yaca plugin config resolution so `kind: compat` without an explicit
   command resolves to the bundled Bun command under `crates/yaca-plugin-compat/adapter`.
2. Inject `YACA_SERVER_URL`, `YACA_DIRECTORY`, `YACA_WORKTREE`, `YACA_PROJECT_ID`,
   `YACA_AGENT`, `YACA_MODEL`, and serialized adapter options into the child env.
3. If current yaca mode cannot provide `YACA_SERVER_URL`, fail the adapter child
   open with a clear message and do not enable Compat hooks for that session.
   This is a yaca-server/CLI follow-up gate, not a stdio protocol change.
4. Ensure disabled/missing Bun behavior does not fail zero-plugin or non-Compat
   runs.
5. Add Rust tests for config parsing, command resolution, missing Bun graceful
   disable, and env injection.

Validation:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Rollback point: disable the `kind: compat` resolver and require explicit
commands for manual adapter testing.

## 8. Phase 8 - Live QA with real Compat plugin

Acceptance mapping: C-AC1, C-AC3, C-AC4.

Live QA must run through the real surface, not just unit tests.

Setup:

1. Start `yaca serve` in tmux with adapter trace enabled and an ephemeral/in-memory
   DB.
2. Configure one real off-the-shelf Compat plugin using either:
   - a public `.opencode/plugins` plugin that implements `tool.execute.before`,
     for example the Trellis Compat injection plugin template, or
   - a public Compat event plugin such as `qeesung/tmux-scout` or CodeIsland.
3. Record the exact source URL and commit in the QA log before running.
4. Do not modify the off-the-shelf plugin except for dependency installation and
   configuration required by its own README. Adapter trace logs may be used as
   evidence that the plugin hook ran.

Commands:

```sh
tmux new-session -d -s yaca-compat-live \
  'YACA_COMPAT_TRACE=1 cargo run -p yaca-cli -- serve --bind 127.0.0.1:0 --db "" 2>&1 | tee /tmp/yaca-compat-live.log'

# In a second tmux pane/session, run a prompt or direct harness action that causes
# the target hook to fire. For tool.execute.before/after, use a prompt or fixture
# turn that requests a known tool call. For event, create a session and send a
# prompt so SessionCreated/Message/Text events stream through the adapter.
```

Required evidence:

- tmux capture showing yaca started and loaded the adapter.
- adapter log line showing the real plugin spec/id loaded.
- adapter trace line showing the specific hook key fired.
- yaca-side effect:
  - for `tool.execute.before`: mutated args or veto observed in yaca tool input or
    `ToolError`;
  - for `tool.execute.after`: rewritten result observed in yaca `ToolResult`;
  - for `event`: plugin hook receives a yaca-derived Compat event, proven by
    adapter trace and/or plugin side effect.
- one SDK callback success, minimum `client.session.create` or another supported
  method used by the selected plugin.

5. **[D5] yaca-side permission-preservation E2E (deterministic, required):** add a
   Rust/`yaca-cli` integration test `compat_permission_preservation` — load the
   adapter with a fixture Compat plugin whose `tool.execute.after` rewrites the
   result to a success string; drive a tool call that yaca's permission plane DENIES
   (original `Err(ToolError::Permission)`); assert the final yaca projection still
   shows a `ToolError` (the denial is NOT masked into a `ToolResult`), proving the
   host enforces parent design §2.6 end-to-end through the adapter. This is the
   yaca-side check that Phase 5's Bun unit test forward-references.

Rollback point: set `plugins.opencode.enabled: false`; the core plugin host and
native plugin path remain enabled.

## 9. Phase 9 - Documentation and compatibility matrix

Acceptance mapping: C-R5.

Tasks:

1. Document supported Compat package version and Bun version.
2. Document supported hooks and explicit no-ops/unsupported hooks.
3. Document SDK-supported methods and yaca-server follow-up gaps.
4. Document loader differences from Compat, especially npm cache location and
   TUI exclusion.
5. Add examples:
   - local `.opencode/plugins/example.ts` hook plugin
   - config npm plugin tuple with options
   - yaca `config.yaml` enabling `kind: compat`
6. Record live QA transcript path and source plugin commit.

Validation:

```sh
rg -n "compat|plugin|Bun|unsupported|1.17.9" README.md crates/yaca-plugin-compat .trellis/tasks/06-21-compat-adapter
```

Rollback point: documentation-only changes can be reverted independently of code.

## 10. Final Quality Gate

Run all applicable gates before marking Child C complete:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

```sh
cd crates/yaca-plugin-compat/adapter
bun install --frozen-lockfile
bun test
bun run typecheck
```

```sh
# Manual/live QA gate
tmux capture-pane -t yaca-compat-live -p > /tmp/yaca-compat-live.tmux.txt
rg -n "adapter loaded|hook fired|tool.execute|event|session.create" /tmp/yaca-compat-live.log /tmp/yaca-compat-live.tmux.txt
```

Completion criteria:

- C-AC1: real off-the-shelf Compat hook fires against yaca with tmux/log evidence.
- C-AC2: Compat `tool:` registration exposes a yaca tool and `tool/call`
  round-trips.
- C-AC3: in-place mutation and throw-to-block produce the correct yaca outcomes.
- C-AC4: at least one SDK callback succeeds against `yaca serve`.
- C-AC5: yaca builds/tests and starts without Bun; enabling the adapter without
  Bun degrades gracefully.

## 11. Overall Rollback Strategy

- Configuration rollback: set `plugins.opencode.enabled: false`.
- Runtime rollback: remove `kind: compat` command resolution while leaving
  native plugin support intact.
- Adapter rollback: remove `crates/yaca-plugin-compat/` package and its docs.
- Server follow-up rollback: revert any yaca-server compatibility endpoints; the
  stdio protocol remains unchanged.
- Lockfile rollback: restore the previous Bun lockfile if dependency upgrades
  cause plugin loader drift.

## 12. Risks To Monitor During Implementation

- Compat loader drift beyond v1.17.9: mitigate by version pin and loader tests
  copied from observed behavior.
- SDK gap creep: do not implement the entire Compat server; add only methods a
  target plugin or acceptance criterion uses.
- Event shape mismatch: prefer documented Compat event names for known yaca
  events and skip unknowns by default.
- Permission bypass by plugin-local filesystem SDK shims: document that plugins
  are trusted code; do not confuse plugin-local reads with yaca tool permission
  decisions.
- Bun absence in Rust CI: keep Bun checks opt-in and validate graceful degrade in
  Rust tests.

## 13. Plan-review gate

Cross-model plan-review runs at the **parent level** over the full plan set
(parent + A + B + C) before any `task.py start`. Do not start Child C
implementation until that gate passes, Children A and B are merged, and the user
approves activation.
