import { CommandPane } from "./CommandPane"
import { Composer } from "./Composer"
import { Footer } from "./Footer"
import { MainPanel } from "./MainPanel"
import { ModeConfirm } from "./ModeConfirm"
import { PendingBlock } from "./PendingBlock"
import { PromptDock } from "./PromptDock"
import { StatusBar } from "./StatusBar"
import { StatusLine } from "./StatusLine"
import { WorkingIndicator } from "./WorkingIndicator"
import { colors } from "../theme"

/** The complete interactive conversation surface moves as one split-tree leaf. */
export function ConversationPane(props: { width: number }) {
  return (
    <box width="100%" height="100%" flexGrow={1} flexBasis={0} flexDirection="column" backgroundColor={colors.bg}>
      <StatusBar width={props.width} />
      <MainPanel />
      <WorkingIndicator />
      <PendingBlock width={props.width} />
      <PromptDock />
      <ModeConfirm />
      <StatusLine />
      <CommandPane />
      <Composer width={props.width} />
      <Footer />
    </box>
  )
}
