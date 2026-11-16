# 0.47.0

## Release packaging

- Backend and TUI/WebUI releases are unified under a single `hya` version (`0.47.0`) and `v<version>` release tags (`v0.47.0`, `v0.47.1-dev.1`, …). Each target ships one combined `hya-<version>-<target>.tar.gz` archive installed by `https://hya.ed-aisys.com/install.sh` and `hya update`.

## Code mode

- New first-party bundle `hya/code-mode`, shipped with `hya` and off by default. `tools.code.mode` in `config.yaml` turns it on: `with_tools` offers the built-in agents the tool `code-mode__run` next to their other tools; `only` offers it alone and leaves every other tool callable from a program. The value is read at backend start. See [Code mode](docs/code-mode.md).
- `code-mode__run` runs a JavaScript program whose effects are recorded one trace line each: `tools.<name>(args)` for every tool of the calling agent's view, `agents.run` to start a worker agent and wait for its typed report, `models.classify` for a configured judge model. Programs can be stored and run again with new `args` (`save_as`, `run`, `list`, `show`).
- In `only` mode a direct model call of a hidden tool is refused with a message that names `code-mode__run`.

## Workflow programs and the orchestrator agent

- New optional bundle `hya-extra/orchestrator`: an agent whose only tool, `workflow`, submits such a program over three specialised workers (scout, runner, editor). A scout's answer on an unchanged work tree is remembered and reused; stored programs survive failures and can be repaired. See [Extra bundles](docs/extra-bundles.md#hya-extraorchestrator) and [ADR-0032](docs/adr/0032-workflow-programs-and-the-orchestrator-agent.md).

## Bundle and plugin interfaces

- Bundle agents may declare `private: true` to stay out of ordinary spawn
  rosters and selectors while remaining usable by Workflows and explicit
  `can_spawn` grants. The first-party goal and plan/implement/review agents
  are private; complete agent metadata marks them hidden.
- Bundle tools can call the other tools of their agent's view: host capabilities `tools.list` and `tool.call` (new error code `-32003`), backed by `ToolCtx::nested`. A nested call is authorized like a model call and its file changes are recorded on the calling tool call.
- New host capability `workspace.state`: a SHA-256 identity of the Git work tree that contains the session's working directory.
- Agent manifest fields `advertise` (which of the agent's tools the model is offered) and `handoff: model | deterministic` (a `deterministic` agent ends an episode without a model-written handoff document).
- `extensions.process.timeout_ms` sets a bundle process's request timeout (1000 to 3600000 ms; default 30 s).
- An `extensions.process` of `kind: bun` whose command starts with `bun` now uses the Bun hya resolves (`$BUN`, the release's own Bun, then `PATH`).

## TUI and WebUI

- Subagent views are read-only for execution: sending prompts or backend commands, undo/redo, and changing model, agent or effort are blocked. Steer or archive the subagent from its parent. The session picker refuses child deletion; archive the child or delete its root instead.
- Refresh in `/bundles` now asks the backend to refresh its bundle catalog, then reloads the list and TUI extensions, showing progress, success, and failures.
- Label prior handoff context as `resumed from handoff (generation …)` rather than as ordinary compaction, and show live tool input without requiring each streamed fragment to be durable.
- The generic `/api` catalog no longer offers the unavailable `DisposeProcess`, `UpgradeProcess`, `StartOauth`, or `CompleteOauth` operations, and retired `invalid` placeholder recognition is removed.

## Context compaction and headless execution

- A long single turn (such as a headless `hya exec` run holding dozens of steps in one assistant message) now folds its old steps into the compaction summary instead of ending with `exceeds context window ... after context reduction`. When no whole message can be folded, `soft`, `snapcompact`, and `handoff` cut inside the in-flight message between whole steps (`ContextCompacted.step_cut { through_part, kept_head }`, `MessageProjection.folded_parts`, reducer version 9); the prompt and the newest `keep_recent` steps stay verbatim, reasoning included. See [Step folds](docs/compaction.md#step-folds).
- When provider-reported occupancy sits below the threshold while the local preflight estimate has already crossed the context window, the reduction ladder falls back to the local estimate so the turn folds before preflight can reject it. The summarizer also sees both ends of long reasoning and tool output.
- Tool-result images are forwarded to vision-capable models (`modalities.input` includes `image`) across Anthropic, OpenAI Responses, and OpenAI Chat protocols (bounded to 2 MiB per result and 16 MiB per request); non-vision routes receive an explicit notice that image/PDF bytes are not shown.
- Subagents that end cleanly with text deliver that final answer as their report without requiring an explicit `report` call; headless runs without an interaction host omit `ask_user` and `plan_exit`, surface a 10%-interval budget clock on tool results, and let late commands use up to half of the remaining time.

## Fixes

- A fold that follows a `shake` spill in the same reduction no longer sends the spilled tool outputs again; their placeholders stay in place.
- An empty final model step no longer ends a headless `hya exec` run prematurely, `write` echoes file contents only when disk bytes differ from what was sent, and `edit` documents a valid 2-character anchor example.
- `glob` and `find` no longer walk a tree without bound. They search breadth-first for at most 30 seconds; a walk that is stopped returns what it found, marked `incomplete`. A `glob` that completes returns what it did before. `find` now lists files in breadth-first order and, like `glob`, no longer descends into symbolic links to directories. Previously a `glob` of `/` did not return.
