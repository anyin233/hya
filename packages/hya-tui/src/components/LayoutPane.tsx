import type { MouseEvent, ScrollBoxRenderable } from "@opentui/core"
import { useTerminalDimensions } from "@opentui/solid"
import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js"
import { useApp, type PaneInputHandle } from "../app/context"
import { createLayoutEditor, layoutEditorBack, layoutEditorChoose, layoutEditorHeading, layoutEditorKey, layoutEditorPaste, layoutEditorRows, reconcileLayoutEditor, type LayoutEditorOutcome } from "../state/layoutEditor"
import { paneLeaves, visiblePaneRoot } from "../state/panes"
import { colors } from "../theme"
import { PaneFrame } from "./PaneFrame"
import type { PaneRenderProps } from "./paneRegistry"

/** A regular selectable pane: its forms and keyboard state belong to this instance. */
export function LayoutPane(props: PaneRenderProps) {
  const { store, controller, ui } = useApp()
  const dimensions = useTerminalDimensions()
  const [state, setState] = createSignal(createLayoutEditor(store.state.paneLayout.root.id))
  let scroll: ScrollBoxRenderable | undefined
  createEffect(() => setState((current) => reconcileLayoutEditor(store.state.paneLayout, current)))
  const rows = createMemo(() => layoutEditorRows(store.state.paneLayout, state()))
  const index = () => state().stage.type === "tree" ? rows().findIndex((row) => row.id === state().selected) : Math.min(state().index, rows().length - 1)
  const visible = createMemo(() => new Set(paneLeaves(visiblePaneRoot(store.state.paneLayout.root, store.state.columns, store.state.sidebar, store.state.projectsSidebar)).map((pane) => pane.id)))
  const detail = () => rows()[index()]?.detail ?? ""
  const hint = () => state().stage.type === "tree" ? "↑↓ select · ← parent · → child · Enter edit"
    : state().stage.type === "weight" ? "Number or content · Enter saves · Esc back" : "↑↓ select · Enter choose · Esc back"
  const apply = (outcome: LayoutEditorOutcome) => {
    setState(outcome.state)
    if (!outcome.layout) return
    // The editor keeps keyboard ownership while modifying other nodes, including passive ones.
    const next = { ...outcome.layout, active: paneLeaves(outcome.layout.root).some((pane) => pane.id === props.node.id && pane.kind === "layout") ? props.node.id : outcome.layout.active }
    store.setPaneLayout(next)
    try { controller.savePreferences({ paneLayout: store.state.paneLayout }) }
    catch (error) {
      const message = `Layout changed, not saved: ${error instanceof Error ? error.message : String(error)}`
      setState((current) => ({ ...current, error: message }))
      store.setStatus(message)
    }
  }
  const key = (name: string) => apply(layoutEditorKey(store.state.paneLayout, state(), { name, sequence: "", ctrl: false, meta: false, shift: false }))
  const input: PaneInputHandle = {
    onKey: (event) => apply(layoutEditorKey(store.state.paneLayout, state(), event)),
    onPaste: (event) => apply(layoutEditorPaste(state(), new TextDecoder().decode(event.bytes))),
  }
  let registeredId: string | undefined
  const unregister = () => { if (registeredId && ui.paneInputs?.get(registeredId) === input) ui.paneInputs.delete(registeredId) }
  createEffect(() => {
    unregister(); registeredId = props.node.id
    ui.paneInputs ??= new Map()
    ui.paneInputs.set(registeredId, input)
  })
  onCleanup(unregister)
  createEffect(() => {
    const at = index(), stage = state().stage.type
    void dimensions().height; void props.width
    queueMicrotask(() => {
      if (!scroll || scroll.isDestroyed || stage === "weight") return
      if (at < scroll.scrollTop) scroll.scrollTop = at
      else if (at >= scroll.scrollTop + scroll.viewport.height) scroll.scrollTop = Math.max(0, at - scroll.viewport.height + 1)
    })
  })
  const mouse = (event: MouseEvent, action: () => void) => {
    if (event.button !== 0) return
    event.stopPropagation()
    store.setPaneLayout({ ...store.state.paneLayout, active: props.node.id })
    action()
  }
  return <PaneFrame kind="layout" focused={props.focused} title={`Layout tree · ${props.node.id}`}>
    <text height={1} flexShrink={0} wrapMode="none" fg={colors.fg}>{layoutEditorHeading(state())}</text>
    <Show when={state().stage.type !== "weight"} fallback={
      <box width="100%" flexGrow={1} flexDirection="column">
        <text height={1} wrapMode="none" fg={colors.accent}>{state().stage.type === "weight" ? `${(state().stage as { value: string }).value}▏` : ""}</text>
        <text width="100%" wrapMode="word" fg={colors.muted}>Relative to siblings. Content uses the pane's needed height in a column.</text>
      </box>
    }>
      <scrollbox ref={(element: ScrollBoxRenderable) => { scroll = element; props.scrollRef(element) }} width="100%" flexGrow={1}>
        <For each={rows()}>{(row, at) => <text height={1} flexShrink={0} wrapMode="none"
          onMouseDown={(event: MouseEvent) => mouse(event, () => apply(layoutEditorChoose(store.state.paneLayout, state(), row.id)))}
          fg={at() === index() ? colors.accent : colors.fg}>
          {`${at() === index() ? "▸" : " "} ${"  ".repeat(Math.min(row.depth ?? 0, Math.max(0, Math.floor((props.width - 24) / 2))))}${row.label}${state().stage.type === "tree" && row.id.startsWith("pane-") && !visible().has(row.id) ? " (hidden)" : ""}`}
        </text>}</For>
      </scrollbox>
    </Show>
    <text height={2} flexShrink={0} width="100%" wrapMode="word" fg={state().error ? colors.error : colors.muted}>
      {state().error ?? `${state().selected} · ${detail()}`}
    </text>
    <text height={2} flexShrink={0} width="100%" wrapMode="word" fg={colors.muted}>{hint()}</text>
    <box height={1} flexShrink={0} flexDirection="row" gap={2}>
      <text fg={colors.accent} onMouseDown={(event: MouseEvent) => mouse(event, () => key("return"))}>{state().stage.type === "tree" ? "[Edit]" : state().stage.type === "weight" ? "[Save]" : "[Choose]"}</text>
      <Show when={state().stage.type !== "tree"}><text fg={colors.muted} onMouseDown={(event: MouseEvent) => mouse(event, () => setState(layoutEditorBack(state())))}>[Back]</text></Show>
    </box>
  </PaneFrame>
}
