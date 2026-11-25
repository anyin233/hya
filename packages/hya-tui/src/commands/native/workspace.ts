import { extensionManager } from "../../extensions/manager"
import { parseSwitch, projectsSidebarVisible } from "../../state/layout"
import { bubblePane, openLayoutPane, insertPane, movePane, wrapPane, closePane, defaultPaneLayout, isSelectablePane, movePaneFocus, rotatePaneFocus, paneKinds, paneLeaves, resizePane, setPaneKind, splitPane, visiblePaneLayout, type PaneDirection, type PaneKind, type PaneLayout, type LayoutDirection } from "../../state/panes"
import { enableSidebar, layoutNeedsWorkspace } from "../config"
import { matchValues, type CommandSpec } from "../registry"

export const layoutCommand: CommandSpec = {
  name: "/layout",
  description: "Edit workspace panes: split, insert, move, bubble, wrap, remove, assign, focus, resize, close, reload, reset, tree, or show",
  argumentHint: "[split|insert|move|bubble|wrap|remove|assign|focus|resize|close|reload|reset|tree|show]",
  complete: ({ words, current, head }, context) => {
    if (words.length === 1) return matchValues(head, current, ["split", "insert", "move", "bubble", "wrap", "remove", "assign", "focus", "resize", "close", "reload", "reset", "tree", "show"])
    if (words[0] === "split" && words.length === 2) return matchValues(head, current, ["up", "left"])
    if (words[0] === "split" && words.length === 3) return matchValues(head, current, paneKinds.filter((kind) => kind !== "conversation" && kind !== "composer"))
    if ((words[0] === "close" || words[0] === "remove") && words.length === 2) {
      const panes = (context.panes ?? []).filter((pane) => pane.kind !== "conversation" && pane.kind !== "composer")
      const names = panes.filter((pane) => panes.filter((other) => other.kind === pane.kind).length === 1).map((pane) => pane.kind)
      return matchValues(head, current, [...names, ...panes.map((pane) => pane.id), ...(context.layoutContainers ?? []).filter((node) => node.removable).map((node) => node.id)])
    }
    const groups = ["root", ...(context.layoutContainers ?? []).map((node) => node.id)]
    const targets = [...groups, ...(context.panes ?? []).map((pane) => pane.id)]
    if (words[0] === "insert" && words.length === 2) return matchValues(head, current, groups)
    const container = (target: string | undefined) => target === "root" ? context.layoutContainers?.[0] : context.layoutContainers?.find((node) => node.id === target)
    if (words[0] === "insert" && words.length === 3) {
      const target = container(words[1])
      return target ? matchValues(head, current, Array.from({ length: target.children.length + 1 }, (_, i) => String(i))) : []
    }
    if (words[0] === "move" && words.length === 4) {
      const target = container(words[2])
      return target ? matchValues(head, current, Array.from({ length: target.children.length + (target.children.includes(words[1] ?? "") ? 0 : 1) }, (_, i) => String(i))) : []
    }
    if (words[0] === "insert" && words.length === 4) return matchValues(head, current, paneKinds.filter((kind) => kind !== "conversation" && kind !== "composer" && kind !== "extension"))
    if (words[0] === "bubble" && words.length === 2) return matchValues(head, current, targets)
    if (words[0] === "bubble" && words.length === 3) return matchValues(head, current, ["previous", "next"])
    if (words[0] === "move" && words.length === 2) return matchValues(head, current, targets)
    if (words[0] === "move" && words.length === 3) return matchValues(head, current, groups)
    if (words[0] === "wrap" && words.length === 2) return matchValues(head, current, targets)
    if (words[0] === "wrap" && words.length === 3) return matchValues(head, current, ["row", "column"])
    if (words[0] === "wrap" && words.length === 4) return matchValues(head, current, paneKinds.filter((kind) => kind !== "conversation" && kind !== "composer" && kind !== "extension"))
    if (words[0] === "wrap" && words.length === 5) return matchValues(head, current, ["before", "after"])
    if (words[0] === "assign" && words.length === 2) return matchValues(head, current, [...paneKinds])
    if ((words[0] === "split" && words.length === 4 && words[2] === "extension") || (words[0] === "assign" && words.length === 3 && words[1] === "extension")) return matchValues(head, current, extensionManager.panels().map((panel) => panel.key))
    if (words[0] === "focus" && words.length === 2) return matchValues(head, current, ["left", "right", "up", "down", "next", "previous"])
    return []
  },
  run: ({ store, actions }, { args }) => {
    const current = store.state.paneLayout
    const command = args[0] ?? "show"
    // The layout tree belongs to the workspace structure (ADR-0036): elsewhere only show and focus work.
    if (!store.state.structure.sidebar.enabled && command !== "show" && command !== "focus") throw new Error(layoutNeedsWorkspace)
    let next: PaneLayout = current
    let reloadedPath: string | undefined
    switch (command) {
      case "tree": {
        if (args.length !== 1) throw new Error("Usage: /layout tree")
        next = openLayoutPane(current)
        break
      }
      case "show": break
      case "insert": {
        const [_, container, index, kind] = args
        if (args.length !== 4 || !container || !index || !/^\d+$/.test(index) || !paneKinds.includes(kind as PaneKind)) throw new Error("Usage: /layout insert <container-id|root> <index> <job>")
        next = insertPane(current, container, Number(index), kind as PaneKind)
        break
      }
      case "bubble": {
        const [_, target, direction] = args
        if (args.length !== 3 || !target || (direction !== "previous" && direction !== "next")) throw new Error("Usage: /layout bubble <node-id|pane-name|root> <previous|next>")
        next = bubblePane(current, target, direction)
        break
      }
      case "move": {
        const [_, target, container, index] = args
        if (args.length !== 4 || !target || !container || !index || !/^\d+$/.test(index)) throw new Error("Usage: /layout move <node-id|pane-name> <container-id|root> <index>")
        next = movePane(current, target, container, Number(index))
        break
      }
      case "wrap": {
        const [_, target, direction, kind, position = "before"] = args
        if (args.length < 4 || args.length > 5 || !target || (direction !== "row" && direction !== "column") || !paneKinds.includes(kind as PaneKind) || (position !== "before" && position !== "after")) throw new Error("Usage: /layout wrap <node-id|pane-name|root> <row|column> <job> [before|after]")
        next = wrapPane(current, target, direction as LayoutDirection, kind as PaneKind, position === "before")
        break
      }
      case "remove": {
        if (args.length !== 2 || !args[1]) throw new Error("Usage: /layout remove <node-id|pane-name>")
        next = closePane(current, args[1])
        break
      }
      case "split": {
        const direction = args[1]
        const kind = args[2] ?? "jobs"
        if ((direction !== "up" && direction !== "left") || !paneKinds.includes(kind as PaneKind) || args.length > (kind === "extension" ? 4 : 3)) throw new Error("Usage: /layout split <up|left> [job] (extension <bundle>#<panel>)")
        next = splitPane(current, direction === "up" ? "horizontal" : "vertical", kind as PaneKind, true, args[3])
        break
      }
      case "assign": {
        const kind = args[1]
        if (!kind || !paneKinds.includes(kind as PaneKind)) throw new Error(`Usage: /layout assign <${paneKinds.join("|")}> (extension <bundle>#<panel>)`)
        next = setPaneKind(current, kind as PaneKind, args[2])
        break
      }
      case "focus": {
        const target = args[1]
        if (!target) throw new Error("Usage: /layout focus <left|right|up|down|next|previous|pane-id>")
        if (["left", "right", "up", "down"].includes(target)) next = { ...current, active: movePaneFocus(visiblePaneLayout(current, store.state.columns, store.state.sidebar, store.state.projectsSidebar), target as PaneDirection, actions.paneBounds?.()).active }
        else if (target === "next" || target === "previous") next = { ...current, active: rotatePaneFocus(visiblePaneLayout(current, store.state.columns, store.state.sidebar, store.state.projectsSidebar), target === "next" ? 1 : -1, actions.paneBounds?.()).active }
        else if (paneLeaves(visiblePaneLayout(current, store.state.columns, store.state.sidebar, store.state.projectsSidebar).root).some((pane) => pane.id === target && isSelectablePane(pane))) next = { ...current, active: target }
        else throw new Error(`Pane ${target} is unselectable, hidden, or unknown`)
        break
      }
      case "resize": {
        const percent = args[1]
        if (!percent || !/^[+-]\d{1,2}$/.test(percent) || Number(percent) === 0) throw new Error("Usage: /layout resize <+10|-10> (percentage points)")
        next = resizePane(current, Number(percent) / 100)
        break
      }
      case "close": {
        if (args.length > 2) throw new Error("Usage: /layout close [pane-name|node-id]")
        next = closePane(current, args[1])
        break
      }
      case "reload": {
        if (args.length !== 1) throw new Error("Usage: /layout reload")
        const loaded = actions.loadPaneLayout()
        next = loaded.layout
        reloadedPath = loaded.path
        break
      }
      case "reset": next = defaultPaneLayout(); break
      default: throw new Error("Usage: /layout [split|insert|move|bubble|wrap|remove|assign|focus|resize|close|reload|reset|tree|show]")
    }
    store.setView("chat")
    const active = paneLeaves(next.root).find((pane) => pane.id === next.active)
    if (next !== current) {
      store.setPaneLayout(next)
      try { if (!reloadedPath) actions.savePreferences({ paneLayout: next }) }
      catch (error) {
        store.setStatus(`Layout changed, not saved: ${error instanceof Error ? error.message : String(error)}`)
        return
      }
    }
    store.setStatus(`${reloadedPath ? `Layout reloaded from ${reloadedPath}` : "Layout"} · ${paneLeaves(next.root).length} pane${paneLeaves(next.root).length === 1 ? "" : "s"} · ${active?.id ?? "?"} ${active?.kind ?? ""} · split|assign|focus|resize|close|reload|reset`)
  },
}

export const projectCommands: CommandSpec[] = [
  {
    name: "/project",
    description: "Open the Project view: list, open/switch, create, edit roots, rename, delete, or start a temporary session",
    run: ({ actions }) => { actions.openProjectOverlay() },
  },
  {
    name: "/projects",
    description: "Alias for /project",
    run: ({ actions }) => { actions.openProjectOverlay() },
  },
  {
    name: "/projects-sidebar",
    description: "Show or hide the left Projects sidebar. Without an argument it toggles what is visible now",
    argumentHint: "[on|off]",
    run: ({ store, actions }, { args }) => {
      const visible = store.state.structure.sidebar.enabled && projectsSidebarVisible(store.state.projectsSidebar, store.state.columns)
      const shown = parseSwitch(args[0], visible, "Usage: /projects-sidebar [on|off]")
      // A single-column preset has no sidebars: `on` turns the sidebar component on for this session (`/set sidebar on`).
      if (shown && !store.state.structure.sidebar.enabled) enableSidebar(actions)
      store.setProjectsSidebar(shown ? "open" : "closed")
      store.setStatus(`Projects sidebar ${projectsSidebarVisible(store.state.projectsSidebar, store.state.columns) ? "shown" : "hidden"}`)
    },
  },
]
