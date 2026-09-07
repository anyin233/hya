# Implementation Plan

1. Verify every preceding child is finished and its focused/mutation evidence is recorded. Reopen any child with an unresolved contract; do not patch around it here.
2. Search all old public author/prepared/control/status terms and remove obsolete code, tests, fixtures, exports, examples, comments, and claims. Use LSP references when a server is available; otherwise perform exhaustive targeted call-site searches.
3. Update the root ubiquitous language and accepted ADR record. Keep `CONTEXT.md` implementation-free; record only hard-to-reverse, surprising tradeoffs in ADRs.
4. Update user and architecture documents for author syntax, runtime semantics, packaging/plugin contributions, first-party/example bundles, Session recovery, slash/SDK control, and sidebar state.
5. Update installer/release/testing guides and confirm the production Compat adapter plus example assets are included.
6. Archive `0.35.2` notes, write the newest-only `0.36.0` changelog, and align Cargo workspace/lock plus TUI package versions.
7. Run every named mutation once on the integrated tree and revert each mutation.
8. Run the full Rust, Compat, TUI, Track P, Track T, and installer gates from the parent plan.
9. Build/install into an isolated prefix and execute both Workflows. Verify no auto-selection, restart interruption, selection restoration, and exact transcript retention.
10. Drive the actual TUI in a PTY and record observed sidebar states and child navigation.
11. Run Trellis check against all backend/frontend/cross-layer specs. Fix findings and rerun affected plus final gates.
12. Use the commit skill. Stage only intentional `0.36.0` feature/task files, inspect the staged diff/file list, commit `feat(workflow): add user-composed workflow platform`, and push.
13. Record commit, push, gate, smoke, and visual evidence; finish/archive this child and then the parent task.

Rollback: do not tag or publish. Before any non-isolated store is opened by `0.36.0`, preserve a backup because old binaries cannot decode new Events/prepared-v2 rows. If final verification fails, do not commit or push.
