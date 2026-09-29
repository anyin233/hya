import { useApp } from "../app/context"
import { footerInstruction } from "../instructions"
import { colors } from "../theme"
import { isDefaultPaneTree } from "../state/panes"

/** Bottom line: what to do next in the current view. */
export function Footer() {
  const { store, ui } = useApp()
  const text = () => ui.command?.active()
    ? "Command pane · type or use Up/Down · Tab chooses · Enter chooses/runs · Esc returns"
    : store.state.view === "chat" && (store.state.paneLayout.active !== "pane-1" || !isDefaultPaneTree(store.state.paneLayout))
      ? "Alt+arrows select pane · /layout split|assign|resize|close|reset · / commands"
    : footerInstruction(store.state.view, Boolean(store.state.selected?.parent))
  return <text width="100%" height={1} fg={colors.muted}>{text()}</text>
}
