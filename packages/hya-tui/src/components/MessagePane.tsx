import { Show } from "solid-js"
import { useApp } from "../app/context"
import { statusLineShown } from "../state/statusline"
import { Composer } from "./Composer"
import { ModeConfirm } from "./ModeConfirm"
import { PendingBlock } from "./PendingBlock"
import { PromptDock } from "./PromptDock"
import { StatusLine } from "./StatusLine"

/** Selectable editor and its interaction controls share the active session projection; the status line (ADR-0036) sits below the input. */
export function MessagePane(props: { width: number }) {
  const { store } = useApp()
  return <box width="100%" height="100%" flexDirection="column" justifyContent="flex-end">
    <PendingBlock width={props.width} />
    <PromptDock />
    <ModeConfirm />
    <Composer width={props.width} />
    <Show when={statusLineShown(store.state, store.state.columns)}>
      <StatusLine width={props.width} />
    </Show>
  </box>
}
