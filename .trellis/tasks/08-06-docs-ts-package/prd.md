# hya-tui-ts package documentation

Child of `08-06-docs-100-percent-coverage`. Work list: the
`packages/hya-tui-ts` entries in `coverage-gap-report.md`.

## Goal

Make `packages/hya-tui-ts` launchable and maintainable from its own documentation.
Today the package has no README, and a reader cannot learn that `src/main.tsx`
requires `--url` and is unlaunchable without a running backend.

## Scope

In scope: `packages/hya-tui-ts/**` — three new README files and TSDoc across the
audited export surface.

Out of scope: `docs/architecture/tui.md`, `docs/tui-reference.md`, and
`docs/tui-keybindings.md`, which belong to child `docs-prose-coverage`. This child
touches no file outside `packages/hya-tui-ts/`.

## New documents required

| Path | Reason |
| --- | --- |
| `packages/hya-tui-ts/README.md` | The package cannot be launched from what is written today |
| `packages/hya-tui-ts/scripts/README.md` | `prune-sdk-server.ts` runs in `install.sh:205` and `release.yml:116`, is asserted by two tests, and has zero comments and zero prose |
| `packages/hya-tui-ts/test/README.md` | Three of eleven suites are architecture guards whose failure messages confuse anyone who has not read them |

### `README.md` outline

What this is (frontend-only; link `UPSTREAM.md` and `docs/architecture/tui.md`) ·
Requirements (bun 1.3.14, a running `hya-backend`) · Install · Commands table
(build, test, typecheck) · Run with the full flag list · Layout (`src/hya` versus
`src/upstream`, theme assets, scripts, test) · Editing rules and upstream re-sync ·
Environment variables · Release-time scripts · the `bunfig.toml` preload note.

### `scripts/README.md` outline

`prune-sdk-server.ts`: `argv[2]` is the runtime directory; it rewrites the SDK
export map so `./v2` resolves to the v2 client, deletes the server and process
dist files, and verifies the result with a spawned import probe. Name its callers
and note it is guarded by `test/runtime-boundary.test.ts`. Then
`generate-logo-art.py`, pointing at its own docstring and
`docs/research/terminal-icon-rendering.md`.

### `test/README.md` outline

Track T scope · which suites need `cargo build -p hya-backend --bin hya-backend` ·
which spawn a PTY · which are invariant guards (boundary, branding-pruning,
runtime-boundary) and what each enforces.

## TSDoc batches

File-disjoint, three writers:

- **S1** — the hya-owned surface: `src/main.tsx`, `src/hya/platform.ts`,
  `src/hya/product.ts`, `src/hya/audit.ts`, `src/hya/static-host.ts`,
  `src/hya/sdk-spine.tsx`, `src/hya/startup-trace.ts`.
- **S2** — the two largest hya-authored files under `src/upstream`:
  `src/upstream/routes/session/subagent-workspace.ts`,
  `src/upstream/routes/session/index.tsx`.
- **S3** — 72 exports with 0 docblocks:
  `src/upstream/feature-plugins/system/diff-viewer-file-tree-utils.ts`,
  `src/upstream/config/keybind.ts`, `src/upstream/keymap.tsx`,
  `src/upstream/config/index.tsx`.

## Requirements

- R1: The three READMEs exist and cover every section in the outlines above.
- R2: Every exported symbol in the S1–S3 files carries a TSDoc block stating
  purpose, parameters, and return value.
- R3: `src/upstream` files are vendored from upstream. Follow the re-sync rules in
  `UPSTREAM.md`; where a file is vendored unchanged, do not add TSDoc that a
  future re-sync would discard — record that constraint in the README instead.
  Confirm which of the S2 and S3 files are hya-authored before editing.
- R4: The README states the `--url` requirement and that the package is
  unlaunchable without a running backend, since that is the first thing a reader
  gets wrong.
- R5: No behavior change. Comments and Markdown only.

## Acceptance Criteria

- [x] AC1: The three README files exist and each covers its outlined sections.
- [x] AC2: Every exported symbol in the S1–S3 files carries a TSDoc block, or is
      listed in the README as vendored-unchanged and therefore excluded.
- [x] AC3: `bun run typecheck` passes.
- [x] AC4: `bun test` is no worse than the pre-task baseline, including
      `test/runtime-boundary.test.ts` and the branding-pruning guard.
- [x] AC5: `git diff` for this child touches only files under
      `packages/hya-tui-ts/`, and only comments and Markdown.
- [x] AC6: `packages/hya-tui-ts/README.md` is linked from `docs/README.md` — the
      link itself is added by child `docs-prose-coverage`'s reconciliation pass,
      so this child only confirms the target exists.

## Notes

- R3 is the one real judgment call here. Adding TSDoc to a file that is re-synced
  verbatim from upstream creates churn that the next sync silently reverts. The
  audit flagged S2 and S3 as "hya-authored files under `src/upstream`", but that
  must be confirmed per file against `UPSTREAM.md` before writing.

## Outcome (2026-08-07)

All six acceptance criteria met.

- `README.md` (197 lines), `scripts/README.md` (64), `test/README.md` (120).
- S1 export surface: **19 exports, 19 with TSDoc**.
- S2/S3 were deliberately NOT given TSDoc. `src/upstream` is the vendored
  OpenCode frontend boundary per `UPSTREAM.md`, so TSDoc added there is discarded
  on the next re-sync. The exclusion and its reasoning are recorded in the package
  README under "Vendored-excluded TSDoc surface (R3)" — which is the escape hatch
  AC2 allows, not a shortfall.
- `bun run typecheck` passes; `bun test` 50/50 across 11 files.
- Diff touches only `packages/hya-tui-ts/`, comments and Markdown only.
