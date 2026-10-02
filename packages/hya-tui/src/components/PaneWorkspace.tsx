import type { BoxRenderable, MouseEvent, ScrollBoxRenderable } from "@opentui/core"
import { useKeyboard, usePaste, useTerminalDimensions } from "@opentui/solid"
import { createEffect, createMemo, createSignal, onCleanup, Show } from "solid-js"
import type { PaneInputHandle } from "../app/context"
import { useApp } from "../app/context"
import { boundaryWeight, isSelectablePane, normalizePaneFocus, paneDefinitions, paneLeaves, renderedWeight, setSplitWeight, visiblePaneRoot, type PaneLeaf, type PaneNode, type PaneSplit } from "../state/panes"
import { pageStep } from "../state/scroll"
import { keyboardOwner } from "../state/focus"
import { paneRegistry } from "./paneRegistry"
import { currentPrompt } from "../state/prompts"
import { pendingLines } from "../state/format"

export { jobsText } from "./paneRegistry"

function PaneLeafView(props: { node: PaneLeaf; width: number }) {
  const { store, controller, ui } = useApp()
  let scroll: ScrollBoxRenderable | undefined
  const scroller = {
    line: (direction: -1 | 1) => { if (scroll) scroll.scrollBy(direction) },
    page: (direction: -1 | 1) => { if (scroll) scroll.scrollBy(direction * pageStep(scroll.viewport.height)) },
    top: () => { if (scroll) scroll.scrollTop = 0 },
    bottom: () => { if (scroll) scroll.scrollTop = scroll.scrollHeight },
  }
  const input = createMemo(() => paneRegistry[props.node.kind].input?.({ controller, scroll: scroller }))
  let registeredInput: PaneInputHandle | undefined
  let registeredId: string | undefined
  const unregister = () => {
    if (!registeredId) return
    if (ui.panes?.get(registeredId) === scroller) ui.panes.delete(registeredId)
    if (registeredInput && ui.paneInputs?.get(registeredId) === registeredInput) ui.paneInputs.delete(registeredId)
  }
  createEffect(() => {
    unregister()
    registeredId = props.node.id
    ui.panes ??= new Map()
    ui.paneInputs ??= new Map()
    registeredInput = input()
    if (registeredInput) {
      ui.panes.set(props.node.id, scroller)
      ui.paneInputs.set(props.node.id, registeredInput)
    }
  })
  onCleanup(unregister)
  const active = () => store.state.paneLayout.active === props.node.id
  const highlighted = () => keyboardOwner(store.state, ui.command?.active() ?? false) === props.node.id
  const select = () => {
    if (!isSelectablePane(props.node)) return
    if (active()) return
    store.setPaneLayout({ ...store.state.paneLayout, active: props.node.id })
  }
  const renderer = createMemo(() => paneRegistry[props.node.kind].render)
  return <box width="100%" height="100%" flexGrow={1} flexBasis={0} onMouseDown={select}>
    <Show when={renderer()} keyed>{(render) => render({
      get node() { return props.node },
      get width() { return props.width },
      get focused() { return highlighted() },
      scrollRef: (element) => { scroll = element },
    })}</Show>
  </box>
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
  const { store, ui } = useApp()
  const minRows = (node: PaneNode): number => {
    if (node.type === "split") return node.axis === "horizontal" ? minRows(node.first) + minRows(node.second) : Math.max(minRows(node.first), minRows(node.second))
    if (node.kind !== "composer") return paneDefinitions[node.kind].minRows
    // Draft updates establish a dependency before the editor mounts its row accessor.
    void store.state.draft
    const prompt = currentPrompt(store.state)?.view
    const pending = pendingLines(store.state, props.width).length
    return (ui.composerRows?.() ?? 1) + 2 + (prompt ? 5 + prompt.body.length + prompt.options.length : 0)
      + (pending ? Math.min(3, pending) + 3 : 0) + (store.state.modeConfirm ? 1 : 0) + (store.state.secretEntry ? 4 : 0)
  }
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
              <box ref={(element: BoxRenderable) => (first = element)} minHeight={node.axis === "horizontal" ? minRows(node.first) : 1} flexGrow={weight()} flexShrink={1} flexBasis={0}>
                <PaneNodeView node={node.first} width={node.axis === "vertical" ? props.width * weight() : props.width} drag={props.drag} />
              </box>
              <box minHeight={node.axis === "horizontal" ? minRows(node.second) : 1} flexGrow={1 - weight()} flexShrink={1} flexBasis={0}>
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
  const { store, controller, ui } = useApp()
  useKeyboard((event) => ui.workspaceInput?.onKey(event))
  usePaste((event) => ui.workspaceInput?.onPaste(event))
  const size = useTerminalDimensions()
  const visibleRoot = createMemo(() => visiblePaneRoot(store.state.paneLayout.root, size().width, store.state.sidebar, store.state.projectsSidebar))
  createEffect(() => {
    const normalized = normalizePaneFocus({ ...store.state.paneLayout, root: visibleRoot() })
    if (normalized.active !== store.state.paneLayout.active) {
      store.setPaneLayout({ ...store.state.paneLayout, active: normalized.active })
    }
  })
  const [live, setLive] = createSignal<{ key: string; weight: number }>()
  const drag: SplitDrag = { handles: new Set(), live }
  let dragging: SplitHandle | undefined
  // The boundary is the second side's first column (a sidebar's border) or the column before it; the innermost split wins.
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
  }
  const move = (event: MouseEvent) => {
    if (!dragging) return
    setLive({ key: splitKey(dragging.node), weight: boundaryWeight(dragging.node, dragging.box.x, dragging.box.width, event.x) })
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
    <box width="100%" height="100%" flexGrow={1} flexBasis={0} flexDirection="row" onMouseDown={down} onMouseDrag={move} onMouseDragEnd={release}>
      <PaneNodeView node={visibleRoot()} width={size().width} drag={drag} />
    </box>
  )
}
