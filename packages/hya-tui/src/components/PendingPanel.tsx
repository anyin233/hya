import { useApp } from "../app/context"
import { pendingText } from "../state/format"
import { Panel } from "./Panel"

/** Right panel: pending permission requests (!) and questions (?). */
export function PendingPanel(props: { visible: boolean }) {
  const { store } = useApp()
  return <Panel title="Pending" width={28} visible={props.visible} text={pendingText(store.state)} />
}
