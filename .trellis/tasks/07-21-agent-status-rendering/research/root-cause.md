# Agent Status Rendering Root Cause

## Scope

The five reported symptoms resolve to three existing boundaries. No new event type or parallel UI state model is needed.

## 1. Prompt variant boundary

The TypeScript prompt submits the selected effort as a top-level `variant` alongside `model`. The legacy `POST /session/:id/message` payload in `session_prompt_legacy.rs` deserializes only `noReply`, `text`, `parts`, and `model`, so serde discards `variant`.

After admitting the user prompt, the handler parses `model` and emits `ModelSwitched` without the selected variant. This overwrites the variant set during session creation before the provider turn starts. The returned projected user message therefore has no effort variant, and the frontend's first-message synchronization clears its local effort selection.

The same overwrite prevents the reasoning-capable route from being selected, so there are no reasoning events for the TUI to render. Compat event conversion and `ReasoningPart` already handle non-empty reasoning start/delta/end data.

`model_ref_from_value` already understands a nested model `variant`, and session creation already serializes it into `ModelRef`. The missing contract is specifically the legacy prompt's separate top-level `variant`, not model parsing in general.

## 2. Observation lifetime policy

`reduceWorkspace` handles a terminal child by removing every unfocused observation pane for that session. A focused terminal pane is marked `closeOnBlur`, and `focusPane` removes it when focus changes. This can erase access to an already synchronized transcript immediately after a fast subagent finishes.

Observation sessions are already synchronized while their panes are open. Explicit `close` and successful `reconcileSessions` cleanup already exist, so terminal status does not need to own pane lifetime.

The two terminal-close tests in `packages/hya-tui-ts/test/subagent-workspace.test.ts` currently assert the behavior that must change.

## 3. Lifecycle presentation

A run-tree node may contain both a transient `member` and a roster registration. `run_member` advances the member through `spawning`, `running`, and terminal states, while the roster can remain `idle`.

Both the observation header and subagent dialog currently choose `roster.status` before `member.status` and display the raw string. For transient work this masks authoritative member state, yielding `Idle` during work and after successful completion.

The existing lifecycle vocabulary is sufficient. Presentation should resolve member status first when a member exists, otherwise roster status, then map working states to visible `Working` activity and successful terminal state to `Finished`.

## Existing Contracts

- Event/projection data remains the source of truth.
- Thinking UI renders only reasoning content present in projected parts.
- Observation panes are read-only and retain explicit close controls.
- State must be text-visible, not color-only; existing `Spinner` provides the activity affordance.
- Frontend focused checks: `bun test`, `bun run typecheck`, `bun run build` in `packages/hya-tui-ts`.
- Rust verification follows the workspace format, clippy, test, and local executable build gates.
- `compat_session_legacy_message_model_api.rs` is the narrow existing server fixture: extend its prompt request with a top-level variant and assert the loaded session model includes it.
- Project policy requires a patch version update from `0.33.14`, archival of the current root changelog, and a new root changelog for this fix before delivery.
