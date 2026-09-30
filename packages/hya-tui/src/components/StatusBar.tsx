/**
 * The top status line (docs/tui.md "Status line"): the Context fields
 * (state/contextFields.ts) as ` · `-separated segments on at most two rows,
 * least essential fields dropped first. Drawn only while no `context` pane is
 * on screen (below 110 columns, `/sidebar off`, or a layout without one), so
 * the Context box and this line never show at the same time.
 */
import { For, Show } from "solid-js"
import { useApp } from "../app/context"
import { contextFields, contextStatusShown, statusLines, type ContextTone } from "../state/contextFields"
import { shownServer } from "../state/format"
import { colors } from "../theme"

/** A function, not a table: the palette is reactive (theme.ts), so read it where it is used. */
const statusColor = (tone: ContextTone): string =>
  tone === "strong" ? colors.fg : tone === "accent" ? colors.accent : tone === "warning" ? colors.warning : tone === "error" ? colors.error : colors.muted

export function StatusBar(props: { width: number }) {
  const { store, server } = useApp()
  const lines = () => statusLines(contextFields(store.state, shownServer(store.state, server)), Math.max(1, props.width))
  return (
    <Show when={contextStatusShown(store.state)}>
      <For each={lines()}>
        {(line) => (
          <text height={1} wrapMode="none">
            <For each={line}>
              {(segment, index) => (
                <>
                  <Show when={index() > 0}><span style={{ fg: colors.muted }}>{" · "}</span></Show>
                  <span style={{ fg: statusColor(segment.tone) }}>{segment.text}</span>
                </>
              )}
            </For>
          </text>
        )}
      </For>
    </Show>
  )
}
