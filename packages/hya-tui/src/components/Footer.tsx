import { useApp } from "../app/context"
import { footerInstruction } from "../instructions"
import { colors } from "../theme"

/** Bottom line: what to do next in the current view. */
export function Footer() {
  const { store } = useApp()
  const text = () => footerInstruction(store.state.view, store.state.savedKeysAvailable, store.state.secretProvider !== undefined, Boolean(store.state.selected?.parent))
  return <text width="100%" height={1} fg={colors.muted}>{text()}</text>
}
