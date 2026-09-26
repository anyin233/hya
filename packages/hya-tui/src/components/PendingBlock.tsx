/**
 * Pending permission requests (!) and questions (?) of other session trees
 * (the open session's own asks and its subagents' are the prompt,
 * components/PromptDock.tsx), as a compact titled box above the status line
 * while any are waiting. Each line names the session it belongs to (its
 * `/open` number and title, state/format.ts `pendingLines`); asks of any
 * session arrive live over the global stream (app/controller.ts). Shows up
 * to three; the rest are counted. `/open <n>` goes to the session to answer
 * there with its prompt; `/approve`, `/deny`, or `/answer` answer from here;
 * `/interactions` lists every detail.
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
          {`${more() > 0 ? `+${more()} more · ` : ""}/open <n> answers there · /approve <id> · /deny <id> · /answer <id> <text> · /interactions`}
        </text>
      </box>
    </Show>
  )
}
