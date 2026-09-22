# Tool-family presets

## Introduction

Five trusted embedded Plugin presets own the exposure policy for Hya's Rust
builtin tools. They record model visibility, compatibility aliases, schema
versions, invocation permission posture, protected names, and URI-scheme
ownership. The split lets each family acquire its own native implementation
later. Tool execution currently remains in `hya-tool`; these policy bundles do
not add executable code or grant package privileges.

The sources live in `bundles/presets/{base,extended,network,channel,todo}-tools`.
Each `bundle.yaml` identifies a `Plugin` and declares `exposure.yaml` as an inert
`extensions.files` asset. The companion file carries trusted preset metadata
that the public Plugin manifest intentionally cannot grant as permissions. The
build prepares every Plugin, verifies its digest and policy, rejects duplicate
names across families, then generates static Rust metadata. Runtime construction
performs no file access or policy parsing. It still checks that every declared
tool has a Rust implementation and that no compiled builtin was omitted.

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
entry must match a Rust `Tool` implementation supplied by `hya-tool`.

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
