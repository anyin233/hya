import type { AppState } from "./store"
import { paneLeaves } from "./panes"

/** The workspace keyboard owner: the focused Projects pane (Ctrl+P), else the active pane. */
export function focusedPane(state: AppState) {
  const leaves = paneLeaves(state.paneLayout.root)
  return state.projectsFocus
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
  if (state.bundlesView) return "bundles"
  if (state.rulesView) return "rules"
  if (state.agentsView) return "agents"
  if (state.extensionOverlay) return state.extensionOverlay
  if (commandsOpen) return "commands"
  return focusedPane(state)?.id
}
