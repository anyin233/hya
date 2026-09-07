# Research: omo (oh-my-openagent) Team Mode

Source: librarian deep-dive of `code-yeongyu/oh-my-openagent`
(SHA `1e48f191a2be2ec176a7fe610deebe2c89b25cdf`) + `lazycodex`. The `team_*`
system lives in the Compat/"Ultimate" side; the Codex/"Light" side has no
`team_*` and uses Codex-native `spawn_agent` roles instead.

## TL;DR for our Rust design

A team = a **lead session + 1–8 members** coordinated through a **file-backed
shared mailbox + file-backed shared task board** (state under
`~/.omo/runtime/{teamRunId}`), with **12 typed `team_*` tools**, **category→model
routing**, **isolated child sessions** per member, **message/task-oriented**
(not transcript) result flow, and an explicit **graceful-shutdown / force-delete**
lifecycle. Token savings come from model routing + context isolation + summary
result flow + session-based continuation.

## 1. Orchestration model

- Team = lead + 1–8 members; shared **mailbox** + shared **task list**; runtime
  state under `~/.omo/runtime/{teamRunId}`; optional per-member worktrees + tmux panes.
- 12 tools (registered when `team_mode.enabled`): `team_create, team_delete,
  team_shutdown_request, team_approve_shutdown, team_reject_shutdown,
  team_send_message, team_task_create, team_task_list, team_task_update,
  team_task_get, team_status, team_list`.
- `team_create` loads a named/inline spec → `createTeamRun(...)` with current
  session as `leadSessionId`. Lead session may be **reused** as the lead;
  members spawned **in parallel** via `bgMgr.launch(...)`. Each member gets:
  `parentSessionId = leadSessionId`, `teamRunId`, `skillContent`, resolved model
  + fallback chain, and **question-permission denied** (cannot ask the user).

## 2. Subagent dispatch — categories, models, skills

- **Category members** are NOT direct agents: resolved through `sisyphus-junior`
  + a category-specific model + a prompt append. **Direct** members use explicit
  `subagent_type`.
- Default category→model map (illustrative of the *idea*, not our model list):
  `ultrabrain→gpt-5.5 xhigh`, `deep→gpt-5.5 medium`, `quick→gpt-5.4-mini`,
  `visual-engineering/artistry→gemini-3.1-pro high`, `unspecified-low→sonnet`,
  `unspecified-high→opus max`, `writing→kimi-k2`.
- `resolveCategoryExecution(...)`: merge built-in + user categories → check model
  availability → resolve config → build **fallback chains** → apply category
  prompt append → return `categoryModel / actualModel / fallbackChain`.
- Team mode **strips the global `sisyphus-junior.model` override** before
  category resolution, specifically to avoid collapsing all workers onto one model.
- **Eligible** member agents: `sisyphus, atlas, sisyphus-junior`; conditional
  `hephaestus`; **hard-rejected**: `oracle, librarian, explore, multimodal-looker,
  metis, momus, prometheus` — because members must be able to WRITE mailbox/task
  runtime state (read-only/consultant agents can't be members).
- Skill injection: for ordinary delegated tasks, `load_skills` → background
  launch `skills` + generated `skillContent`. **In team mode**, the inline schema
  accepts `loadSkills` but the member path hardcodes `load_skills: []` and injects
  behavior via member prompt + generated `systemContent` → team mode is currently
  **prompt-driven**, not a full skill-loaded-member runtime. (A known inconsistency.)

## 3. Session/context isolation + result flow

- Each member = a **separate background child session**. The lead does NOT share
  its full transcript; the member gets a fresh session + member prompt +
  `teamRunId` + optional worktree + comms rules + injected system content.
- **Member contract**: plain assistant text is **NOT visible** to teammates/lead;
  members MUST use `team_send_message`; members must NOT peek at another member's
  session/inbox/pane via shell; messages are delivered to recipients as **new
  turns**; broadcast (`to:"*"`) is **lead-only**.
- Result flow is **message/task oriented, not transcript oriented**: members send
  messages + update task status; the lead reads `team_status` / `team_task_list`.
  Mailbox stores atomic per-message files + processed/acked folders.

## 4. Lifecycle / closure contract

1. `team_create` → 2. lead assigns work (`team_send_message` / `team_task_create`)
→ 3. members claim/report (`team_task_update` + `team_send_message`) → 4. lead
`team_shutdown_request` (lead-only) → 5. `team_approve_shutdown` /
`team_reject_shutdown` (target member or lead) → 6. `team_delete` (lead-only;
core delete **rejects active members**).
- Member wrap-up order: mark task completed/failed → re-check task list for newly
  unblocked work → if nothing left, send one `closure-ready` message to lead → idle.
- **`force=true`** recovery path: cancels background tasks, can mark members
  completed, cleans orphaned/deleting state, can tear down while active.
- Shutdown is its **own typed tool path**, NOT a `team_send_message(kind=...)`
  hack (messaging explicitly rejects shutdown kinds).

## 5. bg_* vs ses_*

- `bg_${uuid8}` = async **task** handle (lifecycle / result collection / cancel).
- `ses_...` = child **session** handle (continue the session later).
- Launches return BOTH; tool metadata stores `backgroundTaskId` + `sessionId`.
- Team members are background-by-design: `createTeamRun` calls `bgMgr.launch(...)`
  per member, waiting only long enough to obtain the child session id.

## 6. How token efficiency is actually achieved (4 concrete mechanisms)

1. **Model routing by category** — cheap models for quick work, expensive
   reasoning models for deep logic.
2. **Context isolation** — each worker has its own child session; the lead does
   NOT absorb full worker transcripts into its own context window.
3. **Summary/task-based result flow** — what returns to the lead is messages,
   task status, unread counts, aggregate status — not the whole child context.
4. **Session-based continuation** — child `sessionId` preserved separately from
   `bg` task id → cheap follow-ups without respawning the worker.

## 7. Rust reimplementation lessons

1. **Separate control plane from execution plane.** Control plane = team runtime
   state + mailbox + task board. Execution plane = background worker sessions.
2. **Explicit typed lifecycle tools**, not ad-hoc message conventions, for
   shutdown/delete.
3. **File-backed mailbox/task board with atomic writes** is simple + robust
   (atomic per-message files, processed/acked dirs, claim locks, stale-lock cleanup).
   — In Rust, for a single-process v0 we can use an **in-memory actor/channel**
   control plane and keep the file-backed option for multi-process later.
4. **Default graceful shutdown ≠ force recovery** — model both explicitly.
5. **Members must be writers** — read-only consultant agents can't be team
   members; the gate/verifier (read-only) is therefore NOT a team member, it sits
   ABOVE the team (ties into the goal engine).
