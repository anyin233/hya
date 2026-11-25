import { matchValues, type CommandSpec } from "../registry"

const workflowActions = ["select", "run"]

export const workflowCommands: CommandSpec[] = [
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
]
