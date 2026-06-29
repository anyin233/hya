# Design - OpenCode-compat Bun adapter (Child C)

> Child C delivers the OpenCode compatibility layer only. The hya plugin
> protocol and host semantics are locked by the parent design; this adapter is
> one `kind: opencode` child process that speaks that protocol over stdio and
> re-emits OpenCode server hooks inside Bun.

## 0. Authoritative Sources And Constraints

- Parent requirement R8 defines the OpenCode adapter as a bundled Bun child that
  loads JS/TS plugins, maps OpenCode `Hooks` to the hya protocol, points an SDK
  `client` at `hya serve`, and runs OpenCode's plugin discovery and npm install
  path. Source: parent PRD requirement **R8**.
- The OpenCode contract verified for the parent is `@opencode-ai/plugin` v1.17.9
  and `@opencode-ai/sdk` v1.17.9. Source: parent PRD **"OpenCode plugin contract"**
  section, [plugin package](/tmp/opencode-src/packages/plugin/package.json:3),
  [SDK package](/tmp/opencode-src/packages/sdk/js/package.json:3).
- The adapter must not change the hya protocol. The locked stdio JSON-RPC
  methods are `initialize`, `hook/<name>`, `event`, `tool/call`, and `shutdown`.
  Source: parent design **§2.1 (method namespace)**.
- `initialize` returns the plugin identity, including `kind`, declared hooks,
  and declared tools. For Child C the returned kind is `opencode`. Source: parent
  design **§2.2 (handshake; tool-schema wire key is `inputSchema`)**.
- Blocking hook replies use the locked outcome model: `continue`, `veto`, and
  answer-specific `defer`. Source: parent design **§2.3 (hook request/reply)**.
- Plugin-registered tools use the locked `tool/call` request/reply shape. Source:
  parent design **§2.5 (tool-call dispatch)**.
- `tool.execute.after` must not mask a permission denial. Source: parent design
  **§2.6 (`tool.execute.after` safety)**.
- hya's current HTTP API is small: `POST /sessions`, `POST /sessions/:id/prompt`,
  `GET /sessions/:id/events`, and `GET /sessions/:id/stream`. Source:
  [README](/chivier-disk/yanweiye/Projects/hya/README.md:68),
  [server routes](/chivier-disk/yanweiye/Projects/hya/crates/hya-server/src/lib.rs:30).
- Current server DTOs are `CreateSessionRequest`, `PromptRequest`, and
  `EventsQuery`; they are intentionally not OpenCode SDK DTOs. Source:
  [api.rs](/chivier-disk/yanweiye/Projects/hya/crates/hya-proto/src/api.rs:6).

## 1. Adapter Architecture

### 1.1 In-repo layout

Place the adapter in a new workspace member with a valid Rust manifest and a
pure Bun payload:

```text
crates/hya-plugin-opencode/
  Cargo.toml                 # tiny packaging crate; no Bun dependency
  README.md                  # supported OpenCode subset and runtime notes
  adapter/
    package.json             # Bun package, pinned deps
    bun.lock                 # committed lockfile
    tsconfig.json
    src/main.ts              # stdio JSON-RPC entry point
    src/protocol.ts          # hya JSON-RPC frame codec, typed messages
    src/loader.ts            # OpenCode config/dir/npm loader clone
    src/init-context.ts      # PluginInput construction
    src/hooks.ts             # hya hook <-> OpenCode hook translation
    src/sdk-client.ts        # OpenCode-SDK-shaped hya client shim
    src/tools.ts             # OpenCode tool registration and tool/call bridge
    test/*.test.ts           # Bun unit/integration tests
```

Rationale: the workspace root uses `members = ["crates/*", "xtask"]`, so a new
directory under `crates/` should be a valid Cargo package to avoid workspace glob
surprises. Source: [Cargo.toml](/chivier-disk/yanweiye/Projects/hya/Cargo.toml:3).
The Rust package only locates/docs the adapter assets; hya core, hya server,
and hya CLI still build and test without Bun.

### 1.2 How hya spawns it

Child A's host already knows how to spawn child processes and call `initialize`.
Child C adds a `kind: opencode` config resolution path, not a protocol path:

