import { useTerminalDimensions } from "@opentui/solid"
import { Composer } from "../components/Composer"
import { Footer } from "../components/Footer"
import { Header } from "../components/Header"
import { MainPanel } from "../components/MainPanel"
import { PendingPanel } from "../components/PendingPanel"
import { SessionsPanel } from "../components/SessionsPanel"
import { StatusLine } from "../components/StatusLine"
import { colors } from "../theme"

/** Terminal widths below which the side panels hide. */
export const layoutBreakpoints = { sessions: 58, pending: 105 } as const

/**
 * Root layout: header, then Sessions | Chat | Pending, then status line,
 * bordered composer, and footer instruction.
 */
export function App() {
  const size = useTerminalDimensions()
  return (
    <box width="100%" height="100%" flexDirection="column" backgroundColor={colors.bg}>
      <Header />
      <box width="100%" flexGrow={1} flexDirection="row">
        <SessionsPanel visible={size().width >= layoutBreakpoints.sessions} />
        <MainPanel />
        <PendingPanel visible={size().width >= layoutBreakpoints.pending} />
      </box>
      <StatusLine />
      <Composer />
      <Footer />
    </box>
  )
}
