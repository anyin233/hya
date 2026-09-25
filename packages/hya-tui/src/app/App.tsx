import { useTerminalDimensions } from "@opentui/solid"
import { createEffect, Show } from "solid-js"
import { Composer } from "../components/Composer"
import { Footer } from "../components/Footer"
import { Header } from "../components/Header"
import { MainPanel } from "../components/MainPanel"
import { PendingBlock } from "../components/PendingBlock"
import { PromptDock } from "../components/PromptDock"
import { Sidebar } from "../components/Sidebar"
import { StatusBar } from "../components/StatusBar"
import { StatusLine } from "../components/StatusLine"
import { WorkingIndicator } from "../components/WorkingIndicator"
import { sidebarVisible, sidebarWidth } from "../state/layout"
import { colors } from "../theme"
import { useApp } from "./context"

export { layoutBreakpoints } from "../state/layout"

/**
 * Root layout: one main column (header, status bar, transcript or view
 * panel, the working indicator for a running turn, pending block for other
 * sessions' asks, the permission/question prompt, status line, bordered
 * composer, footer instruction) and, when shown, the sidebar on the right
 * (state/layout.ts).
 */
export function App() {
  const { store } = useApp()
  const size = useTerminalDimensions()
  createEffect(() => store.setColumns(size().width))
  const shown = () => sidebarVisible(store.state.sidebar, size().width)
  const side = () => sidebarWidth(size().width)
  return (
    <box width="100%" height="100%" flexDirection="row" backgroundColor={colors.bg}>
      <box height="100%" flexGrow={1} flexBasis={0} flexDirection="column" backgroundColor={colors.bg}>
        <Header />
        <StatusBar />
        <MainPanel />
        <WorkingIndicator />
        <PendingBlock width={size().width - (shown() ? side() : 0)} />
        <PromptDock />
        <StatusLine />
        <Composer />
        <Footer />
      </box>
      <Show when={shown()}>
        <Sidebar width={side()} />
      </Show>
    </box>
  )
}