```yaml
plugins:
  opencode:
    kind: opencode
    enabled: true
    # Optional override. If omitted, hya resolves the bundled command.
    command:
      - bun
      - run
      - --cwd
      - ${HYA_BUNDLED_OPENCODE_ADAPTER}/adapter
      - src/main.ts
    timeout_ms: 1000
    env:
      HYA_OPENCODE_TRACE: "0"
    options:
      plugin:
        - ./my-opencode-plugin.ts
        - ["some-opencode-plugin@1.2.3", { enabled: true }]
```

The host injects adapter-only process context through argv/env before the locked
`initialize` frame:

- `HYA_SERVER_URL`: base URL for `hya serve`, for example `http://127.0.0.1:8080`.
- `HYA_DIRECTORY`: current project directory.
- `HYA_WORKTREE`: worktree root.
- `HYA_PROJECT_ID`: stable hya project/workspace id if available, else a hash of
  `HYA_WORKTREE`.
- `HYA_AGENT`, `HYA_MODEL`: defaults used when mapping OpenCode SDK
  `session.create` to hya's `POST /sessions` DTO.
- `HYA_OPENCODE_OPTIONS_JSON`: serialized `options` from the hya plugin config.

This does not alter the parent protocol because it uses ordinary process launch
configuration, like other child plugin commands.

On startup the Bun process:

1. Parses env/options and starts the stdio JSON-RPC reader/writer.
2. Constructs the OpenCode init context.
3. Discovers and loads OpenCode plugins.
4. Collects their `Hooks` objects and `tool:` definitions.
5. Replies to `initialize` with `plugin.kind = "opencode"`, one declared hya
   hook for every supported OpenCode hook present, and one hya tool declaration
   for every OpenCode `tool:` definition.

### 1.3 Loading OpenCode plugins

The adapter mirrors the real OpenCode server plugin loader closely enough for
current plugins:

- Plugin config accepts OpenCode's `plugin?: Array<string | [string, options]>`
  shape. Source: [@opencode-ai/plugin Config](/tmp/opencode-src/packages/plugin/src/index.ts:70).
- Local discovery scans `{plugin,plugins}/*.{ts,js}` under every OpenCode config
  directory. Source: [ConfigPlugin.load](/tmp/opencode-src/packages/opencode/src/config/plugin.ts:18).
- Config directories include `~/.config/opencode` (`Global.Path.config`) and
  `.opencode` directories discovered from the project/worktree. Source:
  [ConfigPaths.directories](/tmp/opencode-src/packages/opencode/src/config/paths.ts:23).
- The adapter therefore scans at least:
  - `${HYA_DIRECTORY}/.opencode/plugin/*.{ts,js}`
  - `${HYA_DIRECTORY}/.opencode/plugins/*.{ts,js}`
  - `${XDG_CONFIG_HOME:-~/.config}/opencode/plugin/*.{ts,js}`
  - `${XDG_CONFIG_HOME:-~/.config}/opencode/plugins/*.{ts,js}`
- Path-like plugin specs are resolved relative to the config file or hya adapter
  option that declared them, matching OpenCode's relative-path normalization.
  Source: [resolvePluginSpec](/tmp/opencode-src/packages/opencode/src/config/plugin.ts:40).
- npm specifiers are installed with Bun into an adapter-managed cache/project
  directory before import. OpenCode's source installs plugin dependencies at
  startup and resolves npm specs through an npm service; the adapter should
  implement this as `bun add <pkg>@<version>` or `bun install` in a dedicated
  adapter cache, never in the user's repo unless the plugin is already local.
  Source: [config dependency install](/tmp/opencode-src/packages/opencode/src/config/config.ts:437),
  [resolvePluginTarget](/tmp/opencode-src/packages/opencode/src/plugin/shared.ts:207).
- Server entrypoint resolution prefers package `exports["./server"]`, then package
  `main` for server plugins, and rejects server exports that resolve outside the
  plugin directory. Source: [resolvePackageEntrypoint](/tmp/opencode-src/packages/opencode/src/plugin/shared.ts:103),
  [resolvePackageFile](/tmp/opencode-src/packages/opencode/src/plugin/shared.ts:89).
- Loader failures are isolated: one missing, invalid, or throwing plugin is logged
  and skipped; later plugins still load. Source: [loader tests](/tmp/opencode-src/packages/opencode/test/plugin/loader-shared.test.ts:663).
