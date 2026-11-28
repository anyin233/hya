import type { ScrollBoxRenderable } from "@opentui/core"
import { useTerminalDimensions } from "@opentui/solid"
import { createEffect, createMemo, createSignal, onCleanup, Show, untrack, type JSX } from "solid-js"
import { AppContext, useApp } from "../app/context"
import type { SessionView } from "../app/sessionViews"
import { Transcript } from "../components/Transcript"
import { colors } from "../theme"
import { extensionManager, type PanelEntry } from "./manager"
import { containsTranscript, RenderTree, type TreeHost } from "./renderTree"
import type { JsonValue, TranscriptRenderNode } from "./wire"

/**
 * Host-virtualized streaming transcript renderer for v2 `{ type: "transcript", sessionId }` nodes.
 * Renders the live session projection directly via `<Transcript secondary />` without
 * cloning transcript trees over the extension JSON-RPC boundary.
 */
export function EmbeddedTranscript(props: { node: TranscriptRenderNode }) {
  const app = useApp()
  const [view, setView] = createSignal<SessionView>()
  const isCurrent = () => app.store.state.selected?.id === props.node.sessionId
  const target = createMemo(() => {
    const sessionId = props.node.sessionId
    const server = app.store.state.serverUrl
    const root = app.store.state.selected?.id ?? "root"
    if (!sessionId || isCurrent()) return undefined
    return JSON.stringify([server, root, sessionId])
  })
  createEffect(() => {
    const key = target()
    if (!key) { setView(undefined); return }
    const [server, root, sessionId] = JSON.parse(key) as [string, string, string]
    const next = untrack(() => app.controller.sessionViews.acquire(JSON.stringify([server, root]), sessionId))
    setView(next)
    onCleanup(next.release)
  })
  return (
    <Show
      when={!isCurrent()}
      fallback={<Transcript secondary />}
    >
      <Show
        when={view()}
        keyed
        fallback={<text wrapMode="word" fg={colors.muted}>{`Loading transcript for ${props.node.sessionId}…`}</text>}
      >
        {(current) => (
          <AppContext.Provider value={{ ...app, store: current.store, ui: {} }}>
            <box width="100%" flexGrow={1} flexDirection="column">
              <Transcript secondary />
            </box>
          </AppContext.Provider>
        )}
      </Show>
    </Show>
  )
}

/**
 * One extension panel's content at `width`×`height` cells; the extension
 * renders asynchronously. `scrollRef` hands its scrollbox to the pane's
 * keyboard scrolling (components/PaneWorkspace.tsx).
 */
export function ExtensionPanel(props: {
  panelKey: string
  width: number
  height: number
  paddingX?: number
  scrollRef?: (element: ScrollBoxRenderable) => void
  transcript?: (node: TranscriptRenderNode) => JSX.Element
}) {
  const view = () => extensionManager.panelView(props.panelKey, Math.max(1, Math.floor(props.width)), Math.max(1, Math.floor(props.height)))
  const [extension, id] = [props.panelKey.slice(0, props.panelKey.lastIndexOf("#")), props.panelKey.slice(props.panelKey.lastIndexOf("#") + 1)]
  const host: TreeHost = {
    onAction: (action, point) => void extensionManager.action(extension, { kind: "panel", id }, action, point),
    transcript: props.transcript ?? ((node) => <EmbeddedTranscript node={node} />),
  }
  return (
    <Show
      when={!containsTranscript(view().node)}
      fallback={
        <box width="100%" height="100%" flexGrow={1} flexDirection="column" paddingX={props.paddingX ?? 1}>
          <Show when={view().node} keyed>
            {(node) => <RenderTree node={node} host={host} />}
          </Show>
        </box>
      }
    >
      <scrollbox {...(props.scrollRef ? { ref: props.scrollRef } : {})} width="100%" flexGrow={1} paddingX={props.paddingX ?? 1}>
        <Show when={view().node} keyed fallback={<text wrapMode="word" fg={view().error ? colors.error : colors.muted}>{view().error ?? "Loading…"}</text>}>
          {(node) => <RenderTree node={node} host={host} />}
        </Show>
      </scrollbox>
    </Show>
  )
}

/**
 * One dynamic named extension pane's content at `width`×`height` cells
 * (`api.registerPane`, v2). Supports multi-instance parameters and
 * host-virtualized `{ type: "transcript", sessionId }` nodes.
 */
export function ExtensionPane(props: {
  paneKey: string
  width: number
  height: number
  instance?: string
  params?: JsonValue
  paddingX?: number
  scrollRef?: (element: ScrollBoxRenderable) => void
  transcript?: (node: TranscriptRenderNode) => JSX.Element
}) {
  const view = () => extensionManager.paneView(
    props.paneKey,
    Math.max(1, Math.floor(props.width)),
    Math.max(1, Math.floor(props.height)),
    { ...(props.instance ? { instance: props.instance } : {}), ...(props.params !== undefined ? { input: props.params } : {}) },
  )
  const [extension, id] = [props.paneKey.slice(0, props.paneKey.lastIndexOf("#")), props.paneKey.slice(props.paneKey.lastIndexOf("#") + 1)]
  const host: TreeHost = {
    onAction: (action, point) => void extensionManager.action(extension, { kind: "pane", id, ...(props.instance ? { instance: props.instance } : {}) }, action, point),
    transcript: props.transcript ?? ((node) => <EmbeddedTranscript node={node} />),
  }
  return (
    <Show
      when={!containsTranscript(view().node)}
      fallback={
        <box width="100%" height="100%" flexGrow={1} flexDirection="column" paddingX={props.paddingX ?? 1}>
          <Show when={view().node} keyed>
            {(node) => <RenderTree node={node} host={host} />}
          </Show>
        </box>
      }
    >
      <scrollbox {...(props.scrollRef ? { ref: props.scrollRef } : {})} width="100%" flexGrow={1} paddingX={props.paddingX ?? 1}>
        <Show when={view().node} keyed fallback={<text wrapMode="word" fg={view().error ? colors.error : colors.muted}>{view().error ?? "Loading…"}</text>}>
          {(node) => <RenderTree node={node} host={host} />}
        </Show>
      </scrollbox>
    </Show>
  )
}

/**
 * The full-screen overlay an extension panel or dynamic pane draws while it holds
 * the keyboard (`project_view`: `/project`, or `pane.open` with `slot: "overlay"`).
 */
export function ExtensionOverlay() {
  const { store } = useApp()
  const size = useTerminalDimensions()
  return (
    <Show when={store.state.extensionOverlay} keyed>
      {(panelKey) => {
        const isDynamicPane = () => extensionManager.panes().some((pane) => pane.key === panelKey)
        const title = () =>
          extensionManager.panels().find((panel) => panel.key === panelKey)?.title
          ?? extensionManager.panes().find((pane) => pane.key === panelKey)?.title
          ?? panelKey
        return (
          <box position="absolute" top={0} left={0} width="100%" height="100%" zIndex={50} border borderColor={colors.focus} titleColor={colors.heading} title={title()} backgroundColor={colors.bg} flexDirection="column">
            <Show
              when={isDynamicPane()}
              fallback={<ExtensionPanel panelKey={panelKey} width={size().width - 4} height={size().height - 2} />}
            >
              <ExtensionPane paneKey={panelKey} width={size().width - 4} height={size().height - 2} />
            </Show>
          </box>
        )
      }}
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
