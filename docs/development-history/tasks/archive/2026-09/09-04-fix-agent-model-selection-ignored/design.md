# Technical Design: Fix Ignored Agent Model Selections

## Problem Boundary

The control plane can persist and display a remembered model while the execution
plane still sends a Session or process default to the provider. The fix must make
one model identity authoritative at the provider-request boundary without
weakening explicit routing precedence.

## Diagnostic Feedback Loop

Add a fast process or public-integration test with a fake provider catalog that
contains two valid models:

- `fake/default`
- `fake/selected`

The test must drive the same public mutation used by the TUI, execute the selected
Agent, capture the fake provider request, and assert `model == "fake/selected"`.
Cover two scenarios:

1. Create/open a root Session on `fake/default`, select `fake/selected` through
   the normal-selection path, then issue the next prompt.
2. Select `fake/selected` for a target subagent through
   `PUT /tui/agent-models/:agent_id`, invoke that Agent, and capture its request.

This is the Phase 1 loop. Preference rows, bootstrap output, and picker markers
are supporting assertions only.

## Investigation Order

After the loop is red:

1. Trace the selected identity from the TUI command through `LocalProvider`,
   `SyncProvider`, and the prompt request payload.
2. Trace the targeted preference through server control, app binding, Session
   admission, `agent_spec_for_binding`, and provider request construction.
3. Compare the model identity captured when a Session opens with the identity
   resolved for each later turn.
4. Rank 3–5 falsifiable hypotheses and change one boundary at a time.
5. Use the debugger where possible; otherwise add temporary tagged
   `[DEBUG-agent-model-ignored]` instrumentation and remove it before delivery.

## Correctness Contracts

- A deliberate normal selection updates execution state only after backend
  persistence succeeds.
- Existing Sessions must not retain a stale default after the user deliberately
  changes the active Agent model.
- Targeted preferences remain per-Agent and immutable inside an admitted
  `TurnBinding`.
- Direct/category Agent policy and request/spawn/Stage overrides suppress memory.
- Provider and provider-local model IDs remain separate until the existing typed
  `ModelRef` boundary; model-local slashes are not split.
- A failed mutation cannot change local, synchronized, Session, or provider state.

## Compatibility

No wire route, Event schema, Workflow DSL, or generated SDK API changes are
planned. Fix the current ownership/resolution path and migrate every affected
caller. Do not add a compatibility shim.

## Verification

- Red then green provider-request regression test.
- Existing preference, root default, spawn, Workflow, fixed-Agent, server, and
  frontend synchronization tests.
- Rust format, Clippy, workspace tests excluding `hya-e2e`, process E2E, TUI
  type-check/tests, and release binary build.
- Reinstall with `./install.sh --prefix "$HOME/.local"`.
- Actual TUI selection against a redacted fake provider, with the selected model
  observed in the provider request before reporting completion.

## Release and Rollback

Update the workspace/TUI version from `0.36.10` to `0.36.11`, archive the prior
root changelog, and write a newest-only fix changelog. Rollback is the atomic
feature commit; do not delete existing preference rows.
