import { useApp } from "../app/context"
import { sessionListText } from "../state/format"
import { Panel } from "./Panel"

/** Left panel: session list, the selected one marked with ▸. */
export function SessionsPanel(props: { visible: boolean }) {
  const { store } = useApp()
  return <Panel title="Sessions" width={27} visible={props.visible} text={sessionListText(store.state)} />
}
