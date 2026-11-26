# Agent Client Protocol

An editor can start `hya acp` as an [ACP v1](https://github.com/agentclientprotocol/agent-client-protocol/tree/main/docs/protocol/v1) agent. The editor owns the stdio connection and chooses the workspace. The adapter attaches to the local database's hya daemon, starting it when needed. The daemon stores the conversation, runs tools and enforces the session's permission policy.

## Connect an editor

Configure a custom agent with executable `hya` and argument `acp`. For example, add this entry to Zed's settings and select **hya** when starting an agent thread.

```json
{
  "agent_servers": {
    "hya": { "type": "custom", "command": "hya", "args": ["acp"] }
  }
}
```

For a local smoke test, use arguments `["--db", "/tmp/hya-editor.db", "--model", "hya/offline", "acp"]`. Open a workspace and send “hello”. The offline provider echoes the prompt in the editor. For a live model, select a configured `provider/model` or omit `--model` to use the backend default. Configure provider credentials with the usual hya config or run `hya login` in a terminal. ACP advertises an empty `authMethods` list. The adapter requires a local backend at version 0.50.0 or later. After an upgrade, use `hya serve restart --db <path>` if an older daemon owns the database.

## ACP v1 interface

The transport is newline-delimited JSON-RPC 2.0 on stdin and stdout. Diagnostics go to stderr. Each connection owns the session IDs it creates. Each session's MCP processes run until `session/close` or the end of the connection. Closing stdin cancels its active prompts, releases its session MCP processes and leaves the shared daemon and saved transcripts available.

| Method | Input and result |
| --- | --- |
| `initialize` | Send `{protocolVersion, clientCapabilities}` before other requests. The response selects version 1, identifies the hya release and advertises `agentCapabilities.sessionCapabilities.close`. |
| `session/new` | Send `{cwd, mcpServers}` with an existing absolute directory and an array of stdio servers. Receive `{sessionId}`. hya resolves the directory using its ordinary Project placement rules. |
| `session/prompt` | Send `{sessionId, prompt: ContentBlock[]}`. Text blocks are concatenated in order. A resource link inserts its name and URI as ` [name](URI) `; reading its contents requires an ordinary tool call. The result is `{stopReason}` after the last update, with `end_turn`, `max_tokens` or `cancelled`. Each session admits one prompt at a time. |
| `session/update` | Agent notification containing `{sessionId, update}`. Text streams as `agent_message_chunk`. A `tool_call` and subsequent `tool_call_update` share a `toolCallId` and carry status and available input/output. |
| `session/request_permission` | Agent request containing `{sessionId, toolCall, options}`. Reply with `{"outcome":{"outcome":"selected","optionId":"allow"}}` or select `deny`. Both choices apply once. An unrecognized selection or a cancelled response denies the request. Pending hya question prompts receive a rejection. |
| `session/cancel` | Client notification containing `{sessionId}`. Cancels the connection's active turn and settles the original prompt. Repeated notifications during cancellation are safe. |
| `session/close` | Send `{sessionId}` and receive `{}`. Cancels the session's active turn as `session/cancel` does and stops its MCP processes once that turn settles. Later requests for the session fail as unknown. The transcript stays in the database. |

For example, after receiving a session ID, an editor can send:

```json
{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"<sessionId>","prompt":[{"type":"text","text":"Explain "},{"type":"resource_link","name":"README","uri":"file:///work/repo/README.md"}]}}
```

Invalid inputs use `-32602`, unknown sessions use `-32002`, and a busy session uses `-32010`. A failed turn returns the backend's error code in `error.data.code`, with `-32000` for credential errors and `-32011` for execution errors. The advertised v1 capabilities cover text, resource links and stdio MCP. Optional session loading, model switching, embedded context, images, audio, HTTP/SSE MCP, editor file operations and editor terminals are outside this interface.

## Session MCP lifetime

Each stdio entry has `name: string`, `command: string` (absolute executable path), `args: string[]` and `env: {name: string, value: string}[]`. Each name is a distinct namespace segment of ASCII letters, digits, underscores or hyphens. Double underscores are reserved for the tool name separator. The adapter launches each command in the session's cwd and connects its streams to the backend through an owner-only temporary Unix socket. A failed discovery releases previously opened peers. Two sessions can use the same server name with different tools. Editor-provided command lines and environment values stay in the adapter process.

The local-owner `hya.v1.Mcp/OpenMcpSession` RPC also has an HTTP binding at `POST /v1/mcp/session` (SSE). Its request is `{workdir: string, model?: string, peers: {name: string, socket: string}[]}`. The first response, `{session: string}`, confirms successful discovery and creation. Keep the response stream open for the lifetime of the exports. Disconnection removes the session overlay. The adapter uses ordinary Turns, Events and Interactions RPCs thereafter. `CancelTurn` accepts `onlyIfCurrent: true` to protect a later turn from a stale cancellation.

The implementation uses the official Rust SDK 5.6.1 with schema 1.22.0. Process coverage runs with `cargo test -p hya-e2e --test it acp_stdio_permissions_and_session_mcp_lifecycle` after building `hya`. Protocol compatibility can be checked with `acp-tck -- hya --model hya/offline acp`. [Rust SDK](https://github.com/agentclientprotocol/rust-sdk) · [ACP conformance suite](https://github.com/agentclientprotocol/acp-tck)
