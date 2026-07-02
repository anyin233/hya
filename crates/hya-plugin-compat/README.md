# hya Compat Adapter

`crates/hya-plugin-compat` provides hya's bundled compatibility layer for
Compat plugins.

The Rust crate pins the supported Compat package versions. The adapter under
[`adapter`](adapter) is a Bun/TypeScript JSON-RPC process that hya launches for
`plugins:` entries with `kind: compat` and no explicit command.

Targeted Compat packages:

- `@opencode-ai/plugin@1.17.9`
- `@opencode-ai/sdk@1.17.9`

## Runtime Coverage

The adapter currently supports:

- plugin config discovery and initialization
- Compat hook registration translation
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

From `crates/hya-plugin-compat/adapter`:

```sh
bun run typecheck
bun test
```

Set `BUN` to choose a Bun executable or `HYA_COMPAT_ADAPTER_DIR` to point
`hya-backend` at an alternate adapter directory.

Known limits are tracked in
[`../../docs/compat-parity.md`](../../docs/compat-parity.md), especially the
Compat SDK client completeness section.
