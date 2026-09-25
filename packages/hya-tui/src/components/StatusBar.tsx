/**
 * E22 status bar (docs/tui.md "Status bar"): one muted line below the
 * header: the permission mode (colored per mode: manual plain, `⚠ yolo` in
 * the error color, a bundle mode's title in the accent color —
 * state/modes.ts `modeDisplay`), the context occupancy `ctx N%` (warning
 * color from 80 %, error color from 95 %; state/format.ts `contextUsage`),
 * the session token total (`12.3k tok`, `SessionInfo.usage`), the workspace
 * directory (shortened), the git branch (`GetVcsStatus`, refreshed on
 * session open and after turns), a compact todo count while the sidebar is
 * hidden, and the connection state. Segments with no data are hidden.
 * Agent, model, session, and server already appear on the header line
 * (components/Header.tsx); this line does not repeat them, so both stay
 * within 80 columns.
 */
import { For } from "solid-js"
import { useApp } from "../app/context"
import { sidebarVisible } from "../state/layout"
import { contextUsage, formatTokens, sessionTokens, statusBarSegments, todosCompactText, truncate, type StatusTone } from "../state/format"
import { effectiveMode, modeDisplay, type ModeTone } from "../state/modes"
import { colors } from "../theme"

const modeColors: Record<ModeTone, string> = { normal: colors.fg, error: colors.error, accent: colors.accent }

export function StatusBar() {
  const { store } = useApp()
  const mode = () => modeDisplay(effectiveMode(store.state), store.state.permissionModes)
  const segments = () => {
    const state = store.state
    const shown = sidebarVisible(state.sidebar, state.columns)
    const tokens = sessionTokens(state.selected?.usage)
    return statusBarSegments({
      mode: mode().text,
      context: contextUsage(state)?.percent,
      tokens: tokens === undefined ? undefined : `${formatTokens(tokens)} tok`,
      directory: state.selected?.workdir ?? "",
      branch: state.gitBranch,
      todos: shown ? undefined : todosCompactText(state.todos),
      connected: state.connected,
      ...(state.web ? { web: state.web } : {}),
    }, state.columns)
  }
  const color = (tone: StatusTone): string => tone === "warning" ? colors.warning : tone === "error" ? colors.error : colors.muted
  /** The segments as spans, clipped to the width: `mode <label>` draws the label in the mode's color. */
  const spans = () => {
    let left = store.state.columns
    const out: { text: string; fg: string }[] = []
    segments().forEach((segment, index) => {
      const pieces = segment.tone === "mode"
        ? [{ text: "mode ", fg: colors.muted }, { text: segment.text.slice(5), fg: modeColors[mode().tone] }]
        : [{ text: segment.text, fg: color(segment.tone) }]
      if (index > 0) pieces.unshift({ text: " · ", fg: colors.muted })
      for (const piece of pieces) {
        if (left <= 0) return
        const text = truncate(piece.text, left)
        left -= text.length
        out.push({ text, fg: piece.fg })
      }
    })
    return out
  }
  return (
    <text height={1} wrapMode="none">
      <For each={spans()}>{(span) => <span style={{ fg: span.fg }}>{span.text}</span>}</For>
    </text>
  )
}
