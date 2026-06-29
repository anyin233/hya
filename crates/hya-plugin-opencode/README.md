# hya OpenCode Adapter

`crates/hya-plugin-opencode` provides hya's bundled compatibility layer for
OpenCode plugins.

The Rust crate pins the supported OpenCode package versions. The adapter under
[`adapter`](adapter) is a Bun/TypeScript JSON-RPC process that hya launches for
`plugins:` entries with `kind: opencode` and no explicit command.

Targeted OpenCode packages:

- `@opencode-ai/plugin@1.17.9`
- `@opencode-ai/sdk@1.17.9`

## Runtime Coverage

The adapter currently supports:

- plugin config discovery and initialization
- OpenCode hook registration translation
- event notifications
- plugin-defined tool calls
- chat params/message transform hooks
- command, message, text-complete, permission, shell-env, and tool before/after
  hooks
- workspace adapter registration metadata
- SDK shims for app logging, path/config/project/agent/skill/tool discovery,
  auth mutation errors, LSP status, formatter status, and VCS helpers
- `shutdown` and dispose-hook execution before process termination

## Running Checks

From `crates/hya-plugin-opencode/adapter`:

```sh
bun run typecheck
bun test
```

Set `BUN` to choose a Bun executable or `HYA_OPENCODE_ADAPTER_DIR` to point
`hya-cli` at an alternate adapter directory.

Known limits are tracked in
[`../../docs/opencode-parity.md`](../../docs/opencode-parity.md), especially the
OpenCode SDK client completeness section.
