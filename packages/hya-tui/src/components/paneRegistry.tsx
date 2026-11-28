import type { ScrollBoxRenderable } from "@opentui/core"
import { Show, type Component } from "solid-js"
import type { DiffScroller, PaneInputHandle } from "../app/context"
import { useApp } from "../app/context"
import { resolveBinding } from "../keys/bindings"
import { modelReference, shownServer } from "../state/format"
import { paneDefinitions, paneKinds, type BundledPaneKind, type PaneDefinition, type PaneKind, type PaneLeaf } from "../state/panes"
import type { AppState } from "../state/store"
import { colors } from "../theme"
import { tuiVersion } from "../version"
import { ExtensionPane, ExtensionPanel } from "../extensions/Host"
import { extensionManager } from "../extensions/manager"
import type { JsonValue } from "../extensions/wire"
import { ConversationPane } from "./ConversationPane"
import { LayoutPane } from "./LayoutPane"
import { MessagePane } from "./MessagePane"
import { PaneFrame } from "./PaneFrame"
import { WorkingIndicator } from "./WorkingIndicator"

export interface PaneRenderProps {
  node: PaneLeaf
  width: number
  height: number
  focused: boolean
  scrollRef(element: ScrollBoxRenderable): void
}
export interface PaneInputContext {
  node: PaneLeaf
  scroll: DiffScroller
}
export interface RegisteredPane extends PaneDefinition {
  render: Component<PaneRenderProps>
  input?(context: PaneInputContext): PaneInputHandle
}

function scrollInput({ scroll }: PaneInputContext): PaneInputHandle {
  return { onKey: (key) => {
    const action = resolveBinding(key)
    if (key.name === "up" && !key.ctrl && !key.meta) scroll.line(-1)
    else if (key.name === "down" && !key.ctrl && !key.meta) scroll.line(1)
    else if (action === "pageUp" || action === "pageDown") scroll.page(action === "pageUp" ? -1 : 1)
    else if (action === "scrollTop") scroll.top()
    else if (action === "scrollBottom") scroll.bottom()
  } }
}

function statusPaneText(state: AppState, server: string): string {
  return [
    `Server      ${shownServer(state, server)}`,
    `Version     ${tuiVersion}/${state.serverVersion || "unknown"}`,
    `Connection  ${state.connected ? "connected" : "disconnected"}`,
    `Session     ${state.selected?.title || state.selected?.id || "none"}`,
    `Agent       ${state.selected?.agent || "none"}`,
    `Model       ${state.selected ? modelReference(state.selected) || "default" : "none"}`,
    `Mode        ${state.selected?.permissionMode || "manual"}`,
    `Directory   ${state.selected?.workdir || "none"}`,
  ].join("\n")
}

function StatusPane(props: PaneRenderProps) {
  const { store, server } = useApp()
  return <PaneFrame kind="status" focused={props.focused}
    title={`${props.focused ? "▸ " : ""}status · ${props.node.id}`} background={colors.bg}>
    <scrollbox ref={props.scrollRef} width="100%" flexGrow={1} paddingY={1}>
      <Show when={extensionManager.replacement("status")?.key} keyed
        fallback={<text width="100%" wrapMode="word" fg={colors.fg}>{statusPaneText(store.state, server)}</text>}>
        {(key) => <ExtensionPanel panelKey={key} width={props.width - 4} height={props.height - 2} paddingX={0} />}
      </Show>
    </scrollbox>
  </PaneFrame>
}

/** Bundle-supplied slots (`projects`, `sessions`, `todos`, `context`, `jobs`) and dynamic v2 panes (`extension` / `<bundle>#<pane>`). */
function BundledPane(props: PaneRenderProps) {
  const panel = () => props.node.kind === "extension"
    ? props.node.panel
    : extensionManager.replacement(props.node.kind as BundledPaneKind)?.key
  const dynamicPane = () => {
    const key = panel()
    return key ? extensionManager.panes().find((item) => item.key === key) : undefined
  }
  const title = () => {
    const base = props.node.title
      ?? dynamicPane()?.title
      ?? extensionManager.panels().find((item) => item.key === panel())?.title
      ?? paneDefinitions[props.node.kind].title
    return props.node.pinned ? `${base} · pinned` : base
  }
  const contentWidth = () => props.width - (paneDefinitions[props.node.kind].selectable ? 4 : 2)
  const contentHeight = () => props.height - (paneDefinitions[props.node.kind].selectable ? 2 : 1)
  return <PaneFrame kind={props.node.kind} focused={props.focused} title={title()}>
    <Show when={panel()} keyed fallback={<text wrapMode="word" fg={colors.muted}>{extensionManager.placeholder(props.node.kind as BundledPaneKind) ?? "Loading…"}</text>}>
      {(key) => (
        <Show
          when={dynamicPane()}
          fallback={<ExtensionPanel panelKey={key} width={contentWidth()} height={contentHeight()} paddingX={0} scrollRef={props.scrollRef} />}
        >
          <ExtensionPane
            paneKey={key}
            width={contentWidth()}
            height={contentHeight()}
            {...(props.node.instance ? { instance: props.node.instance } : {})}
            {...(props.node.params !== undefined ? { params: props.node.params as JsonValue } : {})}
            paddingX={0}
            scrollRef={props.scrollRef}
          />
        </Show>
      )}
    </Show>
  </PaneFrame>
}

function bundledInput(context: PaneInputContext): PaneInputHandle {
  const fallback = scrollInput(context)
  return { onKey: (key) => {
    if (context.node.kind === "extension" && context.node.panel) {
      const pane = extensionManager.panes().find((entry) => entry.key === context.node.panel)
      if (pane?.keys) { void extensionManager.paneKey(pane.key, key, context.node.instance); return }
      const panel = extensionManager.panels().find((entry) => entry.key === context.node.panel)
      if (panel?.keys) { void extensionManager.key(panel.key, key); return }
      fallback.onKey(key)
      return
    }
    const panel = extensionManager.replacement(context.node.kind as BundledPaneKind)
    if (panel?.keys) void extensionManager.key(panel.key, key)
    else if (context.node.kind !== "projects") fallback.onKey(key)
  } }
}

const renderers: Record<PaneKind, Component<PaneRenderProps>> = {
  conversation: () => <ConversationPane />,
  composer: (props) => <MessagePane width={props.width} />,
  activity: () => <WorkingIndicator />,
  status: StatusPane,
  layout: LayoutPane,
  projects: BundledPane,
  sessions: BundledPane,
  todos: BundledPane,
  context: BundledPane,
  jobs: BundledPane,
  extension: BundledPane,
}

/** Every pane uses the same registration and input ownership contract. */
export const paneRegistry: Record<PaneKind, RegisteredPane> = Object.fromEntries(
  paneKinds.map((kind) => [kind, {
    ...paneDefinitions[kind],
    render: renderers[kind],
    input: !paneDefinitions[kind].selectable || kind === "composer" || kind === "layout" ? undefined : bundledInput,
  }]),
) as Record<PaneKind, RegisteredPane>
