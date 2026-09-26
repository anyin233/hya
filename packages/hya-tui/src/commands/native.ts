/** The built-in slash commands. Add a command by appending a `CommandSpec` here. */
import { brief, operations } from "../api"
import { parseApiCommand } from "../client"
import { agentRows, modelRows, relativeTime, sessionRows } from "../state/catalog"
import { copyNotice } from "../composer/clipboard"
import { modelReference, sessionTree, strategyText, webTabBackgroundNotice } from "../state/format"
import { parseSwitch, projectsSidebarVisible, sidebarVisible } from "../state/layout"
import { lastReplyText, transcriptViews } from "../state/messages"
import { effectiveMode, modeRows } from "../state/modes"
import { forkSourceText } from "../state/revert"
import type { PickerAction } from "../state/picker"
import type { BackendInfo } from "../state/store"
import { setTheme, themeName, themes, type ThemeDefinition } from "../theme"
import { CommandRegistry, matchValues, type CommandContext, type CommandInvocation, type CommandSpec } from "./registry"

/**
 * `/sessions` picker row actions (C13): F2 renames, Ctrl+D deletes (never
 * Ctrl+R — that key means refresh), Ctrl+A shows or hides archived sessions,
 * F3 toggles showing every Project's sessions.
 */
export const sessionPickerActions: readonly PickerAction[] = [
  { id: "rename", key: "f2", label: "F2 rename", prompt: "value" },
  { id: "delete", key: "d", ctrl: true, label: "Ctrl+D delete", prompt: "confirm", confirmText: 'Delete "{label}"? This cannot be undone · Enter confirms · Esc cancels' },
  { id: "archived", key: "a", ctrl: true, label: "Ctrl+A archived sessions (show or hide; opening one unarchives it)", prompt: "none" },
  { id: "allProjects", key: "f3", label: "F3 all projects", prompt: "none" },
]

/**
 * `/to-background` and Ctrl+D: quit at once and leave the session running on
 * the daemon (no archive). In a WebUI tab (`--web-tab`) closing the tab
 * already does that, so this only says so.
 */
export function toBackground({ store, actions }: CommandContext): void {
  if (store.state.webTab) {
    store.setStatus(webTabBackgroundNotice)
    return
  }
  actions.quit("background")
}

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

/**
 * Open the `/sessions` picker (C13): a `New session` row first, then the
 * tree, scoped to the active Project unless F3's "all projects" toggle is on
 * (state/store.ts `sessionsPickerAllProjects`); Enter opens, F2 renames,
 * Ctrl+D deletes with confirmation, Ctrl+A shows or hides archived sessions
 * (listed with `includeArchived`, tagged `archived`; opening one unarchives
 * it, like `/resume`).
 */
