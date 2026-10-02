import { Composer } from "./Composer"
import { ModeConfirm } from "./ModeConfirm"
import { PendingBlock } from "./PendingBlock"
import { PromptDock } from "./PromptDock"

/** Selectable editor and its interaction controls share the active session projection. */
export function MessagePane(props: { width: number }) {
  return <box width="100%" height="100%" flexDirection="column" justifyContent="flex-end">
    <PendingBlock width={props.width} />
    <PromptDock />
    <ModeConfirm />
    <Composer width={props.width} />
  </box>
}
