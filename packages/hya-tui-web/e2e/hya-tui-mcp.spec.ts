// The MCP view (`/mcp`): server list, status columns, refresh, close
// (state/mcp.ts, `GET /v1/mcp`). No MCP server is cheap to stand up in
// this fixture: `AddMcpServer` (`POST /v1/mcp`) connects synchronously and
// fails the whole call — not just the row's state — when the command
// does not speak the MCP protocol (tried here with a missing binary and
// with `false`, both surfaced as an HTTP 500 from the add call itself,
// even with `enabled: false`); that looks like a backend gap worth a
// follow-up (`McpServerState.FAILED` is effectively unreachable through
// this route today). So this spec covers the reachable empty state; the
// populated list/detail, connect/disconnect, and the auth pop-up's state
// machine are unit-tested in test/mcp.test.ts (`shownServers`,
// `serverLine`, `serverStateText`, `mcpViewKey`'s auth branch) — noted in
// docs/tui.md "MCP servers".
//
// The detail screen's tool list ("many tools" below) runs against a real
// stdio MCP server: e2e/fixtures/mcp-tools-server.ts, a tiny Bun script
// speaking the `initialize` / `tools/list` subset, configured through the
// backend fixture's `mcpServers` option (config.yaml `mcp:`). `GET /v1/mcp`
// reports a CONNECTED server's namespaced tools (`mcp__many__tool_01`, …);
// the view shows each under the server's own name (`tool_01`). The
// windowing arithmetic (`mcpToolWindow`, the highlight keys) stays
// unit-tested in test/mcp.test.ts too.

import { api, expect, hyaTui, mcpToolsServer, test } from "./hya"

type McpStatus = { servers?: { name: string; state?: string; tools?: string[] }[] }

test.describe("hya TUI MCP view", () => {
  test("/mcp: no servers configured, r refreshes, Esc closes", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await term.type("/mcp")
    await term.press("Enter")
    await term.waitForText("MCP servers")
    await term.waitForText(/SERVER\s+STATE\s+TOOLS\s+AUTH\s+ERROR/)
    await term.waitForText("No MCP servers configured")
    await term.waitForText("0 configured server")
    await term.waitForText("↑↓ move · Enter tools · c connect · x disconnect · a auth · r refresh · / filter · Esc close")

    await term.press("r")
    await term.waitForText("Refreshed")
    await term.press("Escape")
    await term.waitForText("Enter a prompt · /new creates a session")
  })

  test("/mcp shows in the help overlay and command menu, and is listed as a native command", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await term.press("?")
    await term.waitForText("Help · keys and commands")
    await term.type("mcp")
    await term.waitForText(/\/mcp\s+\[local\]\s+Open the MCP view/)
    await term.press("Escape")
    await term.waitForText("Enter a prompt · /new creates a session")
  })

  test.describe("many tools", () => {
    test.use({ mcpServers: { many: { command: mcpToolsServer(40) } } })

    test("/mcp → Enter windows 40 tools at 80 columns, End reaches the last, the hint stays clear", async ({ tui, backend }) => {
      await expect
        .poll(async () => {
          const status = await api<McpStatus>(backend, "GET", "/v1/mcp")
          return status.servers?.find((row) => row.name === "many")?.tools?.length ?? 0
        }, { timeout: 20_000 })
        .toBe(40)

      const term = await tui(hyaTui(backend), { viewport: { width: 690, height: 640 } })
      await term.waitForText("Connected to hya")
      const { cols } = await term.size()
      expect(cols).toBeLessThanOrEqual(82)
      await term.type("/mcp")
      await term.press("Enter")
      await term.waitForText("1 configured server")
      await term.waitForText(/many\s+connected\s+40 tools/)

      await term.press("Enter")
      await term.waitForText("MCP › many")
      await term.waitForText("many · connected")
      await term.waitForText("▸ tool_01")
      await term.waitForText(/↓ \d+ more/)
      expect(await term.text()).not.toContain("tool_40")
      expect(await term.text()).not.toContain("mcp__many__")
      const detailHint = "↑↓ move"
      await term.waitForText("Esc back")

      await term.press("End")
      await term.waitForText("▸ tool_40")
      await term.waitForText(/↑ \d+ more/)
      expect(await term.text()).not.toMatch(/↓ \d+ more/)
      const lines = await term.lines()
      const last = lines.findIndex((line) => line.includes("tool_40"))
      const hint = lines.findIndex((line) => line.includes("Esc back"))
      expect(hint).toBeGreaterThan(last)
      expect(lines[hint]).toContain(detailHint)
      expect(lines.filter((line) => line.includes("Esc back"))).toHaveLength(1)
    })
  })
})
