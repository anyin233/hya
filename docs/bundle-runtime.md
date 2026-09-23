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
hya bundle uninstall acme/search
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
  Native processes receive `PATH`, `HYA_BUNDLE_ROOT`, and `CLAUDE_PLUGIN_ROOT`.
  Bundled stdio MCP servers also use the private directory as cwd and start with
  a cleared environment containing only inherited `PATH`, explicit configured
  environment entries, and the authoritative `HYA_BUNDLE_ROOT`. Explicit MCP
  environment values support bundle-root expansion. Ordinary configured MCP
  servers retain their existing startup behavior.
- The provider's initialized tools and hook names must exactly match declared
  resources. Nonempty dynamic Skill declarations must match packaged Skills.
  A missing executable, failed initialization, or declaration mismatch rejects
  the candidate without changing the published runtime generation.
  Workspace-adapter contributions reject initialization because the bundle
  schema has no resource contract for them; they are never silently ignored.
- Unchanged package/process/schema identities reuse the existing source. New
  bindings after uninstall omit that source; retained bindings keep it alive.
  Materialized files remain until the last retained process owner is dropped.
- Plugin hooks join Full-plane agents in stable source-id order. Agent-bearing
  process hooks are restricted to the owner's `hook_refs`; unselected hooks do
  not execute. Native hook names and payloads are defined in the plugin protocol.
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

The process E2E suite exercises native tools, MCP tools, scoped hooks, package
removal, schema reads, and uninstall. Startup rollback and binding lifetime are
also covered by the installed-bundle refresh integration suite.