async function openSessionsPicker(context: CommandContext, showArchived = false): Promise<void> {
  const { store, client, actions } = context
  const allProjects = store.state.sessionsPickerAllProjects
  // The sidebar keeps the default listing; the archived view is this picker's own.
  const sessions = showArchived ? await client.listSessions({ includeArchived: true }) : store.state.sessions
  const archived = new Set(sessions.filter((session) => session.archived).map((session) => session.id))
  const title = ["Sessions", ...(allProjects ? ["all projects"] : []), ...(showArchived ? ["archived included"] : [])].join(" · ")
  actions.openPicker({
    title,
    rows: sessionRows(sessions, store.state.selected?.id, Date.now(), { activeProjectId: store.state.activeProjectId, allProjects }),
    // Kept at 72 columns or less (docs/tui.md "Sidebar"): the picker box's content
    // width is `min(96, terminalWidth - 4) - 4` (border + `paddingX`), and 80-column
    // terminals are common (`min(96, 80 - 4) - 4 = 72`).
    // Enter-to-open is left implicit to make room for F3.
    hint: `F2 rename · Ctrl+D del · Ctrl+A ${showArchived ? "hides" : "shows"} archived · F3 all · Esc closes`,
    actions: sessionPickerActions,
    onSelect: async (row) => {
      if (row.id === "__new__") { await actions.newSession(); return }
      if (archived.has(row.id)) { await actions.resume(row.id); return }
      await actions.openSession(row.id)
    },
    onAction: async (id, row, value) => {
      if (id === "archived") {
        await openSessionsPicker(context, !showArchived)
        return
      }
      if (id === "allProjects") {
        store.setSessionsPickerAllProjects(!allProjects)
        await openSessionsPicker(context, showArchived)
        return
      }
      if (id === "rename") {
        const title = (value ?? "").trim()
        if (!title) { store.setStatus("Rename cancelled: title cannot be empty"); return }
        const info = await client.updateSession(row.id, { title })
        if (store.state.selected?.id === row.id) store.setSelected(info)
        await actions.refresh()
        store.setStatus(`Renamed to ${title}`)
        await openSessionsPicker(context, showArchived)
      } else if (id === "delete") {
        await actions.deleteSession(row.id)
        const wasOpen = store.state.selected?.id === row.id
        await actions.refresh()
        if (wasOpen) {
          const next = sessionTree(store.state.sessions)[0]?.session
          if (next) await actions.openSession(next.id)
          else store.clearSelected()
        }
        store.setStatus(`Deleted session ${row.id}`)
      }
    },
  })
}

/**
 * Open the `/theme` picker: one row per built-in theme (tag `dark`/`light`),
 * the theme in effect marked. Moving the highlight previews a theme; Enter
 * keeps it and saves it as `theme` in the preferences file (src/prefs.ts);
 * Esc restores the theme in effect when the picker opened.
 */
function openThemePicker({ store, actions }: CommandContext): void {
  const previous = themeName()
  actions.openPicker({
    title: "Theme",
    rows: (Object.values(themes) as ThemeDefinition[]).map((theme) => ({
      id: theme.name, label: theme.label, tag: theme.kind, detail: theme.description, current: theme.name === previous,
    })),
    hint: "↑↓ previews · Enter keeps and saves · Esc restores · type to filter",
    onHighlight: (row) => { setTheme(row.id) },
    onCancel: () => { setTheme(previous) },
    onSelect: (row) => {
      if (!setTheme(row.id)) throw new Error(`Unknown theme ${row.id}`)
      try {
        actions.savePreferences({ theme: row.id })
        store.setStatus(`Theme → ${row.label}`)
      } catch (error) {
        store.setStatus(`Theme → ${row.label} · not saved: ${error instanceof Error ? error.message : String(error)}`)
      }
    },
  })
}

/**
 * Open the `/model` picker: models grouped by provider, the session's model
 * (or the pending choice) marked. With no session the choice is remembered
 * for the next one. The Provider View opens it too (after adding a provider
 * while the session runs on `hya/offline`): `title` and `highlight` (a
 * provider id: its first model is highlighted) are set then.
 */
export function openModelPicker({ store, client, actions }: CommandContext, options: { title?: string; highlight?: string; onChosen?: (model: string) => void } = {}): void {
  const selected = store.state.selected
  const current = selected ? modelReference(selected) : (store.state.pendingModel ?? "")
  // Rows are loaded from the catalog already in `state.models` (no async loading state in the picker itself).
  const rows = modelRows(store.state.models, current)
  const first = options.highlight ? rows.findIndex((row) => row.tag === options.highlight) : -1
  actions.openPicker({
    title: options.title ?? "Model",
    // `current` decides the opening highlight: move it to the provider's first model.
    rows: first < 0 ? rows : rows.map((row, index) => ({ ...row, current: index === first })),
    onSelect: async (row) => {
      const session = store.state.selected
      if (!session) {
        store.setPendingModel(row.id)
        store.setStatus(`Model → ${row.id} · applies when the session is created`)
        options.onChosen?.(row.id)
        return
      }
      store.setSelected(await client.updateSessionModel(session.id, row.id))
      store.setStatus(`Model → ${row.id}`)
      options.onChosen?.(row.id)
      await actions.refresh()
    },
  })
}

