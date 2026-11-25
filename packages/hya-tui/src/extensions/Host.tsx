import type { ScrollBoxRenderable } from "@opentui/core"
import { useTerminalDimensions } from "@opentui/solid"
import { Show } from "solid-js"
import { useApp } from "../app/context"
import { colors } from "../theme"
import { extensionManager, type PanelEntry } from "./manager"
import { RenderTree } from "./renderTree"

/**
 * One extension panel's content at `width`×`height` cells; the extension
 * renders asynchronously. `scrollRef` hands its scrollbox to the pane's
 * keyboard scrolling (components/PaneWorkspace.tsx).
 */
export function ExtensionPanel(props: { panelKey: string; width: number; height: number; paddingX?: number; scrollRef?: (element: ScrollBoxRenderable) => void }) {
  const view = () => extensionManager.panelView(props.panelKey, Math.max(1, Math.floor(props.width)), Math.max(1, Math.floor(props.height)))
  const [extension, id] = [props.panelKey.slice(0, props.panelKey.lastIndexOf("#")), props.panelKey.slice(props.panelKey.lastIndexOf("#") + 1)]
  return (
    <scrollbox {...(props.scrollRef ? { ref: props.scrollRef } : {})} width="100%" flexGrow={1} paddingX={props.paddingX ?? 1}>
      <Show when={view().node} keyed fallback={<text wrapMode="word" fg={view().error ? colors.error : colors.muted}>{view().error ?? "Loading…"}</text>}>
        {(node) => <RenderTree node={node} host={{ onAction: (action, point) => void extensionManager.action(extension, { kind: "panel", id }, action, point) }} />}
      </Show>
    </scrollbox>
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
        <box position="absolute" top={0} left={0} width="100%" height="100%" zIndex={50} border borderColor={colors.focus} titleColor={colors.heading} title={extensionManager.panels().find((panel) => panel.key === panelKey)?.title ?? panelKey} backgroundColor={colors.bg} flexDirection="column">
          <ExtensionPanel panelKey={panelKey} width={size().width - 4} height={size().height - 2} />
        </box>
      )}
    </Show>
  )
}

/** An unplaced sidebar panel is passive; selectable layout panels use PaneFrame. */
export function ExtensionPanelBox(props: { panel: PanelEntry | undefined; panelKey: string; width: number; height: number; highlighted?: boolean; scrollRef?: (element: ScrollBoxRenderable) => void }) {
  return (
    <box width="100%" height="100%" flexGrow={1} flexBasis={0} flexDirection="column" backgroundColor={colors.bg}>
      <text height={1} flexShrink={0} wrapMode="none" fg={colors.heading}>{props.panel?.title ?? props.panelKey}</text>
      <ExtensionPanel panelKey={props.panelKey} width={props.width - 2} height={props.height - 1} {...(props.scrollRef ? { scrollRef: props.scrollRef } : {})} />
    </box>
  )
}
