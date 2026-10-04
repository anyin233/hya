import type { BoxRenderable, MouseEvent, ScrollBoxRenderable } from "@opentui/core"
import { useRenderer, useTerminalDimensions } from "@opentui/solid"
import { createEffect, createMemo, createSignal, Match, onCleanup, Show, Switch } from "solid-js"
import { useApp } from "../app/context"
import { mainContent, modelReference, shownServer } from "../state/format"
import type { AppState } from "../state/store"
import { boundaryWeight, paneLeaves, renderedWeight, setSplitWeight, visiblePaneRoot, type PaneKind, type PaneLeaf, type PaneNode, type PaneSplit } from "../state/panes"
import { pageStep } from "../state/scroll"
import { keyboardOwner } from "../state/focus"
import { colors } from "../theme"
import { tuiVersion } from "../version"
import { ExtensionPanelBox, ExtensionSidebar, extensionColumnWidth } from "../extensions/Host"
import { extensionManager } from "../extensions/manager"
import { ConversationPane } from "./ConversationPane"

/** Jobs shown here are current projection facts: busy sessions, live members, queued prompts, and pending asks. */
export function jobsText(state: AppState): string {
  const lines: string[] = []
  if (state.running && state.selected) lines.push(`● ${state.selected.title || state.selected.id} · turn running`)
  for (const session of state.sessions) {
    if (session.busy && !(state.running && session.id === state.selected?.id)) lines.push(`● ${session.title || session.id} · busy`)
  }
  for (const member of state.members) {
    if (member.status === "MEMBER_STATUS_RUNNING" || member.status === "MEMBER_STATUS_SPAWNING") {
      const activity = member.child ? state.children.get(member.child)?.activity : undefined
      lines.push(`↳ ${member.description || member.agent || member.member} · ${member.status === "MEMBER_STATUS_SPAWNING" ? "starting" : "running"}${activity ? ` · ${activity}` : ""}`)
    }
  }
  if (state.queued.length) lines.push(`${state.queued.length} queued prompt${state.queued.length === 1 ? "" : "s"}`)
  if (state.interactions.length) lines.push(`${state.interactions.length} pending request${state.interactions.length === 1 ? "" : "s"} · F4 to review`)
  return lines.length ? lines.join("\n") : "No active jobs. Busy sessions update on refresh; the open session's turn and members update live."
}

function paneText(state: AppState, kind: PaneKind, server: string): string {
  switch (kind) {
    case "jobs": return jobsText(state)
    case "status": return [
      `Server      ${shownServer(state, server)}`,
      `Version     ${tuiVersion}/${state.serverVersion || "unknown"}`,
      `Connection  ${state.connected ? "connected" : "disconnected"}`,
      `Session     ${state.selected?.title || state.selected?.id || "none"}`,
      `Agent       ${state.selected?.agent || "none"}`,
      `Model       ${state.selected ? modelReference(state.selected) || "default" : "none"}`,
      `Mode        ${state.selected?.permissionMode || "manual"}`,
      `Directory   ${state.selected?.workdir || "none"}`,
    ].join("\n")
    case "models": case "workflows": case "interactions": case "api": return mainContent(state, kind)
    case "projects": case "conversation": case "sessions": case "todos": case "context": case "extension": return ""
  }
}

