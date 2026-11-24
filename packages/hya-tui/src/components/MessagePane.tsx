import { Show } from "solid-js"
import { useApp } from "../app/context"
import { statusLineShown } from "../state/statusline"
import { Composer } from "./Composer"
import { Dock } from "./Dock"
import { StatusLine } from "./StatusLine"

/**
 * Selectable editor and its interaction controls share the active session
 * projection: the dock (components/Dock.tsx), the input, and the status line
 * below it (ADR-0036; a band status line is drawn by the input itself).
 */
export function MessagePane(props: { width: number }) {
  const { store } = useApp()
  return <box width="100%" height="100%" flexDirection="column" justifyContent="flex-end">
    <Dock width={props.width} />
    <Composer width={props.width} />
    <Show when={statusLineShown(store.state, store.state.columns)}>
      <StatusLine width={props.width} />
    </Show>
  </box>
}
