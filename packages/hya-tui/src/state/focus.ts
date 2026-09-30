import type { AppState } from "./store"
import { paneLeaves } from "./panes"

/** The workspace keyboard owner. Projects retains its existing explicit focus flag. */
export function focusedPane(state: AppState) {
  const leaves = paneLeaves(state.paneLayout.root)
  return state.projectsSidebarFocus
    ? leaves.find((pane) => pane.kind === "projects")
    : leaves.find((pane) => pane.id === state.paneLayout.active)
}

/** One border owner, following the same priority as keyboard dispatch. */
export function keyboardOwner(state: AppState, commandsOpen: boolean): string | undefined {
  if (state.secretEntry) return "secret"
  if (state.picker) return "picker"
  if (state.providerView) return state.providerView.form ? "providerForm" : "providers"
  if (state.diffView) return "diff"
  if (state.mcpView) return "mcp"
  if (state.rulesView) return "rules"
  if (state.agentsView) return "agents"
  if (state.projectView) return "project"
  if (commandsOpen) return "commands"
  return focusedPane(state)?.id
}
