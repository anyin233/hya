import type { ScrollBoxRenderable } from "@opentui/core"
import { useTerminalDimensions } from "@opentui/solid"
import { For, Show } from "solid-js"
import { useApp } from "../app/context"
import { paneLeaves, type PaneLayout } from "../state/panes"
import { sidebarMinColumns } from "../state/layout"
import { colors } from "../theme"
import { extensionManager, type PanelEntry } from "./manager"
import { RenderTree } from "./renderTree"

/**
 * One extension panel's content at `width`×`height` cells; the extension
 * renders asynchronously. `scrollRef` hands its scrollbox to the pane's
 * keyboard scrolling (components/PaneWorkspace.tsx).
 */
export function ExtensionPanel(props: { panelKey: string; width: number; height: number; scrollRef?: (element: ScrollBoxRenderable) => void }) {
  const view = () => extensionManager.panelView(props.panelKey, Math.max(1, Math.floor(props.width)), Math.max(1, Math.floor(props.height)))
  const [extension, id] = [props.panelKey.slice(0, props.panelKey.lastIndexOf("#")), props.panelKey.slice(props.panelKey.lastIndexOf("#") + 1)]
  return (
    <scrollbox {...(props.scrollRef ? { ref: props.scrollRef } : {})} width="100%" flexGrow={1} paddingX={1}>
      <Show when={view().node} keyed fallback={<text wrapMode="word" fg={view().error ? colors.error : colors.muted}>{view().error ?? "Loading…"}</text>}>
        {(node) => <RenderTree node={node} host={{ onAction: (action, point) => void extensionManager.action(extension, { kind: "panel", id }, action, point) }} />}
      </Show>
    </scrollbox>
  )
}

/** A one-row extension panel drawn flush (no frame, no scrolling): the Conversation's Context line. */
export function ExtensionLine(props: { panelKey: string; width: number }) {
  const view = () => extensionManager.panelView(props.panelKey, Math.max(1, Math.floor(props.width)), 1)
  return (
    <box width="100%" height={1} flexShrink={0}>
      <Show when={view().node} keyed>{(node) => <RenderTree node={node} />}</Show>
    </box>
  )
}

/**
 * The full-screen overlay an extension panel draws while it holds the
 * keyboard (`project_view`: `/project`), framed like the built-in views.
 */
export function ExtensionOverlay() {
  const { store } = useApp()
  const size = useTerminalDimensions()
  return (
    <Show when={store.state.extensionOverlay} keyed>
      {(panelKey) => (
        <box position="absolute" top={0} left={0} width="100%" height="100%" zIndex={50} border borderColor={colors.accent} title={extensionManager.panels().find((panel) => panel.key === panelKey)?.title ?? panelKey} backgroundColor={colors.bg} flexDirection="column">
          <ExtensionPanel panelKey={panelKey} width={size().width - 4} height={size().height - 2} />
        </box>
      )}
    </Show>
  )
}

/** A bordered extension panel box in the sidebar look, as panes and the extension column draw it. */
export function ExtensionPanelBox(props: { panel: PanelEntry | undefined; panelKey: string; width: number; height: number; highlighted?: boolean; scrollRef?: (element: ScrollBoxRenderable) => void }) {
  return (
    <box width="100%" height="100%" flexGrow={1} flexBasis={0} flexDirection="column" border borderColor={props.highlighted ? colors.accent : colors.border} title={props.panel?.title ?? props.panelKey} backgroundColor={colors.panel}>
      <ExtensionPanel panelKey={props.panelKey} width={props.width - 4} height={props.height - 2} {...(props.scrollRef ? { scrollRef: props.scrollRef } : {})} />
    </box>
  )
}

/** `sidebar` panels that no `/layout` pane shows yet: the extension column's panels. */
function columnPanels(layout: PaneLayout): PanelEntry[] {
  const placed = new Set(paneLeaves(layout.root).flatMap((pane) => pane.panel ? [pane.panel] : []))
  return extensionManager.panels().filter((panel) => panel.placement === "sidebar" && !panel.replaces && !placed.has(panel.key))
}

/** Columns the extension column takes from the workspace at `columns` wide (0 while it is hidden). */
export function extensionColumnWidth(layout: PaneLayout, columns: number): number {
  if (!columnPanels(layout).length || columns < sidebarMinColumns * 3) return 0
  return Math.max(sidebarMinColumns, Math.min(48, Math.floor(columns * 0.25)))
}

/**
 * The extension column at the workspace's right edge: `sidebar` panels that no
 * `/layout` pane shows yet. Hidden while the terminal is too narrow for a sidebar.
 */
export function ExtensionSidebar() {
  const { store } = useApp()
  const size = useTerminalDimensions()
  const panels = () => columnPanels(store.state.paneLayout)
  const width = () => extensionColumnWidth(store.state.paneLayout, size().width)
  return (
    <Show when={width() > 0}>
      <box width={width()} height="100%" flexShrink={0} flexDirection="column">
        <For each={panels()}>{(panel) => <ExtensionPanelBox panel={panel} panelKey={panel.key} width={width()} height={Math.floor(size().height / panels().length)} />}</For>
      </box>
    </Show>
  )
}
