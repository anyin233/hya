import { paneLeaves, pinSubagentViewer, wrapPane } from "../state/panes"
import { subagentRows } from "../state/subagents"
import { matchValues, type CommandSpec } from "./registry"

const usage = "Usage: /subagents [select <session> | pin <viewer-pane> [session] | follow <viewer-pane> | view <session> | interrupt <session> | archive <session> | steer <session> <text> | archived [show|hide|toggle]]"
/** Subcommands that act on one subagent through the API (interrupt, archive, steer). */
const controls = ["interrupt", "archive", "steer"]
export const subagentsCommand: CommandSpec = {
  name: "/subagents",
  description: "Show subagent selector and transcripts without leaving the parent conversation",
  argumentHint: "[select <session> | pin <viewer> [session] | follow <viewer> | view <session> | interrupt <session> | archive <session> | steer <session> <text> | archived [show|hide|toggle]]",
  complete: ({ words, current, head }, context) => {
    if (words.length === 1) return matchValues(head, current, ["select", "pin", "follow", "view", ...controls, "archived"])
    if (words.length === 2 && words[0] === "archived") return matchValues(head, current, ["show", "hide", "toggle"])
    if (words.length === 2 && ["pin", "follow"].includes(words[0]!)) return matchValues(head, current, (context.panes ?? []).filter((pane) => pane.kind === "subagent-viewer").map((pane) => pane.id))
    if ((words.length === 2 && ["select", "view", ...controls].includes(words[0]!)) || (words.length === 3 && words[0] === "pin")) return matchValues(head, current, context.subagents ?? [])
    return []
  },
  run: async ({ store, actions, client }, { args, argumentsText }) => {
    if (controls.includes(args[0] ?? "")) {
      const [action, id] = args
      // Steer text is everything after the session id, kept as typed (inner spacing too).
      const text = action === "steer" ? argumentsText.trim().replace(/^steer\s+\S+/, "").trim() : ""
      if (!id || (action === "steer" ? !text : args.length !== 2)) throw new Error(usage)
      const row = subagentRows(store.state).find((candidate) => candidate.id === id)
      if (!row) throw new Error("Choose a subagent of the open session with /subagents select <session>")
      const name = row.label || row.handle || id
      if (action === "interrupt") {
        const turn = await client.cancelTurn(id, "current") as { state?: string } | undefined
        store.setStatus(turn?.state === "TURN_STATE_CANCELLED" ? `Interrupted ${name}` : `${name} is not running`)
      } else if (action === "archive") {
        const receipt = await client.archiveSubagent(id)
        const nested = receipt.descendants?.length ?? 0
        store.setStatus(`Archived ${receipt.handle || name}${nested ? ` and ${nested} nested subagent${nested === 1 ? "" : "s"}` : ""} · steer it to wake it`)
      } else {
        await client.steerSubagent(id, text)
        store.setStatus(`Steered ${name}`)
      }
      return
    }
    if (args[0] === "archived") {
      if (args.length > 2 || (args[1] && !["show", "hide", "toggle"].includes(args[1]))) throw new Error(usage)
      if (!store.state.selected) throw new Error("Open a conversation before showing subagents")
      store.setSubagentsShowArchived(args[1] === "show" ? true : args[1] === "hide" ? false : !store.state.subagentsShowArchived)
      return
    }
    const rows = subagentRows(store.state)
    const selected = rows.find((row) => row.id === store.state.subagentSelection)?.id ?? rows[0]?.id
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
