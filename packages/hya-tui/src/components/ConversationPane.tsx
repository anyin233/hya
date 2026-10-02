import { MainPanel } from "./MainPanel"
import { colors } from "../theme"

/** Passive viewer: never owns keyboard focus or installs input handlers. */
export function ConversationPane() {
  return <box width="100%" height="100%" flexGrow={1} flexBasis={0} backgroundColor={colors.bg}><MainPanel /></box>
}
