import type { MouseEvent, ScrollBoxRenderable } from "@opentui/core"
import { createEffect, createMemo, createSignal, For, onCleanup, Show, untrack } from "solid-js"
import { AppContext, useApp, type PaneInputHandle } from "../app/context"
import type { SessionView } from "../app/sessionViews"
import { paneLeaves } from "../state/panes"
import { subagentRows } from "../state/subagents"
import { colors } from "../theme"
import { PaneFrame } from "./PaneFrame"
import { Transcript } from "./Transcript"
import type { PaneRenderProps } from "./paneRegistry"

/** Keyboard ownership stays in this selector; preview never calls openSession. */
export function SubagentsPane(props: PaneRenderProps) {
  const { store, controller, ui } = useApp()
  const rows = createMemo(() => subagentRows(store.state))
  const selected = () => rows().findIndex((row) => row.id === store.state.subagentSelection)
  const index = () => Math.max(0, selected())
  let scroll: ScrollBoxRenderable | undefined
  createEffect(() => {
    const row = rows()[index()]
    if (row && row.id !== store.state.subagentSelection) store.selectSubagent(row.id)
  })
  createEffect(() => {
    const at = index()
    queueMicrotask(() => {
      if (!scroll || scroll.isDestroyed) return
      if (at * 2 < scroll.scrollTop) scroll.scrollTop = at * 2
      else if (at * 2 + 2 > scroll.scrollTop + scroll.viewport.height) scroll.scrollTop = Math.max(0, at * 2 + 2 - scroll.viewport.height)
    })
  })
  const move = (delta: number) => {
    const list = rows()
    const row = list[(index() + delta + list.length) % list.length]
    if (row) store.selectSubagent(row.id)
  }
  const command = (text: string) => { void controller.submit(text, "command") }
  const input: PaneInputHandle = { onKey: (key) => {
    if (key.ctrl || key.meta || key.shift) return
    if (key.name === "up") move(-1)
    else if (key.name === "down") move(1)
    else if (key.name === "return" && rows()[index()]) {
      // Prefer a following viewer; otherwise reuse the first. Commands address any instance.
      const viewer = controllerViewer()
      if (viewer) command(`/subagents pin ${viewer} ${rows()[index()]!.id}`)
      else command(`/subagents view ${rows()[index()]!.id}`)
    } else if (key.name === "n" && rows()[index()]) command(`/subagents view ${rows()[index()]!.id}`)
  } }
  const controllerViewer = () => {
    const viewers = paneLeaves(store.state.paneLayout.root).filter((pane) => pane.kind === "subagent-viewer")
    return (viewers.find((pane) => !pane.session) ?? viewers[0])?.id
  }
  createEffect(() => {
    const id = props.node.id
    ui.paneInputs ??= new Map(); ui.paneInputs.set(id, input)
    onCleanup(() => { if (ui.paneInputs?.get(id) === input) ui.paneInputs.delete(id) })
  })
  return <PaneFrame kind="subagents" focused={props.focused} title="Subagents">
    <scrollbox ref={(element: ScrollBoxRenderable) => { scroll = element; props.scrollRef(element) }} width="100%" flexGrow={1}>
      <Show when={rows().length} fallback={<text fg={colors.muted}>No subagents in this conversation.</text>}>
        <For each={rows()}>{(row, at) => <box height={2} flexShrink={0} flexDirection="column" onMouseDown={(event: MouseEvent) => {
          if (event.button !== 0) return
          event.stopPropagation(); store.setPaneLayout({ ...store.state.paneLayout, active: props.node.id }); store.selectSubagent(row.id)
        }}>
          <text height={1} wrapMode="none" fg={at() === index() ? colors.accent : colors.fg}>{`${at() === index() ? "▸" : " "}${"  ".repeat(row.depth)}${row.label} · ${row.status}`}</text>
          <text height={1} wrapMode="none" fg={colors.muted}>{`  ${row.activity || row.id}`}</text>
        </box>}</For>
      </Show>
    </scrollbox>
    <text height={2} flexShrink={0} wrapMode="word" fg={colors.muted}>↑↓ preview · Enter pin · n new viewer · /subagents follow &lt;pane&gt;</text>
  </PaneFrame>
}

/** Each instance owns its scrollbox; identical child sessions share their live watch. */
export function SubagentViewer(props: PaneRenderProps) {
  const app = useApp()
  const [view, setView] = createSignal<SessionView>()
  const id = () => props.node.session ?? app.store.state.subagentSelection ?? subagentRows(app.store.state)[0]?.id
  const target = createMemo(() => {
    const child = id(), root = app.store.state.selected?.id, server = app.store.state.serverUrl
    if (!child || !root || props.width <= 0 || props.height <= 0 || !subagentRows(app.store.state).some((row) => row.id === child)) return undefined
    return JSON.stringify([server, root, child])
  })
  createEffect(() => {
    const key = target()
    if (!key) { setView(undefined); return }
    const [server, root, child] = JSON.parse(key) as [string, string, string]
    const next = untrack(() => app.controller.sessionViews.acquire(JSON.stringify([server, root]), child))
    setView(next)
    onCleanup(next.release)
  })
  return <PaneFrame kind="subagent-viewer" title={`Subagent · ${props.node.id} · ${props.node.session ? "pinned" : "following"}`}>
    <Show when={view()} keyed fallback={<text fg={colors.muted}>{props.node.session ? "Pinned subagent unavailable in this conversation." : "Select a subagent to preview its transcript."}</text>}>
      {(current) => <AppContext.Provider value={{ ...app, store: current.store, ui: {}, controller: { ...app.controller,
        openSession: async (child: string) => { if (subagentRows(app.store.state).some((row) => row.id === child)) app.store.selectSubagent(child) },
      } }}>
        <text height={1} flexShrink={0} wrapMode="none" fg={colors.muted}>{current.store.state.status || `${current.store.state.selected?.agent || "Loading"} · ${id()}`}</text>
        <Transcript secondary />
      </AppContext.Provider>}
    </Show>
  </PaneFrame>
}
