import { useApp } from "../app/context"
import { colors } from "../theme"

/** One muted line with the latest status or error. */
export function StatusLine() {
  const { store } = useApp()
  return <text height={1} fg={colors.muted}>{store.state.status}</text>
}
