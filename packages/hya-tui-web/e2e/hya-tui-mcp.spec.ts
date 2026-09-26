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

import { hyaTui, test } from "./hya"

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
})
