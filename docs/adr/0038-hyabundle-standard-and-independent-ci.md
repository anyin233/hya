# ADR-0038: Standardized `hyabundle` format, `bundles/builtin/` layout, and independent Bundle CI

Date: 2026-11-27
Status: Accepted

## Context

Hya's bundle ecosystem grew organically across five top-level source directories (`bundles/presets/`, `bundles/first-party/`, `bundles/internal/`, `bundles/extra/`, and `bundles/examples/`) and four overlapping extension mechanisms (`extensions.js`, `extensions.process`, `extensions.rust`, and `extensions.libraries`). This created four concrete problems:

1. **Fragmented repository layout:** Built-in bundles that ship inside every release archive were split across three separate directories (`presets`, `first-party`, and `internal`), forcing loaders, rollback snapshots, release scripts, and documentation to track three lists for one release tier.
2. **Inconsistent manifest and descriptor shapes:** JS bundles pointed tool and hook resources directly at `.ts` files, native bundles used `exposure.yaml` alongside empty `{}` placeholder declarations and manually enumerated `native/src/*.rs` under `extensions.files`, and slash commands lacked a first-class `resources.commands` section.
3. **Coupled CI pipelines:** Editing an optional extra bundle under `bundles/extra/` or an example under `bundles/examples/` triggered Hya's full core workspace CI (`ci.yml`: Rust lint/tests, `hya-e2e`, and TUI suites). Packaging also depended on `cargo run -p xtask -- package-bundle`, preventing external or future Marketplace repositories from validating bundles without checking out the Hya Rust workspace.
4. **Unclear runtime contract:** Authors had no single canonical specification distinguishing JS Bundles (out-of-process stdio JSON-RPC 2.0, Claude Code Plugin compatible), Native Bundles (in-process Rust dynamic libraries with ABI verification), and Static Bundles.

## Decision

1. **Unify repository bundle directories into three tiers:**
   - Consolidate all 13 built-in first-party bundles (`presets/*`, `first-party/*`, `internal/compaction`) into `bundles/builtin/`.
   - Keep `bundles/extra/` (6 optional official bundles, structured for future extraction to an independent Marketplace repository) and `bundles/examples/` (developer examples) separate from built-ins.
   - Preserve `hya exec --bare` independence: `bundles/builtin/` upgrades bare mode to full mode, while `--bare` continues running the 6 kernel tools with zero bundle loads (ADR-0033, ADR-0035).

2. **Standardize `hyabundle` (`schema_version: 2`) while preserving v1 compatibility:**
   - Converge runtime declarations under a top-level `runtime:` block (`kind: static | js | native`) with `assets:` replacing `extensions.files` for runtime data files, `resources.commands` promoted to a first-class resource family, and `agents:` supporting standalone Markdown + YAML frontmatter files (`agents/*.md`, aligned with Claude Code Plugins) alongside inline entries.
   - Standardize five descriptor schemas shared across JS and Native runtimes (`tools/`, `hooks/`, `mcp/` for both `stdio` and remote `http`/`sse`, `agents/*.md`, and `skills/` + `commands/`), and drop the requirement for native bundles to list `native/src/*.rs` source files in `bundle.yaml`.
   - Keep `schema_version: 1` (`extensions:`) readable during migration while rejecting conflicting simultaneous `runtime:` + legacy `extensions.{process,libraries}` declarations.
   - Document Native ABI v1 (lockstep `cdylib` symbols + 32-byte ABI digest, reserving `hya_tool_bundle_register_hooks_v1`) and reserve `abi_version: 2` for a future compiler-independent stable C-ABI.

3. **Provide self-contained bundle CLI packaging and verification:**
   - Add `hya bundle pack <source-dir> <output.hyabundle>` to build deterministic public `.hyabundle` archives without `xtask`.
   - Extend `hya bundle verify` to accept source directories as well as `.hyabundle` archives, adding `--strict` (schema, descriptor, and secret-hygiene checks) and `--protocol-smoke` (black-box JSON-RPC 2.0 `initialize`/`shutdown` parity verification).

4. **Split Bundle CI into two independent workflows plus a portable runner:**
   - Add a zero-dependency runner under `scripts/bundle-ci/` (`discover.mjs`, `verify.mjs`) capable of running both in-repo and verbatim in standalone Marketplace repositories.
   - Add `.github/workflows/builtin-bundles.yml` (`builtin-bundles`) dedicated to `bundles/builtin/**`: static validation, per-crate native tests + `cdylib` ABI symbol checks, Bun JS/static checks + protocol smoke, bare-vs-full smoke, and `builtin-bundles-gate`.
   - Add `.github/workflows/bundles.yml` (`bundles`) dedicated to `bundles/extra/**` and `bundles/examples/**` with zero dependency on Hya's Rust workspace tests.
   - Update `.github/scripts/ci_changes.py` so edits under `bundles/extra/` and `bundles/examples/` no longer trigger core Rust, `hya-e2e`, or TUI jobs in `.github/workflows/ci.yml`.

## Consequences

- Every built-in bundle lives under one predictable root (`bundles/builtin/`), simplifying `first_party.rs`, release staging, daemon snapshots, and documentation.
- Changes to `bundles/extra/` and `bundles/examples/` get fast, isolated CI feedback in `bundles.yml` without waiting on Hya's core Rust/E2E/TUI matrix, and `bundles/extra/` can later be lifted into a standalone Marketplace repository alongside `bundles.yml` and `scripts/bundle-ci/`.
- Built-in native and JS bundles receive targeted per-bundle verification (including dynamic-library ABI symbol checks and JSON-RPC protocol smoke tests) in `builtin-bundles.yml`.
- Existing `schema_version: 1` manifests continue to work without breaking current users while bundles migrate incrementally to `schema_version: 2`.

See [hyabundle Specification](../extend/hyabundle-spec.md), [Creating Bundles](../extend/bundles.md), and [Built-in Tools, Agents & Bundles](../reference/tools-and-presets.md).
