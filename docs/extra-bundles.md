# hya-extra bundles

`hya-extra/*` are optional bundles shipped alongside hya, under
`bundles/extra/`. They are not installed by default and are not part of the
twelve trusted [first-party bundles](bundle-runtime.md#first-party-bundles);
each one is an ordinary public `.hyabundle` package that you build and
install like any other bundle described in
[AgentBundle Authoring](agent-bundle-authoring.md). They exist for two
reasons at once: they are useful optional capabilities, and each one is a
coverage fixture exercised by `crates/hya-bundle/tests/extra_bundles.rs` and
the `crates/hya-e2e` process suite (`T2.26` in the
[Agent feature matrix](testing/agent-matrix.md)).

Every `hya-extra/*` bundle follows the same identity rule as the first-party
bundles: `identity.id` starts with `hya-extra/`, `identity.publisher` is
`hya-extra`, and `identity.version` equals hya's own
`[workspace.package].version` (currently `0.41.0`). They are bumped together
with a release, the same way first-party bundles are.

## `hya-extra/zvec-grep`

### Introduction

A `Plugin` bundle that wires [zvec-grep](https://github.com/zvec-ai/zvec-grep)
into hya as a bundled stdio MCP server, plus a Skill that teaches an agent
when semantic search is worth reaching for instead of `grep`/`rg`. zvec-grep
indexes a workspace (code and other text material) and answers natural-
language retrieval questions; this bundle exposes only its narrow
`agent`-toolset surface (`zvec_grep_search`) so installed agents cannot
create, rebuild, or drop an index.

### Usage

Prerequisites:

- Node.js >= 22
- `npm install -g @zvec/zvec-grep` (installs the `zg` CLI on `PATH`)
- Build an index once per workspace with `zg index` before semantic search
  returns results; the Skill tells the agent to ask you to do this rather
  than doing it itself

Package and install:

```sh
cargo run -p xtask -- package-bundle bundles/extra/zvec-grep zvec-grep.hyabundle
hya bundle install zvec-grep.hyabundle
```

Configuration file (only needed if you want to override the daemon's
defaults; this bundle reads none of its own keys today):
`<hya config dir>/bundles/hya-extra%2Fzvec-grep/config.yml` for a user
install, or `.hya/bundles/zvec-grep/config.yml` for a `--project` install.
See [Bundle configuration files](configuration.md#bundle-configuration-files).

### Interface

| Contract | Value |
| --- | --- |
| MCP resource | `resources.mcp` id `zvec-grep`, argv `zg --server --stdio --mcp-toolset agent`, `timeout_ms: 600000` |
| Tool name (full-plane agent, e.g. `build`) | `zvec-grep__mcp__zvec-grep__zvec_grep_search` |
| Tool name (a bundle agent that selects this server, e.g. `hya-extra/scout`) | `<local-server-id>__zvec_grep_search` (the bundle chooses the local id) |
| Tool input | `{"root": "<absolute path>", "query"?: string, "queries"?: [...], "fts"?: [...], "limit"?: number, ...}` — `root` is required and must be absolute |
| Skill id | `zvec-grep` (`resources/skills/zvec-grep/SKILL.md`) |

## `hya-extra/scout`

### Introduction

An `AgentSetBundle` defining one transient subagent, `scout`: a cheap
retrieval agent for orchestrators to spawn with "where/what/how is X"
questions about the local workspace. It answers with file:line evidence
gathered through its own `zvec-grep` MCP server (bundle agents cannot see a
sibling Plugin bundle's resources, so `scout` ships its own copy of the MCP
server declaration) plus the read-only `read`/`grep`/`glob` harness tools. It
has no write, edit, or shell access.

### Usage

Prerequisites: same as `hya-extra/zvec-grep` above (`zg` on `PATH`, an index
built with `zg index`).

Package and install:

```sh
cargo run -p xtask -- package-bundle bundles/extra/scout scout.hyabundle
hya bundle install scout.hyabundle
```

Once installed, any built-in agent (e.g. `build`) can spawn it with the
`task` tool using `subagent_type: "scout"` — no `can_spawn` edit is needed,
because installing an `AgentSetBundle` makes its agents immediately
spawnable from the ordinary built-in roster.

By default `scout` runs on the `quick` model category. Map that category in
your own `categories:` config, or pin `scout`'s model directly in its bundle
config file:
`<hya config dir>/bundles/hya-extra%2Fscout/config.yml`
(or `.hya/bundles/scout/config.yml` for a `--project` install):

```yaml
agents:
  scout:
    model: openai/gpt-5.4-mini
```

### Interface

| Contract | Value |
| --- | --- |
| Agent id | `scout` (also `bundle:hya-extra/scout/agent/scout`) |
| Role / lifecycle | `subagent`, `spawn_lifecycle: transient` |
| Model policy | `{category: quick, reasoning: low}` |
| `resource_view.allow` | `harness:tool/read`, `harness:tool/grep`, `harness:tool/glob`, and its own bundle-local `zvec-grep` MCP server |
| MCP resource | `resources.mcp` id `zvec-grep`, argv `zg --server --stdio --mcp-toolset full`, `timeout_ms: 600000` |
| Tool name as `scout` sees it | `zvec-grep__zvec_grep_search` (also `zvec-grep__zvec_grep_index_status`, etc. from the `full` toolset) |
| Prompt | `prompts/scout.md` — search first with `zvec_grep_search`, verify with `read`/`grep`, keep tool calls few, answer with file:line citations |

## `hya-extra/jev-model-router`

Coming in this release.

## `hya-extra/model-fallback`

Coming in this release.
