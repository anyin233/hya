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

## Fixes

- `glob` and `find` no longer walk a tree without bound. They search breadth-first for at most 30 seconds; a walk that is stopped returns what it found, marked `incomplete`. A `glob` that completes returns what it did before. `find` now lists files in breadth-first order and, like `glob`, no longer descends into symbolic links to directories. Previously a `glob` of `/` did not return.
