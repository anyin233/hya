# Implementation Plan

## 1. Preflight And Configure

- Record installed binary/runtime versions, repository dirty baseline, auth-file
  metadata, and current redacted provider configuration.
- Create a private disposable run directory and mode-`0600` config backup.
- Atomically add `gpt-5.6-sol` to `providers.12th-oai.models` and set
  `default_model: 12th-oai/gpt-5.6-sol`; preserve every unrelated YAML value.
- Validate YAML, file permissions, `hya-backend models`, and exact model catalog.
- Roll back immediately on any validation failure.

## 2. Start Observable E2E Stack

- Start installed `hya-backend` on `127.0.0.1:0` with the disposable DB,
  explicit model, and bounded `HYA_SUBAGENT_*` values.
- Parse the announced URL and verify health, provider/model catalog, agent
  catalog, and disabled experimental background-control capability.
- Start installed `hya-ts` in a real PTY with the explicit model; capture a
  sanitized transcript and root session ID.

## 3. Execute Ordered Slices

- Run discovery, foreground, resume, parallel/category/inline, background,
  nested, resident/team, mail/channel, leave/quiescence, and TUI observation in
  order.
- After each prompt, wait on canonical event predicates rather than fixed sleeps.
- Reply once only to the expected `task` permission; reject any other permission.
- Save redacted per-slice evidence and stop on the first
  non-adherence-independent defect. Permit one tighter-prompt retry only for
  missing requested tool calls.

## 4. Diagnose Any Failure

- Use the first failing canonical boundary to classify configuration, provider
  streaming/tool decode, permission, governor, spawn, resident wake, projection,
  SSE, or TUI reduction/rendering.
- Build the smallest agent-runnable repro before changing source. If a defect is
  confirmed, add an atomic RED regression at the owning seam, make the minimum
  fix, and rerun the failed slice plus relevant CI gates.
- Any fix must include the required `0.33.2` version/changelog update and normal
  commit/push workflow; no speculative source change is part of this plan.

## 5. Finalize

- Replay root/child/grandchild sessions from SQLite; verify route, ancestry,
  tool/result correlation, lifecycle, team events, and bounded completion.
- Stop hya/PTTY helpers, scan retained evidence for credential strings, compare
  repository status with baseline, and remove the private run directory.
- Update task progress/spec only with durable findings, run an independent
  read-only check, and archive/commit/push task records when all criteria pass.

## Validation Gates

- Configuration: YAML parse, exact preserved keys, config mode, model list, and
  live provider catalog.
- E2E: nonce assertions plus canonical event/session replay for every slice.
- TUI: real PTY route/model display, task/resident status, observation open, and
  read-only input invariant.
- Hygiene: no token in logs, no leftover process/runtime, unchanged repository
  dirty baseline, and `git diff --check` for any task/source edits.
