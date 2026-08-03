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

## AgentBundle sidecar mode (0.34.11)

Configured Compat plugins retain their normal discovery and initialization
surface. Bundle mode is narrower: `hya-app` materializes one validated public
Bundle closure for one activation and starts one Bun Compat child with
`activation_id` and `lifecycle` initialization metadata. The child must ACK
matching declarations before Harness marks work Running or polls the model.

`initialize`, `tool/call`, and `hook/*` are request/reply; `event` is a one-way
notification with no id or result. stdout is protocol-only and stderr is a
bounded diagnostic stream. The sidecar never runs an agent loop, receives a
task, returns an agent result, or exposes sidecar send/wait. Transient mode
shuts down and reaps one child after the activation; resident mode reuses one
healthy child across mailbox messages and explicitly creates a fresh child
after loss. There is no transparent retry or replay.

`hya-plugin` owns the raw child, stdio tasks, bounded stderr, exit state,
shutdown, kill-on-drop, and reap. The Bun Compat adapter implements the
Bundle-mode protocol; `hya-app` owns captured-catalog resolution/materialization;
Harness owns the logical runtime and permission/result path. Existing
configured-plugin mode and its tests remain unchanged.
