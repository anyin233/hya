# Sidecar Plugins, Custom Tools & Hooks

When Markdown skills aren't enough—because you want to **register a custom tool**, **block dangerous commands before they run**, **route models dynamically**, or **serve custom HTTP endpoints**—you write an out-of-process **Plugin** or **Bundle Sidecar**.

Every plugin runs in its own child process and talks to Hya over **newline-delimited JSON-RPC 2.0 (NDJSON)** on `stdin` and `stdout` (**plugin protocol version 1**). A crash or timeout in a plugin never brings down Hya.

---

## Two ways to write a plugin

### Path A: High-level Bun JS module (Simplest for tools & basic hooks)

Inside a Bundle (`extensions.js`), export a default module for Hya's built-in Bun adapter (`hya-plugin-bun`):

```js
// extensions/runtime.js
export default async () => ({
  tools: {
    echo: {
      description: "Echo a message back with session metadata",
      parameters: {
        type: "object",
        properties: { text: { type: "string" } },
        required: ["text"],
      },
      async execute(input, context) {
        context.metadata({ title: "Echo", metadata: { session: context.sessionID } })
        return `Echo: ${input.text}`
      },
    },
  },
  "tool.execute.before": async ({ tool, input }) => {
    if (tool === "bash" && String(input?.command ?? "").includes("DROP DATABASE")) {
      return { outcome: "veto", reason: "Refusing destructive SQL in bash" }
    }
  },
})
```

The `context` passed to `execute(input, context)` contains `{ sessionID: string, metadata({ title?, metadata? }): void }`.

### Path B: Full Protocol Process (`extensions.process` or `config.yaml` `plugins:`)

When you need **all 19 lifecycle hooks**, **custom `apis:` HTTP endpoints**, **custom `permission_modes:`**, **host capabilities** (`tool.call`, `session.usage`), or want to write your plugin in **Rust, Go, Python, or standalone TypeScript**, declare an explicit `extensions.process` in `bundle.yaml` (or under `plugins:` in `config.yaml` / `.hya/plugins/<name>/plugin.toml`).

Here is a complete, dependency-free 30-line Bun/Node JSON-RPC 2.0 plugin:

```ts
import { createInterface } from "node:readline"

const reply = (id: unknown, result: unknown) =>
  process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id, result })}\n`)

for await (const line of createInterface({ input: process.stdin })) {
  const msg = JSON.parse(line)
  if (msg.id === undefined || msg.id === null) continue // notification (e.g. `event`)

  switch (msg.method) {
    case "initialize":
      reply(msg.id, {
        protocol_version: 1,
        plugin: { id: "my-guard", version: "1.0.0", kind: "bun" },
        hooks: [{ name: "tool.execute.before" }],
        tools: [],
      })
      break
    case "hook/tool.execute.before":
      reply(msg.id, { outcome: "continue", input: msg.params.input })
      break
    default:
      reply(msg.id, {}) // handles `shutdown` cleanly
  }
}
```

---

## Core JSON-RPC 2.0 Wire Methods

One JSON object per line on `stdin`/`stdout` (max 1 MiB per frame; write debug logs only to `stderr`!):

| Method | Direction | Purpose |
| --- | --- | --- |
| `initialize` | Host → Plugin | Handshake (`{ protocol_version: 1, host: { name, version } }`). Plugin replies with `{ protocol_version: 1, plugin: { id, version, kind }, hooks, tools, apis? }`. (5s timeout.) |
| `shutdown` | Host → Plugin | Graceful drain request (`{}` → `{}`) before exit. |
| `event` | Host → Plugin | One-way notification (`{ envelope }`, no `id`) for plugins subscribed to the `event` hook. |
| `tool/call` | Host → Plugin | Execute a plugin tool (`{ tool, session, call, input, host_capability? }` → `{ ok: bool, output, time_ms? }`). |
| `api/request` | Host → Plugin | Handle a custom Bundle HTTP/gRPC route (`{ api, method, path, path_params, query, body, session?, call, host_capability }` → `{ status?: 200..599, body? }`). |
| `host/capability` | Plugin → Host | Call back into Hya during an active `tool/call` or `api/request` using its temporary `host_capability` token. |
| `hook/<name>` | Host → Plugin | Run a registered lifecycle hook (default 30s timeout, configurable via `extensions.process.timeout_ms`). |

---

## Calling back into Hya (`host/capability`)

During an active `tool/call` or `api/request`, an installed Bundle process receives a short-lived `host_capability` token. Before returning its reply, it can send `host/capability` requests back to Hya over the same `stdout`/`stdin` pipe:

| `method` | Allowed in | What it does |
| --- | --- | --- |
| `context.describe` | Tool & API | Returns session ID, parent session, and workdir metadata. |
| `session.usage` | Tool & Session API | Returns exact token usage (`input`, `cache_read`, `cache_write`, `output`, `thinking`) across `session`, `tree`, or `root`. |
| `workspace.state` | Tool only | Returns an opaque `"<session>:<file-revision>"` string so read-only tools can safely cache answers until a file changes. |
| `permission.assert` | Tool only | Checks `{ action, resource }` against the active session's permission rules. |
| `tools.list` | Tool only | Lists the other tools in the calling agent's view (name, description, JSON Schema). |
| `tool.call` | Tool only | Calls another tool (`{ name, input }`) on behalf of the agent, enforcing all normal user permission prompts and `ProjectScope` rules! |

---

## Lifecycle Hooks Reference

Register only the hooks you use in `initialize` (`hooks: [{ name: "..." }]`):

| Hook name | Role & failure mode | What it can do |
| --- | --- | --- |
| `tool.execute.before` | **Guard (fail-closed!)** | Inspect `{ session, tool, input }`; return `{ outcome: "continue", input }` to rewrite args, or `{ outcome: "veto", reason }` to block execution. **If your plugin crashes or times out here, Hya blocks the tool call automatically for safety.** |
| `tool.execute.after` | Enrichment (fail-open) | Inspect or transform `{ status: "ok" \| "err", output }` before the model sees it. |
| `permission.ask` | Permission chain | Auto-answer prompts (`allow_once`, `allow_always`, `reject`, or `defer` to pass to the next handler / user). |
| `permission.approve` | Mode approver | Answers prompts when the user selects your bundle's custom `permission_modes:` mode. |
| `chat.params` | Enrichment (fail-open) | Rewrite `request` (`model`, `system`, `temperature`, `reasoning`, extra HTTP `headers`) before a provider round starts. |
| `model.fallback` | Failover | Pick `{ outcome: "retry", model: "provider/model" }` or `{ outcome: "give_up" }` when a model fails before streaming. |
| `message.user.before` / `command.execute.before` | Enrichment (fail-open) | Rewrite incoming user prompt or slash-command text before admission. |
| `compaction.before` / `compaction.after` | Observation | Notified before/after automatic 80% context window reduction. |
| `goal.evaluate`, `loop.verifier`, `loop.planner` | Autopilot judges | Custom verdict/planning providers for `hya -p` Goal mode and `hya loop`. |
| `session.start`, `session.end`, `agent.spawn`, `event` | Observation | Observe session lifecycle and committed event envelopes. |
