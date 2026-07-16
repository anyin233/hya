# Design

## Boundaries

- Persistent user configuration: `$HOME/.config/hya/config.yaml`; back it up
  privately, update only `default_model` and `providers.12th-oai.models`, and
  validate through the installed CLI before any prompt.
- Disposable execution state: a mode-`0700` directory under `/tmp/opencode`
  owns SQLite, backend logs, PTY capture, session/event evidence, and helper
  state. It is removed after a redacted result summary is recorded.
- Installed runtime only: `$HOME/.local/bin/hya-backend` and
  `$HOME/.local/bin/hya-ts`; no workspace binaries.
- User intent enters only through the main `hya-ts` prompt. Subagents are
  observed through canonical events and the TUI's read-only manager/panes.

## Runtime Topology

```text
PTY -> installed hya-ts -> loopback Compat HTTP/SSE
                         -> installed hya-backend
                         -> disposable SQLite event log
                         -> 12th-oai/gpt-5.6-sol
```

The backend starts with explicit `--model 12th-oai/gpt-5.6-sol`, bounded
`HYA_SUBAGENT_*` environment overrides, a loopback ephemeral port, and the
disposable database. The frontend attaches with the same explicit model. This
avoids relying on default-selection ambiguity while still proving the persisted
default through a separate CLI/catalog check.

## Capability Matrix

1. Discovery: one exact `list_agents` call.
2. Foreground: one `general` child with explicit model and nonce.
3. Resume: reuse that child via `task_id` and assert no second child creation.
4. Parallel/category/inline: one two-member call; one category-routed general
   child and one named inline child.
5. Background: immediate running result, then later terminal child event.
6. Nested: one child spawns one grandchild at depth 2.
7. Resident/team: inline resident joins a nonce channel and reports ready.
8. Parent reads roster/channels and sends direct/channel mail; resident replies.
9. Resident leaves; final roster/channels and quiescence are verified.
10. TUI manager opens one live resident as a read-only tab or split; typed text
    must not mutate the main prompt while the observation has focus.

Each slice uses a unique nonce. Pass/fail comes from correlated tool call/result
IDs, `SessionCreated` ancestry/model, `Member*`, `AgentRegistered`,
`AgentActivityChanged`, `MailSent`, `ChannelJoined`, and `ChannelLeft` events.

## Controls

- Maximum depth 2, concurrency 2, per-run spawn budget 8, resident turn budget
  16, and mail budget 12.
- No `--yolo`. Automation may answer only expected `task` permissions once;
  every unrelated permission is rejected and ends the slice.
- One rewritten prompt is allowed when the model answers without the requested
  tool. Authentication, protocol, tool errors, wrong model routing, or budget
  kills receive no blind retry.
- Stop the whole backend to terminate an uncooperative background/resident run;
  no unsupported per-job cancellation is assumed.

## Rollback

- Before configuration, copy the current config to the private run directory
  with mode `0600` and record its SHA-256.
- If configuration validation fails, atomically restore that backup.
- A successful E2E keeps `gpt-5.6-sol` as the user's configured default per the
  request. Any runtime defect leaves source untouched and is recorded before a
  separate TDD fix decision.