- Hook execution order is deterministic: plugins initialize and hooks run in load
  order. Source: [loader order test](/tmp/opencode-src/packages/opencode/test/plugin/loader-shared.test.ts:849),
  [Plugin.trigger](/tmp/opencode-src/packages/opencode/src/plugin/index.ts:280).

Supported module shapes:

1. New v1 module shape: default export object with `id` and `server` function,
   where `server` is the OpenCode plugin initializer. File plugins using this
   shape must export `id`, matching OpenCode's `resolvePluginId` behavior. Source:
   [readV1Plugin](/tmp/opencode-src/packages/opencode/src/plugin/shared.ts:272),
   [resolvePluginId](/tmp/opencode-src/packages/opencode/src/plugin/shared.ts:306).
2. Legacy shape: bare plugin function exports. OpenCode loops over module exports,
   dedupes identical function objects, treats functions as server plugins, and
   also accepts objects with a `server` function. Source: [getLegacyPlugins](/tmp/opencode-src/packages/opencode/src/plugin/index.ts:84).

Unsupported loader surfaces in Child C: OpenCode TUI plugins, `oc-themes`, and
plugins that default export both `server` and `tui`. OpenCode also rejects mixed
server/tui defaults. Source: [mixed export check](/tmp/opencode-src/packages/opencode/src/plugin/shared.ts:293).

## 2. OpenCode Init Context

The exact OpenCode server plugin init signature in `@opencode-ai/plugin` v1.17.9
is:

```ts
export type PluginInput = {
  client: ReturnType<typeof createOpencodeClient>
  project: Project
  directory: string
  worktree: string
  experimental_workspace: {
    register(type: string, adapter: WorkspaceAdapter): void
  }
  serverUrl: URL
  $: BunShell
}

export type PluginOptions = Record<string, unknown>

export type Plugin = (input: PluginInput, options?: PluginOptions) => Promise<Hooks>
```

Source: [PluginInput](/tmp/opencode-src/packages/plugin/src/index.ts:56),
[Plugin](/tmp/opencode-src/packages/plugin/src/index.ts:74).

OpenCode constructs `client` with `createOpencodeClient({ baseUrl, directory,
headers, fetch? })`, passes `project`, `directory`, `worktree`, `serverUrl`, and
uses `Bun.$` for `$`. Source: [OpenCode init context](/tmp/opencode-src/packages/opencode/src/plugin/index.ts:139).

The adapter synthesizes:

- `client`: an OpenCode-SDK-shaped object backed by `hya serve` and local shims.
  It should expose the same property groups real plugins commonly touch
  (`session`, `event`, `global`, `app`, `project`, `path`, `file`, `find`,
  `config`, `provider`, `tool`) even when some methods return explicit
  unsupported errors.
- `$`: `Bun.$`, with the exact Bun shell interface expected by plugin typings.
  Source: [BunShell](/tmp/opencode-src/packages/plugin/src/shell.ts:10).
- `project`: `{ id, worktree, time, vcs? }`, using `HYA_PROJECT_ID`,
  `HYA_WORKTREE`, and a best-effort git check.
- `directory`: `HYA_DIRECTORY`.
- `worktree`: `HYA_WORKTREE`.
- `serverUrl`: `new URL(HYA_SERVER_URL)`.
- `experimental_workspace.register`: accepted but documented as a no-op in Child
  C because hya has no matching workspace adapter surface in the locked protocol.

The adapter should not import OpenCode internals at runtime. It may depend on
`@opencode-ai/plugin@1.17.9` for types/tool helpers and `zod`; it implements its
own SDK-shaped client because hya's HTTP paths are not OpenCode's generated SDK
paths.

## 3. Hook Translation Table

OpenCode `Hooks` are ordinary async functions. Most hooks have the shape
`(input, output) => Promise<void>` and mutate `output` in place. Source:
[@opencode-ai/plugin Hooks](/tmp/opencode-src/packages/plugin/src/index.ts:222).
The adapter runs all OpenCode hooks for a given hya hook sequentially in
OpenCode load order, feeding the mutated output from one plugin into the next.

