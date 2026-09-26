import { expect, test } from "bun:test"
import type { McpServerStatus } from "../src/client"
import type { KeyLike } from "../src/keys/bindings"
import {
  mcpToolLabel,
  initialMcpView,
  mcpToolIndex,
  mcpToolWindow,
  mcpViewHint,
  mcpViewKey,
  serverHeaderLine,
  serverLine,
  serverStateText,
  settleMcpView,
  shownServers,
  type McpViewState,
} from "../src/state/mcp"

const key = (name: string, extra: Partial<KeyLike> = {}): KeyLike => ({
  name, ctrl: false, meta: false, shift: false, sequence: name.length === 1 ? name : "", ...extra,
})

const servers: McpServerStatus[] = [
  { name: "alpha", state: "MCP_SERVER_STATE_CONNECTED", tools: ["mcp__alpha__a", "mcp__alpha__b"] },
  { name: "beta", state: "MCP_SERVER_STATE_FAILED", error: "boom", authRequired: true },
]

test("serverStateText maps every McpServerState", () => {
  expect(serverStateText("MCP_SERVER_STATE_DESIRED")).toBe("not connected")
  expect(serverStateText("MCP_SERVER_STATE_CONNECTED")).toBe("connected")
  expect(serverStateText("MCP_SERVER_STATE_DISCONNECTED")).toBe("disconnected")
  expect(serverStateText("MCP_SERVER_STATE_FAILED")).toBe("failed")
  expect(serverStateText(undefined)).toBe("—")
})

test("serverLine and serverHeaderLine fit the width and show tool count, auth, error", () => {
  expect(serverLine(servers[0]!, 80)).toContain("2 tools")
  expect(serverLine(servers[1]!, 80)).toContain("auth required")
  expect(serverLine(servers[1]!, 80)).toContain("boom")
  expect(serverHeaderLine(80)).toContain("SERVER")
  expect(Bun.stringWidth(serverLine(servers[1]!, 30))).toBeLessThanOrEqual(30)
})

test("shownServers filters by name or state on the list screen only", () => {
  expect(shownServers({ screen: "list", filter: "" }, servers)).toHaveLength(2)
  expect(shownServers({ screen: "list", filter: "alpha" }, servers)).toEqual([servers[0]])
  expect(shownServers({ screen: "list", filter: "failed" }, servers)).toEqual([servers[1]])
  expect(shownServers({ screen: "detail", filter: "alpha" }, servers)).toHaveLength(2)
})

test("initialMcpView highlights the first server", () => {
  expect(initialMcpView(servers).server).toBe("alpha")
  expect(initialMcpView([]).server).toBeUndefined()
})

test("settleMcpView keeps the highlight when the row still exists", () => {
  const view: McpViewState = { screen: "list", server: "beta", filter: "", filtering: false }
  expect(settleMcpView(view, servers).server).toBe("beta")
  expect(settleMcpView(view, [servers[0]!]).server).toBe("alpha")
})

test("Enter opens detail, Esc/Left backs out to the list", () => {
  const view: McpViewState = { screen: "list", server: "alpha", filter: "", filtering: false }
  const opened = mcpViewKey(view, key("return"), servers)
  expect(opened).toEqual({ type: "update", view: { ...view, screen: "detail", toolIndex: 0 } })
  const detail: McpViewState = { ...view, screen: "detail" }
  expect(mcpViewKey(detail, key("escape"), servers)).toEqual({ type: "update", view: { ...detail, screen: "list" } })
})

test("c / x dispatch connect / disconnect commands for the highlighted server", () => {
  const view: McpViewState = { screen: "list", server: "alpha", filter: "", filtering: false }
  expect(mcpViewKey(view, key("c", { sequence: "c" }), servers)).toEqual({ type: "command", command: { kind: "connect", server: "alpha" } })
  expect(mcpViewKey(view, key("x", { sequence: "x" }), servers)).toEqual({ type: "command", command: { kind: "disconnect", server: "alpha" } })
})

test("a starts auth only when the server requires it", () => {
  const noAuth: McpViewState = { screen: "list", server: "alpha", filter: "", filtering: false }
  expect(mcpViewKey(noAuth, key("a", { sequence: "a" }), servers).type).toBe("update")
  const needsAuth: McpViewState = { screen: "list", server: "beta", filter: "", filtering: false }
  expect(mcpViewKey(needsAuth, key("a", { sequence: "a" }), servers)).toEqual({ type: "command", command: { kind: "auth", server: "beta" } })
})

test("auth pop-up: typing edits the code, Enter completes, Esc cancels", () => {
  const view: McpViewState = { screen: "list", server: "beta", filter: "", filtering: false, auth: { server: "beta", url: "https://example.com", code: "" } }
  const typed = mcpViewKey(view, key("1", { sequence: "1" }), servers)
  expect(typed).toEqual({ type: "update", view: { screen: "list", server: "beta", filter: "", filtering: false, auth: { ...view.auth!, code: "1" } } })
  const withCode = { ...view, auth: { ...view.auth!, code: "123" } }
  expect(mcpViewKey(withCode, key("return"), servers)).toEqual({ type: "completeAuth", server: "beta", code: "123" })
  expect(mcpViewKey(view, key("return"), servers)).toEqual({ type: "none" })
  const cancelled = mcpViewKey(view, key("escape"), servers)
  expect(cancelled.type).toBe("update")
})

