import { Match, Switch } from "solid-js"
import { useApp } from "../app/context"
import { mainContent, mainTitle } from "../state/format"
import { paneLeaves } from "../state/panes"
import { colors } from "../theme"
import { Panel } from "./Panel"
import { PaneWorkspace } from "./PaneWorkspace"
import { Transcript } from "./Transcript"

/** The main area: the chat transcript, or a titled panel for the other views (models, help, …). */
export function MainPanel() {
  const { store } = useApp()
  return (
    <Switch>
      <Match when={store.state.view === "chat" && paneLeaves(store.state.paneLayout.root).length > 1}>
        <PaneWorkspace />
      </Match>
      <Match when={store.state.view === "chat"}>
        <Transcript />
      </Match>
      <Match when={store.state.view !== "chat"}>
        <Panel title={mainTitle(store.state.view)} text={mainContent(store.state)} background={colors.bg} />
      </Match>
    </Switch>
  )
}
