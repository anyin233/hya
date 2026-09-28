import { useApp } from "../app/context"
import { footerInstruction } from "../instructions"
import { colors } from "../theme"

/** Bottom line: what to do next in the current view. */
export function Footer() {
  const { store, ui } = useApp()
  const text = () => ui.command?.active()
    ? "Command pane · type a name or use Up/Down · Tab completes · Enter runs · Esc returns"
    : footerInstruction(store.state.view, Boolean(store.state.selected?.parent))
  return <text width="100%" height={1} fg={colors.muted}>{text()}</text>
}
