# Editor Integration (ACP)

Want to use Hya right inside your code editor—like **Zed** or any editor that speaks the [Agent Client Protocol (ACP v1)](https://github.com/agentclientprotocol/agent-client-protocol/tree/main/docs/protocol/v1)—without switching windows?

Your editor can launch `hya acp` automatically. Behind the scenes, `hya acp` connects straight to your local Hya background server (starting it if needed), so your editor shares the exact same saved conversations, model keys, and Project permission rules as your terminal and browser UI.

---

## 1. Connect Zed (or another ACP editor)

Add `hya` with argument `"acp"` to your editor's custom agent settings. For example, in **Zed** (`~/.config/zed/settings.json`):

```json
{
  "agent_servers": {
    "hya": {
      "type": "custom",
      "command": "hya",
      "args": ["acp"]
    }
  }
}
```

Open a project folder in Zed, open the Agent panel, pick **hya**, and start chatting!

### Tips for first-time setup
- **Set up your API key once:** `hya acp` uses whatever model and credentials you already configured in Hya (`hya provider add`, `hya login`, or `/key` in the TUI). Override the model per editor entry with `"args": ["--model", "anthropic/claude-sonnet-4-6", "acp"]`.
- **Quick offline test (no API key needed):** Set `"args": ["--db", "/tmp/hya-editor.db", "--model", "hya/offline", "acp"]` and send `"hello"`—the built-in offline model echoes your message right inside your editor.
- **Requires Hya `0.50.0` or newer:** If you just ran `hya update` while an older background server was still running, run `hya serve restart` once so the daemon matches your new binary.

---

## 2. How `hya acp` works

- **Stdio JSON-RPC 2.0:** Your editor talks to `hya acp` over newline-delimited JSON-RPC on `stdin`/`stdout` (logs go to `stderr`).
- **One prompt at a time per session:** Closing the editor tab or `stdin` cancels any active turn, stops editor-launched MCP servers for that session, and leaves your Hya background daemon and saved chat history intact.
- **Permission prompts in your editor:** When Hya wants to run a command or edit an outside file, it sends `session/request_permission` so you can click **Allow** or **Deny** directly inside your editor (each choice applies once; multi-question `ask_user` prompts are declined automatically since ACP v1 only models tool permissions).

---

## 3. ACP v1 protocol reference

| Method | Direction | Summary |
| --- | --- | --- |
| `initialize` | Editor → Hya | Negotiates ACP `protocolVersion: 1`, returns Hya's version, and advertises `agentCapabilities.sessionCapabilities.close` (`authMethods` is empty—configure keys via `hya`). |
| `session/new` | Editor → Hya | Takes `{ cwd, mcpServers }` (absolute directory + optional stdio MCP servers), resolves Project placement, and returns `{ sessionId }`. |
| `session/prompt` | Editor → Hya | Sends `{ sessionId, prompt: ContentBlock[] }` (`text` and `resource_link` blocks; resource links insert `[name](URI)` so the model can inspect them via tools). Resolves with `{ stopReason: "end_turn" \| "max_tokens" \| "cancelled" }`. |
| `session/update` | Hya → Editor | Streams `agent_message_chunk` text deltas plus `tool_call` and `tool_call_update` cards correlated by `toolCallId`. |
| `session/request_permission` | Hya → Editor | Requests approval for `{ sessionId, toolCall, options }`. Reply with `{"outcome": {"outcome": "selected", "optionId": "allow"}}` or `"deny"`. |
| `session/cancel` | Editor → Hya | Notification `{ sessionId }` cancelling the active turn cleanly (idempotent). |
| `session/close` | Editor → Hya | Cancels any active turn on `{ sessionId }` and shuts down its session-scoped MCP processes while keeping the saved transcript in SQLite. |

Example `session/prompt` request with a file link:

```json
{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"<sessionId>","prompt":[{"type":"text","text":"Explain "},{"type":"resource_link","name":"README","uri":"file:///work/repo/README.md"}]}}
```

### Error codes

| JSON-RPC code | When returned |
| --- | --- |
| `-32602` | Invalid parameters (e.g. relative `cwd` or malformed MCP server name). |
| `-32002` | Unknown or already-closed `sessionId`. |
| `-32010` | Session already has an active turn running. |
| `-32000` (`auth_required`) | Model provider rejected credentials (`error.data.code == "provider_auth"`, e.g. expired OAuth or HTTP `401`/`403`). |
| `-32011` | Any other backend turn failure (`error.data.code` carries the Hya error code). |

### Editor-supplied MCP servers (`OpenMcpSession`)

When an editor passes stdio `mcpServers` (`{ name, command, args, env }`, where `name` uses ASCII letters, digits, `_`, or `-` without `__`), `hya acp` spawns them in the session's `cwd`—keeping editor command lines and secrets inside the local adapter process—and bridges their streams to the daemon over owner-only Unix sockets via `hya.v1.Mcp/OpenMcpSession` (`POST /v1/mcp/session` SSE). Two editor sessions can even bind the same MCP server name to different tool sets without colliding.
