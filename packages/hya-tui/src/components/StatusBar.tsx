/**
 * E22 status bar (docs/tui.md "Status bar"): one muted line below the
 * header showing the permission mode (colored per mode: manual plain, `⚠
 * yolo` in the error color, a bundle mode's title in the accent color —
 * state/modes.ts `modeDisplay`), the workspace directory (shortened), the git
 * branch (`GetVcsStatus`, refreshed on session open and after turns), a
 * compact todo count while the sidebar is hidden, and the connection state.
 * Agent, model, session, and server already appear on the header line
 * (components/Header.tsx); this line does not repeat them, so both stay
 * within 80 columns. Context-usage percent and session token totals are
 * part of the design (E22) but are not on the v1 wire yet (no model context
 * limit, no message/turn usage field) and are always hidden here; see
 * docs/tui.md "Status bar" for the tracked gap.
 */
import { Show } from "solid-js"
import { useApp } from "../app/context"
import { sidebarVisible } from "../state/layout"
import { statusBarText, todosCompactText } from "../state/format"
import { effectiveMode, modeDisplay, type ModeTone } from "../state/modes"
import { colors } from "../theme"

const toneColors: Record<ModeTone, string> = { normal: colors.fg, error: colors.error, accent: colors.accent }

export function StatusBar() {
  const { store } = useApp()
  const mode = () => modeDisplay(effectiveMode(store.state), store.state.permissionModes)
  const text = () => {
    const state = store.state
    const shown = sidebarVisible(state.sidebar, state.columns)
    return statusBarText({
      mode: mode().text,
      directory: state.selected?.workdir ?? "",
      branch: state.gitBranch,
      todos: shown ? undefined : todosCompactText(state.todos),
      connected: state.connected,
    }, state.columns)
  }
  /** `mode <label>` then the rest; the label is drawn in the mode's color when it fits whole. */
  const parts = () => {
    const full = text()
    const head = `mode ${mode().text}`
    return full.startsWith(head) ? { label: mode().text, rest: full.slice(head.length) } : undefined
  }
  return (
    <Show when={parts()} fallback={<text height={1} wrapMode="none" fg={colors.muted}>{text()}</text>}>
      {(split) => (
        <text height={1} wrapMode="none">
          <span style={{ fg: colors.muted }}>mode </span>
          <span style={{ fg: toneColors[mode().tone] }}>{split().label}</span>
          <span style={{ fg: colors.muted }}>{split().rest}</span>
        </text>
      )}
    </Show>
  )
}
