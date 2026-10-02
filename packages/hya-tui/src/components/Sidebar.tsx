/**
 * Reusable content for the three right-side pane jobs. Each job owns one
 * rectangle with a heading in the split tree and scrolls within that rectangle.
 */
import { For, Show } from "solid-js"
import type { ScrollBoxRenderable } from "@opentui/core"
import { useApp } from "../app/context"
import type { TodoItem } from "../client"
import { contextFields, contextRows, type ContextTone } from "../state/contextFields"
import { sessionListEntries, shownServer, todoGlyphs, todoStatusText, truncate } from "../state/format"
import { PaneFrame } from "./PaneFrame"
import { paneDefinitions } from "../state/panes"
import { colors, toolColors } from "../theme"

/** Context value color: plain and strong values in the text color, the rest in their theme color. */
const boxColor = (tone: ContextTone): string =>
  tone === "accent" ? colors.accent : tone === "warning" ? colors.warning : tone === "error" ? colors.error : colors.fg

/** Color of a todo item's status glyph: pending muted, in progress accent, completed green, blocked muted. */
function todoColor(status: string): string {
  switch (status) {
    case "in_progress": return colors.accent
    case "completed": return toolColors.done
    default: return colors.muted
  }
}

/** Todos fit their own pane and scroll when the list exceeds its rectangle. */
function TodoList(props: { items: readonly TodoItem[]; width: number }) {
  return (
    <Show when={props.items.length > 0} fallback={<text width="100%" wrapMode="word" fg={colors.muted}>No todos yet</text>}>
      <box width="100%" flexDirection="column">
        <For each={props.items}>
          {(item) => {
            const status = () => todoStatusText(item.status)
            return (
              <text width="100%" height={1} wrapMode="none">
                <span style={{ fg: todoColor(status()) }}>{todoGlyphs[status()] ?? "·"}</span>
                <span style={{ fg: status() === "completed" ? colors.muted : colors.fg }}>{` ${truncate(item.content, Math.max(1, props.width - 2))}`}</span>
              </text>
            )
          }}
        </For>
      </box>
    </Show>
  )
}

/** One right-side section; only selectable kinds receive an enclosing pane frame. */
export function SidebarPane(props: { kind: "sessions" | "todos" | "context"; width: number; active: boolean; scrollRef?: (value: ScrollBoxRenderable) => void }) {
  const { store, server, controller } = useApp()
  const inner = () => Math.max(1, props.width - (paneDefinitions[props.kind].selectable ? 4 : 2))
  return (
    <box width="100%" height="100%" flexGrow={1} flexBasis={0} flexDirection="column" backgroundColor={colors.bg}>
      <Show when={props.kind === "sessions"}>
        <PaneFrame kind="sessions" title="Sessions" focused={props.active}>
          <scrollbox ref={props.scrollRef} width="100%" flexGrow={1}>
            <For each={sessionListEntries(store.state, inner())}>
              {(entry) => entry.separator
                ? <text width="100%" height={1} wrapMode="none" fg={colors.border}>{entry.text}</text>
                : <text width="100%" height={1} wrapMode="none" fg={colors.fg} onMouseDown={(event) => { if (event.button === 2) { if (entry.sessionId) controller.openSessionContext(entry.sessionId, { x: event.x, y: event.y }); return } if (event.button === 0 && entry.sessionId) void controller.openRootSession(entry.sessionId) }}>{entry.text}</text>}
            </For>
          </scrollbox>
        </PaneFrame>
      </Show>
      <Show when={props.kind === "todos"}>
        <PaneFrame kind="todos" title="Todos" focused={props.active}>
          <scrollbox ref={props.scrollRef} width="100%" flexGrow={1}><TodoList items={store.state.todos} width={inner()} /></scrollbox>
        </PaneFrame>
      </Show>
      <Show when={props.kind === "context"}>
        <PaneFrame kind="context" title="Context" focused={props.active}>
          <scrollbox ref={props.scrollRef} width="100%" flexGrow={1}>
            <For each={contextRows(contextFields(store.state, shownServer(store.state, server)), inner())}>
              {(row) => (
                <text width="100%" height={1} wrapMode="none">
                  <span style={{ fg: colors.muted }}>{row.label}</span>
                  <span style={{ fg: boxColor(row.tone) }}>{row.value}</span>
                </text>
              )}
            </For>
          </scrollbox>
        </PaneFrame>
      </Show>
    </box>
  )
}