function PaneLeafView(props: { node: PaneLeaf; width: number }) {
  const { store, server, ui } = useApp()
  const size = useTerminalDimensions()
  /** The extension panel this pane shows: its own (`extension` panes) or one replacing its built-in kind. */
  const extensionKey = () => props.node.kind === "extension" ? props.node.panel : props.node.kind === "conversation" ? undefined : extensionManager.replacement(props.node.kind)?.key
  let scroll: ScrollBoxRenderable | undefined
  const scroller = {
    line: (direction: -1 | 1) => { if (scroll) scroll.scrollBy(direction) },
    page: (direction: -1 | 1) => { if (scroll) scroll.scrollBy(direction * pageStep(scroll.viewport.height)) },
    top: () => { if (scroll) scroll.scrollTop = 0 },
    bottom: () => { if (scroll) scroll.scrollTop = scroll.scrollHeight },
  }
  let registeredId: string | undefined
  createEffect(() => {
    ui.panes ??= new Map()
    if (registeredId && registeredId !== props.node.id && ui.panes.get(registeredId) === scroller) ui.panes.delete(registeredId)
    registeredId = props.node.id
    if (props.node.kind === "conversation") ui.panes.delete(props.node.id)
    else ui.panes.set(props.node.id, scroller)
  })
  onCleanup(() => { if (registeredId && ui.panes?.get(registeredId) === scroller) ui.panes.delete(registeredId) })
  const active = () => store.state.paneLayout.active === props.node.id
  const highlighted = () => keyboardOwner(store.state, ui.command?.active() ?? false) === props.node.id
  const title = () => `${active() ? "▸ " : ""}${props.node.kind} · ${props.node.id}`
  /** A click selects the pane; on the Projects pane it also gives it the keyboard (as Ctrl+P does). */
  const select = () => {
    if (!active()) store.setPaneLayout({ ...store.state.paneLayout, active: props.node.id })
    store.setProjectsFocus(props.node.kind === "projects")
  }
  const placeholderSurface = props.node.kind === "projects" || props.node.kind === "sessions" || props.node.kind === "todos" || props.node.kind === "context" ? props.node.kind : "projects"
  const placeholderTitle = { projects: "Projects", sessions: "Sessions", todos: "Todos", context: "Context" }[placeholderSurface]
  return (
    <Switch>
      <Match when={props.node.kind === "conversation"}>
        <box width="100%" height="100%" flexGrow={1} flexBasis={0} onMouseDown={select}>
          <ConversationPane width={props.width} />
        </box>
      </Match>
      <Match when={extensionKey()} keyed>
        {(key) => (
          <box width="100%" height="100%" flexGrow={1} flexBasis={0} onMouseDown={select}>
            <ExtensionPanelBox panel={extensionManager.panels().find((panel) => panel.key === key)} panelKey={key} width={props.width} height={size().height} highlighted={highlighted()} scrollRef={(element) => (scroll = element)} />
          </box>
        )}
      </Match>
      <Match when={props.node.kind === "projects" || props.node.kind === "sessions" || props.node.kind === "todos" || props.node.kind === "context"}>
        <box width="100%" height="100%" flexGrow={1} flexBasis={0} flexDirection="column" border borderColor={highlighted() ? colors.accent : colors.border} title={placeholderTitle} backgroundColor={colors.panel} paddingX={1} onMouseDown={select}>
          <text width="100%" wrapMode="word" fg={colors.muted}>{extensionManager.placeholder(placeholderSurface) ?? "Loading…"}</text>
        </box>
      </Match>
      <Match when={true}>
        <box width="100%" height="100%" flexGrow={1} flexBasis={0} flexDirection="column" border borderColor={highlighted() ? colors.accent : colors.border} title={title()} backgroundColor={colors.bg} onMouseDown={select}>
          <scrollbox ref={(element: ScrollBoxRenderable) => (scroll = element)} width="100%" flexGrow={1} paddingX={1} paddingY={1}>
            <text width="100%" wrapMode="word" fg={colors.fg}>{paneText(store.state, props.node.kind, server)}</text>
          </scrollbox>
        </box>
      </Match>
      <Match when={false}>
        <text />
      </Match>
    </Switch>
  )
}

/**
 * A drawn vertical split the mouse can resize: its boxes (screen geometry
 * after layout) and the first pane on each side, which name it in the saved
 * tree (`setSplitWeight`).
 */
interface SplitHandle {
  node: PaneSplit
  box: BoxRenderable
  first: BoxRenderable
  firstPane: string
  secondPane: string
}

/** Shared by every split of one workspace: the drawn splits, and the weight a running drag shows. */
interface SplitDrag {
  handles: Set<SplitHandle>
  live: () => { key: string; weight: number } | undefined
}

const splitKey = (node: PaneSplit): string => `${paneLeaves(node.first)[0]!.id}|${paneLeaves(node.second)[0]!.id}`

function PaneNodeView(props: { node: PaneNode; width: number; drag: SplitDrag }) {
  const leaf = () => props.node.type === "pane" ? props.node : undefined
  const split = () => props.node.type === "split" ? props.node : undefined
  return (
    <>
      <Show when={leaf()} keyed>{(node) => <PaneLeafView node={node} width={props.width} />}</Show>
      <Show when={split()} keyed>
        {(node) => {
          const key = splitKey(node)
          // A drag shows its weight live; the saved weight applies otherwise. Either way the right sidebar keeps its minimum.
          const weight = () => renderedWeight({ ...node, weight: props.drag.live()?.key === key ? props.drag.live()!.weight : node.weight }, props.width)
          let box: BoxRenderable | undefined
          let first: BoxRenderable | undefined
          if (node.axis === "vertical") {
            const handle = {
              node,
              firstPane: paneLeaves(node.first)[0]!.id,
              secondPane: paneLeaves(node.second)[0]!.id,
              get box() { return box! },
              get first() { return first! },
            }
            props.drag.handles.add(handle)
            onCleanup(() => props.drag.handles.delete(handle))
          }
          return (
            <box ref={(element: BoxRenderable) => (box = element)} width="100%" height="100%" flexGrow={1} flexBasis={0} flexDirection={node.axis === "vertical" ? "row" : "column"}>
              <box ref={(element: BoxRenderable) => (first = element)} flexGrow={weight()} flexShrink={1} flexBasis={0}>
                <PaneNodeView node={node.first} width={node.axis === "vertical" ? props.width * weight() : props.width} drag={props.drag} />
              </box>
              <box flexGrow={1 - weight()} flexShrink={1} flexBasis={0}>
                <PaneNodeView node={node.second} width={node.axis === "vertical" ? props.width * (1 - weight()) : props.width} drag={props.drag} />
              </box>
            </box>
          )
        }}
      </Show>
    </>
  )
}

