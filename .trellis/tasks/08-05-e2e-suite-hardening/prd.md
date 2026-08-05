# E2E test suite hardening

Parent task. Owns the source requirements, the child task map, and the
cross-child acceptance criteria for acting on the 2026-08-05 E2E coverage audit.

## Goal

Turn the process-level E2E suite (`crates/hya-e2e`, "Track P") from an
unenforced, branch-local feature tour into a CI-gated suite with measured
coverage, a live scenario registry, and coverage of the swarm tool surface this
release introduces.

## Source requirement

User request (2026-08-05): act on the four gaps identified by the E2E coverage
audit of this repository. The audit is reproduced below as the requirement
baseline; every number was measured, not estimated.

### Measured baseline (2026-08-05)

Suite location at audit time: `crates/hya-e2e` on branch
`codex/modular-harness-native-swarm-runtime-refresh`, **not** on `main`.

> **Superseded 2026-08-05:** child 1 landed the branch on `main` (pushed,
> `origin/main` = `16bde844`). `crates/hya-e2e`, `crates/hya-bundle`,
> `crates/hya-updater`, and `docs/testing/` now exist on `main`, so children
> 2–5 are unblocked. The Track P numbers below still hold; the corrected
> full-workspace suite size is ~1324 tests.

| Fact | Value | How measured |
| --- | --- | --- |
| Track P result | 19 passed / 0 failed / 0 ignored | `cargo test -p hya-e2e -- --test-threads=1` |
| Track P size | 15 test files, 19 test fns, 2510 LOC (1399 harness + 1111 tests) | `wc -l` |
| Registered scenario IDs | 19 (Track P) + 3 (Track T) + 8 (Track I index-only) | `crates/hya-e2e/matrix.toml` |
| Built-in tool coverage | 8 of 25 primary tool names | `ToolRegistry::builtins()` in `crates/hya-tool/src/tool.rs` vs e2e tool calls |
| HTTP route coverage | 14 of 131 unique routes (~11%) | `.route("…")` in `crates/hya-server/src` vs paths used by e2e |
| CI enforcement | none as a dedicated gate | `.github/workflows/ci.yml` has no e2e step; only a commented `docs/testing/ci-agent-e2e-snippet.yml` |
| `matrix.toml` consumers | zero | repo-wide grep finds no runner reading it |
| Line coverage | never measured | `cargo-llvm-cov` and `cargo-tarpaulin` not installed, no CI collection |

### The four gaps to close

1. **Not gated in CI, and run in a configuration its own docs call unstable.**
   `ci.yml` is byte-identical on `main` and the branch, with no e2e step.
   `cargo test --workspace --jobs 1` does pull `hya-e2e` in, but `--jobs 1`
   bounds codegen parallelism, not test threads — so `p03`, `p11`, `p14`, `p15`
   each run 2 tests concurrently, spawning concurrent backends, which
   `docs/testing/process-e2e.md` explicitly says requires `--test-threads=1`.

2. **Swarm tool surface has zero process-level coverage.** `send`, `roster`,
   `channels`, `join`, `leave`, `list_agents` are the feature line this branch
   is named after and have only in-process tests.

3. **Line coverage is unmeasured.** Scenario inventory exists; code-level
   coverage data does not.

4. **`matrix.toml` is a dead registry.** No runner consumes it, so scenario IDs
   can drift from reality unnoticed. IDs `T1.1`, `T1.6`, `T2.4`–`T2.6` are
   undefined anywhere in the repo — untracked gaps in the numbering.

## Decisions taken during planning

- **D1 — Landing strategy: fast-forward the whole branch into `main`.**
  `main` is a strict ancestor of `codex/modular-harness-native-swarm-runtime-refresh`
  (0 ahead / 75 behind), so the merge is a fast-forward, but it lands the entire
  feature line: 517 files, +91,107 / −4,882 lines, including `hya-bundle`,
  `hya-updater`, vendored `sevenz-rust2-0.20.2`, the swarm runtime, and 129
  changed `hya-server` files. A selective e2e-only cherry-pick was rejected as
  infeasible: `p11_hyabundle` depends on `crates/hya-bundle`, which does not
  exist on `main`. User confirmed the full ff-merge on 2026-08-05.
