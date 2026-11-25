import { effectiveMode, modeRows } from "../../state/modes"
import { matchValues, type CommandContext, type CommandSpec } from "../registry"

async function respondAndReload(context: CommandContext, respond: Promise<unknown>): Promise<void> {
  await respond
  context.store.setInteractions(await context.client.listInteractions())
}

export const permissionModeCommand: CommandSpec = {
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
}

export const interactionCommands: CommandSpec[] = [
  {
    name: "/pending",
    description: "Open the oldest pending request in another session",
    run: async ({ actions }) => actions.reviewPending(),
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
]
