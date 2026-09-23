# Bundle Runtime

Bundles can carry native process providers and MCP server declarations. The
runtime prepares their complete contribution set before publishing a generation.
A running round retains its captured tools, hooks, resources, and process owners.
Root turns adopt successfully prepared updates at subsequent model-round
boundaries; bound child/Workflow activations retain their inherited snapshot.
Installation, replacement, and removal also affect subsequent root admissions.
Raw native executables are carried by `extensions.rust` and started through the
existing out-of-process plugin protocol.

First-party in-process tool families can carry a Rust dynamic library under
`extensions.libraries`. All five trusted tool-family presets use this form. Their
`hya_tool_bundle_abi_v1` export must match the host's ABI digest, and
`hya_tool_bundle_register_v1` registers tools after the bundle identity and
declared names are checked. `hya_tool_bundle_with_runtime_v1` enters the
bundle's Tokio runtime on each tool future poll while the host runtime
remains available to host services. The backend and library must be built from the
same workspace and toolchain; Rust trait objects have no stable plugin ABI.
The release asset stores each trusted family package under `bundles/` beside
`bin/`. Installed third-party bundles do not gain in-process execution by
declaring a library resource.

## First-party bundles

Hya's own tools, agents, Skills, commands, channel policy and workflows are
bundles that the backend loads when it starts. None of their content is
compiled into the binary. `hya_bundle::FIRST_PARTY_BUNDLES` lists the twelve
trusted identities:

| Identity | Source | Supplies |
| --- | --- | --- |
| `hya/base-tools`, `hya/extended-tools`, `hya/network-tools`, `hya/channel-tools`, `hya/todo-tools` | `bundles/presets/<family>-tools` | Tool exposure policy and native tool library |
| `hya/core-skills` | `bundles/presets/core-skills` | Builtin Skills |
| `hya/core-commands` | `bundles/presets/core-commands` | `/init` and `/review` prompt templates |
| `hya/core-agents` | `bundles/presets/core-agents` | Builtin agent roster, prompts and reserved ids |
| `hya/agent-channels` | `bundles/presets/agent-channels` | Default channel capabilities |
| `hya/goal-loop`, `hya/plan-impl-review`, `hya/subagents` | `bundles/first-party/<name>` | First-party AgentSet and Workflow bundles |

`first_party_source` picks one source per identity:

- **Installed layout.** A backend in `<prefix>/bin/` loads only
  `<prefix>/bundles/hya-<name>.hyabundle`. Trust comes from the installation
  directory plus the exact identity allowlist; the loaded package must contain
  exactly that identity.
- **Cargo builds.** A backend or test binary under `target/<profile>/` reads the
  in-tree source directory, so edits apply on the next start without a
  rebuild. A package staged under `target/<profile>/bundles/` is used only when
  the source directory is absent.

`first_party_bundle(identity)` prepares each bundle once per process and keeps
the verified catalog for the process lifetime. A missing or mismatched
trusted bundle is a startup error, like a missing tool library.

### Release assets

Every first-party bundle is released at the hya version: each `bundle.yaml`
identity version equals `[workspace.package].version`, and a test enforces it.
A release builds each supported target natively: `x86_64-unknown-linux-gnu`,
`aarch64-unknown-linux-gnu`, and `aarch64-apple-darwin`. It publishes:

- `hya-<version>-<target>.tar.gz` per target, containing `bin/hya`,
  `bundles/hya-<name>.hyabundle` for all twelve bundles, and
  `lib/hya/bun-adapter`. Extracting it gives a working installed layout.
- One standalone asset per bundle, byte-identical to the archived copy:
  `hya-<name>-<version>-<target>.hyabundle` for the five native tool families
  on each target, and `hya-<name>-<version>.hyabundle` once for the seven
  platform-independent bundles. The release fails if two targets built a
  platform-independent bundle with different bytes. To replace a bundle in an
  installed layout, save the asset as `<prefix>/bundles/hya-<name>.hyabundle`.
- `SHA256SUMS-<target>` from each target job and a combined `SHA256SUMS`
  covering every archive and bundle asset, plus build provenance attestations
  for each file.