| hya v1 hook/protocol surface | OpenCode hook key | Adapter input | Adapter output / hya outcome | Notes |
|---|---|---|---|---|
| `event` notification | `event` | `{ event }`, where `event` is an OpenCode-style event union | No hya reply | Convert hya `Envelope` to closest OpenCode event. Unknown hya events may be emitted as `hya.<type>` only when trace compatibility mode is enabled; default off for off-the-shelf plugins. |
| Session/message lifecycle observation | `event` | Same as above | No hya reply | `SessionCreated` -> `session.created`; `Error` -> `session.error`; `TextDelta` -> `message.part.delta`; `ToolResult`/`ToolError` -> `message.part.updated` with a tool part. OpenCode event union source: [Event union](/tmp/opencode-src/packages/sdk/js/src/gen/types.gen.ts:704). |
| `message.user.before` | `chat.message` | OpenCode input `{ sessionID, agent?, model?, messageID? }`, output `{ message, parts }` synthesized from the hya prompt text | `continue { text }` | Flatten mutated text parts back to the hya prompt. File/agent/subtask parts are unsupported in v1 and ignored with a warning. Source: [chat.message signature](/tmp/opencode-src/packages/plugin/src/index.ts:234). |
| `chat.params` | `experimental.chat.system.transform` | `{ sessionID?, model }`, output `{ system: string[] }` | `continue { request.system }` | Runs before `chat.params` so system mutation is reflected in the hya `CompletionRequest.system`. Source: [system transform signature](/tmp/opencode-src/packages/plugin/src/index.ts:291). |
| `chat.params` | `chat.params` | `{ sessionID, agent, model, provider, message }`, output `{ temperature, topP, topK, maxOutputTokens, options }` | `continue { request }` with supported fields merged | hya request supports `model`, `system`, `messages`, `tools`, `temperature`, `max_output_tokens`, and `reasoning`; OpenCode `topP`, `topK`, and provider `options` are captured in adapter metadata but cannot be applied until hya providers grow those fields. Source: [CompletionRequest](/chivier-disk/yanweiye/Projects/hya/crates/hya-provider/src/lib.rs:110), [chat.params signature](/tmp/opencode-src/packages/plugin/src/index.ts:247). |
| `tool.execute.before` | `tool.execute.before` | `{ tool, sessionID, callID }`, output `{ args }` seeded from hya tool input | `continue { input: output.args }` or `veto { reason }` | If the OpenCode hook returns normally, mutated `output.args` becomes the next hya tool input. If it throws, adapter returns hya `veto`, matching OpenCode's throw-to-block behavior. Source: [tool before signature](/tmp/opencode-src/packages/plugin/src/index.ts:266), [OpenCode trigger site](/tmp/opencode-src/packages/opencode/src/session/tools.ts:87). |
| `tool.execute.after` | `tool.execute.after` | `{ tool, sessionID, callID, args }`, output `{ title, output, metadata }` seeded from hya tool result/error | `continue { result }` | Convert hya JSON result to OpenCode's string-centric `output`. If the plugin mutates `output.output`, return a hya JSON result preserving `title` and `metadata`; do not allow a plugin to synthesize hya permission errors NOR rewrite an original permission `Err` into `Ok`, matching parent safety rule §2.6. Source: [tool after signature](/tmp/opencode-src/packages/plugin/src/index.ts:274), [parent safety rule §2.6](/chivier-disk/yanweiye/Projects/hya/.trellis/tasks/06-21-plugin-system/design.md:204). |
| `permission.ask` | `permission.ask` | OpenCode `Permission` object plus output `{ status }` | `allow` -> `allow_once`; `deny` -> `reject`; `ask` -> `defer` | `ask` means fall back to hya's normal permission flow. Source: [permission.ask signature](/tmp/opencode-src/packages/plugin/src/index.ts:261), [Permission type](/tmp/opencode-src/packages/sdk/js/src/gen/types.gen.ts:423). |
| Plugin tool registration in `initialize.tools` | `tool` object on returned `Hooks` | OpenCode `ToolDefinition` from `tool({ description, args, execute })` | hya `tools` declarations in `initialize` and `tool/call` replies | Convert Zod args to JSON schema for hya `ToolSchema`; invoke `execute(args, context)` on `tool/call`. Source: [OpenCode tool helper](/tmp/opencode-src/packages/plugin/src/tool.ts:45), [hya ToolSchema](/chivier-disk/yanweiye/Projects/hya/crates/hya-proto/src/model.rs:46). |
| `goal.evaluate` | none | n/a | `defer` | No OpenCode equivalent; leave to hya. |
| `loop.verifier` / `loop.planner` | none | n/a | `defer` | No OpenCode equivalent; leave to hya. |

