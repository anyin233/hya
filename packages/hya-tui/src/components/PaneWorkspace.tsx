import type { BoxRenderable, MouseEvent, ScrollBoxRenderable } from "@opentui/core"
import { useRenderer, useKeyboard, usePaste, useTerminalDimensions } from "@opentui/solid"
import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js"
import type { PaneInputHandle } from "../app/context"
import { useApp } from "../app/context"
import { isSelectablePane, layoutRects, paneDefinitions, paneLeaves, paneNodes, setContainerBoundary, visiblePaneRoot, wrapPane, setPaneSize, type LayoutDirection, type PaneLeaf, type PaneNode, type Rect } from "../state/panes"
import { sidebarMinColumns } from "../state/layout"
import { pageStep } from "../state/scroll"
import { keyboardOwner } from "../state/focus"
import { extensionManager } from "../extensions/manager"
import { paneRegistry } from "./paneRegistry"
import { workingLineText } from "../state/activity"
import { currentPrompt } from "../state/prompts"
import { pendingLines } from "../state/format"
export { jobsText } from "./paneRegistry"

/** One stable mounted instance per id; tree edits change its bounds, never its parent. */
function PaneLeafView(props: { node: PaneLeaf; rect?: Rect }) {
  const { store, controller, ui } = useApp()
  let box: BoxRenderable | undefined, scroll: ScrollBoxRenderable | undefined
  const scroller = {
    line: (direction: -1 | 1) => { if (scroll) scroll.scrollBy(direction) },
    page: (direction: -1 | 1) => { if (scroll) scroll.scrollBy(direction * pageStep(scroll.viewport.height)) },
    top: () => { if (scroll) scroll.scrollTop = 0 },
    bottom: () => { if (scroll) scroll.scrollTop = scroll.scrollHeight },
  }
  const input = createMemo(() => paneRegistry[props.node.kind].input?.({ node: props.node, scroll: scroller }))
  let registeredInput: PaneInputHandle | undefined, registeredId: string | undefined
  const bounds = (): Rect => props.rect && box ? { left: box.x, top: box.y, right: box.x + box.width, bottom: box.y + box.height } : { left: 0, top: 0, right: 0, bottom: 0 }
  const unregister = () => {
    if (!registeredId) return
    if (ui.panes?.get(registeredId) === scroller) ui.panes.delete(registeredId)
    if (ui.paneBounds?.get(registeredId) === bounds) ui.paneBounds.delete(registeredId)
    if (registeredInput && ui.paneInputs?.get(registeredId) === registeredInput) ui.paneInputs.delete(registeredId)
  }
  createEffect(() => {
    unregister()
    registeredId = props.node.id
    ui.panes ??= new Map(); ui.paneInputs ??= new Map(); ui.paneBounds ??= new Map()
    ui.paneBounds.set(props.node.id, bounds)
    registeredInput = input()
    if (registeredInput) { ui.panes.set(props.node.id, scroller); ui.paneInputs.set(props.node.id, registeredInput) }
  })
  onCleanup(unregister)
  const highlighted = () => keyboardOwner(store.state, ui.command?.active() ?? false) === props.node.id
  const renderer = createMemo(() => paneRegistry[props.node.kind].render)
  return <box ref={(element: BoxRenderable) => box = element} position="absolute" left={props.rect?.left ?? 0} top={props.rect?.top ?? 0}
    width={props.rect ? Math.max(0, props.rect.right - props.rect.left) : 1} height={props.rect ? Math.max(0, props.rect.bottom - props.rect.top) : 1}
    visible={!!props.rect && props.rect.right > props.rect.left && props.rect.bottom > props.rect.top} overflow="hidden"
    onMouseDown={() => { if (isSelectablePane(props.node) && store.state.paneLayout.active !== props.node.id) store.setPaneLayout({ ...store.state.paneLayout, active: props.node.id }) }}>
    <Show when={renderer()} keyed>{(render) => render({ get node() { return props.node }, get width() { return props.rect ? props.rect.right - props.rect.left : 1 }, get height() { return props.rect ? props.rect.bottom - props.rect.top : 1 }, get focused() { return highlighted() }, scrollRef: (element) => scroll = element })}</Show>
  </box>
}

