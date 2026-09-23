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
duplicate names across families. Runtime
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
| `hya/extended-tools` | `invalid`, `lsp`, `skill`, `list_agents`, `task`, `workflow`, `search_agent`, `kill`, `plan_exit` |
| `hya/network-tools` | `webfetch`, `websearch` |
| `hya/channel-tools` | `send`, `list_channel`, `report` |
| `hya/todo-tools` | `todo__read`, `todo__update_status`, `todo__update_content` |

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
temporary path, checks the lockstep ABI digest, and loads its tools.
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
