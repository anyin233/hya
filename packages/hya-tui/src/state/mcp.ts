/**
 * The MCP view (`/mcp`, docs/tui.md "MCP servers"): pure state and keys over
 * `GET /v1/mcp` and its connect / disconnect / auth routes
 * (components/McpView.tsx renders it; app/mcp.ts owns the calls).
 *
 * Screens: `list` (every configured server: name, state, tool count, error)
 * and `detail` (one server's tools, opened with Enter). `a` on a server that
 * needs a login starts its OAuth flow: the authorization URL is shown and
 * copied (OSC 52) at once, then a one-line pop-up (`auth`) takes the
 * callback code; Enter completes the flow, Esc cancels the pop-up only.
 */
import type { McpServerStatus } from "../client"
import type { KeyLike } from "../keys/bindings"
import { truncate } from "./format"

export type McpScreen = "list" | "detail"

export interface McpBusy {
  kind: "refresh" | "connect" | "disconnect" | "auth" | "authComplete"
  label: string
  startedAt: number
  server: string
}

export interface McpNotice {
  text: string
  tone: "info" | "ok" | "error"
}

/** The OAuth callback-code pop-up, open once `StartMcpAuth` returned a URL. */
export interface McpAuthPopup {
  server: string
  url: string
  code: string
}

export interface McpViewState {
  screen: McpScreen
  server: string | undefined
  filter: string
  filtering: boolean
  busy?: McpBusy
  notice?: McpNotice
  auth?: McpAuthPopup
}

export type McpCommand =
  | { kind: "connect"; server: string }
  | { kind: "disconnect"; server: string }
  | { kind: "auth"; server: string }

export type McpViewOutcome =
  | { type: "none" }
  | { type: "update"; view: McpViewState }
  | { type: "close" }
  | { type: "cancelBusy" }
  | { type: "refresh" }
  | { type: "command"; command: McpCommand }
  | { type: "completeAuth"; server: string; code: string }

export interface McpKeyRow {
  keys: string
  description: string
  hint?: string
  screens: readonly McpScreen[]
}

export const mcpKeyRows: readonly McpKeyRow[] = [
  { keys: "Up / Down", description: "Move the highlight over servers", hint: "↑↓ move", screens: ["list"] },
  { keys: "Enter", description: "Show the highlighted server's tools", hint: "Enter tools", screens: ["list"] },
  { keys: "c", description: "Connect the highlighted server now", hint: "c connect", screens: ["list", "detail"] },
  { keys: "x", description: "Disconnect the highlighted server", hint: "x disconnect", screens: ["list", "detail"] },
  { keys: "a", description: "Start the server's login; the URL is copied at once, then enter the callback code", hint: "a auth", screens: ["list", "detail"] },
  { keys: "r", description: "Refresh server status", hint: "r refresh", screens: ["list", "detail"] },
  { keys: "/", description: "Filter the rows by typing; Enter keeps the filter, Esc clears it", hint: "/ filter", screens: ["list"] },
  { keys: "Esc", description: "Cancel a running call, close the code pop-up, back out of the filter, back to the list, close the view", screens: ["list", "detail"] },
]

/** `McpServerState` in words. */
export function serverStateText(state: string | undefined): string {
  const name = (state ?? "").replace(/^MCP_SERVER_STATE_/, "")
  switch (name) {
    case "DESIRED": return "not connected"
    case "CONNECTED": return "connected"
    case "DISCONNECTED": return "disconnected"
    case "FAILED": return "failed"
    default: return "—"
  }
}

function cell(text: string, width: number): string {
  const cut = truncate(text, width)
  return cut + " ".repeat(Math.max(0, width - Bun.stringWidth(cut)))
}

/** One server row: name, state, tool count, auth requirement, error (fits `width`). */
export function serverLine(server: McpServerStatus, width: number): string {
  const tools = server.tools?.length ?? 0
  const line = `${cell(server.name, 20)} ${cell(serverStateText(server.state), 14)} ${cell(`${tools} tool${tools === 1 ? "" : "s"}`, 10)} ${cell(server.authRequired ? "auth required" : "", 14)} ${server.error || ""}`
  return truncate(line.trimEnd(), width)
}

export function serverHeaderLine(width: number): string {
  return truncate(`${cell("SERVER", 20)} ${cell("STATE", 14)} ${cell("TOOLS", 10)} ${cell("AUTH", 14)} ERROR`, width)
}

function matches(haystack: string, filter: string): boolean {
  const words = filter.toLowerCase().split(/\s+/).filter(Boolean)
  const text = haystack.toLowerCase()
  return words.every((word) => text.includes(word))
}

/** Servers shown on the list screen, filtered by name or state. */
export function shownServers(view: Pick<McpViewState, "screen" | "filter">, servers: readonly McpServerStatus[]): McpServerStatus[] {
  if (view.screen !== "list" || !view.filter) return [...servers]
  return servers.filter((row) => matches(`${row.name}\n${serverStateText(row.state)}`, view.filter))
}

