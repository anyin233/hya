/**
 * The toggleable right sidebar (Ctrl+B, `/sidebar`): the session list, the
 * live todo list (E23: seeded from `GetSessionTodo`, kept current by the
 * controller's refresh), and the open session's context. Titled, bordered
 * boxes in the panel color.
 */
import { For, Show } from "solid-js"
import { useApp } from "../app/context"
import type { TodoItem } from "../client"
import { contextText, sessionListText, todoGlyphs, todoStatusText, truncate } from "../state/format"
import { colors, toolColors } from "../theme"

function SideBox(props: { title: string; grow?: boolean; children: import("solid-js").JSX.Element }) {
  return (
    <box
      width="100%"
      flexGrow={props.grow ? 1 : 0}
      flexShrink={props.grow ? 1 : 0}
      flexBasis={props.grow ? 0 : undefined}
      border
      borderColor={colors.border}
      title={props.title}
      backgroundColor={colors.panel}
      flexDirection="column"
      paddingX={1}
    >
      {props.children}
    </box>
  )
}

/** Color of a todo item's status glyph: pending muted, in progress accent, completed green, blocked muted. */
function todoColor(status: string): string {
  switch (status) {
    case "in_progress": return colors.accent
    case "completed": return toolColors.done
    default: return colors.muted
  }
}

/** Most todo rows the sidebar box shows before a `+N more` row; keeps the `Context` box below it on screen. */
const sidebarTodoRows = 6

/**
 * Kept compact like the box it replaced (a fixed-size `text` placeholder):
 * a bounded number of rows, not a growing scrollbox, so a long list does not
 * push the `Context` box below the visible area.
 */
function TodoList(props: { items: readonly TodoItem[]; width: number }) {
  const shown = () => props.items.slice(0, sidebarTodoRows)
  const hidden = () => props.items.length - shown().length
  return (
    <Show when={props.items.length > 0} fallback={<text width="100%" wrapMode="word" fg={colors.muted}>No todos yet</text>}>
      <box width="100%" flexDirection="column">
        <For each={shown()}>
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
        <Show when={hidden() > 0}>
          <text width="100%" height={1} wrapMode="none" fg={colors.muted}>{`+${hidden()} more`}</text>
        </Show>
      </box>
    </Show>
  )
}

export function Sidebar(props: { width: number }) {
  const { store, server } = useApp()
  // Inner width: the border and one column of padding on each side.
  const inner = () => Math.max(1, props.width - 4)
  return (
    <box width={props.width} height="100%" flexShrink={0} flexDirection="column" backgroundColor={colors.bg}>
      <SideBox title="Sessions" grow>
        <scrollbox width="100%" flexGrow={1}>
          <text width="100%" wrapMode="word" fg={colors.fg}>{sessionListText(store.state, inner())}</text>
        </scrollbox>
      </SideBox>
      <SideBox title="Todos">
        <TodoList items={store.state.todos} width={inner()} />
      </SideBox>
      <SideBox title="Context">
        <text width="100%" wrapMode="none" fg={colors.fg}>{contextText(store.state, server, inner())}</text>
      </SideBox>
    </box>
  )
}
