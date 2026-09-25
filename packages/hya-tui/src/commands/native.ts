/** The built-in slash commands. Add a command by appending a `CommandSpec` here. */
import { brief, operations } from "../api"
import { parseApiCommand } from "../client"
import { modelReference, sessionTree } from "../state/format"
import { parseSwitch, sidebarVisible } from "../state/layout"
import { CommandRegistry, matchValues, type CommandContext, type CommandInvocation, type CommandSpec } from "./registry"

const workflowActions = ["select", "run"]
const switchValues = ["on", "off"]

function keyUsage(): Error { return new Error("Usage: /key set|remove <provider> or /login <provider>") }

async function removeKey(context: CommandContext, provider: string): Promise<void> {
  await context.client.removeProviderKey(provider)
  context.store.setView("keys")
  await context.actions.refresh()
  context.store.setStatus(`Removed key for ${provider} · restart backend to apply`)
}

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
    description: "Show commands and keys",
    run: ({ store }) => { store.setView("help") },
  },
  {
    name: "/sessions",
    description: "Refresh the session list and show the sidebar",
    run: async ({ store, actions }) => {
      store.setView("chat")
      // The list lives in the sidebar; show it when the width hides it.
      if (!sidebarVisible(store.state.sidebar, store.state.columns)) store.setSidebar("open")
      await actions.refresh()
      store.setStatus("Use /open <id> or /open <number>")
    },
  },
  {
    name: "/new",
    description: "Create a session",
    argumentHint: "[agent] [model]",
    complete: ({ words, current, head }, context) => {
      if (words.length === 1) return matchValues(head, current, context.agents)
      if (words.length === 2) return matchValues(head, current, context.models)
      return []
    },
    run: ({ actions }, { args }) => actions.newSession(args[0], args[1]),
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
    description: "Change current session model, or show the current model and available models with no argument",
    argumentHint: "[provider/model]",
    complete: ({ words, current, head }, context) => words.length === 1 ? matchValues(head, current, context.models) : [],
    run: async ({ store, client, actions }, { args }) => {
      const selected = store.state.selected
      if (!selected) throw new Error("Usage: /model [provider/model] in a session")
      if (!args[0]) {
        store.setStatus(`Model ${modelReference(selected) || "default"} · available: ${store.state.models.map((model) => model.id).join(", ") || "none"}`)
        return
      }
      store.setSelected(await client.updateSessionModel(selected.id, args[0]))
      await actions.refresh()
    },
  },
  {
    name: "/agent",
    description: "Change current session agent, or show the current agent and available agents with no argument",
    argumentHint: "[name]",
    complete: ({ words, current, head }, context) => words.length === 1 ? matchValues(head, current, context.agents) : [],
    run: async ({ store, client, actions }, { args }) => {
      const selected = store.state.selected
      if (!selected) throw new Error("Usage: /agent [name] in a session")
      if (!args[0]) {
        store.setStatus(`Agent ${selected.agent} · available: ${store.state.agents.filter((agent) => !agent.hidden).map((agent) => agent.name).join(", ") || "none"}`)
        return
      }
      store.setSelected(await client.updateSession(selected.id, { agent: args[0] }))
      await actions.refresh()
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
      store.setStatus(`Compacted · ${result.strategy || "done"}`)
    },
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
    description: "Show connection, backend version, directory, session, agent, model, and permission mode",
    run: ({ store, client }) => {
      const selected = store.state.selected
      const lines = [
        `Server      ${client.baseUrl}`,
        `Version     ${store.state.serverVersion || "unknown"}`,
        `Directory   ${client.directory}`,
        `Session     ${selected ? (selected.title || selected.id) : "none"}`,
        `Agent       ${selected?.agent ?? "none"}`,
        `Model       ${selected ? (modelReference(selected) || "default") : "none"}`,
        `Mode        ${selected?.permissionMode || "manual"}`,
      ]
      store.setStatusText(lines.join("\n"))
      store.setView("status")
    },
  },
  {
    name: "/keys",
    description: "List saved provider key names",
    run: async ({ store, actions }) => { store.setView("keys"); await actions.refresh() },
  },
  {
    name: "/key",
    description: "Enter a key in a concealed prompt, or delete a saved key",
    argumentHint: "set|remove <provider>",
    complete: ({ words, current, head }, context) => {
      if (words.length === 1) return matchValues(head, current, ["set", "remove"])
      if (words.length === 2 && words[0] === "set") return matchValues(head, current, context.providers)
      if (words.length === 2 && words[0] === "remove") return matchValues(head, current, context.savedKeys)
      return []
    },
    run: async (context, { args }) => {
      const [action, provider] = args
      if (!provider || !["set", "remove"].includes(action ?? "")) throw keyUsage()
      if (action === "remove") await removeKey(context, provider)
      else context.actions.beginKeyEntry(provider)
    },
  },
  {
    name: "/login",
    description: "Alias for /key set",
    argumentHint: "<provider>",
    complete: ({ words, current, head }, context) => words.length === 1 ? matchValues(head, current, context.providers) : [],
    run: ({ actions }, { args }) => {
      if (!args[0]) throw keyUsage()
      actions.beginKeyEntry(args[0])
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
    name: "/exit",
    description: "Quit the TUI (Ctrl+C twice, Ctrl+D on an empty input)",
    run: ({ actions }) => { actions.quit() },
  },
  {
    name: "/quit",
    description: "Alias for /exit",
    run: ({ actions }) => { actions.quit() },
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
