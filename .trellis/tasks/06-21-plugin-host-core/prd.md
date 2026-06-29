# Plugin host & hook-dispatch core (Child A)

> Parent: [06-21-plugin-system](../06-21-plugin-system/prd.md). All cross-cutting
> decisions (D1–D7), the verified hya architecture, the v1 hook inventory, and the
> OpenCode contract live in the parent PRD. This child owns the MVP foundation.

## Goal

Build the foundational layer that every plugin (native or the OpenCode adapter)
relies on: a new `hya-plugin` crate carrying the **JSONL / JSON-RPC-style IPC
protocol** + the **host/manager** that spawns plugins as child processes, plus the
**hook-dispatch seams** inside `hya-core` for the v1 hook set, the config/loading
surface, and the `hya-cli` bootstrap. Ships with a native example plugin and the
per-hook failure posture. This is the contract Child B (tools) and Child C
(OpenCode adapter) consume.

## Scope (owns)

Parent requirements: **R1, R2, R3, R4, R6, R7, R9, R10** (R7's hooks/loading side;
R5 tool registration is Child B; R8 is Child C).

## Dependency ordering

- **No upstream dependency** — this is the first child to execute (A → B → C).
- Must publish a **stable protocol contract** (message schema + version
  negotiation) because B and C build on it. Protocol-affecting changes after this
  child lands require coordinated updates to B/C.

## Requirements

- **A-R1 (R1).** A hook-dispatch trait seam in `hya-core` (e.g.
  `Option<Arc<dyn HookDispatcher>>` on `SessionEngine`, installed via a new
  `with_hooks` builder) wired at: the `emit` funnel (`event`), `admit_user_prompt`
  (`message.user.before`), `request_from_messages` (`chat.params`), the tool loop
  (`tool.execute.before` veto + `tool.execute.after`), the permission plane
  (`permission.ask`), session/message lifecycle (observe), and the goal/loop gates
  (`goal.evaluate`, `loop.verifier`/`loop.planner`).
- **A-R2 (R2).** A `hya-plugin` crate: protocol types + a host/manager that
  spawns each plugin child process, performs handshake/registration, owns the
  stdio read/write tasks, correlates request ids, and handles shutdown.
- **A-R3 (R3).** Blocking interception: engine sends `(input, output)`, awaits the
  reply within the per-hook timeout, applies payload mutations and/or veto.
  `event` is async/coalesced and never blocks the turn.
- **A-R4 (R4).** The v1 hook set above, each with a typed input/output payload.
- **A-R5 (R6).** Per-hook `posture: safe|open`. Guards fail safe; enrichment fails
  open. Per-hook timeout + child crash detection + restart.
- **A-R6 (R7).** `plugins:` config section + native dir-scan
  (`~/.config/hya/plugins/`, `.hya/plugins/`) with a `plugin.toml` manifest +
  per-plugin `enabled`. A single bootstrap in `hya-cli` wires the host before the
  registry/router freeze into `Arc`, across all five modes (exec/rpc/goal/tui/serve).
- **A-R7 (R10).** Zero overhead and zero behavior change when no plugins are
  configured (dispatcher absent ⇒ inert).
- **A-R8 (R9).** Quality gate green; new logic covered by tests (TDD).

## Acceptance criteria

- [ ] **A-AC1 (AC1).** A native example plugin's `tool.execute.before` fires in a
      real turn, mutates an arg (tool receives the mutated arg), and a veto path
      blocks the tool — proven by test + a `hya exec` QA transcript.
- [ ] **A-AC2 (AC3).** A `chat.params` plugin changes temperature/model/system and
      the change is provably reflected in the built provider request (unit test on
      the request builder + live transcript).
- [ ] **A-AC3 (AC4).** A plugin that exceeds the timeout: an enrichment hook
      proceeds with the original payload; a guard hook fails safe (blocked / normal
      permission fallback). Both asserted.
- [ ] **A-AC4 (AC5).** Killing the plugin mid-turn neither crashes nor hangs hya;
      the turn completes per posture; the host records the crash and restarts on
      next use.
- [ ] **A-AC5 (AC6).** A plugin declared in `config.yaml` and one dropped into the
      scanned dir (with `plugin.toml`) both load and run; `enabled: false` disables
      one.
- [ ] **A-AC6 (AC8).** Full quality gate green; with no plugins configured every
      existing test passes. Overhead per parent AC8 protocol: HARD GATE — unit test
      asserts default `SessionEngine::new` leaves `hooks == None`, and the `None`
      branch makes zero dispatcher calls + zero heap allocations on the `emit` path
      (counting allocator); PERF EVIDENCE — `--features bench` microbench (`emit`
      100_000×, `run_turn`/`FakeProvider` 1_000×) shows the `None` path within +3%
      median vs the pre-plugin baseline. Numbers in the QA log.
- [ ] **A-AC7.** Protocol codec round-trips every message kind (unit tests); the
      protocol version/capability handshake is covered.

## Out of scope

- Plugin-registered tools (Child B), OpenCode adapter (Child C), out-of-process
  providers, OS sandbox, and the deferred hook inventory (parent "Out of scope").

## Notes

- Design + implement detail in `design.md` / `implement.md` (this is a complex
  task — both required before `task.py start`).
