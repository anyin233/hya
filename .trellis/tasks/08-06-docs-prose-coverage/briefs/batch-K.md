# Batch K - plugin-protocol.md, compat-plugins.md

You are writing documentation for the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`. This is a Rust workspace for a
terminal-first coding agent with a Bun/OpenTUI frontend.

## Your batch

You own exactly 2 file(s). Do not create or edit any other file.

- `docs/plugin-protocol.md`  **(new file)**
- `docs/compat-plugins.md`  **(new file)**

You have **21 gap entries** and **0 stale claims** to resolve.

Both files are NEW. Paired because the compat adapter implements the protocol; you own both so the hook vocabulary stays consistent. Do not add the links to docs/README.md or docs/configuration.md that some entries mention -- the reconciliation pass and Batch A own those files.

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
   list does not count. 6 of your entries are status `thin`, meaning the
   feature IS already mentioned but unusably so -- those need real content, not a
   second mention.
7. Do not run `git commit`. Writing the files is enough.

## Work list

Each entry was produced by an agent that read the source. Treat it as a work list
and a starting point, not as verified truth -- rule 1 still applies.

### `docs/plugin-protocol.md`

**1. Plugin JSON-RPC wire protocol and method table (items 1,3,4,5,6,7,8): frame classification, initialize/shutdown/event/tool_call, hook/<name> namespace** — `thin` · severity high

- Source: `crates/hya-plugin/src/protocol.rs:7, crates/hya-plugin/src/messages.rs:14-20`
- Evidence: Searched docs/configuration.md, docs/architecture/runtime.md, docs/hya-pi-compat-comparison.md, docs/project-structure.md, AGENTS.md, docs/adr/0009-*.md. Every hit is a one-line description ('Stdio JSON-RPC plugin host', 'native stdio JSON-RPC system'). The only wire detail anywhere is docs/agent-bundle-authoring.md:88-90 and docs/architecture/runtime.md:142-147, which describe the SAME protocol but framed as the AgentBundle *sidecar* ABI, never as the contract a plugin author implements. No doc gives a frame shape, a method list, params, or a result. crates/hya-plugin/src/protocol.rs and messages.rs have file-level //! lines only; HookName, HookPosture, InitializeParams etc. carry zero rustdoc.
- Write: Full protocol reference. (a) Transport: newline-delimited JSON objects on the child's stdin/stdout, every frame has jsonrpc:"2.0". Frame::parse classifies: method+id = Request, method without id = Notification, result XOR error = Response; a frame with BOTH result and error is rejected. (b) Method table with params and result for each: `initialize` (request/reply; params {protocol_version:1, host:{name,version}}; reply is the plugin's full declaration), `shutdown` (request/reply, params {}), `event` (host->plugin NOTIFICATION, no id, no reply, params {envelope:<Envelope>}, sent only to plugins that registered the `event` hook), `tool/call` (request/reply, params {tool, session, call, input}, result {ok, output, time_ms?}), and `hook/<wire-name>` (request/reply; the method string is the literal prefix `hook/` plus the hook's wire name, e.g. `hook/tool.execute.before`). (c) A copy-pasteable minimal Python or Rust plugin that answers initialize and one hook, mirroring the style of the runnable MCP fixture already in docs/configuration.md:464.

**2. Plugin JSON-RPC error codes -32601/-32602/-32603 and app code 1 = VETO (item 2)** — `undocumented` · severity high

- Source: `crates/hya-plugin/src/protocol.rs:9`
- Evidence: grep -rniF '-32601', 'VETO', 'veto' across docs/**/*.md (excluding changes/ and superpowers/) and the five root docs returns ZERO hits. protocol.rs `codes` module has no rustdoc on the constants.
- Write: An 'Error codes' table: METHOD_NOT_FOUND=-32601 (host called a method the plugin does not implement), INVALID_PARAMS=-32602, INTERNAL_ERROR=-32603, and the app-defined VETO=1, which specifically means a guard hook refused the action. State that a plugin returning code 1 from a guard hook is the wire-level way to veto, and that any other error code from a Safe-posture hook is also converted into a veto by the host.

**3. The eleven hook names and their payloads/outcomes (items 9-17): event, command.execute.before, experimental.text.complete, message.user.before, chat.params, tool.execute.before, tool.execute.after, permission.ask, plus the dead goal.evaluate / loop.verifier / loop.planner** — `undocumented` · severity high

- Source: `crates/hya-plugin/src/messages.rs:24-40`
- Evidence: grep for 'permission.ask', 'chat.params', 'message.user.before', 'command.execute.before', 'experimental.text.complete', 'goal.evaluate', 'loop.verifier', 'loop.planner' across in-scope docs: ZERO hits each. 'tool.execute.before'/'tool.execute.after'/'event' appear only in docs/agent-bundle-authoring.md:78,84, docs/architecture/runtime.md:132 and the bundle examples, purely as the 3 legal BUNDLE hook ids, with no payload or outcome semantics. docs/configuration.md:635 says only 'command/message/text/chat hooks, event notifications, permission hooks, shell/tool hooks'. docs/architecture/runtime.md:205 says 'Hookable surfaces include events, command/user message admission, chat params/messages, text completion, permission asks, and tool before/after hooks' — a list of categories with no names.
- Write: One subsection per hook, each with: exact wire name, params JSON, allowed return outcomes, and its default posture. `event` — params {envelope}, notification, no reply, posture Open. `command.execute.before` — {session, command, arguments, text}; may return outcome `continue` with rewritten text; enrichment. `experimental.text.complete` — {session, message, part, text}; `continue` with rewritten text. `message.user.before` — {session, text}; `continue` with rewritten text. `chat.params` — {session, message, request:<WireCompletionRequest>}; `continue` with a fully rewritten request (model, system, messages, tools, temperature, max_output_tokens, reasoning, headers). `tool.execute.before` — {session, message, call, tool, input}; `continue` with rewritten input OR `veto` with a reason; GUARD, default posture Safe. `tool.execute.after` — {session, message, call, tool, input, result}; `continue` with a rewritten WireToolResult. `permission.ask` — {session?, action, resource}; returns allow_once | allow_always | reject{feedback?} | defer; GUARD, default posture Safe. Then an explicit warning subsection: `goal.evaluate`, `loop.verifier`, and `loop.planner` parse from plugin.toml and from the initialize reply and are registered, but crates/hya-plugin/src/dispatcher.rs has no arm for them — they are NEVER dispatched. Do not build a plugin on them.

**4. Hook posture model (items 18-21): safe/open values, per-hook defaults, force_safer tightening, and handshake > plugin.toml > default precedence** — `undocumented` · severity high

- Source: `crates/hya-plugin/src/messages.rs:89,98; crates/hya-plugin/src/host/connection.rs:29,65`
- Evidence: grep -rniF 'posture' across all in-scope docs: ZERO hits. crates/hya-plugin/src/host/connection.rs has zero doc comments; HookPosture in messages.rs has none either.
- Write: A 'Hook posture' section: posture is per-hook failure policy with two wire values, `safe` and `open` (serde snake_case). Safe = a hook call that fails or times out vetoes the action; Open = the failure is logged and the pipeline continues unchanged. Defaults: `permission.ask` and `tool.execute.before` default to Safe, every other hook defaults to Open. Posture can only be TIGHTENED: force_safer() ORs the declared posture with the hook default, so a plugin that declares `open` on a Safe-by-default hook still runs Safe. Resolution precedence per registered hook: posture in the initialize reply wins, else the manifest's posture_overrides entry, else HookName::default_posture().

**5. initialize reply schema (items 22-26): plugin{id,version,kind}, hooks[], tools[], workspaceAdapters[], and the rust|compat|other|opencode kind wire values** — `undocumented` · severity high

- Source: `crates/hya-plugin/src/messages.rs:138,142,149,161,168`
- Evidence: grep 'workspaceAdapters' and 'inputSchema' in in-scope docs: the only 'inputSchema' hit is docs/configuration.md:480 inside the unrelated MCP fixture. docs/configuration.md:640 says only 'The configured plugin ID must match the handshake ID'. docs/compat-parity.md:90 and :120 mention 'workspace adapter registration metadata' and the /experimental/workspace/adapter route but never the field name or shape.
- Write: Give the complete initialize reply JSON with every field. `plugin: {id, version, kind}` — id MUST equal the configured/manifest id or the host aborts with IdentityMismatch. `kind` wire values are snake_case `rust` (default) | `compat` | `other`, and `opencode` is accepted as a back-compat alias for `compat`. `hooks: [{name, posture?}]` — only hooks listed here are ever dispatched to this plugin. `tools: [{name, description, inputSchema}]` — note inputSchema is camelCase; each entry becomes a first-class hya Tool. `workspaceAdapters: [{type, name, description}]` — aggregated across all loaded plugins and served verbatim at GET /experimental/workspace/adapter. Show one complete example reply.

**6. Plugin tool declaration rules (items 27,28): non-object inputSchema silently drops the tool; a plugin tool call without a session errors** — `undocumented` · severity medium

- Source: `crates/hya-plugin/src/plugin_tool.rs:18,45`
- Evidence: crates/hya-plugin/src/plugin_tool.rs has ZERO doc comments. No in-scope doc mentions input-schema validation for plugin tools; docs/configuration.md:642 only says plugin tools 'are published through the same immutable runtime registry as builtins and MCP tools'.
- Write: A 'Plugin tools' gotchas subsection: PluginTool::try_new SILENTLY DROPS any declared tool whose inputSchema.type is not exactly the string "object" — the tool never reaches the model and no error is raised, so an author sees their tool simply missing. Also: a plugin tool invoked without ToolCtx.session fails; the host mints a fresh ToolCallId for every call and maps a `tool/call` reply of ok:false into a ToolError carrying the returned output.

**7. Transport limits for configured plugins (items 29,30,31): 1 MiB MAX_LINE_BYTES, DEFAULT_CALL_TIMEOUT 30s / INITIALIZE_TIMEOUT 5s / SHUTDOWN_TIMEOUT 1s, 64 KiB bounded stderr tail** — `thin` · severity medium

- Source: `crates/hya-plugin/src/codec.rs:9; crates/hya-plugin/src/client.rs:26,29`
- Evidence: docs/agent-bundle-authoring.md:90 documents '1 MiB frame cap', '5 second' init and '30 second' request limits — but only for the Bundle sidecar. No in-scope doc states these apply to ordinary configured plugins. The 1s SHUTDOWN_TIMEOUT, the OversizedLine teardown, and the 64 KiB STDERR_TAIL_BYTES are absent everywhere ('MAX_LINE_BYTES', 'STDERR_TAIL' return zero hits). docs/configuration.md:627 describes timeout_ms only as 'Optional request timeout' with no stated default.
- Write: A 'Limits and timeouts' table applying to every plugin, not just Bundle sidecars: single NDJSON frame cap MAX_LINE_BYTES = 1 MiB (exceeding it raises PluginError::OversizedLine and tears down the transport, killing the connection); DEFAULT_CALL_TIMEOUT = 30s per request, overridable per plugin with the `timeout_ms` config key; INITIALIZE_TIMEOUT = 5s for the handshake; SHUTDOWN_TIMEOUT = 1s, after which the host SIGKILLs and reaps the child. Also: for bundle-spawned children the host keeps the last 64 KiB of stderr (STDERR_TAIL_BYTES) readable via ChildGuard::stderr_tail(), while configured plugins simply INHERIT the host's stderr — so a configured plugin's stderr goes straight to the user's terminal. Update the docs/configuration.md timeout_ms row to say 'Per-call request timeout; defaults to 30000 ms.'

**8. The two spawn modes (items 32,33): PluginClient::spawn for configured plugins vs PluginClient::spawn_bundle for sidecars** — `thin` · severity medium

- Source: `crates/hya-plugin/src/client.rs:321,334`
- Evidence: docs/configuration.md:628 says only that `env` holds 'Environment variables passed to the plugin process as configured'. Nothing states whether env replaces or overlays the host environment; nothing describes kill_on_drop or stderr inheritance. The bundle mode's env_clear() and activation-dir cwd appear nowhere in scope ('env_clear' returns zero hits); docs/architecture/runtime.md:125 only says 'hya-plugin owns the child, stdio, bounded stderr, shutdown, termination, and reap'.
- Write: A short 'How the child is spawned' section contrasting the two modes. Configured-plugin mode (PluginClient::spawn): stdin/stdout piped, stderr INHERITED from the host, kill_on_drop set, and the config `env` map OVERLAID on top of the host environment (it does not replace it). Bundle sidecar mode (PluginClient::spawn_bundle): env_clear() so the child gets NO inherited environment, cwd set to the activation directory, stderr piped into a bounded tail, and strict transport enabled so any timeout permanently taints the connection closed.

**9. Supervision state machine (items 38,40): restart budget MAX_RESTARTS=3 per RESTART_WINDOW=60s then permanent Disabled; the PluginStatus enum Alive|Dead|DeclarationDrift|Disabled** — `thin` · severity medium

- Source: `crates/hya-plugin/src/host.rs:29,32`
- Evidence: docs/hya-pi-compat-comparison.md:341 says 'crashes mark a plugin dead, later calls can respawn it, and repeated failures disable it' — no numbers, no window, no state names. grep 'PluginStatus', 'DeclarationDrift', 'MAX_RESTARTS' in in-scope docs: zero hits. crates/hya-plugin/src/host.rs has 5 doc-comment lines total and none on the constants or the enum.
- Write: A 'Supervision and restart budget' section with the concrete numbers: MAX_RESTARTS = 3 within a RESTART_WINDOW = 60s sliding window. Exceeding it sets the disabled flag PERMANENTLY for the rest of the host process lifetime — every later call returns PluginError::Disabled and there is no automatic re-enable; the user must restart hya. Document PluginStatus as the observable state, queryable via PluginHost::plugin_status(id), with all four values and what each means: Alive (live client), Dead (client cleared, will lazily respawn on next call), DeclarationDrift (latched, never used again), Disabled (restart budget exhausted).

**10. Event fan-out backpressure: 256-slot per-plugin channel, drop-on-overflow with a warning every 256 drops (item 41)** — `undocumented` · severity medium

- Source: `crates/hya-plugin/src/host.rs:27`
- Evidence: No in-scope doc mentions event delivery capacity, dropping, or backpressure for plugins. docs/configuration.md:635 lists 'event notifications' as a supported feature with no delivery guarantee.
- Write: State the delivery guarantee explicitly, because it is lossy: each event-subscribing plugin gets its own 256-slot mpsc channel. If a plugin is slower than the engine, envelopes are DROPPED (not queued, not retried), and a warning is logged once every EVENT_DROP_WARN_EVERY = 256 drops. Tell plugin authors the `event` hook is best-effort telemetry and must never be used as the sole source of truth for state.

**11. Hook chain semantics (items 42,43): load-order-preserving parallel connect, enrichment folding vs guard short-circuit, and the 'guard failed safe' veto message** — `undocumented` · severity high

- Source: `crates/hya-plugin/src/host.rs:298; crates/hya-plugin/src/dispatcher.rs:29`
- Evidence: No in-scope doc explains what happens when two plugins register the same hook. docs/configuration.md's Plugins section and docs/architecture/runtime.md:203-209 both stop at 'hook dispatch' with no ordering or folding rule. dispatcher.rs has 4 doc-comment lines and none describe folding.
- Write: A 'Multiple plugins on one hook' section: connect_all_observed handshakes every plugin CONCURRENTLY but re-sorts the results by declared index, so hook chains always fold in configured load order (config entries first, then manifests) regardless of handshake timing. Enrichment hooks (command.execute.before, message.user.before, experimental.text.complete, chat.params, tool.execute.after) FOLD: plugin N's output becomes plugin N+1's input, and a failing plugin is skipped with its input passed through. Guard hooks short-circuit: tool.execute.before returns on the FIRST veto and never consults later plugins, and a Safe-posture failure is converted into a veto whose reason is the literal string 'guard failed safe: <plugin> (<error>)' — document that string so users can recognize it in the UI.

### `docs/compat-plugins.md`

**1. Compat adapter CLI: --help/-h, --version, repeatable --bundle-extension <absolute-path> (item 57)** — `undocumented` · severity medium

- Source: `crates/hya-plugin-compat/adapter/src/main.ts:20`
- Evidence: grep 'bundle-extension' across all in-scope docs: ZERO hits. crates/hya-plugin-compat/README.md (out of the audited scope) also omits it. docs/configuration.md:630 only says hya 'uses the bundled Bun adapter from crates/hya-plugin-compat/adapter'.
- Write: Document the adapter's startup argument parser: `--help`/`-h` prints usage, `--version` prints the version, and `--bundle-extension <absolute-path>` is REPEATABLE, REQUIRES an absolute path (relative paths are rejected), and may be preceded by a bare `--` separator. Explain that this is how AgentBundle sidecars hand the adapter their materialized JS entrypoints, and that hya-app appends `-- --bundle-extension <abs path>` once per selected entrypoint when launching the sidecar.

**2. Compat adapter method table and the OpenCode-to-hya hook-name translation table (items 58,60)** — `thin` · severity high

- Source: `crates/hya-plugin-compat/adapter/src/runtime.ts:80; crates/hya-plugin-compat/adapter/src/registration.ts:7`
- Evidence: docs/compat-parity.md:90 lists coverage categories ('server hooks, plugin tools, chat params/messages transforms, command/message/text hooks, events, shell env, permissions...') but no mapping. The out-of-scope crates/hya-plugin-compat/README.md has the same category list. No doc anywhere tells an OpenCode plugin author which of their hooks survive translation.
- Write: Two tables. (1) Methods the adapter answers: initialize, shutdown, tool/call, hook/message.user.before, hook/chat.params, hook/command.execute.before, hook/experimental.text.complete, hook/permission.ask, hook/tool.execute.before, hook/tool.execute.after, and the id-less `event` notification; anything else returns METHOD_NOT_FOUND. (2) The OpenCode-hook to hya-hook translation, which is many-to-one and therefore lossy: event->event, command.execute.before->command.execute.before, experimental.text.complete->experimental.text.complete, chat.message->message.user.before, and FIVE OpenCode hooks all collapse onto chat.params (chat.params, chat.headers, experimental.chat.messages.transform, experimental.chat.system.transform, tool.definition), permission.ask->permission.ask, BOTH shell.env and tool.execute.before->tool.execute.before, tool.execute.after->tool.execute.after. Duplicates after mapping are collapsed to one registration.

**3. Compat dispose hooks run on shutdown in reverse registration order (item 59)** — `undocumented` · severity medium

- Source: `crates/hya-plugin-compat/adapter/src/runtime.ts:123`
- Evidence: grep 'dispose' in scope returns only unrelated hits (the /instance/dispose and /global/dispose HTTP routes in docs/compat-parity.md). No in-scope doc mentions plugin dispose functions. The out-of-scope crate README mentions 'dispose-hook execution before process termination' with no ordering or error rule.
- Write: Document the shutdown sequence: on receiving `shutdown`, and BEFORE replying and exiting, the adapter awaits each loaded plugin's `dispose` function in REVERSE registration order (last loaded disposes first). A dispose that throws is logged to stderr and does NOT abort the remaining dispose calls or the shutdown reply. Note the 1s host-side SHUTDOWN_TIMEOUT means a slow dispose gets the process SIGKILLed.

**4. Compat plugin discovery order and config directories (items 61,62)** — `undocumented` · severity high

- Source: `crates/hya-plugin-compat/adapter/src/loader/discovery.ts:83; crates/hya-plugin-compat/adapter/src/loader/config_dirs.ts:5`
- Evidence: docs/configuration.md:50 documents a discovery order, but for the entirely different `hya --import compat` config-import command, not for the running adapter. No in-scope doc describes which files the adapter itself reads at startup or in what precedence.
- Write: Give the exact ordered discovery list the adapter walks: (1) the global config dir — config.json, opencode.json, opencode.jsonc; (2) $COMPAT_CONFIG; (3) every ancestor opencode.json/opencode.jsonc walking from the worktree boundary down to cwd; (4) each of the .opencode / ~/.opencode / custom dirs — their opencode.json/jsonc plus EVERY .js/.ts file under their plugin/ and plugins/ subdirectories; (5) $COMPAT_CONFIG_CONTENT. State the precedence rule: later duplicates WIN, via reverse dedup on package identity. Also list the config directories: global is $XDG_CONFIG_HOME/compat or ~/.config/compat; project dirs are <ancestor>/.opencode for every directory from cwd up to the worktree; home is ~/.opencode; plus the optional $COMPAT_CONFIG_DIR.

**5. Compat plugin input object handed to each plugin factory (item 66): {client, directory, worktree, project, serverUrl, $, experimental_workspace}** — `undocumented` · severity medium

- Source: `crates/hya-plugin-compat/adapter/src/initialize.ts:185`
- Evidence: grep 'experimental_workspace' in scope: zero hits. No in-scope doc describes what an OpenCode plugin factory receives when the adapter calls it.
- Write: Document the object each plugin factory is called with: `client` (the SDK client shim), `directory`, `worktree`, `project`, `serverUrl`, `$` (the Bun shell), and `experimental_workspace: { register(type, adapter) }`. Explain that adapters registered through experimental_workspace.register whose adapter carries a string `name` and `description` are collected into the `workspaceAdapters` array of the initialize reply and thereby surface at GET /experimental/workspace/adapter; adapters missing those string fields are dropped.

**6. Compat plugin module shapes and path/package resolution (items 69,70,71,72)** — `undocumented` · severity medium

- Source: `crates/hya-plugin-compat/adapter/src/loader/shape.ts:56,90; crates/hya-plugin-compat/adapter/src/loader/package.ts:14,30`
- Evidence: No in-scope doc describes what a compat plugin module must export or how a spec string is resolved. grep 'node_modules' in scope hits only docs about the hya-tui-ts install; grep 'npm' hits only unrelated lines.
- Write: A 'Plugin module shapes and resolution' section. Accepted shapes: (a) v1 — a default export with id/server/tui keys, where `server` MUST be a function, a tui-only module loads as zero hooks, and a module with BOTH server and tui is an error; (b) legacy — a module where every export is either a function or a {server} record. Local resolution: file://, ./relative, and absolute specs resolve against the CONFIG FILE's directory (not cwd); a directory resolves to itself if it contains package.json, otherwise to index.ts/tsx/js/mjs/cjs, otherwise PluginPathResolutionError. npm resolution: walks node_modules upward for the package, prefers exports["./server"] then main, and raises NpmPluginPackageError if the resolved entry escapes the package directory. Finally, warn that any spec containing `compat-openai-codex-auth` or `compat-copilot-auth` is SILENTLY dropped before loading.

**7. Compat hook behavior quirks (items 73,74,75): shell.env merges only into the `shell` tool, a throwing tool.execute.before becomes a veto, and the permission.ask status mapping** — `thin` · severity medium

- Source: `crates/hya-plugin-compat/adapter/src/hooks.ts:91,134; crates/hya-plugin-compat/adapter/src/permission_hooks.ts:52`
- Evidence: docs/compat-parity.md:90 lists 'shell env' and 'permissions' among covered hook categories with no semantics. grep 'shell.env' in scope: zero hits. Nothing documents the allow/deny/ask -> allow_once/reject/defer mapping or the never-produced allow_always.
- Write: Three behavior notes an OpenCode plugin author will otherwise hit blind. (1) shell.env hooks run ONLY when the intercepted tool is literally named "shell"; the collected env map is merged OVER input.env, and any Error thrown by a shell.env hook is swallowed. (2) A compat tool.execute.before hook that THROWS is translated on the hya wire into {outcome:"veto", reason:<error message>} — throwing blocks the tool rather than being ignored. (3) permission.ask status mapping: compat `allow` -> allow_once, `deny` -> reject with NO feedback field, `ask` -> defer. The compat path can never produce allow_always, so 'always allow' from a compat plugin is impossible.

**8. Compat tool registry and result normalization (item 76)** — `undocumented` · severity medium

- Source: `crates/hya-plugin-compat/adapter/src/tool.ts:72`
- Evidence: docs/compat-parity.md:90 says the adapter covers 'plugin tools'. No in-scope doc describes how hook.tool.<name> definitions become hya tools, how the schema is derived, or what results are normalized to. grep 'z.toJSONSchema' in scope: zero hits.
- Write: Document the tool path: the adapter collects `hook.tool.<name>` definitions and the FIRST registration of a name wins (later duplicates are ignored). inputSchema is derived from Zod args via z.toJSONSchema, or taken directly from a raw JSON-schema record. Results are normalized to {title, output, metadata, attachments?}. Note that ctx.ask() is NOT supported and throws UnsupportedToolAskError — compat tools cannot prompt the user.

**9. Compat event converter coverage (item 77)** — `undocumented` · severity medium

- Source: `crates/hya-plugin-compat/adapter/src/event_converters.ts:22`
- Evidence: docs/compat-parity.md:90 says the adapter covers 'events'. No in-scope doc lists which hya envelopes actually reach a compat plugin, nor that unlisted ones are dropped.
- Write: List the translated envelope set exactly, and state that ANYTHING NOT LISTED IS SILENTLY DROPPED before reaching the plugin: session_created, session_titled, command_executed, message_started, message_finished, text_start, text_delta, text_replace, text_end, reasoning_start, reasoning_delta, reasoning_end, tool_input_start, tool_input_delta, tool_call_requested, tool_result, tool_error, and error.

**10. Compat SDK version pins COMPAT_PLUGIN_VERSION / COMPAT_SDK_VERSION = 1.17.9 (item 78)** — `undocumented` · severity low

- Source: `crates/hya-plugin-compat/src/lib.rs:1`
- Evidence: No in-scope doc names the pinned @opencode-ai/plugin or @opencode-ai/sdk version. The versions appear only in crates/hya-plugin-compat/README.md, which is outside the audited doc scope (though docs/README.md:95 links to it as the compat-adapter entrypoint).
- Write: State the targeted package versions the adapter implements — @opencode-ai/plugin@1.17.9 and @opencode-ai/sdk@1.17.9 — note that they are pinned in Rust at crates/hya-plugin-compat/src/lib.rs, and tell authors to check this pin before assuming a newer OpenCode SDK feature is available.

## When you are done

Report, in this order:

1. Each file you wrote and its approximate line count.
2. How many of the 21 gap entries you resolved. If any remain, name them.
3. Any entry where the source CONTRADICTED the work list, with the `file:line`
   you checked and what the code actually does.
4. Any claim you could NOT confirm from source and therefore omitted.
5. Any code defect you noticed. Do not fix it; just name it.
