/**
 * The toggleable right sidebar (Ctrl+B, `/sidebar`): the session list, a todo
 * area (filled in by the todo panel work; a placeholder until then), and the
 * open session's context. Titled, bordered boxes in the panel color.
 */
import { useApp } from "../app/context"
import { contextText, sessionListText } from "../state/format"
import { colors } from "../theme"

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
        <text width="100%" wrapMode="word" fg={colors.muted}>No todos yet</text>
      </SideBox>
      <SideBox title="Context">
        <text width="100%" wrapMode="none" fg={colors.fg}>{contextText(store.state, server, inner())}</text>
      </SideBox>
    </box>
  )
}
