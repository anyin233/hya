/**
 * E22 status bar (docs/tui.md "Status bar"): one muted line below the
 * header showing the permission mode (placeholder text only; S12 adds
 * switching and colors), the workspace directory (shortened), the git
 * branch (`GetVcsStatus`, refreshed on session open and after turns), a
 * compact todo count while the sidebar is hidden, and the connection state.
 * Agent, model, session, and server already appear on the header line
 * (components/Header.tsx); this line does not repeat them, so both stay
 * within 80 columns. Context-usage percent and session token totals are
 * part of the design (E22) but are not on the v1 wire yet (no model context
 * limit, no message/turn usage field) and are always hidden here; see
 * docs/tui.md "Status bar" for the tracked gap.
 */
import { useApp } from "../app/context"
import { sidebarVisible } from "../state/layout"
import { statusBarText, todosCompactText } from "../state/format"
import { colors } from "../theme"

export function StatusBar() {
  const { store } = useApp()
  const text = () => {
    const state = store.state
    const shown = sidebarVisible(state.sidebar, state.columns)
    return statusBarText({
      mode: state.selected?.permissionMode || "manual",
      directory: state.selected?.workdir ?? "",
      branch: state.gitBranch,
      todos: shown ? undefined : todosCompactText(state.todos),
      connected: state.connected,
    }, state.columns)
  }
  return <text height={1} wrapMode="none" fg={colors.muted}>{text()}</text>
}
