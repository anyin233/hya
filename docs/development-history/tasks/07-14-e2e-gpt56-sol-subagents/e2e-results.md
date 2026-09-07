# E2E Results

## Run Boundary

- Installed runtime: `hya-backend 0.33.1` and real installed `hya-ts` PTYs.
- Route: `12th-oai/gpt-5.6-sol` for every created root and child session.
- Limits: depth `2`, concurrency `2`, spawn budget `8`, resident turn budget
  `16`, message budget `12`; no `--yolo`.
- Usage: 17 successful provider rounds plus one failed provider request, below
  the approved 30-call cap.

## Passed Slices

- Discovery: root `hysec_92uxTd7xCGB6RcfNM1I2`; exactly one `list_agents`
  request/result and no extra tool.
- Foreground: child `hysec_dQIvdB53aieiMyifQmKA`; exact parent/model,
  `FOREGROUND_CHILD_OK F56B8`, complete member lifecycle, correlated result.
- Resume: reused `hysec_dQIvdB53aieiMyifQmKA`; no second child was created and
  the same event log contains `RESUME_CHILD_OK R56C9`.
- Parallel/category/inline: disposable category root
  `hysec_AGYrW2XzR6vmGtIKKzTC`; children
  `hysec_Po6sxse9MwZPH0xdurCj` (`general`, configured category) and
  `hysec_V9ka2M90Ifrv94zprBpl` (`inline-e2e`) were created at the same
  timestamp, used the exact route, and returned distinct nonce summaries.
- Background: child `hysec_OvOnNgbIxPVBM422wgzP`; running result at root event
  sequence `273` preceded spawn/running/finished events `274`, `276`, and `290`;
  terminal summary was `BACKGROUND_CHILD_OK B56E1`.
- TUI routing/status: both real PTYs rendered `gpt-5.6-sol` / `12th-oai` and
  subagent status. Child-view navigation was not proven and is not counted.

## Stop Condition

- Nested root request was event `403`; depth-1 child
  `hysec_nlTgwOsbqSlCSKBwa9y3` was created on the exact route and admitted its
  prompt at events `409`-`413`.
- The child started an assistant response at event `414`, then finished with
  `error` at `415` before any `StepStarted`, tool call, or permission request.
- Root `MemberFinished` event `416` and correlated task result `422` record
  `http: 524 <unknown status code>: error code: 524` after 130418 ms.
- This proves the request passed spawn/governor admission and failed at the
  upstream provider transport boundary. Per the approved plan, no retry or
  source change was attempted.

## Not Run

- Resident registration/wake, roster, direct/channel send, join, leave,
  quiescence, and read-only child-input isolation were skipped after the HTTP
  524 immediate-stop condition.

## Hygiene

- Expected `task` permissions only were approved once; no unrelated permission
  appeared.
- Final user config is mode `0600`, structurally differs from its backup only by
  the requested default/model entry, and auth hash is unchanged.
- Offline SQLite replay reproduced the passing slices and nested failure.
- Credential scan passed; all PTYs/backends were stopped before private runtime
  artifacts were removed.
