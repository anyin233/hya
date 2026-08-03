# 0.34.10

- Ship `hya bundle install`, `list`, `uninstall`, and `info`, including
  file-only `info -f`, with atomic registry updates, immutable built-ins, and
  idempotent same-digest installs.
- Publish installed public-static catalogs lazily and atomically for new root
  turns and TUI/catalog refreshes; in-flight and child turns stay pinned to
  their existing catalog snapshots.
- Require the exact lowercase `.hyabundle` suffix while treating package magic
  bytes as format authority. Private packages remain inspection-only with
  `authentication=unverified`, `payload=opaque`, and
  `activation unsupported-in-0.34.10`.
- Activate one static agent definition and its Markdown prompt from each
  installable package. The strict public profile admits no external static-skill
  file, bundle runner, sandbox, executable tool/MCP/hook/JS/Rust activation, or
  new permission plane.
