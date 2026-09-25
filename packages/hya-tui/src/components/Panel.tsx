import type { JSX } from "solid-js"
import { colors } from "../theme"

/** A bordered, titled panel with a word-wrapped scrolling text body (the non-chat views). */
export function Panel(props: { title: string; text: string; background?: string }): JSX.Element {
  return (
    <box
      width="100%"
      flexGrow={1}
      flexBasis={0}
      border
      borderColor={colors.border}
      title={props.title}
      backgroundColor={props.background ?? colors.panel}
      flexDirection="column"
    >
      <scrollbox width="100%" flexGrow={1}>
        <text width="100%" wrapMode="word" fg={colors.fg}>{props.text}</text>
      </scrollbox>
    </box>
  )
}
