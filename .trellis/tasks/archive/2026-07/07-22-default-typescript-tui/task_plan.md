# Task Plan

## Goal

Ship the smallest verified migration that makes the TypeScript TUI the default `hya` experience and removes the Rust TUI executable path.

## Phases

- [complete] Trace command, runtime, packaging, and test paths.
- [complete] Resolve product and compatibility decisions.
- [complete] Merge parallel planning reviews.
- [complete] Write and review Trellis planning artifacts.
- [complete] Activate the task after user review.
- [complete] Migrate the shared launcher CLI and default `hya` entrypoint.
- [complete] Route bare-backend sessions through `--session`.
- [complete] Verify installed and packaged startup chains.
- [complete] Update active documentation and `0.33.29` release metadata.
- [complete] Run full verification and review the final diff/status.
- [pending] Main agent: review durable specs, commit, push, and archive the task.

## Errors

- CodeGraph did not return the requested `hya-ts` launcher symbols for two exact queries; use targeted raw reads for those files as permitted for uncovered indexed details.
- An earlier `rg` query targeted the absent task `research/` directory and returned a harmless path-not-found error; this task has no required research artifacts.
- The first final `cargo fmt --all --check` found mechanical formatting drift in `frontend_cli.rs`; `cargo fmt --all` corrected it and the check then passed.
- Final release smoke found `hya --version` unsupported; focused RED tests also exposed canonical launch errors mislabeled as `hya-ts:`. Both are corrected at the shared `argv[0]` identity boundary.
- The first canonical version test revision moved `output.stdout` before formatting `output` in a later assertion; borrowing stdout fixed the test-only compile error before behavioral RED verification.
- The first workspace-wide test run exposed an `ETXTBSY` race while copying the live `hya` executable; writing already-read bytes for the relocated fixture removed the inode race.
- The missing-sibling test exposed generic Rust `Error:` branding; the shim now prints a canonical `hya:` launch error and exits failure.
