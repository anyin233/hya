# 0.43.24

## Fixes

- **The `ert` checkout includes the upstream merge and its TUI features.** Reconciled the old `ert` history with the current 0.43.x frontend, preserving the branch's commits and the older release notes under `docs/changes/ert/`.
- **Direct gRPC connections work in the current OpenTUI frontend.** `bun packages/hya-tui/src/main.ts --grpc HOST:PORT --dir PATH` connects to a `hya.v1` listener without starting a daemon. Sessions, turns, event streams, and the API command view use the same gRPC contract; the package carries its protobuf definitions for installed releases.

The previous 0.43.23 notes are archived in `docs/changes/CHANGELOG_0.43.23.md`.
