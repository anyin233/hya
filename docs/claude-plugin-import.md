# Claude Code plugin import

Hya imports a local Claude Code plugin into the ordinary bundle model. The
importer does not create a Claude-specific registry or a placeholder Agent. A
plugin with no `agents/*.md` becomes an agentless `Plugin`; a plugin with one
or more agent files becomes an `AgentSetBundle` containing every declared
agent. Both outputs pass through the same strict namespace, source-path,
resource-closure, preparation, digest, install, and generation rules as a
hand-authored bundle.

## Usage

Import a local plugin directory with:

```sh
hya bundle install --claude ./my-plugin
```

Select an entry from a local marketplace with
`hya bundle install --claude './marketplace#code-review'`.

The directory may contain either `plugin.json` or
`.claude-plugin/plugin.json`. For example, a plugin named `Code Review` at
version `1.2.3` imports as bundle `claude/code-review`, namespace
`code-review`, publisher `claude`, and version `1.2.3`.

The adapter can emit the intermediate source envelope directly:

```sh
bun run crates/hya-plugin-claude/adapter/src/main.ts \
  --emit-bundle-manifest --plugin-dir ./my-plugin
```

Installed process-backed bundles launch the adapter with:

```sh
bun run <resolved-adapter>/src/main.ts \
  --bundle-runtime runtime/claude-plugin.json --plugin-id <installed-id>
```

`kind: claude` resolves and prepends the adapter executable. Its manifest
`command` is adapter argument data, currently
`["--bundle-runtime", "runtime/claude-plugin.json"]`. The process runs with
the private materialized bundle root as its working directory. Hook commands
run from the staged `claude-plugin/` directory; both the environment variable
and literal command placeholder `${CLAUDE_PLUGIN_ROOT}` resolve to that path.

## Mapping and interfaces

| Claude source | Bundle output |
| --- | --- |
| `plugin.json` | `identity`, namespace, and an exact staged source copy |
| `agents/*.md` | One `AgentSetBundle.agents[]` entry per file, with a packaged prompt |
| `skills/*/SKILL.md` | `resources.skills` |
| `commands/*.md` | `resources.skills` prompt templates |
| `.mcp.json` | One validated `resources.mcp` entry per `mcpServers` member |
| `hooks/hooks.json` | Process-backed `resources.hooks` plus the runtime snapshot |
| Other regular plugin files | `extensions.files`, preserving command/script closure |

Agent IDs come from frontmatter `name` and are sanitized and deduplicated in
sorted file order. `model` becomes `model_policy.model` unless its value is
`inherit`; `tools` become lowercase `harness:tool/<name>` allow references and
must pass ordinary bundle closure validation. Imported agents use role
`subagent`; like every spawned agent they run as resident actors (the
manifest carries no spawn lifecycle). Hya does not infer a privileged main
Agent, spawn graph, tool permission expansion, or workflow from Claude metadata.

The emitted JSON envelope has this exact shape:

```text
{ "manifest": <bundle.yaml UTF-8 string>,
  "files": [{ "path": <relative path>, "content": <UTF-8 content> }, ...] }
```

The generated runtime snapshot is versioned with `format_version: 1` and
contains the translated identity, namespace, skill declarations, and parsed
hook matcher groups. Runtime mode accepts exactly one of `--plugin-dir <dir>`
or `--bundle-runtime <file>`.

Hook mapping is `PreToolUse` → `tool.execute.before`, `PostToolUse` →
`tool.execute.after`, `PreCompact` → `compaction.before`,
`SessionStart`/`SessionEnd` → `session.start`/
`session.end`, and `SubagentStart` → `agent.spawn`. Only
Claude command hooks are executable. Unknown events, malformed matcher groups,
prompt/agent hook forms, `UserPromptSubmit`, and unmatched lifecycle hooks
(`Stop`, `SubagentStop`, and `Notification`) fail import with an explicit diagnostic; they are never
silently discarded. Hya has no lifecycle point with the same semantics for
those three events. The current hya wire treats `message.user.before` as
continue-only, so it cannot represent Claude's possible prompt veto exactly.
Imported hooks cannot
gain lifecycle authority over session or engine stop decisions.

Marketplace metadata resolves local entries without allowing traversal. Git
entries (`source: {source: "git", repo: "…"}`) are shallow-cloned into a fresh
temporary directory for one bounded import action and removed in `finally`,
including after failures. Other remote marketplace source kinds return an
explicit unsupported reason. The backend install command resolves the
marketplace entry before invoking `--emit-bundle-manifest`.

All imported files must be regular UTF-8 files. Symlinks are rejected so the
prepared package remains a closed source tree. `.git` and `node_modules`
directories are excluded from the staged source copy.
