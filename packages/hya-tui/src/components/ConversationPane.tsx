import { Composer } from "./Composer"
import { MainPanel } from "./MainPanel"
import { ModeConfirm } from "./ModeConfirm"
import { PendingBlock } from "./PendingBlock"
import { PromptDock } from "./PromptDock"
import { WorkingIndicator } from "./WorkingIndicator"
import { colors } from "../theme"

/** The complete interactive conversation surface moves as one split-tree leaf. */
export function ConversationPane(props: { width: number }) {
  return (
    <box width="100%" height="100%" flexGrow={1} flexBasis={0} flexDirection="column" backgroundColor={colors.bg}>
      <MainPanel />
      <WorkingIndicator />
      <PendingBlock width={props.width} />
      <PromptDock />
      <ModeConfirm />
      <Composer width={props.width} />
    </box>
  )
}
