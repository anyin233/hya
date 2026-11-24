/**
 * The status line below the input (ADR-0036, state/statusline.ts): model,
 * permission mode, context occupancy, and the configured extra segments,
 * fitted to the width; the session title right-aligned when it fits. One
 * borderless row: it is information, never a focus target.
 */
import { For, Show } from "solid-js"
import { useApp } from "../app/context"
import { separator, statusLine, type SegmentTone } from "../state/statusline"
import { colors } from "../theme"

const toneColor = (tone: SegmentTone): string => tone === "fg" ? colors.fg : tone === "accent" ? colors.focus : tone === "warning" ? colors.warning : tone === "error" ? colors.error : colors.muted

export function StatusLine(props: { width: number }) {
  const { store } = useApp()
  const inner = () => Math.max(1, Math.floor(props.width) - 2)
  const line = () => statusLine(store.state, store.state.structure.statusline.segments, inner())
  const gap = () => {
    const fitted = line()
    if (!fitted.right) return 0
    const left = fitted.left.reduce((sum, part, index) => sum + Bun.stringWidth(part.text) + (index ? separator.length : 0), 0)
    return Math.max(2, inner() - left - Bun.stringWidth(fitted.right.text))
  }
  return (
    <box width="100%" height={1} flexShrink={0} paddingX={1}>
      <text height={1} wrapMode="none">
        <For each={line().left}>
          {(part, index) => (
            <>
              <Show when={index() > 0}><span style={{ fg: colors.border }}>{separator}</span></Show>
              <span style={{ fg: toneColor(part.tone) }}>{part.text}</span>
            </>
          )}
        </For>
        <Show when={line().right}>
          {(right) => <span style={{ fg: toneColor(right().tone) }}>{" ".repeat(gap()) + right().text}</span>}
        </Show>
      </text>
    </box>
  )
}