`cargo run -p xtask -- stage-first-party-bundles` produces both the archived
packages and the assets and refuses a release version that any bundle does not
carry. The release smoke test checks each asset against the archive and runs
`hya bundle list` from the extracted archive.

The engine keeps safety-critical logic and its prompt contracts in Rust:
admission, permissions, events, lifecycle, and the compaction and handoff
templates whose headings the engine parses.

## Usage

Package each required UTF-8 support file explicitly. For example:

```yaml
kind: Plugin
identity: { id: acme/search, version: 1.0.0, publisher: acme }
extensions:
  process: { kind: rust, command: [python3, '${BUNDLE_ROOT}/provider.py'] }
  files: [{ id: provider, path: provider.py }]
resources:
  tools: [{ id: lookup, path: lookup.json }]
```

`provider.py` must implement the [native plugin protocol](plugin-protocol.md).
It must initialize with plugin id `search`, kind `rust`, and exactly the declared
`lookup` tool. The `rust` process kind selects the native ABI; it does not compile
source code. Interpreters and executable dependencies must already be available.

To ship the executable itself, place the compiled binary at `bin/provider` and
declare it in the same bundle:

```yaml
kind: Plugin
identity: { id: acme/search-native, version: 1.0.0, publisher: acme }
extensions:
  rust: [{ id: provider, path: bin/provider }]
  process: { kind: rust, command: ['${BUNDLE_ROOT}/bin/provider'] }
resources:
  tools: [{ id: lookup, path: lookup.json }]
```

Build the binary for the target operating system and architecture before
packaging. `extensions.rust` accepts raw bytes; preparation stores canonical
Base64 in the prepared catalog, verifies the SHA-256 digest of the original
bytes, and includes the executable in the exact archive closure. Runtime
materialization writes those bytes into a private directory with executable
permission. The first process command argument must refer to one declared
native executable. A failed launch rejects the new generation while old
bindings retain their previous process and files. The executable must speak
the [native plugin protocol](plugin-protocol.md); declaring a binary alone
does not register any tools or grant extra host capabilities.

```sh
cargo run -p xtask -- package-bundle ./search ./search.hyabundle
hya bundle install ./search.hyabundle
hya bundle info acme/search
hya bundle remove acme/search
```

A Full-plane agent can call `search__lookup`. For an AgentBundle or
AgentSetBundle, select `lookup` in the owning agent's `resource_view.allow`;
that agent calls it as `lookup`. Other agents do not inherit private exports.

An MCP resource points to a JSON `McpServerConfig`, for example
`{"command":["python3","${BUNDLE_ROOT}/mcp.py"]}`. Package `mcp.py` in
`extensions.files`. A Plugin server `index` exporting `find` is callable as
`search__mcp__index__find`. A bundle agent selects the server resource `index`
and calls `index__find`. Resource-view aliases rename the server prefix.

An agentless Plugin using `extensions.js` starts the shipped Bun adapter with
its explicitly declared tool/hook entrypoints and bundle namespace as plugin
id. It needs no synthetic Agent or explicit `extensions.process` command.
Agent-bearing JavaScript bundles retain their activation-scoped sidecars.

## Interfaces and lifecycle

- `extensions.process: { kind: rust | bun | claude, command: string[] }` declares
  one provider. `command` is nonempty and contains no blank argument. Rust/Bun
  commands are complete argv arrays. Claude imports generate adapter argv;
  see [Claude import](claude-plugin-import.md).
- `extensions.files: { id: string, path: string, aliases?: string[] }[]` packages
  inert support files. All resource paths are normalized relative paths. Files
  are content-addressed along with their prepared bundle.
