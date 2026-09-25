import { Match, Switch } from "solid-js"
import { useApp } from "../app/context"
import { mainContent, mainTitle } from "../state/format"
import { colors } from "../theme"
import { Panel } from "./Panel"
import { Transcript } from "./Transcript"

/** The main area: the chat transcript, or a titled panel for the other views (models, help, …). */
export function MainPanel() {
  const { store } = useApp()
  return (
    <Switch>
      <Match when={store.state.view === "chat"}>
        <Transcript />
      </Match>
      <Match when={store.state.view !== "chat"}>
        <Panel title={mainTitle(store.state.view)} text={mainContent(store.state)} background={colors.bg} />
      </Match>
    </Switch>
  )
}
