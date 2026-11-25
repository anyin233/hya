import { sessionRows } from "../../state/catalog"
import { sessionNumbers, sessionTree, strategyText } from "../../state/format"
import type { PickerAction } from "../../state/picker"
import { matchValues, type ArgumentPosition, type CommandContext, type CommandSpec, type Completion } from "../registry"
import type { CompletionContext } from "../../completion"

/**
 * Session id completions (`/open`, `/resume`): a titled session shows as
 * `title (id)` and also matches by its title; the replacement is always the id.
 */
function matchSessions({ head, current }: ArgumentPosition, sessions: CompletionContext["sessions"]): Completion[] {
  const prefix = current.toLowerCase()
  return sessions
    .filter((session) => session.id.toLowerCase().startsWith(prefix) || session.title?.toLowerCase().startsWith(prefix))
    .map((session) => ({ replacement: `${head}${session.id}`, label: `${head}${session.title ? `${session.title} (${session.id})` : session.id}` }))
    .sort((a, b) => a.label.localeCompare(b.label))
}

/**
 * `/sessions` picker row actions (C13): F2 renames, Ctrl+D deletes,
 * Ctrl+A shows or hides archived sessions,
 * F3 toggles showing every Project's sessions.
 */
export const sessionPickerActions: readonly PickerAction[] = [
  { id: "rename", key: "f2", label: "F2 rename", prompt: "value" },
  { id: "delete", key: "d", ctrl: true, label: "Ctrl+D delete", prompt: "confirm", confirmText: 'Delete "{label}"? This cannot be undone · Enter confirms · Esc cancels' },
  { id: "archived", key: "a", ctrl: true, label: "Ctrl+A archived sessions (show or hide; opening one unarchives it)", prompt: "none" },
  { id: "allProjects", key: "f3", label: "F3 all projects", prompt: "none" },
]

/**
 * Open the `/sessions` picker (C13): a `New session` row first, then the
 * tree, scoped to the active Project unless F3's "all projects" toggle is on
 * (state/store.ts `sessionsPickerAllProjects`); Enter opens, F2 renames,
 * Ctrl+D deletes with confirmation, Ctrl+A hides or shows archived sessions
 * (listed with `includeArchived` by default, tagged `archived`; opening one unarchives
 * it, like `/resume`).
 */
async function openSessionsPicker(context: CommandContext, showArchived = true): Promise<void> {
  const { store, client, actions } = context
  const allProjects = store.state.sessionsPickerAllProjects
  // The sidebar keeps the default listing; the archived view is this picker's own.
  const sessions = showArchived ? await client.listSessions({ includeArchived: true }) : store.state.sessions
  const archived = new Set(sessions.filter((session) => session.archived).map((session) => session.id))
  const title = ["Sessions", ...(allProjects ? ["all projects"] : []), ...(showArchived ? ["archived included"] : [])].join(" · ")
  actions.openPicker({
    title,
    rows: sessionRows(sessions, store.state.selected?.id, Date.now(), { activeProjectId: store.state.activeProjectId, allProjects, numbered: store.state.sessions }),
    // Kept at 72 columns or less (docs/tui.md "Sidebar"): the picker box's content
    // width is `min(96, terminalWidth - 4) - 4` (border + `paddingX`), and 80-column
    // terminals are common (`min(96, 80 - 4) - 4 = 72`).
    // Enter-to-open is left implicit to make room for F3.
    hint: `F2 rename · Ctrl+D del · Ctrl+A ${showArchived ? "hides" : "shows"} archived · F3 all · Esc closes`,
    actions: sessionPickerActions.map((action) => action.id === "delete"
      ? { ...action, disabled: (row) => sessions.find((session) => session.id === row.id)?.parent ? "Archive the subagent or delete the root" : undefined }
      : action),
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
        if (sessions.find((session) => session.id === row.id)?.parent) {
          store.setStatus("Archive the subagent or delete the root")
          return
        }
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

export const sessionNavigationCommands: CommandSpec[] = [
  {
    name: "/sessions",
    description: "Pick a session (open, rename with F2, or delete with Ctrl+D)",
    run: async (context) => {
      const { store, actions } = context
      store.setView("chat")
      await actions.refresh()
      await openSessionsPicker(context)
    },
  },
  {
    name: "/resume",
    description: "Reopen a session and unarchive it: pick one of the active Project's sessions (archived ones included, newest first), or name its id",
    argumentHint: "[id]",
    complete: (position, context) => position.words.length === 1 ? matchSessions(position, context.sessions) : [],
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
    run: async ({ store, actions }, { args }) => {
      if (args[0] === "--temp") await actions.newTemporarySession(args[1], args[2])
      else await actions.newSession(args[0], args[1])
      // A new session waits for its first prompt: typing goes to the composer, not
      // back to the Projects sidebar the command pane was opened from (CommandPane `close`).
      store.setProjectsFocus(false)
    },
  },
  {
    // The backend catalog lists `/clear` ("start a fresh session") for the client to run.
    name: "/clear",
    description: "Start a fresh session in the active Project (same as /new)",
    run: async ({ store, actions }) => {
      await actions.newSession()
      store.setProjectsFocus(false)
    },
  },
  {
    name: "/open",
    description: "Open a session",
    argumentHint: "<id|number>",
    complete: (position, context) => position.words.length === 1 ? matchSessions(position, context.sessions) : [],
    run: async ({ store, actions }, { args }) => {
      const target = args[0]
      // The sidebar's numbers (state/format.ts `sessionNumbers`): the active Project's sessions, subagents as 2.1.3.
      const id = target && /^\d+(?:\.\d+)*$/.test(target)
        ? [...sessionNumbers(store.state.sessions, store.state.activeProjectId)].find(([, number]) => number === target)?.[0]
        : target
      if (!id) throw new Error("Usage: /open <session id or number>")
      await actions.openSession(id)
    },
  },
]

export const sessionHistoryCommands: CommandSpec[] = [
  {
    name: "/rename",
    description: "Rename the current session",
    argumentHint: "<title>",
    run: async ({ store, client }, { argumentsText }) => {
      const selected = store.state.selected
      const title = argumentsText.trim()
      if (!selected || !title) throw new Error("Usage: /rename <title> in a session")
      const renamed = await client.updateSession(selected.id, { title })
      store.setSelected(renamed)
      // The list row too (sidebar, `/open` completion), without waiting for the global stream's `sessionUpdated`.
      store.patchSessionRow(selected.id, { title: renamed.title ?? title })
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
    description: "Remove earlier context now (the route's native compaction where it has one); your messages and the todo list are kept",
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
]