Unsupported OpenCode hook keys in Child C are explicit no-ops with startup
warnings if observed in a plugin: `config`, `auth`, `provider`, `chat.headers`,
`command.execute.before`, `shell.env`, `tool.definition`,
`experimental.chat.messages.transform`, `experimental.session.compacting`,
`experimental.compaction.autocontinue`, `experimental.provider.small_model`, and
`experimental.text.complete`. They are outside Child C's PRD subset unless they
map through a hya v1 hook above. Source: [Child C scope](/chivier-disk/yanweiye/Projects/hya/.trellis/tasks/06-21-opencode-adapter/prd.md:38).

## 4. Outcome Semantics At The Adapter Boundary

For each blocking hya hook request:

1. Decode the hya request into the OpenCode `input` and mutable `output` object.
2. For each loaded OpenCode hook in order:
   - await the hook;
   - keep the same mutable `output` object for the next plugin;
   - if it throws, stop the chain.
3. Encode the final state as a hya outcome:
   - normal return -> `continue` with the mutated field;
   - `tool.execute.before` throw -> `veto` with the error message;
   - `permission.ask` `status: "ask"` -> `defer`;
   - unsupported/no hook -> `continue` with original payload or `defer` for
     answer hooks.

For non-guard hooks, thrown errors are returned to the hya host as hook errors
and the parent failure posture applies. The adapter should also log them to
stderr with plugin id/spec/hook name. This keeps failure policy in the locked
host rather than duplicating it in TypeScript.

## 4b. Semantic mismatch: in-process live SDK vs out-of-process JSON-RPC

> [MERGE] Contributed by the architecture/honesty planner; the single most
> important risk the fidelity pass under-treated. OpenCode plugins assume an
> **in-process, synchronous** SDK + Bun `$`; hya runs them **out-of-process behind
> one blocking RPC**. Five leak points and their handling:

1. **Synchronous `await client.x()` mid-hook (re-entrancy / deadlock).** An
   OpenCode plugin commonly calls `await client.session.message.list(id)` from
   inside a hook. In hya: the `hook/<name>` RPC is in flight while `run_turn` is
   **blocked awaiting the reply** (parent design §3.1); the plugin's HTTP call hits
   `hya serve` on the same engine.
   - **Reads are safe by construction**: the hya engine holds **no per-session
     lock across hook dispatch** (the dispatcher is invoked between awaits, not
     inside a Mutex guard), and `GET /events|/stream` only briefly take the store
     mutex. So `session.message.list` / `event.subscribe` cannot deadlock.
   - **Re-entrant writes are rejected**: a plugin calling `client.session.prompt(...)`
     from inside a hook would re-enter `run_turn` on the same session. The shim
     binds the active session into a `currentHookContext` and throws
     `HyaReentrantWriteError` while any hook is in flight (per-call, not blanket).
   - The per-hook timeout (parent §4: `tool.execute.before` 1s, `permission.ask`
     5s) bounds any leak regardless.
2. **Sequencing N OpenCode plugins inside one child.** The adapter runs its loaded
   OpenCode plugins sequentially in OpenCode load order for each hook, producing one
   combined outcome it returns as a single hya outcome; hya then continues its
   outer plugin chain. Consistent both ways (§3).
3. **`throw` to block vs `veto`.** Only `tool.execute.before` maps a thrown error to
   `veto{reason}`; every other hook's throw follows hya posture (§4) — safe →
   `defer`, open → original payload — always logged.
4. **Partial in-place `output` mutation.** The adapter passes the OpenCode handler
   the same `output` object, awaits the void promise, then serializes the mutated
   object as the outcome. For `chat.params` only whitelisted keys survive (§3);
   for `tool.execute.after` the wire-tagged original error kind is preserved and
   permission-kind synthesis is rejected.
