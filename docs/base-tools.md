# Base tools preset

## Introduction

`hya/base-tools` is the trusted embedded Plugin preset that owns the exposure
policy for Hya's Rust builtin tools. It records which implementations are model
visible, their compatibility aliases, schema versions, invocation permission
posture, protected names, and URI-scheme ownership. Tool execution remains in
`hya-tool`; the preset cannot add executable code or grant package privileges.

The source lives in `bundles/presets/base-tools`. `bundle.yaml` identifies the
resource as a `Plugin` and declares `exposure.yaml` as an inert
`extensions.files` asset. The companion file carries trusted preset metadata
that the public Plugin manifest intentionally cannot grant as permissions. The
build prepares the Plugin, verifies its digest and policy, then generates static
Rust metadata. Runtime construction performs no file access or policy parsing.
It still checks that every declared tool has a Rust implementation and that no
compiled builtin was omitted.

## Usage

Applications continue to construct builtins with `ToolRegistry::builtins()`.
No installation or user configuration is required. For inspection, use
`base_tools_preset()`:

```rust
use hya_tool::{ToolRegistry, base_tools_preset};

let metadata = base_tools_preset();
assert!(metadata.is_protected("read"));
let registry = ToolRegistry::builtins();
assert!(registry.resolve("shell").is_some());
```

To change builtin exposure, update `exposure.yaml` together with the tool's
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

`base_tools_preset().bundle_digest()` exposes the canonical prepared Plugin
digest, and `prepared_catalog_bytes()` exposes the exact build-validated bytes
for audit and replay metadata.
