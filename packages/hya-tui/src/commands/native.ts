/** The built-in slash commands. Add a command by appending a `CommandSpec` here. */
import { brief, operations } from "../api"
import { parseApiCommand } from "../client"
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
      const id = target && /^\d+$/.test(target) ? store.state.sessions[Number(target) - 1]?.id : target
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
    description: "Change current session model",
    argumentHint: "<provider/model>",
    complete: ({ words, current, head }, context) => words.length === 1 ? matchValues(head, current, context.models) : [],
    run: async ({ store, client, actions }, { args }) => {
      const selected = store.state.selected
      if (!selected || !args[0]) throw new Error("Usage: /model <provider/model> in a session")
      store.setSelected(await client.updateSessionModel(selected.id, args[0]))
      await actions.refresh()
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
    description: "Cancel current turn",
    run: async ({ store, client }) => {
      const selected = store.state.selected
      if (!selected || !store.state.turnId) throw new Error("No active turn")
      await client.cancelTurn(selected.id, store.state.turnId)
      store.setStatus("Cancellation requested")
    },
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