export function initialMcpView(servers: readonly McpServerStatus[]): McpViewState {
  return { screen: "list", server: shownServers({ screen: "list", filter: "" }, servers)[0]?.name, filter: "", filtering: false }
}

function settle(view: McpViewState, servers: readonly McpServerStatus[]): McpViewState {
  const rows = shownServers(view, servers)
  return rows.some((row) => row.name === view.server) || !rows.length ? view : { ...view, server: rows[0]!.name }
}

/** Keep the highlight on its row after a reload. */
export function settleMcpView(view: McpViewState, servers: readonly McpServerStatus[]): McpViewState {
  return settle(view, servers)
}

const printable = (key: KeyLike): boolean => !key.ctrl && !key.meta && key.sequence.length === 1 && key.sequence >= " " && key.sequence !== "\x7f"
const isEnter = (key: KeyLike): boolean => !key.meta && (key.name === "return" || key.name === "enter" || key.name === "kpenter")

function move(view: McpViewState, servers: readonly McpServerStatus[], step: number): McpViewState {
  const rows = shownServers(view, servers)
  if (!rows.length) return view
  const at = rows.findIndex((row) => row.name === view.server)
  return { ...view, server: rows[(at + step + rows.length) % rows.length]!.name }
}

/** One key in the auth code pop-up. */
function authKey(auth: McpAuthPopup, key: KeyLike): McpViewOutcome {
  if (key.name === "escape") return { type: "update", view: { screen: "list", server: auth.server, filter: "", filtering: false, notice: { tone: "info", text: "Login cancelled" } } }
  if (isEnter(key)) return auth.code ? { type: "completeAuth", server: auth.server, code: auth.code } : { type: "none" }
  if (key.name === "backspace") return { type: "update", view: { screen: "list", server: auth.server, filter: "", filtering: false, auth: { ...auth, code: auth.code.slice(0, -1) } } }
  if (printable(key)) return { type: "update", view: { screen: "list", server: auth.server, filter: "", filtering: false, auth: { ...auth, code: auth.code + key.sequence } } }
  return { type: "none" }
}

/** One key while the MCP view is open. */
export function mcpViewKey(view: McpViewState, key: KeyLike, servers: readonly McpServerStatus[]): McpViewOutcome {
  if (view.auth) return authKey(view.auth, key)
  if (view.busy) return key.name === "escape" ? { type: "cancelBusy" } : { type: "none" }
  if (key.name === "up") return { type: "update", view: move(view, servers, -1) }
  if (key.name === "down") return { type: "update", view: move(view, servers, 1) }
  if (view.filtering) {
    if (key.name === "escape") return { type: "update", view: settle({ ...view, filtering: false, filter: "" }, servers) }
    if (isEnter(key)) return { type: "update", view: { ...view, filtering: false } }
    if (key.name === "backspace") return { type: "update", view: settle({ ...view, filter: view.filter.slice(0, -1) }, servers) }
    if (printable(key)) return { type: "update", view: settle({ ...view, filter: view.filter + key.sequence }, servers) }
    return { type: "none" }
  }
  if (key.name === "escape" || (key.name === "left" && view.screen === "detail")) {
    if (view.filter) return { type: "update", view: settle({ ...view, filter: "" }, servers) }
    if (view.screen === "detail") return { type: "update", view: { ...view, screen: "list" } }
    return key.name === "escape" ? { type: "close" } : { type: "none" }
  }
  if (key.ctrl || key.meta) return { type: "none" }
  if (key.sequence === "/" && view.screen === "list") return { type: "update", view: { ...view, filtering: true } }
  const server = servers.find((row) => row.name === view.server)
  if ((isEnter(key) || key.name === "right") && view.screen === "list") {
    return server ? { type: "update", view: { ...view, screen: "detail" } } : { type: "none" }
  }
  if (!"cxar".includes(key.sequence) || key.sequence.length !== 1) return { type: "none" }
  if (key.sequence === "r") return { type: "refresh" }
  if (!server) return { type: "update", view: { ...view, notice: { tone: "info", text: "No server selected" } } }
  switch (key.sequence) {
    case "c": return { type: "command", command: { kind: "connect", server: server.name } }
    case "x": return { type: "command", command: { kind: "disconnect", server: server.name } }
    case "a":
      if (!server.authRequired) return { type: "update", view: { ...view, notice: { tone: "info", text: `${server.name} does not need a login` } } }
      return { type: "command", command: { kind: "auth", server: server.name } }
    default: return { type: "none" }
  }
}

/** The footer hint for the current screen, filter, or pop-up. */
export function mcpViewHint(view: McpViewState): string {
  if (view.auth) return view.auth.code ? "Enter completes the login · Esc cancels" : "Type the callback code · Esc cancels"
  if (view.busy) return `${view.busy.label}… · Esc cancels`
  if (view.filtering) return "Type to filter · Enter keeps the filter · Esc clears it"
  const keys = mcpKeyRows.filter((row) => row.hint && row.screens.includes(view.screen)).map((row) => row.hint!)
  return [...keys, view.screen === "detail" ? "Esc back" : "Esc close"].join(" · ")
}
