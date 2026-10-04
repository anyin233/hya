import type { MouseEvent, Selection } from "@opentui/core"
import { useRenderer, useSelectionHandler, useTerminalDimensions } from "@opentui/solid"
import { createEffect } from "solid-js"
import { AgentsView } from "../components/AgentsView"
import { CommandPane } from "../components/CommandPane"
import { DiffView } from "../components/DiffView"
import { BundlesView } from "../components/BundlesView"
import { McpView } from "../components/McpView"
import { Picker } from "../components/Picker"
import { PaneWorkspace } from "../components/PaneWorkspace"
import { ProviderView } from "../components/ProviderView"
import { RulesView } from "../components/RulesView"
import { paintSelection } from "../components/selection"
import { copyNotice } from "../composer/clipboard"
import { extensionContext } from "../extensions/context"
import { ExtensionOverlay } from "../extensions/Host"
import { extensionManager } from "../extensions/manager"
import { shownServer } from "../state/format"
import { colors } from "../theme"
import { useApp } from "./context"

export function App() {
  const { store, controller, server } = useApp()
  const size = useTerminalDimensions()
  const renderer = useRenderer()
  useSelectionHandler((selection: Selection) => {
    const text = selection.getSelectedText()
    if (!text) return
    store.setStatus(copyNotice(text, controller.copyText(text)))
  })
  extensionManager.configure({
    release: () => store.setProjectsFocus(false),
    close: (panelKey) => { if (store.state.extensionOverlay === panelKey) store.setExtensionOverlay(undefined) },
    open: (panelKey) => { if (extensionManager.replacement("project_view")?.key === panelKey) store.setExtensionOverlay(panelKey) },
    executeCommand: (command, origin) => controller.hostCommand(command, origin),
  })
  let paintTimer: ReturnType<typeof setTimeout> | undefined
  const paint = (event: MouseEvent): void => {
    if (event.button !== 0) return
    // Run after the click handler has updated the layout. Traversing renderables
    // during mouse-down can leave selection state attached to nodes whose geometry
    // is about to move, making an ordinary click select part of the UI.
    if (paintTimer !== undefined) clearTimeout(paintTimer)
    paintTimer = setTimeout(() => {
      paintTimer = undefined
      paintSelection(renderer.root, colors.selection)
    }, 0)
  }
  createEffect(() => extensionManager.setContext(extensionContext(store.state, size(), { server: shownServer(store.state, server), items: extensionManager.statusFields().map(({ label, value, priority }) => ({ label, text: value, priority })) })))
  // What `api.fs` may read: the active Project's roots (none: every call is refused).
  createEffect(() => extensionManager.setRoots(store.state.projects.find((project) => project.id === store.state.activeProjectId)?.roots ?? []))
  createEffect(() => store.setColumns(size().width))
  return (
    <box width="100%" height="100%" flexDirection="row" backgroundColor={colors.bg} onMouseDrag={paint}>
      <CommandPane />
      <box width="100%" height="100%" flexGrow={1}>
        <PaneWorkspace />
      </box>
      <ProviderView />
      <DiffView />
      <McpView />
      <BundlesView />
      <RulesView />
      <AgentsView />
      <Picker />
      <ExtensionOverlay />
    </box>
  )
}
