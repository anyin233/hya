import type { ScrollBoxRenderable } from "@opentui/core"
import { createEffect, Match, onCleanup, Show, Switch } from "solid-js"
import { useApp } from "../app/context"
import { contextText, mainContent, modelReference, sessionListText, shownServer, todosText } from "../state/format"
import type { AppState } from "../state/store"
import type { PaneKind, PaneLeaf, PaneNode, PaneSplit } from "../state/panes"
import { pageStep } from "../state/scroll"
import { colors } from "../theme"
import { Transcript } from "./Transcript"

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
    case "sessions": return sessionListText(state)
    case "todos": return todosText(state.todos)
    case "context": return contextText(state, shownServer(state, server), 54)
    case "status": return [
      `Server      ${shownServer(state, server)}`,
      `Version     ${state.serverVersion || "unknown"}`,
      `Connection  ${state.connected ? "connected" : "disconnected"}`,
      `Session     ${state.selected?.title || state.selected?.id || "none"}`,
      `Agent       ${state.selected?.agent || "none"}`,
      `Model       ${state.selected ? modelReference(state.selected) || "default" : "none"}`,
      `Mode        ${state.selected?.permissionMode || "manual"}`,
      `Directory   ${state.selected?.workdir || "none"}`,
    ].join("\n")
    case "models": case "workflows": case "interactions": case "api": return mainContent(state, kind)
    case "conversation": return ""
  }
}

function PaneLeafView(props: { node: PaneLeaf }) {
  const { store, server, ui } = useApp()
  let scroll: ScrollBoxRenderable | undefined
  const scroller = {
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
  const title = () => `${active() ? "▸ " : ""}${props.node.kind} · ${props.node.id}`
  return (
    <box
      width="100%" height="100%" flexGrow={1} flexBasis={0} flexDirection="column"
      border borderColor={active() ? colors.accent : colors.border}
      title={title()} backgroundColor={colors.bg}
      onMouseDown={() => store.setPaneLayout({ ...store.state.paneLayout, active: props.node.id })}
    >
      <Show when={props.node.kind === "conversation"} fallback={
        <scrollbox ref={(element: ScrollBoxRenderable) => (scroll = element)} width="100%" flexGrow={1} paddingX={1} paddingY={1}>
          <text width="100%" wrapMode="word" fg={colors.fg}>{paneText(store.state, props.node.kind, server)}</text>
        </scrollbox>
      }>
        <Transcript />
      </Show>
    </box>
  )
}

function PaneNodeView(props: { node: PaneNode }) {
  return (
    <Switch>
      <Match when={props.node.type === "pane"}>
        <PaneLeafView node={props.node as PaneLeaf} />
      </Match>
      <Match when={props.node.type === "split"}>
        <box width="100%" height="100%" flexGrow={1} flexBasis={0} flexDirection={(props.node as PaneSplit).axis === "vertical" ? "row" : "column"}>
          <box flexGrow={(props.node as PaneSplit).weight} flexShrink={1} flexBasis={0}>
            <PaneNodeView node={(props.node as PaneSplit).first} />
          </box>
          <box flexGrow={1 - (props.node as PaneSplit).weight} flexShrink={1} flexBasis={0}>
            <PaneNodeView node={(props.node as PaneSplit).second} />
          </box>
        </box>
      </Match>
    </Switch>
  )
}

/** Recursively tile the central workspace; global prompts and the composer stay docked below it. */
export function PaneWorkspace() {
  const { store } = useApp()
  return <PaneNodeView node={store.state.paneLayout.root} />
}
