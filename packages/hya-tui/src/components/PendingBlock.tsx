/**
 * Pending permission requests (!) and questions (?) of other session trees
 * (the open session's own asks and its subagents' are the prompt,
 * components/PromptDock.tsx), as a compact borderless summary above the status line
 * while any are waiting. Each line names the session it belongs to (its
 * `/open` number and title, state/format.ts `pendingLines`); asks of any
 * session arrive live over the global stream (app/controller.ts). Shows up
 * to three; the rest are counted. /pending opens the oldest request's root session
 * and shows its normal numbered choices. `/interactions` lists every detail.
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
  const inner = () => Math.max(10, (props.width ?? size().width) - 2)
  const lines = () => pendingLines(store.state, inner())
  const more = () => lines().length - shown
  return (
    <Show when={lines().length > 0}>
      <box
        width="100%"
        flexShrink={0}
        backgroundColor={colors.panel}
        flexDirection="column"
        paddingX={1}
      >
        <text height={1} wrapMode="none" fg={colors.warning}>{`Pending (${lines().length})`}</text>
        <For each={lines().slice(0, shown)}>
          {(line) => <text height={1} wrapMode="none" fg={colors.fg}>{line}</text>}
        </For>
        <text height={1} wrapMode="none" fg={colors.muted}>
          {`${more() > 0 ? `+${more()} more · ` : ""}/pending review request · /sessions past chats · /interactions details`}
        </text>
      </box>
    </Show>
  )
}