/**
 * Recursively tile every visible workspace rectangle, including the
 * interactive conversation. A left-button drag that starts on the boundary
 * of a side-by-side split (a sidebar's edge) moves that boundary: the drag
 * is drawn live, and on release the weight goes into the saved layout
 * (`paneLayout` in the preferences file).
 */
export function PaneWorkspace() {
  const { store, controller } = useApp()
  const size = useTerminalDimensions()
  const renderer = useRenderer()
  const visibleRoot = createMemo(() => visiblePaneRoot(store.state.paneLayout.root, size().width, store.state.sidebar, store.state.projectsSidebar))
  createEffect(() => {
    const leaves = paneLeaves(visibleRoot())
    if (!leaves.some((pane) => pane.id === store.state.paneLayout.active)) {
      const active = leaves.find((pane) => pane.kind === "conversation")?.id ?? leaves[0]!.id
      store.setPaneLayout({ ...store.state.paneLayout, active })
      store.setProjectsFocus(false)
    }
  })
  const [live, setLive] = createSignal<{ key: string; weight: number }>()
  const drag: SplitDrag = { handles: new Set(), live }
  let dragging: SplitHandle | undefined
  // The boundary is the second side's first column (a sidebar's border) or the column before it; the innermost split wins.
  let workspace: BoxRenderable | undefined
  const down = (event: MouseEvent) => {
    dragging = undefined
    if (event.button !== 0) return
    let best: SplitHandle | undefined
    for (const handle of drag.handles) {
      const { box, first } = handle
      if (!box || !first || event.y < box.y || event.y >= box.y + box.height) continue
      const boundary = first.x + first.width
      if (event.x !== boundary && event.x !== boundary - 1) continue
      if (!best || box.width < best.box.width) best = handle
    }
    dragging = best
    if (!best || !workspace) return
    // A press on text next to the border also starts a text selection (OpenTUI starts it before
    // dispatching the press); while it drags, moves go to the hovered text, not here.
    renderer.clearSelection()
    // OpenTUI otherwise captures the drag on whatever is under its first move, and a pane re-renders
    // its rows as the boundary moves (Sessions, Context): the captured row is destroyed and the drag
    // stops. Capture on this box, which lives as long as the workspace. `setCapturedRenderable` is
    // OpenTUI's own drag capture, private in its typings (@opentui/core 0.x renderer.d.ts).
    const capture = renderer as unknown as { setCapturedRenderable(target: BoxRenderable): void }
    capture.setCapturedRenderable(workspace)
  }
  const move = (event: MouseEvent) => {
    if (!dragging) return
    const weight = boundaryWeight(dragging.node, dragging.box.x, dragging.box.width, event.x)
    setLive({ key: splitKey(dragging.node), weight })
  }
  const release = () => {
    const handle = dragging
    const shown = live()
    dragging = undefined
    setLive(undefined)
    if (!handle || !shown) return
    const next = setSplitWeight(store.state.paneLayout, handle.firstPane, handle.secondPane, shown.weight)
    if (next === store.state.paneLayout) return
    store.setPaneLayout(next)
    try { controller.savePreferences({ paneLayout: next }) }
    catch (error) { store.setStatus(`Layout changed, not saved: ${error instanceof Error ? error.message : String(error)}`) }
  }
  return (
    <box ref={(element: BoxRenderable) => (workspace = element)} width="100%" height="100%" flexGrow={1} flexBasis={0} flexDirection="row" onMouseDown={down} onMouseDrag={move} onMouseDragEnd={release}>
      <box width="100%" height="100%" flexGrow={1} flexBasis={0}><PaneNodeView node={visibleRoot()} width={size().width - extensionColumnWidth(store.state.paneLayout, size().width)} drag={drag} /></box>
      <ExtensionSidebar />
    </box>
  )
}
