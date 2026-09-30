import type { AppState } from "./store"
import { paneLeaves } from "./panes"

/** The workspace keyboard owner. Projects retains its existing explicit focus flag. */
export function focusedPane(state: AppState) {
  const leaves = paneLeaves(state.paneLayout.root)
  return state.projectsSidebarFocus
    ? leaves.find((pane) => pane.kind === "projects")
    : leaves.find((pane) => pane.id === state.paneLayout.active)
}
