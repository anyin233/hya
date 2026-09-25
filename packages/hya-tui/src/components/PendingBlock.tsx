/**
 * Pending permission requests (!) and questions (?) of other session trees
 * (the open session's own asks and its subagents' are the prompt,
 * components/PromptDock.tsx), as a compact titled box above the status line
 * while any are waiting. Shows up to three; the rest are counted. Answer
 * with `/approve`, `/deny`, or `/answer`, or open the session; `/interactions`
 * lists every detail.
 */
import { useTerminalDimensions } from "@opentui/solid"
import { For, Show } from "solid-js"
import { useApp } from "../app/context"
import { pendingLines } from "../state/format"
import { colors } from "../theme"

const shown = 3

export function PendingBlock(props: { width?: number }) {
  const { store } = useApp()
  const size = useTerminalDimensions()
  const inner = () => Math.max(10, (props.width ?? size().width) - 4)
  const lines = () => pendingLines(store.state, inner())
  const more = () => lines().length - shown
  return (
    <Show when={lines().length > 0}>
      <box
        width="100%"
        flexShrink={0}
        border
        borderColor={colors.border}
        title={`Pending (${lines().length})`}
        backgroundColor={colors.panel}
        flexDirection="column"
        paddingX={1}
      >
        <For each={lines().slice(0, shown)}>
          {(line) => <text height={1} wrapMode="none" fg={colors.fg}>{line}</text>}
        </For>
        <text height={1} wrapMode="none" fg={colors.muted}>
          {`${more() > 0 ? `+${more()} more · ` : ""}/approve <id> · /deny <id> · /answer <id> <text> · /interactions`}
        </text>
      </box>
    </Show>
  )
}
