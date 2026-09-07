# Findings

- Installed `0.33.1` already has a working OpenAI-compatible `12th-oai`
  provider and private saved auth; only the requested model/default are missing.
- The model-facing subagent surface is seven tools. `task` multiplexes all spawn
  modes; there are no separate cancel/status/result or worktree/tmux tools.
- The experimental background-control endpoint is explicitly disabled, while
  model-facing `task(background: true)` has a real non-blocking runtime path.
- Durable team evidence lives on the team-root event log and is replayable from
  SQLite; parent prose is not sufficient proof.
- The active `0.33.1` build parses `HYA_SUBAGENT_MAX_DEPTH`,
  `HYA_SUBAGENT_MAX_CONCURRENCY`, `HYA_SUBAGENT_BUDGET`,
  `HYA_SUBAGENT_TURN_BUDGET`, and `HYA_SUBAGENT_MESSAGE_BUDGET`; planned values
  map to `2`, `2`, `8`, `16`, and `12`.
- Installed startup contracts are `hya-backend --model <route> --db <path>
  serve --bind 127.0.0.1:0` and `hya-ts --server <url> --model <route>`.
- The server exposes health/provider/model/agent/capability checks, pending
  permissions, and native canonical replay at `/sessions/:id/events`.
- User config validation passed: mode remains `0600`, unrelated YAML is
  structurally identical, the auth-file hash is unchanged, and installed
  `hya-backend models` lists `12th-oai/gpt-5.6-sol` exactly once.
- The persistent user config has no `categories:` registry. A real category
  resolution check therefore requires a disposable `XDG_CONFIG_HOME`; passing
  an unconfigured category would only test fallback and is not acceptable
  evidence.
- Installed GPT 5.6 Sol passed discovery, foreground, same-session resume,
  concurrent category/inline, and non-blocking background execution with
  canonical event evidence.
- The nested child passed depth/budget/semaphore admission and recorded its user
  prompt, but the provider returned HTTP 524 before `StepStarted` or any nested
  tool call. This is an upstream transport failure, not a governor deadlock.
- The approved immediate-stop rule left resident/mailbox and full read-only TUI
  observation untested; no source defect or source change was established.
