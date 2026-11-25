import { relativeTime } from "../../state/catalog"
import { modelReference, thinkingEffortLabel } from "../../state/format"
import { forkSourceText } from "../../state/revert"
import type { BackendInfo } from "../../state/store"
import { matchValues, type CommandSpec } from "../registry"

/**
 * `/status`'s Backend row: the daemon's pid, database, and start time
 * (ADR-0023); `via --backend/--server` when the URL was given explicitly.
 * `serverPid` (bootstrap) fills in a pid nothing else named.
 */
export function backendText(backend: BackendInfo | undefined, serverPid?: number, now = Date.now()): string {
  if (backend?.remoteBridge) return "remote · through this TUI's relay bridge (/disconnect-remote leaves it)"
  const pid = backend?.pid ?? serverPid
  const parts = ["daemon"]
  if (pid) parts.push(`pid ${pid}`)
  if (backend?.db) parts.push(`db ${backend.db}`)
  if (backend?.startedAt) {
    const ago = relativeTime(new Date(backend.startedAt).toISOString(), now)
    if (ago) parts.push(`started ${ago} ago`)
  }
  if (!backend || backend.explicit) parts.push("via --backend/--server")
  return parts.join(" · ")
}

export const statusCommand: CommandSpec = {
  name: "/status",
  description: "Show connection, backend version, directory, session, agent, model, permission mode, and WebUI",
  run: ({ store, client }) => {
    const selected = store.state.selected
    const lines = [
      `Server      ${store.state.serverLabel ? `${store.state.serverLabel} · via ${client.baseUrl}` : client.baseUrl}`,
      `Version     ${store.state.serverVersion || "unknown"}`,
      `Directory   ${client.directory}`,
      `Session     ${selected ? (selected.title || selected.id) : "none"}`,
      ...(selected?.forkedFrom ? [`Forked      ${forkSourceText(selected.forkedFrom, store.state.sessions)!.replace(/^forked /, "")}`] : []),
      `Agent       ${selected?.agent ?? "none"}`,
      `Model       ${selected ? (modelReference(selected) || "default") : "none"}`,
      `Thinking    ${selected ? thinkingEffortLabel(selected) : "default"}`,
      `Mode        ${selected?.permissionMode || "manual"}`,
      `Backend     ${backendText(store.state.backend, store.state.serverPid)}`,
    ]
    const web = store.state.web
    if (web) lines.push(`WebUI       ${web.url ? web.url.replace(/\/$/, "") : `unavailable: ${web.error ?? ""} · hya --port <N>`}`)
    store.setStatusText(lines.join("\n"))
    store.setView("status")
  },
}

export const connectionCommands: CommandSpec[] = [
  {
    name: "/refresh",
    description: "Refresh all views",
    run: async ({ actions }) => { await actions.refresh(); await actions.refreshMessages() },
  },
  {
    name: "/reconnect",
    description: "Find or start the backend now (after hya serve stop)",
    run: ({ actions }) => actions.reconnect(),
  },
  {
    name: "/connect-remote",
    description: "Connect to a remote backend through a relay link (hidden entry without one; the link is never shown or kept in history)",
    argumentHint: "[link] [--transport auto|grpc|ws] [--relay-ca <pem>]",
    complete: ({ current, head }) => current.startsWith("--") ? matchValues(head, current, ["--transport", "--relay-ca"]) : [],
    run: ({ actions }, { args }) => actions.connectRemote(args),
  },
  {
    name: "/disconnect-remote",
    description: "Stop the relay bridge and go back to the local backend",
    run: ({ actions }) => actions.disconnectRemote(),
  },
]
