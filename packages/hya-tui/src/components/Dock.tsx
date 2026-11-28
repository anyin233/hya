/**
 * The dock between the transcript and the input (ADR-0036): pending asks of
 * other sessions, busy child sessions, open todos, the permission/question
 * prompt, and the yolo confirmation, in that order. Each group takes rows
 * only while it has something to say; the input stays anchored at the bottom
 * of its pane, so a group appearing or going away moves the transcript's
 * bottom edge, never the input.
 */
import { For, Show } from "solid-js"
import { useApp } from "../app/context"
import { childDockRows, todoDockRows, type DockRow } from "../state/dock"
import { colors } from "../theme"
import { ModeConfirm } from "./ModeConfirm"
import { PendingBlock } from "./PendingBlock"
import { PromptDock } from "./PromptDock"

const toneColor = (tone: DockRow["tone"]): string => tone === "activity" ? colors.activity : tone === "warning" ? colors.warning : tone === "success" ? colors.success : colors.muted

function DockRows(props: { rows: DockRow[] }) {
  return (
    <For each={props.rows}>
      {(row) => (
        <text height={1} wrapMode="none">
          <span style={{ fg: toneColor(row.tone) }}>{` ${row.glyph}`}</span>
          <span style={{ fg: row.tone === "muted" ? colors.muted : colors.fg }}>{` ${row.text}`}</span>
        </text>
      )}
    </For>
  )
}

export function Dock(props: { width: number }) {
  const { store } = useApp()
  const children = () => childDockRows(store.state)
  const todos = () => todoDockRows(store.state, store.state.columns)
  return (
    <>
      <PendingBlock width={props.width} />
      <Show when={children().length > 0}><DockRows rows={children()} /></Show>
      <Show when={todos().length > 0}><DockRows rows={todos()} /></Show>
      <PromptDock />
      <ModeConfirm />
    </>
  )
}
