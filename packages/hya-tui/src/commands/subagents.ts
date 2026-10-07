import { paneLeaves, pinSubagentViewer, wrapPane } from "../state/panes"
import { subagentRows } from "../state/subagents"
import { matchValues, type CommandSpec } from "./registry"

const usage = "Usage: /subagents [select <session> | pin <viewer-pane> [session] | follow <viewer-pane> | view <session>]"
export const subagentsCommand: CommandSpec = {
  name: "/subagents",
  description: "Show subagent selector and transcripts without leaving the parent conversation",
  argumentHint: "[select <session> | pin <viewer> [session] | follow <viewer> | view <session>]",
  complete: ({ words, current, head }, context) => {
    if (words.length === 1) return matchValues(head, current, ["select", "pin", "follow", "view"])
    if (words.length === 2 && ["pin", "follow"].includes(words[0]!)) return matchValues(head, current, (context.panes ?? []).filter((pane) => pane.kind === "subagent-viewer").map((pane) => pane.id))
    if ((words.length === 2 && ["select", "view"].includes(words[0]!)) || (words.length === 3 && words[0] === "pin")) return matchValues(head, current, context.subagents ?? [])
    return []
  },
  run: ({ store, actions }, { args }) => {
    const rows = subagentRows(store.state)
    const selected = store.state.subagentSelection ?? rows[0]?.id
    let layout = store.state.paneLayout
    const add = (kind: "subagents" | "subagent-viewer"): string => {
      const before = new Set(paneLeaves(layout.root).map((pane) => pane.id))
      const selector = paneLeaves(layout.root).find((pane) => pane.kind === "subagents")
      layout = kind === "subagent-viewer" && selector
        ? wrapPane(layout, selector.id, "column", kind, false)
        : wrapPane(layout, "root", "row", kind, false)
      const added = paneLeaves(layout.root).find((pane) => !before.has(pane.id))
      if (!added) throw new Error("Cannot add pane: workspace pane limit reached")
      return added.id
    }
    const check = (id: string | undefined): string => {
      if (!id || !rows.some((row) => row.id === id)) throw new Error("Choose a subagent of the open session with /subagents select <session>")
      return id
    }
    if (!args.length) {
      if (!store.state.selected) throw new Error("Open a conversation before showing subagents")
      let selector = paneLeaves(layout.root).find((pane) => pane.kind === "subagents")?.id
      if (!selector) selector = add("subagents")
      if (!paneLeaves(layout.root).some((pane) => pane.kind === "subagent-viewer")) add("subagent-viewer")
      layout = { ...layout, active: selector }
    } else if (args[0] === "select" && args.length === 2) {
      store.selectSubagent(check(args[1])); return
    } else if (args[0] === "pin" && (args.length === 2 || args.length === 3)) {
      layout = pinSubagentViewer(layout, args[1]!, check(args[2] ?? selected))
    } else if (args[0] === "follow" && args.length === 2) {
      layout = pinSubagentViewer(layout, args[1]!)
    } else if (args[0] === "view" && args.length === 2) {
      const id = check(args[1]), active = layout.active
      const viewer = add("subagent-viewer")
      layout = { ...pinSubagentViewer(layout, viewer, id), active }
    } else throw new Error(usage)
    // Persist the same leaf/session contract used by /layout reload.
    actions.savePreferences({ paneLayout: layout })
    store.setPaneLayout(layout)
  },
}
