# Tool-family presets

## Introduction

Five trusted first-party Plugin presets own the exposure policy for Hya's Rust
builtin tools. They record model visibility, compatibility aliases, schema
versions, invocation permission posture, protected names, and URI-scheme
ownership. All five families own their concrete Rust implementations and ship lockstep
dynamic libraries inside public `.hyabundle` packages. `hya-tool` supplies the
`Tool` interface, registry, native loader, permission model, and session planes.

The sources live in `bundles/presets/{base,extended,network,channel,todo}-tools`.
Each `bundle.yaml` identifies a `Plugin` and declares `exposure.yaml` as an inert
`extensions.files` asset. Every source also declares its Cargo manifest and Rust implementation modules
as source assets. The companion file carries trusted preset metadata
that the public Plugin manifest intentionally cannot grant as permissions. Each
family's bundle is loaded from its [first-party bundle](bundle-runtime.md#first-party-bundles)
source once per process, which verifies its digest and policy and rejects
duplicate names across families — unless exactly one of the two entries
declares `overrides: <the other family>` (see [Overrides](#overrides)). Runtime
registry construction reads native family packages when present, validates
their prepared content and declared tool sets, and checks that every policy
entry has a loaded implementation.

A build tool can now stage one target-specific Rust executable into any of
these policy sources. It adds an `extensions.rust` executable, a `kind: rust`
process command, and one `resources.tools` declaration per canonical policy
name before writing a public package. This prepares a native family package;
it does not change the default builtin registry. The supplied executable must
implement the [plugin protocol](plugin-protocol.md) and announce exactly the
declared tools before the runtime can publish it.

| Bundle | Canonical tool names |
| --- | --- |
| `hya/base-tools` | `read`, `write`, `edit`, `ls`, `glob`, `find`, `grep`, `ask_user`, `bash`, `apply_patch` |
| `hya/extended-tools` | `lsp`, `skill`, `list_agents`, `task`, `workflow`, `search_agent`, `archive`, `plan_exit`, `project_activity`, `wait` |
| `hya/network-tools` | `webfetch`, `websearch` |
| `hya/channel-tools` | `send`, `list_channel`, `report`, `wait` (overrides extended-tools' `wait`) |
| `hya/todo-tools` | `todo__read`, `todo__update_status`, `todo__update_content` |

The registry holds 28 canonical names: `wait` is exported by two families and
installed once.

`ask_user` is the canonical name for the requested `ask` function; `question` remains its hidden alias. The
user-assigned `lsp` and `task` tools belong to the extended family.

`ask_user` asks structured questions through the interaction plane. For example,
`{"questions":[{"question":"Ship?","header":"confirm","options":[{"label":"yes","description":"proceed"}]}]}`
returns the selected labels in `metadata.answers`. If any question is cancelled
(including an empty free-text reply), the call returns `ToolError::Cancelled`;
the engine records an error rather than a completed tool result.

## Usage

Applications continue to construct builtins with `ToolRegistry::builtins()`.
No installation or user configuration is required. For inspection, use
`tool_bundle_presets()` or the compatibility accessor `base_tools_preset()`:

```rust
use hya_tool::{ToolRegistry, base_tools_preset, tool_bundle_presets};

let metadata = base_tools_preset();
assert!(metadata.is_protected("read"));
assert_eq!(tool_bundle_presets().len(), 5);
let registry = ToolRegistry::builtins();
assert!(registry.resolve("shell").is_some());
```

To change builtin exposure, update the owning family's `exposure.yaml` together with the tool's
documentation and parity tests. Adding an entry does not create a tool: every
entry must match a Rust `Tool` implementation supplied by the owning family.

To stage a built native family executable, use:

```sh
cargo run -p xtask -- package-native-tool-bundle \
  bundles/presets/todo-tools target/release/hya-todo-tools \
  dist/todo-tools.hyabundle
```

The command reads the family source, preserves its declared files, adds
`native/tool-runtime` with the exact executable bytes, and writes a
deterministic package. It does not compile the executable or install the
package. The executable must be built for the target platform first.

To build and package one trusted family implementation for the current target:

```sh
cargo build -p hya-todo-tools --lib
cargo run -p xtask -- package-native-tool-library \
  bundles/presets/todo-tools \
  target/debug/libhya_todo_tools.so \
  dist/bundles/hya-todo-tools.hyabundle
```

Use `.dylib` instead of `.so` on macOS. The command embeds the raw library
bytes as `extensions.libraries`, adds one tool declaration per policy entry,
and writes the public package. Release builds put that package beside the
backend's `bin` directory in `bundles/`. There, `ToolRegistry::builtins()` inspects
the package, checks its identity and declared names, materializes the verified
library in the private native cache (or a temporary directory when the cache
is unavailable), checks the lockstep ABI digest, and loads its tools. Temporary
copies are deleted after loading; verified cache copies can be reused. The
loaded library stays mapped for the process lifetime.
`builtin_bundle_origin(name)` returns the identity for a loaded native tool.
Cargo builds load the library Cargo just linked instead: a library in the
executable's `deps/` directory wins, then one beside the executable. A package
staged under `target/debug/bundles/` is used only when neither exists, so a
stale package cannot shadow a fresh build or slow backend startup with full
package verification. The Rust ABI is not stable across independent builds;
build the backend and library from the same workspace and toolchain.

Build and package `hya-base-tools`, `hya-extended-tools`,
`hya-network-tools`, and `hya-channel-tools` with the same command, using
the matching `libhya_<family>_tools.so` (or `.dylib`) and
`hya-<family>-tools.hyabundle` names. All five packages are required for
`ToolRegistry::builtins()`. The host continues to provide session-scoped
services through `ToolCtx`; each tool body and schema lives in its bundle.
The loader enters each bundle's Tokio runtime for native future polls while
the host runtime remains available to session planes. Bundled filesystem,
process, timer, and HTTP operations use the bundle's reactor.

### Bash process completion

`bash` captures stdout and stderr through pipes by default; `pty: true` attaches
both streams to a real terminal. A daemon that calls `setsid()` can retain those
output descriptors after the command exits. Both capture modes bound the final
drain so such a daemon cannot hang the tool or turn a successful command into a
timeout.

For example,
`{"command":"trap '' HUP; (sleep 0.3; printf 'bg-ok') &","timeout":30}`
waits for the background child in the shell's process group and captures `bg-ok`.
Ignoring SIGHUP lets the child survive the PTY shell's exit.
Use `"pty":true` in the same input when the command needs a terminal.
The input fields remain `command: string` (required), `env: object<string,
string>`, `timeout: number` (seconds; zero disables the deadline), `cwd: string`,
and `pty: boolean` (default false).

On Unix, once the shell has exited and its process group is absent
(`kill(-pgid, 0)` reports `ESRCH`), both modes receive one fixed 200 ms drain
window from the first observation of the empty group. Arriving output does not
extend it. At the cutoff, capture reads immediately buffered bytes without
waiting, stopping at EOF, no readable bytes, or 1 MiB per stream (one combined
stream for a PTY), so a flooding foreign writer cannot hold the tool open.
The tool then returns the shell's real exit code even if a foreign-session daemon
retains an output fd. Children still in the original process group continue to
be waited for.
Timeout or cancellation kills the original group and allows a 100 ms drain
window, followed by the same bounded non-blocking drain before closing
capture, regardless of foreign-session fd holders.
Those detached processes are outside the killed group and remain the caller's
responsibility.

The result remains `{title, output, metadata}`: `metadata.exit` is the shell's
integer exit code (or null for a signal/timeout), `metadata.timedOut` is a boolean,
and `metadata.durationMs` records elapsed milliseconds. Timeout returns a
structured result with `timedOut: true`; explicit cancellation returns
`ToolError::Cancelled`.

`glob` and `find` traverse directories breadth-first without following directory
symlinks, with a 30-second traversal budget. For example,
`glob {"path": "src", "pattern": "*.rs"}` returns the lexically first 100
matching paths and the total match count. When traversal stops at the budget,
that count covers only matches seen so far, `metadata.truncated` and
`metadata.incomplete` are `true`, and the output ends with a blank line and
`(Search stopped after 30 seconds before every directory was visited, so results are incomplete. Use a more specific path or pattern.)`
If no matches were seen, the text before that note is
`No files found in the part searched`. `find` adds `"incomplete": true` to
its result object when traversal stops at the budget. Both incomplete keys
are absent when traversal completes.

### Path boundary

The file tools (`read`, `write`, `edit`, `ls`, `glob`, `find`, `grep`,
`apply_patch`, and `lsp`) may touch anything inside the session's workspace
roots without an extra ask. A Project session's roots are its Project's
roots; any other session's single root is its workdir (ADR-0024, ADR-0026).
Every tool uses one shared helper, `hya_tool::ProjectScope`, which resolves
symlinks before deciding, so a link inside a root that points elsewhere counts
as outside.

A path outside every root raises an `ExternalDirectory` ask for a concrete
`<dir>/*` pattern, before the tool's normal `read`/`edit` check. `<dir>` is
the canonical directory the path really lives in (symlinks resolved), and an
"allow always" on the ask covers exactly that directory: not its
subdirectories, and glob characters in the path are never interpreted. `apply_patch`
refuses such a path with an input error instead of asking. `bash` has no path
boundary: its `cwd` may be anywhere, and only its command rules apply.

For example, with roots `/work/app` and `/work/lib` and workdir `/work/app`,
`read {"path": "/work/lib/src/mod.rs"}` runs without asking, while
`read {"path": "/etc/hosts"}` asks `ExternalDirectory` for `/etc/*` (on
macOS, where `/etc` links to `/private/etc`, for `/private/etc/*`), and in
yolo (`danger`) mode runs without asking.

```rust
use std::path::{Path, PathBuf};
use hya_tool::ProjectScope;

let roots = [PathBuf::from("/work/app"), PathBuf::from("/work/lib")];
let scope = ProjectScope::new(Path::new("/work/app"), &roots);
let inside = scope.contains(Path::new("/work/lib/src/mod.rs"));
let ask = scope.outside_dir_pattern(Path::new("/etc/hosts")); // "/etc/*" (canonical)
```

`ProjectScope` interface (`crates/hya-tool/src/project_scope.rs`):

| Item | Contract |
| --- | --- |
| `ProjectScope::new(workdir, roots)` | Canonicalizes each root once; an unresolvable root keeps its lexical absolute form; empty `roots` means `[workdir]`. |
| `ProjectScope::for_ctx(ctx)` | `new(&ctx.workdir, &ctx.roots)`. |
| `contains(path) -> bool` | Relative paths resolve against the workdir. Canonicalizes the path, or its nearest existing ancestor plus the missing remainder (a `..` in the remainder is outside). Component-wise containment in any root. Unreadable paths and symlink loops are outside. |
| `outside_dir_pattern(path) -> String` | `<canonical parent>/*`, the file-tool ask resource: the path is resolved like `contains` (a symlinked file names its target's directory) and falls back to its lexical form when it cannot be resolved. |
| `outside_directory_pattern(dir) -> String` | `<canonical dir>/*`, the directory-tool (`ls`, `find`) ask resource. |
| `authorize(plane, path, pattern)` | Asks `ExternalDirectory` for `pattern(scope)` unless `contains(path)`. |

## Interface definitions

The companion policy has this closed shape:

```yaml
schema_version: 1
identity: hya/base-tools
protected_names: [read]
schemes:
  - { scheme: example, tool: read, writable: false }
tools:
  - name: read
    schema_version: 1
    permission: read_only
    exposed: true
    aliases:
      - { name: legacy_read, visibility: hidden }
    # optional: replace another family's same-named tool when both load
    # overrides: hya/extended-tools
```

`permission` is one of `read_only`, `task`, `tool`, `command`, or `mcp`. It
controls invocation defaults: read-only and task calls default to allow,
general tool and MCP calls default to ask, and command calls authorize both the
tool and the submitted command. This metadata does not bypass resource-level
permission checks inside a tool.

`visibility` is `hidden` or `public`. Both forms dispatch to the canonical Rust
implementation; hidden aliases are excluded from advertised schemas. Each
tool's `schema_version` versions its existing `ToolSchema` contract. The preset
does not duplicate JSON schemas, so the Rust implementation remains their
single content owner.

### Overrides

`overrides: <family identity>` (optional, string) on a tool entry declares that
this family's implementation replaces the named family's same-named canonical
tool whenever both families are loaded. It is the only way two families may
export one name; the winner is declared, never implied by load order or by
identity sorting (`hya/channel-tools` sorts before `hya/extended-tools`, so an
order-based rule would pick wrong). Policy loading rejects an `overrides` that
names the family itself or an unknown family, a shared name where neither or
both entries declare the override, and any shared alias.

The one use today is `wait`: `hya/extended-tools` exports a `wait` that wakes
on subagent progress only, and `hya/channel-tools` exports
`{ name: wait, schema_version: 1, permission: read_only, overrides: hya/extended-tools }`,
which also wakes on mail for the caller. `ToolRegistry::builtins()` loads all
five families, so the channel-aware `wait` is the one installed;
`ToolRegistry::from_tool_families(&[...])` builds a registry from a subset of
families (for example without `hya/channel-tools`, which installs the
extended-tools `wait`). `builtin_bundle_origin("wait")` names the winner.

```rust
use hya_tool::ToolRegistry;
let registry = ToolRegistry::from_tool_families(&[
    "hya/base-tools", "hya/extended-tools", "hya/network-tools", "hya/todo-tools",
]);
assert_eq!(registry.builtin_bundle_origin("wait"), Some("hya/extended-tools"));
assert_eq!(ToolRegistry::builtins().builtin_bundle_origin("wait"), Some("hya/channel-tools"));
```

`protected_names` declares names that runtime source composition must not mask.
The initial protected set is `read`. `schemes` declares a scheme, its canonical
tool owner, and whether that scheme permits writes; the initial preset owns no
external URI schemes.

Each preset's `bundle_digest()` exposes its canonical prepared Plugin digest,
and `prepared_catalog_bytes()` exposes its exact build-validated bytes for
audit and replay metadata. `tool_bundle_presets()` returns all five policies in
base, extended, network, channel, and TODO order. Each policy keeps its own
tool names, aliases, permissions, schemes, and protected names.

The staged package adds these manifest entries without modifying the source
`bundle.yaml` on disk:

```yaml
resources:
  tools:
    - { id: todo__read, path: declarations/tool.json }
extensions:
  rust:
    - { id: runtime, path: native/tool-runtime }
  process:
    kind: rust
    command: ['${BUNDLE_ROOT}/native/tool-runtime']
```

It repeats the `resources.tools` row for each canonical tool in the family's
`exposure.yaml`. The declaration file is `{}`; the executable's `initialize`
reply owns the input schemas. Package preparation validates the executable's
raw bytes, paths, and manifest closure; runtime activation checks the announced
tool set against the declarations.

The in-process library package instead adds `extensions.libraries` and no
`extensions.process`:

```yaml
resources:
  tools:
    - { id: todo__read, path: declarations/tool.json }
extensions:
  libraries:
    - { id: runtime, path: native/libhya_todo_tools.so }
```

`extensions.libraries` is a list of `{id, path}` raw byte resources. A family
package must have one library named `runtime`; its exported C symbols are
`hya_tool_bundle_abi_v1(*mut u8)` and
`hya_tool_bundle_register_v1(*mut Vec<Arc<dyn Tool>>)` and
`hya_tool_bundle_with_runtime_v1(*const c_void, unsafe extern "C" fn(*mut c_void), *mut c_void)` and
`hya_tool_bundle_share_deadline_v1(*const c_void)`.
The host checks the 32-byte ABI digest before sharing its deadline cell or calling `register`, and the
library remains mapped for the process lifetime. The runtime callback polls
one tool future synchronously while the bundle's Tokio runtime is entered.
Calls with an existing host runtime stay on their original task, preserving
task-local admission context. Calls without one use a temporary joined worker.
A mismatch or missing
library prevents builtin registry construction.
Calls made outside an existing Tokio runtime use a one-call host runtime;
the same bundle ABI and permission context apply.

### Legacy cleanup

The `invalid` placeholder tool is no longer registered. Artifact reads return the
captured bytes (with any requested handle projection); artifact post-processing
hooks are no longer supported.
The six hidden aliases remain dispatchable because current frontend consumers
still recognize them. `apply_patch` and `plan_exit` remain available to the
first-party core agents.

Use `send.channel`, `archive.target`, `wait.targets`, `wait.timeout_secs`,
and `workflow.action`; their former parameter aliases are removed.
For example, wait for a worker with `{"targets":["worker"],"timeout_secs":30}`.

## Replacing a native tool family

Native families are replaceable trusted code. Replacement keeps the existing
family identity and exposure policy while changing its implementations; it is
not a stable third-party Rust ABI. Build the replacement and host together with
the same workspace sources, dependency lockfile, compiler, target and build
settings. Never copy an independently compiled trait object into the host.

### Usage

Implement `hya_tool::Tool` for every canonical name in the selected family's
`exposure.yaml`, and export the three functions below from a `cdylib`. A minimal
working replacement for the TODO family is in
`crates/hya-tool/tests/fixtures/replacement_tools.rs`. To build and try that
fixture in a disposable layout (Linux):

```sh
CARGO_BUILD_JOBS=8 cargo build --locked -p hya-tool --examples
CARGO_BUILD_JOBS=8 cargo test --locked -p hya-tool --test native_replacement
```

The test copies its host into a private directory and installs the replacement
as `libhya_todo_tools.so` beside it. It constructs only `hya/todo-tools` through
`ToolRegistry::from_tool_families`, invokes `todo__read`, and checks the fixture's
unique result and echoed input. A second library reports a wrong ABI digest;
its registration aborts if called, proving rejection precedes registration.
No shared build library is overwritten. `cargo test -p hya-tool` builds both
fixture examples automatically; when running only this integration test, build
both examples first with `cargo build --locked -p hya-tool --examples`.

For deployment, package the replacement library with
`package-native-tool-library` as shown above, preserving the family source's
identity and declarations. Use the expected filename
`libhya_<family>_tools.so` (`.dylib` on macOS). Discovery, in order, is
`<executable-directory>/deps/<filename>`, then
`<executable-directory>/<filename>`, then the family's `.hyabundle` under
`bundles/` in the executable directory (or its parent for `bin/` and `deps/`).
All families selected by the registry must be present. Libraries are cached by
family for the process lifetime, so restart the host to select new bytes.

### Interface definitions and validation

All exports use `unsafe extern "C"` and unmangled symbol names:

| Symbol | Parameters and contract |
| --- | --- |
| `hya_tool_bundle_abi_v1` | `out: *mut u8`: write exactly 32 bytes from `hya_tool::native_bundle::abi_digest_v1()` to the host's writable buffer. |
| `hya_tool_bundle_register_v1` | `out: *mut Vec<Arc<dyn Tool>>`: append the family's implementations to the host's live vector, only after ABI validation. |
| `hya_tool_bundle_with_runtime_v1` | `handle: *const c_void, callback: unsafe extern "C" fn(*mut c_void), state: *mut c_void`: synchronously poll through `hya_tool::native_bundle::with_runtime_v1`; retain neither pointer nor callback state. |

| `hya_tool_bundle_share_deadline_v1` | `cell: *const c_void`: call `hya_tool::native_bundle::share_deadline_v1` once at load to adopt the host's process-lifetime deadline cell from the matching lockstep build. |

The digest covers the native ABI v1 discriminator, backend version, OS,
architecture, relevant Rust sizes/alignments, and `tool.rs` / `deadline.rs` source bytes. It is
a lockstep guard, not proof that arbitrary native code is safe or a complete
compiler fingerprint. The loader resolves required symbols and rejects a
mismatch with `built for a different hya-tool ABI` before registration. Registry
construction also checks implementations against the selected exposure policy.
Packaged libraries additionally pass package identity, digest, declared-tool-set
and single `runtime` library validation; no process extension is allowed.
Verified bytes use the disposable native cache described in
[bundle runtime](bundle-runtime.md#native-tool-startup-cache).

## Budgeted Bash timeouts

An in-process `hya exec --time-budget 900 "implement the change"` installs a
wall-clock deadline shared with the native Bash library. Bash keeps the normal
300-second default, `timeout: 0` disables its requested timeout, and positive
values clamp to `1..=3600` seconds. With a run budget, all of these are additionally
capped to `max(remaining - grace, 5s)` by `RunDeadline::cap`. A shorter requested
timeout is preserved. This changes only the execution timeout; the fixed 200 ms
orphan grace, 100 ms post-kill drain and bounded final drain still apply.

A capped result retains `{title, output, metadata}`. `metadata.timeoutSeconds`
is the effective rounded limit (`number`), `metadata.deadlineCapped: true`
(`boolean`, omitted when not shortened) survives the host output cap, and
`output` includes a `<shell_metadata>` explanation naming the limit and seconds
remaining. `timeoutClamped: true` continues to describe ordinary argument
clamping when the run deadline did not replace it. For example, with 200 seconds
left in a 900-second run, a 300-second request runs for about 155 seconds,
reserving the 45-second guard. Member `wait` uses the same cap; its existing
request and response fields are unchanged.

## Output limit audit

The host cap remains necessary. Tool-local limits constrain different parts of
a result and do not establish the host's complete envelope contract. In
particular, native Grep allows 4,096 display rows, whereas the host allows 2,000;
post-edit diagnostic text shows up to 20 errors per file and, for write, five
other files, while raw diagnostic metadata is returned without that count
bound. The host permits 16 diagnostic rows of at most 2 KiB each. The host also bounds
titles to 512 bytes and serialized coding envelopes to 256 KiB. Hooks can
replace results, and replacement native libraries can return different data.
Thus no coding policy's entire second pass is provably redundant.

The following are implementation limits, not promises of a fully bounded
serialized response. `Default` results use the host's 5,000-character threshold
and artifact spill; coding results use its structured field/envelope bounds.

| Tool | Own limit before the host pass |
| --- | --- |
| `read` | File/directory text fitted to 50 KiB; file default 2,000 lines. Titles, paths, media attachments and channel-read data are not all bounded by this text limit. |
| `write` | Output and display text each 50 KiB; diagnostic text has row limits but raw diagnostic metadata does not; title/path and the whole envelope have no equivalent cap. |
| `edit` | Output and patch each 50 KiB; fallback text only included when at most 50 KiB. Raw diagnostic metadata and title/path still need host bounds. |
| `grep` | Output and serialized display budgets 50 KiB; match limit at most 200, context at most five, display at most 4,096 rows; warnings at most 16 / 4,096 bytes total. Host row and envelope limits differ. |
| `bash` | Captured inline bytes and rendered output bounded to 50 KiB, overflow saved to artifact; output path serialized limit 32 KiB. This does not establish every host metadata/title/envelope bound. |
| `glob` | 100 results; 30-second breadth-first traversal budget; incomplete searches set `metadata.incomplete` and `metadata.truncated` to `true`. No total text/serialized-byte bound. |
| `ls` | No tool-local output bound. |
| `find` | 30-second breadth-first traversal budget; incomplete searches set `incomplete: true`. No result-row or byte bound. |
| `ask_user` | No byte bound on selected/free-text answers. |
| `apply_patch` | No complete output-envelope cap; diagnostic text uses shared row limits but raw diagnostic metadata does not. |
| `lsp` | Plane response; no native wrapper output-byte cap. |
| `skill` | File sample limited to ten entries; skill body has no output-byte cap. |
| `list_agents` | Catalog response; no output-byte cap. |
| `task` | Spawn-plane response; no output-byte cap. |
| `workflow` | Workflow-plane response; no output-byte cap. |
| `search_agent` | Search response; no output-byte cap. |
| `archive` | Lifecycle-plane response; no output-byte cap. |
| `plan_exit` | Interaction response; no output-byte cap. |
| `project_activity` | Requested row limit (schema 1–200, default 50); no wrapper byte cap. |
| `wait` | Both family implementations delegate to planes; no wrapper byte cap. |
| `webfetch` | Response body rejected above 5 MiB; much larger than the host default threshold. |
| `websearch` | Optional requested context character limit; no unconditional native output cap. |
| `send` | Mailbox-plane response; no wrapper byte cap. |
| `list_channel` | Mailbox-plane response; no wrapper byte cap. |
| `report` | Lifecycle-plane response; no wrapper byte cap. |
| `todo__read` | Entire todo snapshot; no byte cap. |
| `todo__update_status` | Entire todo snapshot; no byte cap. |
| `todo__update_content` | Entire todo snapshot plus added IDs; no byte cap. |

MCP and plugin results also retain the host cap. Row limits and optional input
limits alone cannot replace a serialized output bound.
