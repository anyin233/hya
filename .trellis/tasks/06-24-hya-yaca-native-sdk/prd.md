# Wire hya frontend to yaca backend via native in-process Rust SDK (no HTTP)

## Goal

Replace hya-sdk HTTP/reqwest + SSE transport with a native in-process Rust binding: build yaca_server::router(AppState) in-process, drive via tower::oneshot, bridge GlobalBus events in-process. Accept gate: hya completes a full session turn against yaca with ZERO HTTP calls (no TCP listener, no reqwest).

## Requirements

- TBD

## Acceptance Criteria

- [ ] TBD

## Notes

- Keep `prd.md` focused on requirements, constraints, and acceptance criteria.
- Lightweight tasks can remain PRD-only.
- For complex tasks, add `design.md` for technical design and `implement.md` for execution planning before `task.py start`.
