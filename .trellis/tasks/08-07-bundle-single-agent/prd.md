# Bundle = one agent, isolated tool plane

## Goal

Make an AgentBundle carry exactly one agent. Move the built-in agents out of the
bundle system into a Rust-native registry. Clamp the tool plane of a bundle agent
to internal public tools plus its own bundle resources.

After this change the bundle system is the subagent definition system: a user
installs one bundle per specialist agent, and each installed agent runs in a
known, narrow capability plane.

## Background

Today one bundle holds a list of agents (`agents: [...]`). Two built-in bundles
ship 15 agents. Each agent declares `harness_access: none | basic | full`, which
the author controls. `full` gives the agent every tool in the live registry,
including MCP server tools, plugin tools, and user or project skills.

This has three problems:

1. A bundle is both "a package of agents" and "an agent definition". The two
   roles conflict.
2. An installed bundle can declare `harness_access: full` and read the whole
   host capability surface. The bundle author controls the clamp, not the host.
3. Built-in agents use the same machinery as installed bundles. This forces the
   bundle format to serve internal needs that no bundle author has.

## Decisions

These are settled. Do not re-open them during implementation.

| # | Decision |
| --- | --- |
| D1 | A bundle contains exactly one agent. The manifest key is `agent:` (a map), not `agents:` (a list). |
| D2 | The 15 built-in agents leave the bundle system. They become Rust-native definitions in `hya-core`, with prompts loaded by `include_str!`. `bundles/builtin/` and the `hya-app` `build.rs` prepare step are deleted. |
| D3 | A bundle agent sees: the internal public tool plane (the builtin `ToolRegistry` snapshot captured at registry construction) plus its own bundle tools, skills, and MCP. It sees no MCP server tools, no plugin tools, and no user or project skills. |
| D4 | `harness_access` is removed from the manifest. The tool plane comes from the agent origin, not from the author. |
| D5 | A bundle agent keeps `role`. A bundle can still define a `main` agent. |
| D6 | A bundle agent keeps `can_spawn`. It can name built-in agents and agents from other installed bundles. A spawned agent runs in its own plane. |
| D7 | The v1 manifest format is dropped with no migration. The `api_version` key is removed from the manifest. See "Flagged assumption" below. |

### Flagged assumption (confirm before Phase 2)

D7 says "no version number". This PRD applies that to the **author-facing**
manifest only: `api_version` is deleted.

The **internal** prepared document keeps its `format_version` field, bumped to
`2`. That field is not an author surface. It is the guard that makes a stale row
in the installed-bundle SQLite registry fail closed instead of decoding into the
wrong shape. Removing it would make old rows decode as a partly valid new
document. If you want it gone as well, say so and the design drops it, and the
registry migration must then delete every installed row.

## Requirements

### R1 — Single-agent bundle format

- R1.1 The manifest declares `agent:` as a single map. A manifest with `agents:`
  is rejected with a clear error that names the new key.
- R1.2 The agent block uses one `id` field. The separate `local_id` and
  `stable_id` pair is collapsed. `bundle:<bundle_id>/agent/<id>` stays a valid
  reference.
- R1.3 `api_version` is removed. A manifest that still carries it is rejected
  with an error that says the key is no longer used.
- R1.4 `harness_access` is removed. A manifest that still carries it is rejected
  with an error that says the tool plane is host-controlled.
- R1.5 The single-file `bundle.hya.md` form keeps working. Its markdown body is
  the prompt of the one agent.
- R1.6 `PreparedBundle` holds `agent: PreparedAgent`, not `agents: Vec<_>`.
  `PreparedBundleIndex` holds `stable_agent_id`, not `stable_agent_ids`.

### R2 — Built-in agents leave the bundle system

- R2.1 The 15 built-in agents are defined in Rust in `hya-core`: `build`,
  `plan`, `explore`, `general`, `compaction`, `summary`, `title`, `hya-main`,
  `hya-planner`, `hya-implementer`, `hya-reviewer`, `hya-tester`, `hya-docs`,
  `hya-explorer`, `hya-release`.
- R2.2 Each built-in keeps its current id, role, description, spawn lifecycle,
  and prompt text. Prompt bodies stay in markdown files and are compiled in with
  `include_str!`.
- R2.3 `bundles/builtin/`, `crates/hya-app/build.rs`, the embedded
  `builtin-bundles.json` artifact, and `builtin_catalog()` are removed.
- R2.4 The bundle catalog accepts an empty bundle set. A fresh install has zero
  installed bundles and must still start and resolve every built-in agent.
- R2.5 Built-in agent ids are reserved. Building a catalog rejects an installed
  bundle whose agent id equals a built-in agent id.

### R3 — Clamped tool plane for bundle agents

- R3.1 A built-in agent gets the full harness plane: the live tool registry
  snapshot, harness skills, and harness MCP exports.
- R3.2 A bundle agent gets the internal public plane: the builtin tool snapshot
  captured when the runtime registry was built, plus its own bundle tools,
  skills, and MCP resources.