- **D2 — CI is edited directly.** `.github/workflows/ci.yml` is modified in
  place rather than shipped as a detached patch. The pre-existing snippet's
  "add when the token has workflow write scope" caveat is treated as resolved.
- **D3 — Work happens on `main` after landing.** All four gap-closing children
  target `main`, not the feature branch.

## Constraints

- `main` and `origin/main` are in sync, as are the branch and its remote. The
  merge is therefore an outward-facing publish of the whole feature line.
- The `main` working tree is dirty at planning time: 12 tracked files modified
  or deleted (including `crates/hya-tool/Cargo.toml`, which the branch also
  touches) plus 8 untracked paths. This must be resolved before any merge.
- Every child obeys `AGENTS.md`: TDD gate, area verification commands, commit
  and push rules.
- Oracle strength must not regress. `docs/testing/process-e2e.md` §"Oracle rules
  (do not weaken)" is binding on any new scenario.

## Child task map

Ordering is a real dependency chain, not a preference: children 2–5 all need
`crates/hya-e2e` present on `main`, which only child 1 delivers.

| # | Child | Closes | Depends on |
| --- | --- | --- | --- |
| 1 | `08-05-land-swarm-branch-to-main` | prerequisite (D1) | — |
| 2 | `08-05-ci-gate-e2e-tracks` | Gap 1 | child 1 |
| 3 | `08-05-e2e-swarm-tool-scenarios` | Gap 2 | child 1; lands cleanest after child 2 so new scenarios are gated on arrival |
| 4 | `08-05-coverage-baseline-llvm-cov` | Gap 3 | child 1 |
| 5 | `08-05-matrix-toml-runner` | Gap 4 | child 1; child 3 adds IDs the runner must accept |

## Cross-child acceptance criteria

Verified by the parent at final integration review, after all children are done.

- [ ] `crates/hya-e2e`, `docs/testing/`, and `matrix.toml` all exist on `main`.
- [ ] A clean `main` checkout running the documented CI gate executes Track P
      with `--test-threads=1` and fails the build if any scenario fails.
- [ ] Built-in tool coverage rises from 8/25; every swarm tool (`send`,
      `roster`, `channels`, `join`, `leave`, `list_agents`) is exercised through
      a real backend process by at least one Track P scenario.
- [ ] A line-coverage number for the workspace exists, is reproducible by a
      documented command, and is recorded as a baseline artifact.
- [ ] `matrix.toml` is validated by an automated check that fails on drift
      between registered IDs and actual test functions, and the `T1.1`, `T1.6`,
      `T2.4`–`T2.6` numbering gaps are either defined or formally retired.
- [ ] `docs/testing/README.md` and `agent-matrix.md` describe the state that
      actually exists — no stale "optional" / "not required for the PR gate"
      language once the gate is real.
- [ ] Full quality gate green on `main`: `cargo fmt --all --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`, workspace tests,
      Track P, Track T, and `scripts/verify-no-http.sh`.

## Out of scope

- Raising HTTP route coverage beyond the swarm surface (14/131 stays low by
  design this round; a broader route sweep is a separate future task).
- Provider wire coverage for Anthropic / Google through the process path.
- Plugin-system Track P scenarios.
- Failure-path scenarios (abort mid-turn, provider 429/retry, backend restart,
  MCP disconnect). Recorded here as known remaining gaps, deliberately deferred.
- PTY-driven TUI scenarios beyond the existing presentation smoke test.

## Notes

- Audit evidence and the reasoning behind each gap live in this file; children
  reference it rather than restating it.
- Known remaining gaps after this task tree completes are listed under
  "Out of scope" and should seed the next round of E2E work.