- `${BUNDLE_ROOT}` in command arguments expands to a private temporary directory.
  Native processes use that directory as cwd. A declared relative file argument
  is also resolved against that directory, including MCP command arguments.
  `${BUNDLE_CONFIG_DIR}` and `${BUNDLE_CONFIG_FILE}` expand to the bundle's
  absolute [configuration directory and `config.yml`](configuration.md#bundle-configuration-files).
- Process environment (`extensions.process`, including the implicit Bun process
  of a JavaScript Plugin): the environment is cleared, then set to exactly
  `PATH` (inherited), `HYA_BUNDLE_ROOT`, `CLAUDE_PLUGIN_ROOT`,
  `HYA_BUNDLE_CONFIG_DIR`, and `HYA_BUNDLE_CONFIG_FILE`. `HOME` is not passed.
- Bundled stdio MCP environment (`resources.mcp`): the server uses the private
  directory as cwd and starts with a cleared environment. It gets the inherited
  `PATH`, then `HYA_BUNDLE_CONFIG_DIR` and `HYA_BUNDLE_CONFIG_FILE`, then the
  declared `env` map (a declared key overrides the keys before it), then the
  host-owned `HYA_BUNDLE_ROOT`. `HOME` is not passed. Declared `env` values
  support the root and config expansions. Ordinary configured MCP servers keep
  their existing startup behavior.
- The configuration file need not exist. Its content digest (or absence) is
  part of a process/MCP bundle's runtime identity. If you edit it, that bundle's
  providers restart at the next root binding, like a changed package.
- The provider's initialized tools and hook names must exactly match declared
  resources. Nonempty dynamic Skill declarations must match packaged Skills.
  A missing executable, failed initialization, or declaration mismatch rejects
  the candidate without changing the published runtime generation.
  Workspace-adapter contributions reject initialization because the bundle
  schema has no resource contract for them; they are never silently ignored.
- Unchanged package/process/schema/configuration identities reuse the existing source. New
  bindings after uninstall omit that source; retained bindings keep it alive.
  Materialized files remain until the last retained process owner is dropped.
- Plugin hooks reach every agent's sessions in stable source-id order: built-in
  Full-plane agents and bundle agents alike. A bundle agent's chain is every
  installed Plugin's hooks, then its own bundle's process hooks restricted to
  the owner's `hook_refs` (unselected hooks do not execute), then its
  activation sidecar's restricted hooks. A Plugin never dispatches twice in one
  chain. Native hook names and payloads are defined in the plugin protocol;
  `chat.params` also carries the session's `root_session` and `agent`. A
  process-backed bundle (explicit `extensions.process`) may declare
  `model.fallback` to pick the next model after a pre-stream provider failure;
  implicit JavaScript Plugins keep the `event`/`tool.execute.*` hook set.
- Command and user-message admission hooks resolve from the fresh immutable
  binding admitted for that input. Session event hooks continue receiving that
  session's envelopes outside an active turn through the captured hook chain;
  an active turn dispatches its combined bundle and sidecar chain exactly once.
  Child sessions capture their inherited binding before the initial prompt, so
  lifecycle and admission hooks do not rebind against live registry state.
- MCP exports retain `ToolPermission::Mcp`; other process tools retain ordinary
  tool permissions. Resource-view selection and aliases do not grant permission.
  MCP background execution applies to bundled exports as well as configured
  servers. Completion prompts reuse the originating agent and runtime binding.
  Provider-facing expanded names must fit the normal 64-character tool-name
  limit. Shorten the server alias when necessary.
- Selected `permission.ask` hooks run through the turn's captured permission
  interceptor chain. A defer continues to configured interceptors and the normal
  user approval path; explicit deny rules cannot be overridden by a hook.
- URI scheme claims retain bundle ownership. An agent-bearing bundle's scheme
  tool resolves through its selected local resource; a Plugin claim resolves
  through its namespaced runtime export.
- Every tool call into an installed bundle's process (any kind: `rust`, `bun`,
  `claude`, and the implicit Bun process of a JavaScript Plugin) carries a
  call-scoped `host_capability`; bundle API requests carry one too. The operations
  are read-only or permission-checked (`context.describe`,
  `permission.assert`, `session.usage`); see
  [Plugin protocol](plugin-protocol.md#request-scoped-host-capabilities).

## Bundle API endpoints

A bundle with an explicit `extensions.process` may register its own HTTP
endpoints (`apis: [{ id, method, scope, path, description?, request_schema?,
response_schema? }]`, see
[AgentBundle authoring](agent-bundle-authoring.md#api-endpoints-apis) for the
fields and the path template grammar). The lifecycle:

- **Prepare** rejects endpoints without an explicit `extensions.process`,
  validates ids, methods, scopes, and templates, rejects overlapping templates
  under one method and scope, checks that schema files are declared extension
  files holding JSON, and records the endpoints (sorted) in the prepared
  catalog. The declared endpoints are part of the runtime source identity,
  like schemas, so a changed declaration restarts the process.
- **Start**: the process's initialize reply must list exactly the declared
  endpoint ids in `apis`, or the candidate is rejected like a tool or hook
  mismatch. The published runtime source then carries the endpoints (with
  their parsed templates and JSON Schemas) and a provider bound to that
  generation's process.
- **Serve**: a request to
  `/v1/sessions/{session}/bundles/{bundle}/{path…}` (session scope) or
  `/v1/bundles/{bundle}/api/{path…}` (global scope) checks the session (session
  scope only), refreshes the installed catalog if it changed (a failed refresh
  is logged and the current generation keeps serving), resolves the bundle in
  the live published generation, matches `{path…}` against the bundle's
  templates for that scope, and sends the process `api/request` with a
  request-scoped read-only capability — bound to the session for session scope,
  to no session for global scope. The process's `status` and `body` are the
  HTTP response, verbatim.
- **Generation swap**: each request resolves the generation that is live when
  it arrives and retains that generation's process and materialized root until
  it completes; requests after a swap (reinstall, config edit, uninstall) use
  the new generation, or answer `bundle_api_not_found` once the bundle is gone.
  In-process state (a store the process keeps in memory) does not survive a
  restart; the bundle must persist it itself if it has to survive one.
- **Errors** (host side; a process may answer any `200..=599` itself):

  | Condition | Code | HTTP | gRPC |
  | --- | --- | --- | --- |
  | Unknown session (session scope) | `session_not_found` | 404 | `NOT_FOUND` |
  | Unknown bundle, a bundle without endpoints, or no template of the scope matches the path under any method | `bundle_api_not_found` | 404 | `NOT_FOUND` |
  | The path matches only under other methods (the `Allow` header lists them) | `bundle_api_method_not_allowed` | 405 | `UNIMPLEMENTED` |
  | Body over 512 KiB, a non-empty body that is not JSON, a bad percent escape, an unparsable query, an unknown method (gRPC) | `bundle_api_bad_request` | 400 | `INVALID_ARGUMENT` |
  | Process error, crash, timeout (30 s), malformed reply, status outside `200..=599`, or a body on `204`/`205`/`304` | `bundle_api_failed` | 502 | `UNAVAILABLE` |

**Limits.** Request body at most 512 KiB of JSON (half the 1 MiB stdio frame
cap, leaving room for the request envelope and escaping); the reply travels in
one stdio frame, so its body must stay under 1 MiB — a larger frame is a
protocol violation that restarts the process. Concrete request paths are at
most 4096 bytes; templates at most 256 bytes and 16 segments; 64 endpoints per
bundle. The request timeout is the process's normal request timeout (30 s by
default).

**Security.** Bundle code runs as the user with the user's OS privileges, like
every bundle process; registering an endpoint only lets HTTP clients reach
code that is already trusted. The endpoints sit behind exactly the same
access control as every other `/v1` route (the server binds `127.0.0.1` by
default and adds no per-route authentication), so anyone who can reach the
server can call them. The host capability stays read-only for every method:
`POST`/`PUT`/`PATCH`/`DELETE` change only what the bundle process itself
owns, never hya's event log. The process sees no request headers (in
particular no `Authorization`), only method, path, path parameters, query,
and JSON body.

`GET /v1/bundle-apis` lists `{ bundle, api, method, scope, path, description,
requestSchema?, responseSchema? }` for every published endpoint. Endpoints are
a bundle-process feature only: configured plugins (`plugins:`) and the Bun
extension adapter never serve them.

The process E2E suite exercises native tools, MCP tools, scoped hooks, package
removal, schema reads, bundle API endpoints (`p34_bundle_apis`), and uninstall.
Startup rollback and binding lifetime are also covered by the installed-bundle
refresh integration suite.
