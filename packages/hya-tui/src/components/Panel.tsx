import type { JSX } from "solid-js"
import { colors } from "../theme"
import { PaneFrame } from "./PaneFrame"

/** Non-chat output occupies the passive conversation viewer without a focus border. */
export function Panel(props: { title: string; text: string; background?: string }): JSX.Element {
  return <PaneFrame kind="conversation" title={props.title} background={props.background ?? colors.bg}>
    <scrollbox width="100%" flexGrow={1}>
      <text width="100%" wrapMode="word" fg={colors.fg}>{props.text}</text>
    </scrollbox>
  </PaneFrame>
}