test("r refreshes, Esc on the list closes the view", () => {
  const view: McpViewState = { screen: "list", server: "alpha", filter: "", filtering: false }
  expect(mcpViewKey(view, key("r", { sequence: "r" }), servers)).toEqual({ type: "refresh" })
  expect(mcpViewKey(view, key("escape"), servers)).toEqual({ type: "close" })
})

test("r refreshes even with no server selected (empty list)", () => {
  const view: McpViewState = { screen: "list", server: undefined, filter: "", filtering: false }
  expect(mcpViewKey(view, key("r", { sequence: "r" }), [])).toEqual({ type: "refresh" })
})

test("mcpToolLabel shows a tool under the server's own name, dropping the mcp__<server>__ namespace", () => {
  expect(mcpToolLabel("many", "mcp__many__tool_01")).toBe("tool_01")
  expect(mcpToolLabel("many", "mcp__many__")).toBe("mcp__many__")
  expect(mcpToolLabel("many", "mcp__other__ping")).toBe("mcp__other__ping")
  expect(mcpToolLabel("many", "search")).toBe("search")
})

test("mcpToolWindow keeps the highlighted tool in view and reports the more-above/below counts", () => {
  expect(mcpToolWindow(20, 0, 5)).toEqual({ start: 0, end: 5, moreAbove: 0, moreBelow: 15 })
  expect(mcpToolWindow(20, 19, 5)).toEqual({ start: 15, end: 20, moreAbove: 15, moreBelow: 0 })
  expect(mcpToolWindow(20, 10, 5)).toEqual({ start: 6, end: 11, moreAbove: 6, moreBelow: 9 })
  expect(mcpToolWindow(3, 1, 5)).toEqual({ start: 0, end: 3, moreAbove: 0, moreBelow: 0 })
})

test("Up/Down/PgUp/PgDn/Home/End move the tool highlight on the detail screen instead of switching servers", () => {
  const many = { name: "gamma", state: "MCP_SERVER_STATE_CONNECTED", tools: Array.from({ length: 20 }, (_, i) => `tool${i}`) }
  const all = [...servers, many]
  const view: McpViewState = { screen: "detail", server: "gamma", filter: "", filtering: false, toolIndex: 0 }
  const down = mcpViewKey(view, key("down"), all)
  expect(down).toEqual({ type: "update", view: { ...view, toolIndex: 1 } })
  expect(mcpToolIndex((down as { view: McpViewState }).view)).toBe(1)
  expect(mcpViewKey(view, key("pagedown"), all)).toEqual({ type: "update", view: { ...view, toolIndex: 10 } })
  expect(mcpViewKey({ ...view, toolIndex: 15 }, key("pageup"), all)).toEqual({ type: "update", view: { ...view, toolIndex: 5 } })
  expect(mcpViewKey(view, key("end"), all)).toEqual({ type: "update", view: { ...view, toolIndex: 19 } })
  expect(mcpViewKey({ ...view, toolIndex: 19 }, key("home"), all)).toEqual({ type: "update", view: { ...view, toolIndex: 0 } })
  // The list screen keeps switching servers on Up/Down.
  const listView: McpViewState = { screen: "list", server: "alpha", filter: "", filtering: false }
  expect(mcpViewKey(listView, key("down"), servers)).toEqual({ type: "update", view: { ...listView, server: "beta" } })
})

test("Enter into detail resets the tool highlight to 0", () => {
  const view: McpViewState = { screen: "list", server: "alpha", filter: "", filtering: false, toolIndex: 7 }
  expect(mcpViewKey(view, key("return"), servers)).toEqual({ type: "update", view: { ...view, screen: "detail", toolIndex: 0 } })
})

test("mcpViewHint reflects auth, busy, filtering, and screen defaults", () => {
  expect(mcpViewHint({ screen: "list", server: undefined, filter: "", filtering: false, auth: { server: "beta", url: "u", code: "" } })).toContain("Type the callback code")
  expect(mcpViewHint({ screen: "list", server: undefined, filter: "", filtering: false, auth: { server: "beta", url: "u", code: "12" } })).toContain("Enter completes")
  expect(mcpViewHint({ screen: "list", server: undefined, filter: "", filtering: false, busy: { kind: "refresh", label: "Refreshing", startedAt: 0, server: "" } })).toContain("Esc cancels")
  expect(mcpViewHint({ screen: "detail", server: "alpha", filter: "", filtering: false })).toContain("Esc back")
  expect(mcpViewHint({ screen: "list", server: "alpha", filter: "", filtering: false })).toContain("Esc close")
})
