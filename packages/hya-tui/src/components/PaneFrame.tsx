import { Show, type JSX } from "solid-js"
import { paneDefinitions, type PaneKind } from "../state/panes"
import { colors } from "../theme"

/** A pane border always denotes selectable content; passive titles are plain text. */
export function PaneFrame(props: { kind: PaneKind; title: string; focused?: boolean; background?: string; children: JSX.Element }) {
  const selectable = () => paneDefinitions[props.kind].selectable
  return <box width="100%" height="100%" flexGrow={1} flexShrink={1} flexBasis={0}
    // An empty edge list avoids OpenTUI auto-enabling borders when borderColor is set.
    border={selectable() ? true : []} borderColor={props.focused && selectable() ? colors.accent : colors.border}
    title={selectable() ? props.title : undefined}
    backgroundColor={props.background ?? (selectable() ? colors.panel : colors.bg)}
    flexDirection="column" paddingX={1}>
    <Show when={!selectable()}><text height={1} flexShrink={0} wrapMode="none" fg={colors.muted}>{props.title}</text></Show>
    {props.children}
  </box>
}
