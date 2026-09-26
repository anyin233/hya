// A tiny stdio MCP server for the /mcp spec (e2e/hya-tui-mcp.spec.ts):
// speaks the `initialize` / `tools/list` / `tools/call` subset hya uses,
// one JSON-RPC message per line. `bun mcp-tools-server.ts <count>` lists
// `tool_01` … `tool_<count>` (default 40), each with an object input schema.
const count = Number(process.argv[2] ?? 40)
const width = String(count).length < 2 ? 2 : String(count).length
const tools = Array.from({ length: count }, (_, index) => ({
  name: `tool_${String(index + 1).padStart(width, "0")}`,
  description: `Fixture tool ${index + 1}`,
  inputSchema: { type: "object", properties: {} },
}))

function reply(id: unknown, result: unknown): void {
  process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id, result })}\n`)
}

let buffered = ""
process.stdin.setEncoding("utf8")
process.stdin.on("data", (chunk: string) => {
  buffered += chunk
  let newline = buffered.indexOf("\n")
  while (newline >= 0) {
    const line = buffered.slice(0, newline).trim()
    buffered = buffered.slice(newline + 1)
    newline = buffered.indexOf("\n")
    if (!line) continue
    const request = JSON.parse(line) as { id?: unknown; method?: string }
    if (request.id === undefined) continue
    if (request.method === "initialize") {
      reply(request.id, { protocolVersion: "2024-11-05", capabilities: { tools: {} }, serverInfo: { name: "fixture", version: "0.0.1" } })
    } else if (request.method === "tools/list") {
      reply(request.id, { tools })
    } else if (request.method === "tools/call") {
      reply(request.id, { content: [{ type: "text", text: "ok" }], isError: false })
    } else {
      reply(request.id, {})
    }
  }
})
process.stdin.on("end", () => process.exit(0))
