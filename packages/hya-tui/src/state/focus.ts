import type { AppState } from "./store"
import { isSelectablePane, paneLeaves } from "./panes"

/** Only the layout's active selectable leaf can own workspace input. */
export function focusedPane(state: AppState) {
  return paneLeaves(state.paneLayout.root).find((pane) => pane.id === state.paneLayout.active && isSelectablePane(pane))
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
