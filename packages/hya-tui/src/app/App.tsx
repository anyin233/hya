import type { MouseEvent, Selection } from "@opentui/core"
import { useRenderer, useSelectionHandler, useTerminalDimensions } from "@opentui/solid"
import { createEffect } from "solid-js"
import { AgentModelsView } from "../components/AgentModelsView"
import { DiffView } from "../components/DiffView"
import { McpView } from "../components/McpView"
import { Picker } from "../components/Picker"
import { PaneWorkspace } from "../components/PaneWorkspace"
import { ProviderView } from "../components/ProviderView"
import { RulesView } from "../components/RulesView"
import { paintSelection } from "../components/selection"
import { copyNotice } from "../composer/clipboard"
import { ProjectView } from "../components/ProjectView"
import { colors } from "../theme"
import { useApp } from "./context"

export { layoutBreakpoints } from "../state/layout"

/**
 * Root layout: one editable split tree owns Projects, Conversation, Sessions,
 * Todos, and Context. The full-screen
 * Provider (`/key`), Diff (`/diff`), MCP (`/mcp`), Saved Rules (`/rules`),
 * and Agent Models (`/agent-models`) views are drawn over both when one of
 * them is open (at most one at a time), and the modal picker
 * (components/Picker.tsx) over everything.
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
  return (
    <box width="100%" height="100%" flexDirection="row" backgroundColor={colors.bg} onMouseDown={paint}>
      <PaneWorkspace />
      <ProviderView />
      <DiffView />
      <McpView />
      <RulesView />
      <AgentModelsView />
      <ProjectView />
      <Picker />
    </box>
  )
}
