import { Show } from "solid-js"
import { Composer } from "./Composer"
import { MainPanel } from "./MainPanel"
import { ModeConfirm } from "./ModeConfirm"
import { PendingBlock } from "./PendingBlock"
import { PromptDock } from "./PromptDock"
import { WorkingIndicator } from "./WorkingIndicator"
import { colors } from "../theme"
import { sidebarVisible } from "../state/layout"
import { useApp } from "../app/context"
import { ExtensionLine } from "../extensions/Host"
import { extensionManager } from "../extensions/manager"
/** The complete interactive conversation surface moves as one split-tree leaf. */
export function ConversationPane(props: { width: number }) {
  const { store } = useApp()
  return (
    <box width="100%" height="100%" flexGrow={1} flexBasis={0} flexDirection="column" backgroundColor={colors.bg}>
      {/* The Context summary while the sidebar is hidden: the `context_line` extension panel. */}
      <Show when={store.state.ready && !sidebarVisible(store.state.sidebar, store.state.columns)}>
        <Show when={extensionManager.replacement("context_line")} keyed fallback={<text width="100%" height={1} wrapMode="none" fg={colors.muted}>{extensionManager.placeholder("context_line") ?? ""}</text>}>
          {(panel) => <ExtensionLine panelKey={panel.key} width={props.width} />}
        </Show>
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