interface Boundary { id: string; index: number; secondIndex: number; direction: LayoutDirection; first: Rect; second: Rect; container: Rect }
export function PaneWorkspace() {
  const { store, controller, ui } = useApp()
  useKeyboard((event) => ui.workspaceInput?.onKey(event))
  usePaste((event) => ui.workspaceInput?.onPaste(event))
  const size = useTerminalDimensions()
  const renderer = useRenderer()
  let workspace: BoxRenderable | undefined
  // New sidebar contributions become ordinary, selectable leaves. Closing one stays closed.
  const discoveredPanels = new Set<string>()
  createEffect(() => {
    for (const panel of extensionManager.panels().filter((entry) => entry.placement === "sidebar" && !entry.replaces)) {
      if (discoveredPanels.has(panel.key)) continue
      discoveredPanels.add(panel.key)
      if (paneLeaves(store.state.paneLayout.root).some((pane) => pane.panel === panel.key)) continue
      try {
        const active = store.state.paneLayout.active
        const added = wrapPane(store.state.paneLayout, "root", "row", "extension", false, panel.key)
        const next = setPaneSize(added, added.active, { mode: "weight", value: 1 / 3 })
        store.setPaneLayout({ ...next, active })
      } catch (error) { store.setStatus(`Extension pane: ${error instanceof Error ? error.message : String(error)}`) }
    }
  })
  const [revision, setRevision] = createSignal(0)
  const invalidate = () => setRevision((value) => value + 1)
  ui.invalidateLayout = invalidate
  onCleanup(() => { if (ui.invalidateLayout === invalidate) ui.invalidateLayout = undefined })
  const [live, setLive] = createSignal<{ id: string; index: number; secondIndex: number; ratio: number }>()
  const displayed = createMemo(() => { const edit = live(); return edit ? setContainerBoundary(store.state.paneLayout, edit.id, edit.index, edit.ratio, edit.secondIndex) : store.state.paneLayout })
  const displayedRoot = createMemo(() => displayed().root)
  const savedRoot = createMemo(() => store.state.paneLayout.root)
  const visibleRoot = createMemo(() => visiblePaneRoot(displayedRoot(), size().width, store.state.sidebar, store.state.projectsSidebar))
  const minimum = (node: PaneNode, direction: LayoutDirection): number => {
    if (node.type === "split") {
      const values = node.children.map((child) => minimum(child.node, direction))
      const count = node.direction === direction ? values.reduce((a, b) => a + b, 0) : Math.max(...values)
      return direction === "row" && paneLeaves(node).every((pane) => ["sessions", "todos", "context"].includes(pane.kind)) ? Math.max(sidebarMinColumns, count) : count
    }
    if (direction === "row") return ["sessions", "todos", "context"].includes(node.kind) ? sidebarMinColumns : paneDefinitions[node.kind].minColumns
    if (node.kind === "activity") return workingLineText(store.state, Date.now()) ? 1 : 0
    if (node.kind !== "composer") return paneDefinitions[node.kind].minRows
    void store.state.draft; void store.state.secretEntry
    const prompt = currentPrompt(store.state)?.view, pending = pendingLines(store.state, size().width).length
    return (ui.composerHeight?.() ?? 3) + (prompt ? 5 + prompt.body.length + prompt.options.length : 0)
      + (pending ? Math.min(3, pending) + 3 : 0) + (store.state.modeConfirm ? 1 : 0)
  }
  const rects = createMemo(() => { revision(); return layoutRects(visibleRoot(), { left: 0, top: 0, right: size().width, bottom: size().height }, minimum) })
  createEffect(() => {
    const eligible = paneLeaves(visibleRoot()).filter((pane) => isSelectablePane(pane) && (() => { const bounds = rects().get(pane.id); return bounds && bounds.right > bounds.left && bounds.bottom > bounds.top })())
    if (!eligible.length || eligible.some((pane) => pane.id === store.state.paneLayout.active)) return
    const active = eligible.find((pane) => pane.kind === "composer")?.id ?? eligible[0]!.id
    store.setPaneLayout({ ...store.state.paneLayout, active })
  })
  const nodes = createMemo(() => new Map(paneLeaves(savedRoot()).map((pane) => [pane.id, pane])))
  const ids = createMemo(() => [...nodes().keys()])
  const boundaries = (): Boundary[] => paneNodes(visibleRoot()).flatMap((node) => {
    if (node.type === "pane") return []
    const saved = paneNodes(displayedRoot()).find((item) => item.id === node.id)
    if (!saved || saved.type === "pane") return []
    const drawn = node.children.flatMap((child) => {
      const rect = rects().get(child.node.id)
      return rect && rect.right > rect.left && rect.bottom > rect.top ? [{ node: child.node, rect }] : []
    })
    return drawn.slice(0, -1).flatMap((child, position) => {
      const next = drawn[position + 1]!, container = rects().get(node.id)
      const index = saved.children.findIndex((item) => paneNodes(item.node).some((descendant) => descendant.id === child.node.id))
      const secondIndex = saved.children.findIndex((item) => paneNodes(item.node).some((descendant) => descendant.id === next.node.id))
      return container && index >= 0 && secondIndex > index ? [{ id: node.id, index, secondIndex, direction: node.direction, first: child.rect, second: next.rect, container }] : []
    })
  })
  let dragging: Boundary | undefined
  const down = (event: MouseEvent) => {
    dragging = undefined
    if (event.button !== 0) return
    for (const edge of boundaries()) {
      const row = edge.direction === "row", boundary = row ? edge.second.left : edge.second.top
      const coordinate = row ? event.x : event.y, cross = row ? event.y : event.x
      const low = row ? edge.container.top : edge.container.left, high = row ? edge.container.bottom : edge.container.right
      if ((coordinate !== boundary && coordinate !== boundary - 1) || cross < low || cross >= high) continue
      const area = (edge.container.right - edge.container.left) * (edge.container.bottom - edge.container.top)
      const previous = dragging && (dragging.container.right - dragging.container.left) * (dragging.container.bottom - dragging.container.top)
      if (!dragging || area < previous!) dragging = edge
    }
    if (dragging && workspace) {
      renderer.clearSelection()
      ;(renderer as unknown as { setCapturedRenderable(target: BoxRenderable): void }).setCapturedRenderable(workspace)
    }
  }
  const move = (event: MouseEvent) => {
    if (!dragging) return
    const row = dragging.direction === "row", start = row ? dragging.first.left : dragging.first.top, end = row ? dragging.second.right : dragging.second.bottom
    if (end <= start) return
    setLive({ id: dragging.id, index: dragging.index, secondIndex: dragging.secondIndex, ratio: (row ? event.x - start : event.y - start) / (end - start) })
  }
  const release = () => {
    const edit = live(); dragging = undefined; setLive(undefined)
    if (!edit) return
    const next = setContainerBoundary(store.state.paneLayout, edit.id, edit.index, edit.ratio, edit.secondIndex)
    store.setPaneLayout(next)
    try { controller.savePreferences({ paneLayout: next }) }
    catch (error) { store.setStatus(`Layout changed, not saved: ${error instanceof Error ? error.message : String(error)}`) }
  }
  return <box ref={(element: BoxRenderable) => workspace = element} width="100%" height="100%" overflow="hidden" onMouseDown={down} onMouseDrag={move} onMouseDragEnd={release}>
    <For each={ids()}>{(id) => <PaneLeafView node={nodes().get(id)!} rect={rects().get(id)} />}</For>
  </box>
}
