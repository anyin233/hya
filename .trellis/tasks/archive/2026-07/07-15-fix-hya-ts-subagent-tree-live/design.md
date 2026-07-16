# Design

## Resolution Rule

No product source changes before a child-bearing run fails at an owned boundary.
The task has three outcomes:

1. Current `0.33.7` passes after reinstall: close as not reproduced, with no
   version, changelog, source, commit, or push churn.
2. Provider, permission, timing, or harness failure: preserve sanitized evidence
   and report the block; do not patch product code.
3. Endpoint, parser, resource, rendering, observation, or retry failure: replay
   it from the same SQLite store, add one boundary-owned RED test, then make the
   smallest root-cause fix.

The passing branch does not claim a historical root cause. It establishes only
that the installed child-bearing contract works after the `0.33.7` reinstall.

## Runtime Boundary

```text
xterm PTY -> installed hya -> one root prompt
                               -> one foreground task call
                                  -> build child: Rust runtime/tree summary
                                  -> build child: TypeScript roster/test summary

xterm PTY -> installed hya-ts --server <same backend> --session <same root>
                                      -> GET /session/{root}/tree

installed hya-backend -> loopback HTTP/SSE -> private temporary SQLite
                      -> 12th-oai/gpt-5.6-sol
```

One mode-`0700` directory under `/tmp/opencode` owns the SQLite database and
temporary PTY transcripts. Both frontends attach to the same explicit backend;
no auto-spawned in-memory backend participates. Temporary transcripts may be
used for assertions but are deleted and never copied into task evidence.

The backend runs without `--yolo` and with:

- `HYA_SUBAGENT_MAX_DEPTH=1`
- `HYA_SUBAGENT_MAX_CONCURRENCY=2`
- `HYA_SUBAGENT_BUDGET=2`
- `HYA_SUBAGENT_TURN_BUDGET=8`
- `HYA_SUBAGENT_MESSAGE_BUDGET=8`

The root gets one prompt and a ten-minute wall timeout. Under the current
`0.33.8` default permission model, `task` is allowed without prompting. If an
effective strict/configured policy instead asks, only the two expected `task`
permissions for the two `build` members may be approved once. Any other
permission, write, edit, shell, extra task, nested task, third child, or
unexpected question stops the run.

## Evidence Contract

Model prose is evidence only for the requested summary content. Runtime claims
come from the shared event-sourced store and public boundary:

| Contract | Evidence |
|---|---|
| Two distinct assignments | Root task input plus two admitted child sessions |
| Recursive tree | `GET /session/{root}/tree` and production `parseRunTree` |
| Child type/status/roster | Parsed member and available roster fields |
| Installed roster | Transcript-synchronized `Ctrl+X O` frame |
| Read-only child | Focused child header and absent child prompt submission |
| Retry | Existing PTY test's retained failure and exact request count |
| Root synthesis | Final root response contains both assignment labels |

Only field names, counts, session IDs, status values, labels, and sanitized
failure summaries may remain in task records. Credentials, headers, full
prompts, provider bodies, and full transcripts do not.

## Defect Branch

The first failing boundary owns the single regression:

- Tree lineage, assembly, or roster metadata:
  `crates/hya-app/tests/nested_spawn_tree.rs`.
- TypeScript parsing, loader state, or retry transition:
  `packages/hya-tui-ts/test/subagent-workspace.test.ts`.
- Visible roster, input wiring, or read-only observation:
  `packages/hya-tui-ts/test/pty-smoke.test.ts`.

A source edit requires a stable failure on the same backend and store, followed
by an expected RED in one of those locations. No second tree projection,
endpoint, compatibility path, generic retry layer, or diagnostic framework is
introduced without evidence that the existing owner cannot satisfy the
contract.

## Planner Merge

- Some planners proposed an API-created root. The merged plan uses the real
  `hya` TUI for root creation and prompt submission because the user explicitly
  requested a live `hya` spawn; HTTP is read-only observation and diagnosis.
- Some planners proposed a provider counting relay. The merged plan uses one
  prompt, governor limits, and a wall timeout because this is a two-child run;
  a relay would add a provider-protocol failure source.
- Some planners proposed a one-shot live HTTP fault proxy. The merged plan keeps
  the existing deterministic PTY retry test and uses the real backend for the
  natural child-bearing path. A proxy is added only if a live-only retry defect
  is actually observed.
- All planners preferred a no-source-change result when the current installed
  path passes. The merge adopts that outcome and avoids manufactured release or
  commit changes.

## Cleanup

On every exit path, stop both PTYs and the backend, wait for process exit, remove
the temporary SQLite database and transcripts, verify the listener is closed,
and compare repository status with the baseline. Never revert unrelated dirty
files.