- R3.3 A bundle agent gets no harness skills. Project and user skills discovered
  from the working directory are not visible to it.
- R3.4 A bundle agent gets no harness MCP exports and no plugin-contributed
  tools.
- R3.5 A `resource_view.allow` entry that names `harness:skill/...` or
  `harness:mcp/...` in a bundle manifest fails the view with an unresolved
  reference error. It does not silently resolve to nothing.
- R3.6 The clamp holds for every path that builds a resource view, including the
  sidecar tool binding path.

### R4 — Spawn graph across the two origins

- R4.1 Agent resolution is one lookup across built-ins and installed bundles.
  A caller does not need to know the origin of an agent.
- R4.2 A built-in ordinary agent can spawn every other ordinary agent, built-in
  or installed. Installing a bundle makes its agent spawnable at once, with no
  edit to any built-in definition.
- R4.3 The reserved system agents `compaction`, `summary`, and `title` stay
  unspawnable by ordinary agents.
- R4.4 A bundle `can_spawn` entry that names an agent that is not installed is
  skipped when the roster is built. It is an error only when a spawn of that id
  is actually attempted.
- R4.5 Prepare no longer validates `can_spawn` targets. A single-agent bundle
  cannot resolve a cross-bundle reference at prepare time.

### R5 — Compatibility and operator experience

- R5.1 An installed bundle prepared under the old format fails to load with an
  error that names the bundle and tells the operator to reinstall it.
- R5.2 The bundle registry migration clears stale rows so a failed decode does
  not block startup for every later turn.
- R5.3 `hya bundle list` / `info` report one agent per bundle. `hya agent list`
  reports built-ins and installed bundle agents in one list, with the origin
  shown.
- R5.4 The runtime semantic fingerprint stays content-derived. Built-in agents
  contribute a digest over their canonical definition, not over a prepared
  bundle document.

### R6 — Documentation

- R6.1 `docs/agent-bundle-authoring.md` is rewritten for the single-agent format
  and the clamped plane.
- R6.2 `docs/examples/bundle.hya.md` and the three `bun-*` examples are updated.
- R6.3 `CONTEXT.md` ubiquitous language is updated: "AgentBundle catalog" no
  longer covers built-ins.
- R6.4 A new ADR records the split between the built-in agent registry and the
  bundle system, and the reason for the host-controlled clamp.

## Non-goals

- No bundle dependency or version resolution between bundles.
- No per-bundle permission prompts or a new consent UI.
- No change to the sidecar ABI, the hook protocol, or the plugin protocol.
- No change to `role` semantics or to selector mode strings.
- No new package archive format. `.7z` public and private packages stay.

## Constraints

- Rust workspace. Follow `AGENTS.md`: TDD gate first, then implementation.
- The clamp must be fail-closed. If the plane cannot be determined, deny.
- `basic_tools` in `RuntimeSnapshot` is the internal public plane. Its
  builtins-only property must become an asserted invariant, not a convention.
- Trellis scripts need Python 3.12 or newer. The system `python3` is 3.10 and
  fails on `.trellis/scripts/common/task_context.py:240`. Use
  `~/.local/share/uv/python/cpython-3.13-linux-x86_64-gnu/bin/python3.13`.

## Acceptance criteria

- [ ] AC1 A manifest with `agents:` is rejected, and the error names `agent:`.
- [ ] AC2 A manifest with `api_version` or `harness_access` is rejected with a
      key-specific error.
- [ ] AC3 `PreparedBundle` exposes exactly one agent at the type level. It is
      not possible to construct a prepared bundle with zero or two agents.
- [ ] AC4 `bundles/builtin/` and `crates/hya-app/build.rs` no longer exist, and
      the workspace builds.
- [ ] AC5 All 15 built-in agents resolve by id from a runtime built with zero
      installed bundles.
- [ ] AC6 A test proves a bundle agent cannot see an MCP export, a plugin tool,
      or a project skill that a built-in agent can see in the same runtime.
- [ ] AC7 A test proves a bundle `resource_view.allow` of `harness:mcp/...` or
      `harness:skill/...` fails the view.
- [ ] AC8 A test proves an installed bundle agent becomes spawnable by `build`
      without editing any built-in definition.
- [ ] AC9 A test proves an installed bundle whose agent id is `build` is
      rejected at catalog build.
- [ ] AC10 A test proves `compaction`, `summary`, and `title` stay unspawnable
      by an ordinary agent.
- [ ] AC11 A test proves a bundle `can_spawn` entry for a missing agent is
      skipped in the roster and errors only on a spawn attempt.
- [ ] AC12 A stale installed-bundle row produces a named, actionable error and
      does not wedge later turns.
- [ ] AC13 `cargo build --workspace`, `cargo clippy --workspace --all-targets
      -- -D warnings`, `cargo fmt --check`, and `cargo test --workspace` pass.
- [ ] AC14 Docs in R6 are updated and the ADR is added.

## Notes

- Work happens in a new git worktree off `main`.
- Design is in `design.md`. The ordered execution plan is in `implement.md`.
