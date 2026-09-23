# hya-extra/zvec-grep

Optional `Plugin` bundle that wires [zvec-grep](https://github.com/zvec-ai/zvec-grep)
into hya as a bundled stdio MCP server, plus a Skill that teaches an agent
when to reach for semantic search instead of `grep`/`rg`.

## Prerequisites

- Node.js >= 22
- `npm install -g @zvec/zvec-grep` (provides the `zg` CLI)
- An index built with `zg index` in the target workspace root before semantic
  search returns results

## Package and install

```sh
cargo run -p xtask -- package-bundle bundles/extra/zvec-grep zvec-grep.hyabundle
hya bundle install zvec-grep.hyabundle
```

## What it exposes

- MCP server `zvec-grep` running `zg --server --stdio --mcp-toolset agent`,
  which registers exactly one tool, `zvec_grep_search`.
- Skill `zvec-grep` describing the search-vs-grep decision and index lifecycle.

This bundle does not ship an Agent; any full-plane agent (e.g. `build`) can
call the MCP tool once installed, and any bundle agent can select the MCP
server resource in its own `resource_view`. See
[`docs/extra-bundles.md`](../../../docs/extra-bundles.md) for the exact tool
name the model sees and configuration details.

This file is not packaged; it is not declared in `bundle.yaml`.
