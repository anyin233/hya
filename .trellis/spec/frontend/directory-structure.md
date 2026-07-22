# Directory Structure

> How frontend code is organized in this project.

---

## Overview

The shipped terminal frontend lives in `packages/hya-tui-ts`. The small Rust
packages around it are process boundaries, not presentation layers:

- `crates/hya` replaces itself with adjacent `hya-ts`.
- `crates/hya-ts` owns CLI parsing, runtime/backend discovery, process-group
  handoff, and cleanup.
- `packages/hya-tui-ts` owns terminal rendering and interaction.
- `hya-backend` owns runtime composition and the HTTP/SSE surface.

`crates/hya-tui` and `crates/hya-tui-lib` are retained compatibility crates. No
shipped binary launches them, so new interactive behavior does not belong there.

---

## Directory Layout

```text
packages/hya-tui-ts/
|-- src/
|   |-- main.tsx             # Bun entrypoint
|   |-- hya/                 # hya-owned product/platform/SDK integration
|   `-- upstream/            # retained SolidJS/OpenTUI frontend boundary
|       |-- component/
|       |-- context/
|       |-- feature-plugins/
|       |-- prompt/
|       |-- routes/
|       |-- theme/
|       |-- ui/
|       `-- util/
|-- test/                    # Bun tests
|-- scripts/                 # runtime preparation and asset generation
|-- package.json
|-- bun.lock
|-- bunfig.toml
`-- tsconfig.json

crates/hya/src/main.rs       # canonical exec shim
crates/hya-ts/src/           # supervisor library and binary
```

---

## Module Organization

- Put hya-specific platform, product, boundary validation, or SDK integration in
  `src/hya/`.
- Keep retained frontend components, routes, contexts, themes, and utilities in
  their existing `src/upstream/` domain directory.
- Reuse the existing context/provider and feature-plugin boundaries before
  adding another cross-cutting state path.
- Keep backend execution and persistence out of the package. Access them through
  `@opencode-ai/sdk/v2` over the configured server URL.
- Keep launcher concerns in `crates/hya-ts`; do not teach the frontend to spawn
  or locate `hya-backend`.

The upstream provenance and excluded boundary are recorded in
`packages/hya-tui-ts/UPSTREAM.md`.

---

## Naming Conventions

Follow the surrounding TypeScript module. Use `kebab-case` filenames in the
retained frontend, `PascalCase` for Solid components, and `camelCase` for
functions, signals, and context accessors.