const workflowActions = ["select", "run"]
const switchValues = ["on", "off"]

async function respondAndReload(context: CommandContext, respond: Promise<unknown>): Promise<void> {
  await respond
  context.store.setInteractions(await context.client.listInteractions())
}

/** Run an unregistered `/name args` as a backend command turn, creating a session if needed. */
export async function backendCommand(context: CommandContext, invocation: CommandInvocation): Promise<void> {
  const { store, client, actions } = context
  if (!store.state.selected) await actions.newSession()
  const selected = store.state.selected
  if (!selected) throw new Error("Session creation failed")
  const name = invocation.name.slice(1)
  const turn = await client.createCommandTurn(selected.id, name, invocation.argumentsText)
  // `CreateTurn` returns the user message id as the turn id for a command
  // turn (same as a prompt turn); the transcript shows what the user typed
  // instead of the backend's expanded template text (state/messages.ts).
  store.rememberCommand(turn.id, invocation.text)
  store.setTurn(turn.id)
  store.setStatus(turn.id ? `Command ${name} · ${turn.state.toLowerCase()}` : `Command ${name} finished`)
  actions.scheduleRefresh()
}

export const nativeCommandSpecs: CommandSpec[] = [
  {
    name: "/help",
    description: "Show every key and command in a filterable overlay (also ? on an empty input)",
    run: ({ actions }) => { actions.openHelp() },
  },
  {
    name: "/sessions",
    description: "Pick a session (open, rename with F2, or delete with Ctrl+D), and show the sidebar",
    run: async (context) => {
      const { store, actions } = context
      store.setView("chat")
      // The list lives in the sidebar; show it when the width hides it.
      if (!sidebarVisible(store.state.sidebar, store.state.columns)) store.setSidebar("open")
      await actions.refresh()
      await openSessionsPicker(context)
    },
  },
  {
    name: "/resume",
    description: "Reopen a session and unarchive it: pick one of the active Project's sessions (archived ones included, newest first), or name its id",
    argumentHint: "[id]",
    complete: ({ words, current, head }, context) => words.length === 1 ? matchValues(head, current, context.sessions) : [],
    run: ({ actions }, { args }) => actions.resume(args[0]),
  },
  {
    name: "/new",
    description: "Create a session; --temp creates a temporary one (no Project)",
    argumentHint: "[agent] [model] | --temp",
    complete: ({ words, current, head }, context) => {
      if (words.length === 1) return matchValues(head, current, ["--temp", ...context.agents])
      if (words.length === 2) return matchValues(head, current, context.models)
      return []
    },
    run: ({ actions }, { args }) => {
      if (args[0] === "--temp") return actions.newTemporarySession(args[1], args[2])
      return actions.newSession(args[0], args[1])
    },
  },
  {
    name: "/open",
    description: "Open a session",
    argumentHint: "<id|number>",
    complete: ({ words, current, head }, context) => words.length === 1 ? matchValues(head, current, context.sessions) : [],
    run: async ({ store, actions }, { args }) => {
      const target = args[0]
      // Numbers count in the sidebar's order (subagent sessions nested under their parent).
      const id = target && /^\d+$/.test(target) ? sessionTree(store.state.sessions)[Number(target) - 1]?.session.id : target
      if (!id) throw new Error("Usage: /open <session id or number>")
      await actions.openSession(id)
    },
  },
  {
    name: "/models",
    description: "List models",
    run: async ({ store, actions }) => { store.setView("models"); await actions.refresh() },
  },
  {
    name: "/model",
    description: "Pick a session model (models grouped by provider), or set one directly; with no session the choice is remembered for the next one",
    argumentHint: "[provider/model]",
    complete: ({ words, current, head }, context) => words.length === 1 ? matchValues(head, current, context.models) : [],
    run: async ({ store, client, actions }, { args }) => {
      const selected = store.state.selected
      if (args[0]) {
        if (!selected) {
          store.setPendingModel(args[0])
          store.setStatus(`Model → ${args[0]} · applies when the session is created`)
          return
        }
        store.setSelected(await client.updateSessionModel(selected.id, args[0]))
        store.setStatus(`Model → ${modelReference(store.state.selected!) || args[0]}`)
        await actions.refresh()
        return
      }
      openModelPicker({ store, client, actions })
    },
  },
  {
    name: "/agent",
    description: "Pick a visible session agent, or set one directly; with no session the choice is remembered for the next one",
    argumentHint: "[name]",
    complete: ({ words, current, head }, context) => words.length === 1 ? matchValues(head, current, context.agents) : [],
    run: async ({ store, client, actions }, { args }) => {
      const selected = store.state.selected
      if (args[0]) {
        if (!selected) {
          store.setPendingAgent(args[0])
          store.setStatus(`Agent → ${args[0]} · applies when the session is created`)
          return
        }
        store.setSelected(await client.updateSession(selected.id, { agent: args[0] }))
        store.setStatus(`Agent → ${args[0]}`)
        await actions.refresh()
        return
      }
      actions.openPicker({
        title: "Agent",
        rows: agentRows(store.state.agents, selected ? selected.agent : (store.state.pendingAgent ?? "")),
        onSelect: async (row) => {
          if (!selected) {
            store.setPendingAgent(row.id)
            store.setStatus(`Agent → ${row.id} · applies when the session is created`)
            return
          }
          store.setSelected(await client.updateSession(selected.id, { agent: row.id }))
          store.setStatus(`Agent → ${row.id}`)
          await actions.refresh()
        },
      })
    },
  },
  {
    name: "/rename",
    description: "Rename the current session",
    argumentHint: "<title>",
    run: async ({ store, client }, { argumentsText }) => {
      const selected = store.state.selected
      const title = argumentsText.trim()
      if (!selected || !title) throw new Error("Usage: /rename <title> in a session")
      store.setSelected(await client.updateSession(selected.id, { title }))
      store.setStatus(`Renamed to ${title}`)
    },
  },
  {
    name: "/compact",
    description: "Compact the session's context now",
    run: async ({ store, client }) => {
      const selected = store.state.selected
      if (!selected) throw new Error("Usage: /compact in a session")
      store.setStatus("Compacting…")
      const result = await client.compactSession(selected.id)
      store.setStatus(`Compacted · ${result.strategy ? strategyText(result.strategy) : "done"}`)
    },
  },
  {
    name: "/undo",
    description: "Revert the last prompt: hide it and every later message, restore the files its tools changed, and put the prompt back in the input",
    run: ({ actions }) => actions.undo(),
  },
  {
    name: "/redo",
    description: "Undo the pending /undo: bring the messages and file changes back (only until the next prompt)",
    run: ({ actions }) => actions.redo(),
  },
  {
    name: "/fork",
    description: "Fork the session into a new one: at the latest message, or before a picked prompt (which goes back into the input)",
    run: ({ actions }) => { actions.fork() },
  },
  {
    name: "/summarize",
    description: "Summarize the session into a new message",
    run: async ({ store, client, actions }) => {
      const selected = store.state.selected
      if (!selected) throw new Error("Usage: /summarize in a session")
      store.setStatus("Summarizing…")
      await client.summarizeSession(selected.id)
      store.setStatus("Summarized")
      await actions.refreshMessages()
    },
  },
  {
    name: "/todos",
    description: "Show the session's todo list",
    run: async ({ store, client }) => {
      const selected = store.state.selected
      if (!selected) throw new Error("Usage: /todos in a session")
      store.setView("todos")
      store.setTodos(await client.getSessionTodo(selected.id))
    },
  },
  {
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
        `Mode        ${selected?.permissionMode || "manual"}`,
        `Backend     ${backendText(store.state.backend, store.state.serverPid)}`,
      ]
      const web = store.state.web
      if (web) lines.push(`WebUI       ${web.url ? web.url.replace(/\/$/, "") : `unavailable: ${web.error ?? ""} · hya --port <N>`}`)
      store.setStatusText(lines.join("\n"))
      store.setView("status")
    },
  },
  {
    name: "/permissions",
    description: "Pick the session's permission mode (manual, yolo, bundle modes), or set one directly",
    argumentHint: "[mode]",
    complete: ({ words, current, head }, context) => words.length === 1 ? matchValues(head, current, context.permissionModes ?? []) : [],
    run: async ({ store, client, actions }, { args }) => {
      if (args[0]) {
        await actions.requestPermissionMode(args[0])
        return
      }
      const modes = await client.listPermissionModes()
      store.setPermissionModes(modes)
      actions.openPicker({
        title: "Permission mode",
        rows: modeRows(modes, effectiveMode(store.state)),
        onSelect: (row) => actions.requestPermissionMode(row.id),
      })
    },
  },
  {
    name: "/key",
    description: "Open the Provider View: providers, keys, add a provider, fetch and test models, edit model metadata",
    run: ({ actions }) => { actions.openProviders() },
  },
  {
    name: "/diff",
    description: "Open the Diff view: the working tree diff, split per file",
    run: ({ actions }) => { actions.openDiff() },
  },
  {
    name: "/mcp",
    description: "Open the MCP view: server status, tools, connect/disconnect, login",
    run: ({ actions }) => { actions.openMcp() },
  },
  {
    name: "/rules",
    description: "Open the Saved Rules view: saved permission decisions, delete",
    run: ({ actions }) => { actions.openRules() },
  },
  {
    name: "/agent-models",
    description: "Open the Agent Models view: per-agent default model, pick or clear",
    run: ({ actions }) => { actions.openAgentModels() },
  },
  {
    name: "/project",
    description: "Open the Project view: list, open/switch, create, edit roots, rename, delete, or start a temporary session",
    run: ({ actions }) => { actions.openProjectView() },
  },
  {
    name: "/projects",
    description: "Alias for /project",
    run: ({ actions }) => { actions.openProjectView() },
  },
  {
    name: "/projects-sidebar",
    description: "Show or hide the left Projects sidebar. Without an argument it toggles what is visible now",
    argumentHint: "[on|off]",
    run: ({ store }, { args }) => {
      const visible = projectsSidebarVisible(store.state.projectsSidebar, store.state.columns)
      store.setProjectsSidebar(parseSwitch(args[0], visible, "Usage: /projects-sidebar [on|off]") ? "open" : "closed")
      store.setStatus(`Projects sidebar ${projectsSidebarVisible(store.state.projectsSidebar, store.state.columns) ? "shown" : "hidden"} · Ctrl+P toggles`)
    },
  },
  {
    name: "/workflows",
    description: "List workflows",
    run: async ({ store, client, actions }) => {
      store.setView("workflows")
      await actions.refresh()
      const selected = store.state.selected
      store.setWorkflowState(selected ? await client.getWorkflowState(selected.id) : undefined)
    },
  },
  {
    name: "/workflow",
    description: "Select or run a workflow in the current session",
    argumentHint: "select <name> | run [name]",
    complete: ({ words, current, head }, context) => {
      if (words.length === 1) return matchValues(head, current, workflowActions)
      if (words.length === 2 && workflowActions.includes(words[0] ?? "")) return matchValues(head, current, context.workflows)
      return []
    },
    run: async ({ store, client }, { args }) => {
      const selected = store.state.selected
      if (!selected || !workflowActions.includes(args[0] ?? "")) throw new Error("Usage: /workflow select|run [name] in a session")
      const action = args[0]!
      const name = args[1] ?? ""
      if (action === "select" && !name) throw new Error("Usage: /workflow select <name>")
      await client.submitWorkflow(selected.id, action === "select" ? { select: { name } } : { run: { name } })
      store.setWorkflowState(await client.getWorkflowState(selected.id))
      store.setView("workflows")
      store.setStatus(`Workflow ${action} submitted`)
    },
  },
  {
    name: "/interactions",
    description: "Show pending permissions and questions",
    run: async ({ store, actions }) => { store.setView("interactions"); await actions.refresh() },
  },
  {
    name: "/approve",
    description: "Allow a pending permission request",
    argumentHint: "<interaction id>",
    complete: ({ words, current, head }, context) => words.length === 1 ? matchValues(head, current, context.interactions) : [],
    run: async (context, { args }) => {
      if (!args[0]) throw new Error("Usage: /approve <interaction id>")
      await respondAndReload(context, context.client.respondPermission(args[0], true))
    },
  },
  {
    name: "/deny",
    description: "Reject a pending permission request",
    argumentHint: "<interaction id>",
    complete: ({ words, current, head }, context) => words.length === 1 ? matchValues(head, current, context.interactions) : [],
    run: async (context, { args }) => {
      if (!args[0]) throw new Error("Usage: /deny <interaction id>")
      await respondAndReload(context, context.client.respondPermission(args[0], false))
    },
  },
  {
    name: "/answer",
    description: "Answer a pending question",
    argumentHint: "<interaction id> <text>",
    complete: ({ words, current, head }, context) => words.length === 1 ? matchValues(head, current, context.interactions) : [],
    run: async (context, { args }) => {
      if (!args[0] || args.length < 2) throw new Error("Usage: /answer <interaction id> <text>")
      await respondAndReload(context, context.client.respondQuestion(args[0], args.slice(1).join(" ")))
    },
  },
  {
    name: "/cancel",
    description: "Cancel current turn (Esc)",
    run: ({ actions }) => actions.cancelTurn(),
  },
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
  {
    name: "/sidebar",
    description: "Show or hide the sidebar (Ctrl+B)",
    argumentHint: "[on|off]",
    complete: ({ words, current, head }) => words.length === 1 ? matchValues(head, current, switchValues) : [],
    run: ({ store }, { args }) => {
      const shown = parseSwitch(args[0], sidebarVisible(store.state.sidebar, store.state.columns), "Usage: /sidebar [on|off]")
      store.setSidebar(shown ? "open" : "closed")
      store.setStatus(`Sidebar ${shown ? "shown" : "hidden"} · Ctrl+B toggles`)
    },
  },
  {
    name: "/thinking",
    description: "Expand or collapse reasoning blocks (Ctrl+O)",
    argumentHint: "[on|off]",
    complete: ({ words, current, head }) => words.length === 1 ? matchValues(head, current, switchValues) : [],
    run: ({ store }, { args }) => {
      const expanded = parseSwitch(args[0], store.state.thinking, "Usage: /thinking [on|off]")
      store.setThinking(expanded)
      store.setStatus(`Reasoning ${expanded ? "expanded" : "collapsed"} · Ctrl+O toggles`)
    },
  },
  {
    name: "/tools",
    description: "Expand or collapse tool call cards (Ctrl+G)",
    argumentHint: "[on|off]",
    complete: ({ words, current, head }) => words.length === 1 ? matchValues(head, current, switchValues) : [],
    run: ({ store }, { args }) => {
      const expanded = parseSwitch(args[0], store.state.tools ?? false, "Usage: /tools [on|off]")
      store.setTools(expanded)
      store.setStatus(`Tool calls ${expanded ? "expanded" : "collapsed"} · Ctrl+G toggles`)
    },
  },
  {
    name: "/theme",
    description: "Pick the color theme (live preview while moving; Enter keeps and saves it, Esc restores)",
    run: (context) => { openThemePicker(context) },
  },
  {
    name: "/copy",
    description: "Copy the last assistant reply's text to the clipboard (OSC 52)",
    run: ({ store, actions }) => {
      const text = lastReplyText(transcriptViews(store.state))
      if (text === undefined) {
        store.setStatus("Nothing to copy: no assistant reply yet")
        return
      }
      store.setStatus(copyNotice(text, actions.copyText(text)))
    },
  },
  {
    name: "/editor",
    description: "Edit the input in $VISUAL / $EDITOR (fallback vi); the text comes back into the input (Ctrl+X Ctrl+E)",
    run: ({ actions }) => { actions.openEditor() },
  },
  {
    name: "/vim",
    description: "Turn vim mode in the input on or off (saved); Esc switches to normal mode",
    argumentHint: "[on|off]",
    complete: ({ words, current, head }) => words.length === 1 ? matchValues(head, current, switchValues) : [],
    run: ({ store, actions }, { args }) => {
      const on = parseSwitch(args[0], store.state.vim, "Usage: /vim [on|off]")
      store.setVim(on)
      const text = on ? "Vim mode on · Esc for normal mode, i to insert" : "Vim mode off"
      try {
        actions.savePreferences({ vim: on })
        store.setStatus(text)
      } catch (error) {
        store.setStatus(`${text} · not saved: ${error instanceof Error ? error.message : String(error)}`)
      }
    },
  },
  {
    name: "/notifications",
    description: "Turn desktop notifications on or off (saved): a turn finishing, or a permission/question ask, while the terminal is unfocused",
    argumentHint: "[on|off]",
    complete: ({ words, current, head }) => words.length === 1 ? matchValues(head, current, switchValues) : [],
    run: ({ store, actions }, { args }) => {
      const on = parseSwitch(args[0], store.state.notifications, "Usage: /notifications [on|off]")
      store.setNotifications(on)
      const text = `Desktop notifications ${on ? "on" : "off"}`
      try {
        actions.savePreferences({ notifications: on })
        store.setStatus(text)
      } catch (error) {
        store.setStatus(`${text} · not saved: ${error instanceof Error ? error.message : String(error)}`)
      }
    },
  },
  {
    name: "/exit",
    description: "Quit the TUI and archive the session (Ctrl+C twice); an empty session is deleted. /resume brings an archived one back",
    run: ({ actions }) => { actions.quit("archive") },
  },
  {
    name: "/quit",
    description: "Alias for /exit",
    run: ({ actions }) => { actions.quit("archive") },
  },
  {
    name: "/to-background",
    description: "Quit the TUI at once and leave the session running on the backend daemon, not archived (Ctrl+D on an empty input). Not in a WebUI tab: close the tab instead",
    terminalOnly: true,
    run: (context) => { toBackground(context) },
  },
  {
    name: "/api",
    description: "List all v1 HTTP operations, or call one",
    argumentHint: "[METHOD /v1/path [JSON object]]",
    complete: ({ words, current, head }, context) => {
      if (words.length === 1) return matchValues(head, current, ["GET", "POST", "PUT", "PATCH", "DELETE"])
      if (words.length === 2) {
        const method = words[0]?.toUpperCase() ?? ""
        return context.apiOperations
          .filter((operation) => operation.startsWith(`${method} `))
          .map((operation) => operation.slice(method.length + 1))
          .filter((path) => path.toLowerCase().startsWith(current.toLowerCase()))
          .sort()
          .map((path) => `${head}${path}`)
      }
      return []
    },
    run: async ({ store, client }, { args, text }) => {
      store.setView("api")
      if (args.length > 0) {
        const request = parseApiCommand(text)
        const result = await client.request<unknown>(request.method, request.path, request.body)
        store.setApiOutput(`${request.method} ${request.path}\n\n${brief(result)}\n\n/api shows the operation catalog.`)
      } else store.setApiOutput(`Use /api METHOD /v1/path [JSON object].\n\n${operations()}`)
    },
  },
]

/** A registry holding every native command; unknown names run as backend command turns. */
export function createCommandRegistry(): CommandRegistry {
  const registry = new CommandRegistry(backendCommand)
  for (const spec of nativeCommandSpecs) registry.register(spec)
  return registry
}
