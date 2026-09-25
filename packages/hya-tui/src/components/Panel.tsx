import type { JSX } from "solid-js"
import { colors } from "../theme"

/** A bordered, titled panel with one word-wrapped scrolling text body. */
export function Panel(props: {
  title: string
  text: string
  width?: number
  background?: string
  visible?: boolean
  /** Keep the view pinned to the newest line (the transcript). */
  sticky?: boolean
  scrollRef?: (scroll: import("@opentui/core").ScrollBoxRenderable) => void
}): JSX.Element {
  const grows = () => props.width === undefined
  return (
    <box
      width={props.width}
      flexGrow={grows() ? 1 : undefined}
      flexBasis={grows() ? 0 : undefined}
      flexShrink={grows() ? undefined : 1}
      visible={props.visible ?? true}
      border
      borderColor={colors.border}
      title={props.title}
      backgroundColor={props.background ?? colors.panel}
      flexDirection="column"
    >
      <scrollbox
        ref={props.scrollRef}
        width="100%"
        flexGrow={1}
        stickyScroll={props.sticky}
        stickyStart={props.sticky ? "bottom" : undefined}
      >
        <text width="100%" wrapMode="word" fg={colors.fg}>{props.text}</text>
      </scrollbox>
    </box>
  )
}
