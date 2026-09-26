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
| `hya/extended-tools` | `invalid`, `lsp`, `skill`, `list_agents`, `task`, `workflow`, `search_agent`, `archive`, `plan_exit`, `wait` |
| `hya/network-tools` | `webfetch`, `websearch` |
| `hya/channel-tools` | `send`, `list_channel`, `report`, `wait` (overrides extended-tools' `wait`) |
| `hya/todo-tools` | `todo__read`, `todo__update_status`, `todo__update_content` |

The registry holds 28 canonical names: `wait` is exported by two families and
installed once.

`ask_user` is the existing canonical name for the requested `ask` function;
`question` remains its hidden compatibility alias. The user-assigned `lsp`,
`task`, and `invalid` tools belong to the extended family.

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
the package, checks its identity and declared names, extracts the library to a
private temporary directory, checks the lockstep ABI digest, and loads its
tools. The extracted copy is deleted right after loading; the loaded library
stays mapped for the process lifetime.
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
`hya_tool_bundle_with_runtime_v1(*const c_void, unsafe extern "C" fn(*mut c_void), *mut c_void)`.
The host checks the 32-byte ABI digest before calling `register`, and the
library remains mapped for the process lifetime. The runtime callback polls
one tool future synchronously while the bundle's Tokio runtime is entered.
Calls with an existing host runtime stay on their original task, preserving
task-local admission context. Calls without one use a temporary joined worker.
A mismatch or missing
library prevents builtin registry construction.
Calls made outside an existing Tokio runtime use a one-call host runtime;
the same bundle ABI and permission context apply.
