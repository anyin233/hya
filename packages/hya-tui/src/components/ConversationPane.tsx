import { Show } from "solid-js"
import { Composer } from "./Composer"
import { MainPanel } from "./MainPanel"
import { ModeConfirm } from "./ModeConfirm"
import { PendingBlock } from "./PendingBlock"
import { PromptDock } from "./PromptDock"
import { WorkingIndicator } from "./WorkingIndicator"
import { colors } from "../theme"
import { contextFields, contextStatus } from "../state/contextFields"
import { shownServer } from "../state/format"
import { sidebarVisible } from "../state/layout"
import { useApp } from "../app/context"

/** The complete interactive conversation surface moves as one split-tree leaf. */
export function ConversationPane(props: { width: number }) {
  const { store, server } = useApp()
  return (
    <box width="100%" height="100%" flexGrow={1} flexBasis={0} flexDirection="column" backgroundColor={colors.bg}>
      <Show when={!sidebarVisible(store.state.sidebar, store.state.columns)}>
        <text width="100%" height={1} wrapMode="none" fg={colors.muted}>{contextStatus(contextFields(store.state, shownServer(store.state, server)), props.width)}</text>
      </Show>
      <MainPanel />
      <WorkingIndicator />
      <PendingBlock width={props.width} />
      <PromptDock />
      <ModeConfirm />
      <Composer width={props.width} />
    </box>
  )
}
