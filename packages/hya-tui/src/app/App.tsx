import type { MouseEvent, Selection } from "@opentui/core"
import { useRenderer, useSelectionHandler, useTerminalDimensions } from "@opentui/solid"
import { createEffect, Show } from "solid-js"
import { Composer } from "../components/Composer"
import { Footer } from "../components/Footer"
import { Header } from "../components/Header"
import { MainPanel } from "../components/MainPanel"
import { ModeConfirm } from "../components/ModeConfirm"
import { PendingBlock } from "../components/PendingBlock"
import { Picker } from "../components/Picker"
import { ProviderView } from "../components/ProviderView"
import { paintSelection } from "../components/selection"
import { copyNotice } from "../composer/clipboard"
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
 * sessions' asks, the permission/question prompt, the one-line yolo
 * confirmation, status line, bordered composer, footer instruction) and,
 * when shown, the sidebar on the right (state/layout.ts). The full-screen
 * Provider View (`/key`, components/ProviderView.tsx) is drawn over both
 * when open, and the modal picker (components/Picker.tsx) over everything.
 *
 * Mouse selection: a left press paints the theme's selection color on the
 * text renderables (components/selection.ts); releasing a drag copies the
 * selected text with OSC 52 and the status line says `Copied N chars`
 * (docs/tui.md "Copy").
 */
export function App() {
  const { store, controller } = useApp()
  const size = useTerminalDimensions()
  const renderer = useRenderer()
  useSelectionHandler((selection: Selection) => {
    const text = selection.getSelectedText()
    if (!text) return
    store.setStatus(copyNotice(text, controller.copyText(text)))
  })
  const paint = (event: MouseEvent): void => {
    if (event.button === 0) paintSelection(renderer.root, colors.selection)
  }
  createEffect(() => store.setColumns(size().width))
  const shown = () => sidebarVisible(store.state.sidebar, size().width)
  const side = () => sidebarWidth(size().width)
  return (
    <box width="100%" height="100%" flexDirection="row" backgroundColor={colors.bg} onMouseDown={paint}>
      <box height="100%" flexGrow={1} flexBasis={0} flexDirection="column" backgroundColor={colors.bg}>
        <Header />
        <StatusBar />
        <MainPanel />
        <WorkingIndicator />
        <PendingBlock width={size().width - (shown() ? side() : 0)} />
        <PromptDock />
        <ModeConfirm />
        <StatusLine />
        <Composer />
        <Footer />
      </box>
      <Show when={shown()}>
        <Sidebar width={side()} />
      </Show>
      <ProviderView />
      <Picker />
    </box>
  )
}