5. **Token-delta firehose into `event`.** The adapter's `event` handler is
   fire-and-forget with a bounded internal buffer (16 events/plugin, drop-oldest +
   sampled warn), so a slow OpenCode `event` handler never back-pressures hya's
   `emit`.

## 5. SDK-shaped Client -> `hya serve`

OpenCode's generated SDK exposes many groups. Source: [OpencodeClient groups](/tmp/opencode-src/packages/sdk/js/src/gen/sdk.gen.ts:1157).
Child C supports the subset common server plugins call, and documents gaps
instead of silently pretending to be full OpenCode.

| OpenCode SDK method/group | Adapter backing | Current hya endpoint | Status / gap |
|---|---|---|---|
| `client.session.create({ body: { parentID?, title? } })` | Translate to hya create session using adapter defaults for agent/model/workdir | `POST /sessions` | Supported. hya requires `agent`, `model`, `workdir`; adapter supplies them from env/context. Source: [CreateSessionRequest](/chivier-disk/yanweiye/Projects/hya/crates/hya-proto/src/api.rs:6), [OpenCode SessionCreateData](/tmp/opencode-src/packages/sdk/js/src/gen/types.gen.ts:2082). |
| `client.session.prompt({ path: { id }, body: { parts, ... } })` | Flatten OpenCode text parts into one hya prompt string | `POST /sessions/:id/prompt` | Supported for text-only parts. File/agent/subtask parts need a future hya prompt DTO, not a protocol change. Source: [PromptRequest](/chivier-disk/yanweiye/Projects/hya/crates/hya-proto/src/api.rs:20), [OpenCode SessionPromptData](/tmp/opencode-src/packages/sdk/js/src/gen/types.gen.ts:2588). |
| `client.session.messages({ path: { id } })` | Prefer future hya projection endpoint; interim can replay events and reduce in adapter tests only | none direct; current `GET /sessions/:id/events` | Gap. Smallest hya-server follow-up: `GET /sessions/:id/messages` returning projected `Message[]` or OpenCode-compatible `{ info, parts }[]`. Source: [Event replay endpoint](/chivier-disk/yanweiye/Projects/hya/crates/hya-server/src/lib.rs:104), [OpenCode SessionMessagesData](/tmp/opencode-src/packages/sdk/js/src/gen/types.gen.ts:2548). |
| `client.session.get({ path: { id } })` | Session info from server | none direct | Gap. Smallest follow-up: `GET /sessions/:id` returning id, parent, agent, model, workdir, optional title/time. |
| `client.event.subscribe()` / `client.global.event()` | Adapter-local stream from hya protocol `event` notifications | `GET /sessions/:id/stream` only | Supported inside adapter for loaded plugins without server changes. Gap for exact HTTP SDK parity: add global `GET /stream` or `GET /events/stream` SSE, optional `session` filter. Source: [hya stream](/chivier-disk/yanweiye/Projects/hya/crates/hya-server/src/lib.rs:121), [OpenCode event endpoints](/tmp/opencode-src/packages/sdk/js/src/gen/sdk.gen.ts:1145). |
| `client.app.log({ body })` | Write adapter log to stderr/tracing; optionally POST when server shim exists | none | Gap. Smallest follow-up: `POST /log` accepting `{ service, level, message, extra }`, matching OpenCode `AppLogData`. Source: [AppLogData](/tmp/opencode-src/packages/sdk/js/src/gen/types.gen.ts:3268). |
| `client.project.current()`, `client.path.get()`, `client.vcs.get()` | Local context/filesystem/git shim | none needed | Supported locally from `directory`, `worktree`, and `git` if present. Source: [ProjectCurrentData](/tmp/opencode-src/packages/sdk/js/src/gen/types.gen.ts:1716). |
| `client.file.read/list/status`, `client.find.text/files/symbols` | Local filesystem and `rg`/glob shim constrained to `directory` | none needed | Optional adapter-local support for plugins that inspect files. Must not bypass hya permissions for hya tool execution; this only serves plugin init/runtime calls. Source: [FileReadData](/tmp/opencode-src/packages/sdk/js/src/gen/types.gen.ts:3231), [FindTextData](/tmp/opencode-src/packages/sdk/js/src/gen/types.gen.ts:3138). |
| `client.config.get/providers`, `client.provider.list/auth` | Minimal read-only shape from hya config and adapter defaults | none | Gap for exact provider/auth compatibility. Return documented `UnsupportedSdkMethodError` unless a target plugin requires it. OpenCode auth/provider hooks are out of Child C scope. |
| `client.tool.ids/list` | Future hya tool listing endpoint or Child B registry export | none | Gap. If a target plugin needs it, smallest follow-up is `GET /tools` with model/provider query returning hya `ToolSchema[]`; otherwise unsupported. Source: [OpenCode ToolListData](/tmp/opencode-src/packages/sdk/js/src/gen/types.gen.ts:1981). |
| `client.auth.*`, `client.mcp.*`, `client.lsp.*`, `client.tui.*`, `client.pty.*` | none | none | Unsupported in Child C. TUI and auth/provider plugin surfaces are out of scope. |

