import { useApp } from "../app/context"
import { statusLineText, workingLineText } from "../state/activity"
import { colors } from "../theme"

/**
 * One muted line with the latest status or error. While the working line
 * shows a running turn, the turn runner's progress texts (`Running · msg_…`)
 * are left out (state/activity.ts `statusLineText`).
 */
export function StatusLine() {
  const { store } = useApp()
  const text = () => statusLineText(store.state.status, workingLineText(store.state, Date.now()) !== undefined)
  return <text height={1} fg={colors.muted}>{text()}</text>
}
