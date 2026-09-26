import { useApp } from "../app/context"
import { headerText, shownServer } from "../state/format"
import { colors } from "../theme"

/** Top line: selected session, agent, model, and server URL. */
export function Header() {
  const { store, server } = useApp()
  return <text height={1} fg={colors.accent}>{headerText(store.state, shownServer(store.state, server))}</text>
}
