# yaca OpenCode Adapter

This package will host the Bun-based OpenCode plugin adapter. The current
skeleton is intentionally small: it gives `yaca-cli` a stable command target for
`kind: opencode` plugins while preserving the existing Rust build when Bun is not
installed.

Targeted OpenCode packages:

- `@opencode-ai/plugin@1.17.9`
- `@opencode-ai/sdk@1.17.9`

Unsupported in this skeleton: plugin discovery, hook translation, SDK shims, and
OpenCode `tool:` execution. Those are implemented in later adapter phases.