Server follow-ups are hya-server/API additions only. They do not change the
stdio plugin protocol and should be coordinated as separate tasks if a chosen
real-world plugin needs them.

## 6. Optional Bun Runtime Dependency

Bun is required only when a `kind: opencode` plugin is enabled.

- `cargo build --workspace`, `cargo test --workspace`, hya core, hya server,
  and hya CLI without enabled OpenCode adapter must not require Bun.
- The Rust side resolves the bundled command lazily. If `bun` is missing, the
  plugin host marks the `opencode` child disabled and emits a clear warning:
  `OpenCode adapter disabled: Bun was not found on PATH; install Bun or disable plugins.opencode`.
- A missing Bun binary is not a protocol error and not a core startup failure.
- Bun tests run only in the adapter package and are skipped with a clear message
  on machines without Bun unless the adapter is explicitly enabled in CI.
- The adapter package pins `@opencode-ai/plugin@1.17.9`, `@opencode-ai/sdk@1.17.9`
  for type compatibility, plus the tested Bun version in CI metadata. Minimum
  Bun version should be the oldest version that passes adapter tests during
  implementation; record the exact version in the final Child C artifacts.

## 7. Version Pin

Pin and test against:

```json
{
  "dependencies": {
    "@opencode-ai/plugin": "1.17.9",
    "@opencode-ai/sdk": "1.17.9",
    "zod": "^3 || ^4",
    "zod-to-json-schema": "^3.24.0"
  }
}
```

The pin matches the parent PRD's verified source and the sparse clone package
versions. Source: parent PRD **"OpenCode plugin contract"** section,
[plugin package](/tmp/opencode-src/packages/plugin/package.json:3),
[SDK package](/tmp/opencode-src/packages/sdk/js/package.json:3).

## 8. Compatibility Honesty

Supported in Child C:

- OpenCode server plugin init signature and options.
- Legacy bare function exports and v1 default `{ id, server }` modules.
- OpenCode local dir scan and npm plugin spec installation with Bun.
- Hooks: `event`, `chat.message` where it maps to user prompt text,
  `experimental.chat.system.transform`, `chat.params`, `permission.ask`,
  `tool.execute.before`, `tool.execute.after`, and `tool:` registration.
- SDK-shaped `client` for the subset in section 5.

Not supported in Child C:

- OpenCode TUI plugins.
- Auth/provider plugins as OpenCode provider integrations.
- OpenCode command/shell/tool-definition hooks unless later hya hooks exist.
- Full OpenCode generated SDK parity.
- OS sandboxing; OpenCode plugins are trusted code running in the Bun child,
  matching the parent plugin trust boundary.

## 9. Merge Notes From Parallel Planning

- The adapter should not ask for a new Trellis task at execution time; this is an
  existing Child C planning artifact. The current no-active-task session state
  only prevents creating an unrelated new task without consent.
- The strongest hya-side risk is SDK surface mismatch, not hook dispatch. The
  design therefore uses an SDK-shaped hya client instead of trying to mount the
  generated OpenCode SDK directly on hya's current paths.
- The strongest OpenCode-fidelity risk is loader drift. The design pins v1.17.9
  and implements the exact currently observed loader behaviors that real plugins
  depend on: server entrypoint preference, legacy exports, config-order execution,
  isolated failures, and directory scans.
