# Compat-compat Bun adapter (Child C)

> Parent: [06-21-plugin-system](../06-21-plugin-system/prd.md). The verified
> Compat plugin contract (hook names + signatures, loading model, runtime) is in
> the parent PRD. The IPC protocol + host are from Child A
> ([06-21-plugin-host-core](../06-21-plugin-host-core/prd.md)); plugin tools are
> from Child B ([06-21-plugin-tools](../06-21-plugin-tools/prd.md)).

## Goal

Deliver **direct compatibility for Compat plugins** by shipping a single bundled
**Bun adapter** that yaca runs as one `kind: compat` plugin child. The adapter
hosts Compat JS/TS plugins in Bun, exposes them the Compat init context
(`client` SDK + `$` shell + project/dir/worktree), translates Compat `Hooks` ⇄
the yaca IPC protocol, and points the SDK `client` at a `yaca serve` instance.

## Scope (owns)

Parent requirement: **R8**. Target the common **server** hooks; the Compat
**TUI** plugin surface is excluded.

## Dependency ordering

- **Depends on Child A** (protocol + host) and **Child B** (so Compat `tool:`
  registrations map onto yaca plugin tools). Execute **after** A and B.
- May require a small `yaca serve` capability check (the SDK client needs the
  server surface that Compat plugins call back into).

## Requirements

- **C-R1 (R8).** A Bun adapter process that speaks yaca's IPC protocol on
  stdin/stdout as a `kind: compat` plugin, and on its side loads Compat plugin
  modules.
- **C-R2 (R8).** Loader supporting BOTH Compat authoring shapes: legacy bare
  exported plugin functions and the newer target-specific module shape
  (`{ server }`); plus Compat's discovery (`.opencode/plugins/` +
  `~/.config/opencode/plugins/` dir-scan) and npm specifiers with Bun install.
- **C-R3 (R8).** Hook translation for the common server hooks that map to yaca's
  v1 set: `event`, `tool.execute.before`, `tool.execute.after`, `chat.params`
  (+ `chat.message`/system where they map), `permission.ask`, and plugin `tool:`
  registration → yaca plugin tools (via Child B). Preserve Compat's
  `(input, output)` in-place mutation + throw-to-block semantics, mapped to yaca's
  mutate/veto + per-hook posture.
- **C-R4 (R8).** Provide the Compat init context: a `client` SDK pointed at
  `yaca serve` (document which server endpoints must exist; build a shim if a
  needed endpoint is missing) and the Bun `$` shell.
- **C-R5 (compat honesty).** Explicitly document the supported vs unsupported hook
  subset and known divergences (e.g. `experimental.*`, TUI hooks, any SDK calls
  yaca's server cannot yet satisfy). No silent partial behavior.
- **C-R6 (R9).** Quality gate green for the Rust side; the adapter has its own
  Bun/TS test(s). Bun is an optional runtime dependency — yaca core must build/run
  without it (adapter simply unavailable if Bun is absent).

## Acceptance criteria

- [ ] **C-AC1 (AC7).** A real off-the-shelf Compat plugin using
      `tool.execute.before`/`after` (or `event`) runs against yaca via the adapter
      and its hook provably fires — live QA evidence (tmux/log transcript).
- [ ] **C-AC2 (R8).** An Compat plugin that registers a `tool:` exposes that tool
      to the yaca model and a call round-trips end-to-end.
- [ ] **C-AC3 (C-R3).** Compat mutate-in-place (e.g. `chat.params`) and
      throw-to-block (`tool.execute.before`) both produce the correct yaca-side
      effect (param change / blocked tool), respecting posture.
- [ ] **C-AC4 (C-R4).** A plugin SDK callback (`client...`) against `yaca serve`
      succeeds for at least one documented endpoint.
- [ ] **C-AC5 (C-R6).** yaca builds and all existing tests pass with Bun absent;
      enabling the adapter without Bun degrades gracefully with a clear message.

## Out of scope

- Compat **TUI** plugins (`@opencode-ai/plugin/tui`), `experimental.*` hooks not
  in yaca's v1 set, embedding a JS engine in-process (we spawn Bun), and full SDK
  surface parity (only document + shim what target plugins actually call).

## Notes

- Complex task: `design.md` + `implement.md` required before `task.py start`.
- Compatibility is **best-effort against a moving target** (Compat docs/loader
  drift, issue #20139). Pin a tested Compat plugin-pkg version in design.md.
