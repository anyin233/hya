# E2E debug GPT 5.6 Sol subagents

## Goal

Configure the installed hya `0.33.1` user environment to use
`12th-oai/gpt-5.6-sol`, then drive the installed `hya-ts` through a real PTY
and verify every supported model-facing subagent capability with canonical
event/session evidence.

## Confirmed Facts

- Installed `hya`, `hya-backend`, and the prepared `hya-ts` runtime are
  `0.33.1` under `$HOME/.local`.
- The existing `12th-oai` provider is OpenAI-compatible at the already-working
  `https://api.12th.day/v1` API root and has a mode-`0600` saved auth token.
- The provider currently serves `gpt-5.4` and `gpt-5.5`; `gpt-5.6-sol` must be
  added to its model list and selected as `default_model`.
- The supported subagent tools are `list_agents`, `task`, `roster`, `send`,
  `channels`, `join`, and `leave`. `task` covers foreground, resume,
  multi-member, background, nested, category/model override, inline-agent, and
  resident execution.
- The separate experimental background-subagent control endpoint is disabled
  and is not part of this E2E.

## Requirements

- Preserve all existing providers, MCP servers, plugins, commands, and auth;
  modify only the default model and `12th-oai.models`, with a private backup and
  exact rollback path.
- Launch installed `hya-backend` on loopback with a disposable SQLite database
  and attach installed `hya-ts` in a real PTY using the exact model route.
- Drive prompts through `hya-ts`; use HTTP/session replay only for read-only
  evidence collection and expected permission replies needed by automation.
- Verify discovery; foreground; resume; parallel category/inline members;
  background; nested spawn; resident registration; roster; direct and channel
  mail; join; leave; quiescence; and read-only TUI subagent observation.
- Treat parent prose as untrusted test evidence. Require matching tool calls,
  tool results, child session ancestry/model, and team events.
- Reject shell, edit, write, external-directory, web, and unrelated tool
  requests. Keep all generated DB, transcripts, logs, and temporary agent
  definitions outside the repository.
- Bound depth to 2, concurrency to 2, and resident turn/message budgets to
  prevent runaway fan-out or mail loops.
- Stop on transport/auth/runtime failures; permit at most one rewritten prompt
  for a model-adherence failure.
- Preserve every pre-existing dirty worktree path and leave no hya/tmux process
  or credential-bearing artifact after the run.

## Acceptance Criteria

- [ ] `hya-backend models` and the live provider catalog expose
      `12th-oai/gpt-5.6-sol` with tool capability, and every created root/child
      session records that exact route.
- [ ] A real `hya-ts` turn invokes `list_agents` and returns the effective
      non-hidden catalog.
- [ ] Foreground spawn and `task_id` resume use the same child session and
      produce distinct nonce-bearing turns.
- [ ] One multi-member call creates two concurrent children, including category
      resolution and an ephemeral inline agent.
- [ ] Background spawn returns a running job/session before a later terminal
      member event; nested spawn creates exactly root -> child -> grandchild.
- [ ] A resident registers, joins a channel, appears in roster/channels, wakes
      for direct and channel mail, replies to main, leaves the channel, and
      returns idle without a runaway synthesis loop.
- [ ] `hya-ts` renders subagent status and opens at least one read-only
      observation view without routing typed input to the child.
- [ ] Every requested tool call has a matching result and no tool error; all
      required canonical events replay from the disposable SQLite database.
- [ ] User config retains the new default/model entry, repository status matches
      its pre-run baseline, credentials are absent from retained evidence, and
      no test process or temporary runtime remains.

## Out of Scope

- Unsupported subagent cancel/status/result APIs, worktree/tmux management
  tools, load testing, release work, and source changes unless the E2E exposes a
  reproducible product defect.

## Open Decision

- Approve or replace the recommended execution cap: 30 remote model turns,
  30 minutes, and one model-adherence retry per slice.
