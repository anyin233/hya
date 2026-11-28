# ADR-0040: The context kernel is the only core

Date: 2026-11-28
Status: Accepted

## Context

[ADR-0039](0039-hya-bare-and-v2-modular-architecture.md) split hya into
`hya-bare` and fourteen first-party bundles under `bundles/builtin/`. The split
was done at the packaging level: each domain got a bundle directory, a native
library, and a manifest. Most of the business logic did not move with it. On
`8a7d52dc` about 14.6k production lines of bundle domain code still live under
`crates/` (resident teams, mailboxes, Workflow compilation, goal and loop entry
points, LSP and formatter planes), and `hya --bare` still starts the
multi-agent supervisor and the mailbox service. The Native ABI host contract
speaks the multi-agent and channel domains, so a third-party bundle cannot
offer an alternative to a first-party one.

hya is meant to be a harness where a user can inject logic at any point. That
only holds if the first-party bundles use the same injection points as
everyone else, and if the core has no second code path for them.

## Decision

1. **The core is the context-maintenance kernel.** It owns the event log, the
   deterministic Projection, the context View $(m, g, w)$, the three
   primitives (model append, tool append, rewrite), the turn loop, provider
   routing, the tool registry and permission plane, and the standard kernel
   tools.
2. **Everything else is a bundle.** The core exposes generic injection
   points: tools, rewriters, hooks, services, BundleApi, projection reducers,
   URI schemes, drivers, CLI commands, and TUI contributions. It never names a
   bundle or a bundle concept.
3. **First-party bundles get no privileges.** They load through the public
   Native ABI v2 and `@hya/tui-sdk` v2 only, like a third-party bundle.
4. **One turn path, one interface.** Every turn runs through
   `kernel::Harness`, and each harness callback dispatches to one ordered hook
   chain. A turn whose chains are all empty is the bare kernel. There is no
   separate bare or full turn path.
5. **Bare to full is composition, not a switch.** The runtime's only input is
   the set of enabled bundles. Any subset is a valid configuration: bare,
   bare+A, bare+A+B, and so on. Full is shorthand for "enable every builtin
   bundle". A bundle that needs another declares it as a prerequisite, and
   the loader rejects a set that misses one.
6. **Bare is already fully extensible.** MCP, Code Mode, the plugin host, and
   skills stay in bare. The five standard tools (`read`, `write`, `edit`,
   `bash`, `grep`) stay in core, together with `todo`, `rewrite`, and `skill`.
7. **Every injection point has a consumer.** An injection point exists
   because a bundle uses it today. The core does not add a point, or a second
   implementation of one, speculatively.
8. **Rings.** The core is three rings, each depending only on the rings
   inside it:
   - **R0, context kernel:** event log, Projection, View, primitives, turn
     loop, providers, tool registry, permission plane, standard tools.
   - **R1, injection host:** the engine's extension host, hook chains,
     runtime registry, Native ABI, and the bundle loader.
   - **R2, serving surface:** the `/v2` server, the CLI, and the daemon.

   The TUI, the WebUI, and the SDK are clients of `/v2`, not core. Bundle
   authoring and installation tooling is not core either. Size reports count
   the standard tools and the clients on their own rows.

### Boundary guard

`cargo test -p xtask` runs a boundary guard configured by
`crates/xtask/boundary.toml`. It fails when:

- a crate under `crates/` depends on a crate from `bundles/` (any dependency
  table);
- core source names a bundle domain word in an identifier. The denylist is
  `subagent`, `resident`, `episode`, `team`, `mailbox`, `channel`,
  `workflow`, `goal`, `loop_driver`, `lsp`, and `formatter`. Words are matched
  on `snake_case` and `camelCase` boundaries in Rust, TypeScript, and
  protobuf identifiers; comments, string literals, tests, and generated code
  do not count. A short exemption list covers generic vocabulary that shares
  a word, such as `std::fmt::Formatter` and async `mpsc::channel` queues;
- a crate depends (normal or build dependency) on a crate of an outer ring, a
  client, or tooling. The `[rings]` map places every directory under
  `crates/` and `packages/` in exactly one group: R0, R1, R2, the `hya`
  executable (which composes R2 with bundle tooling and the client library,
  and on which nothing depends), clients, bundle tooling, or repository
  tooling. `cargo run -p xtask -- ring-report` counts production lines per
  ring from the same map, with the five standard tools on their own row.

The guard started with an allowlist of the violations on `8a7d52dc`. An entry
that no longer matches a violation also fails the guard, so the allowlist
only shrinks. The core is sealed when the allowlist is empty.

### Workflow resources are validated by their bundle

Core bundle preparation checks only what every bundle shares: the manifest
schema, resource paths, and digests. The Workflow bundle compiles and
validates its `workflows/` resources when it registers and reports errors
through the normal bundle load diagnostics. This gives up install-time
validation of Workflow sources; in exchange, the core stops linking a domain
compiler.

## Consequences

- A replacement for any first-party bundle can be written against
  `hya-runtime-abi` and `@hya/tui-sdk` alone.
- Each moved domain disappears from bare: with a bundle disabled, none of its
  services, tools, hooks, URI schemes, or CLI commands exist.
- The Native ABI changes in lockstep while the host contract loses its
  domain methods; `abi_digest_v2` changes with it.
- Users who relied on install-time Workflow validation see Workflow errors at
  bundle load instead.

## Alternatives rejected

1. **Keep first-party bundles privileged.** Hand-wiring first-party services
   in the engine is faster to write, but it keeps their concepts in the core
   and leaves third parties a weaker contract than first parties have.
2. **A bare/full mode switch.** Two modes double the test surface and hide
   configurations between them; composition makes every subset testable.
3. **A convention instead of a guard.** The split from ADR-0039 was a
   convention, and the domain code stayed in the core. A failing check keeps
   the boundary from eroding again.

See [ADR-0033](0033-three-primitive-kernel.md),
[ADR-0034](0034-context-is-m-g-and-w.md),
[ADR-0035](0035-full-mode-follows-the-kernel.md), and
[ADR-0039](0039-hya-bare-and-v2-modular-architecture.md).
