# OpenCode-compat Bun adapter (Child C)

> Parent: [06-21-plugin-system](../06-21-plugin-system/prd.md). The verified
> OpenCode plugin contract (hook names + signatures, loading model, runtime) is in
> the parent PRD. The IPC protocol + host are from Child A
> ([06-21-plugin-host-core](../06-21-plugin-host-core/prd.md)); plugin tools are
> from Child B ([06-21-plugin-tools](../06-21-plugin-tools/prd.md)).

## Goal

Deliver **direct compatibility for OpenCode plugins** by shipping a single bundled
**Bun adapter** that hya runs as one `kind: opencode` plugin child. The adapter
hosts OpenCode JS/TS plugins in Bun, exposes them the OpenCode init context
(`client` SDK + `$` shell + project/dir/worktree), translates OpenCode `Hooks` ⇄
the hya IPC protocol, and points the SDK `client` at a `hya serve` instance.

## Scope (owns)

Parent requirement: **R8**. Target the common **server** hooks; the OpenCode
**TUI** plugin surface is excluded.

## Dependency ordering

- **Depends on Child A** (protocol + host) and **Child B** (so OpenCode `tool:`
  registrations map onto hya plugin tools). Execute **after** A and B.
- May require a small `hya serve` capability check (the SDK client needs the
  server surface that OpenCode plugins call back into).

## Requirements

- **C-R1 (R8).** A Bun adapter process that speaks hya's IPC protocol on
  stdin/stdout as a `kind: opencode` plugin, and on its side loads OpenCode plugin
  modules.
- **C-R2 (R8).** Loader supporting BOTH OpenCode authoring shapes: legacy bare
  exported plugin functions and the newer target-specific module shape
  (`{ server }`); plus OpenCode's discovery (`.opencode/plugins/` +
  `~/.config/opencode/plugins/` dir-scan) and npm specifiers with Bun install.
- **C-R3 (R8).** Hook translation for the common server hooks that map to hya's
  v1 set: `event`, `tool.execute.before`, `tool.execute.after`, `chat.params`
  (+ `chat.message`/system where they map), `permission.ask`, and plugin `tool:`
  registration → hya plugin tools (via Child B). Preserve OpenCode's
  `(input, output)` in-place mutation + throw-to-block semantics, mapped to hya's
  mutate/veto + per-hook posture.
- **C-R4 (R8).** Provide the OpenCode init context: a `client` SDK pointed at
  `hya serve` (document which server endpoints must exist; build a shim if a
  needed endpoint is missing) and the Bun `$` shell.
- **C-R5 (compat honesty).** Explicitly document the supported vs unsupported hook
  subset and known divergences (e.g. `experimental.*`, TUI hooks, any SDK calls
  hya's server cannot yet satisfy). No silent partial behavior.
- **C-R6 (R9).** Quality gate green for the Rust side; the adapter has its own
  Bun/TS test(s). Bun is an optional runtime dependency — hya core must build/run
  without it (adapter simply unavailable if Bun is absent).

## Acceptance criteria

- [ ] **C-AC1 (AC7).** A real off-the-shelf OpenCode plugin using
      `tool.execute.before`/`after` (or `event`) runs against hya via the adapter
      and its hook provably fires — live QA evidence (tmux/log transcript).
- [ ] **C-AC2 (R8).** An OpenCode plugin that registers a `tool:` exposes that tool
      to the hya model and a call round-trips end-to-end.
- [ ] **C-AC3 (C-R3).** OpenCode mutate-in-place (e.g. `chat.params`) and
      throw-to-block (`tool.execute.before`) both produce the correct hya-side
      effect (param change / blocked tool), respecting posture.
- [ ] **C-AC4 (C-R4).** A plugin SDK callback (`client...`) against `hya serve`
      succeeds for at least one documented endpoint.
- [ ] **C-AC5 (C-R6).** hya builds and all existing tests pass with Bun absent;
      enabling the adapter without Bun degrades gracefully with a clear message.

## Out of scope

- OpenCode **TUI** plugins (`@opencode-ai/plugin/tui`), `experimental.*` hooks not
  in hya's v1 set, embedding a JS engine in-process (we spawn Bun), and full SDK
  surface parity (only document + shim what target plugins actually call).

## Notes

- Complex task: `design.md` + `implement.md` required before `task.py start`.
- Compatibility is **best-effort against a moving target** (OpenCode docs/loader
  drift, issue #20139). Pin a tested OpenCode plugin-pkg version in design.md.
