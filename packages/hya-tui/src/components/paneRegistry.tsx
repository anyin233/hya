import type { ScrollBoxRenderable } from "@opentui/core"
import { Show, type Component } from "solid-js"
import type { DiffScroller, PaneInputHandle } from "../app/context"
import { resolveBinding } from "../keys/bindings"
import { useApp } from "../app/context"
import { mainContent, modelReference, shownServer } from "../state/format"
import type { AppState } from "../state/store"
import { paneDefinitions, paneKinds, type PaneDefinition, type PaneKind, type PaneLeaf } from "../state/panes"
import { PaneFrame } from "./PaneFrame"
import { colors } from "../theme"
import { ConversationPane } from "./ConversationPane"
import { MessagePane } from "./MessagePane"
import { WorkingIndicator } from "./WorkingIndicator"
import { ExtensionPanel } from "../extensions/Host"
import { extensionManager } from "../extensions/manager"
import { LayoutPane } from "./LayoutPane"
import { tuiVersion } from "../version"

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
  if (state.interactions.length) lines.push(`${state.interactions.length} pending request${state.interactions.length === 1 ? "" : "s"} · /pending to review`)
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
    case "composer": case "activity": case "projects": case "conversation": case "sessions": case "todos": case "context": case "layout": case "extension": return ""
  }
}

function TextPane(props: PaneRenderProps) {
  const { store, server } = useApp()
  return <PaneFrame kind={props.node.kind} focused={props.focused}
    title={`${props.focused ? "▸ " : ""}${props.node.kind} · ${props.node.id}`} background={colors.bg}>
    <scrollbox ref={props.scrollRef} width="100%" flexGrow={1} paddingY={1}>
      <Show when={extensionManager.replacement(props.node.kind as "jobs" | "status" | "models" | "workflows" | "interactions" | "api")?.key} keyed
        fallback={<text width="100%" wrapMode="word" fg={colors.fg}>{paneText(store.state, props.node.kind, server)}</text>}>
        {(key) => <ExtensionPanel panelKey={key} width={props.width - 4} height={props.height - 2} paddingX={0} />}
      </Show>
    </scrollbox>
  </PaneFrame>
}
/** Built-ins and custom panels share the layout's border and keyboard ownership. */
function BundledPane(props: PaneRenderProps) {
  const panel = () => props.node.kind === "extension" ? props.node.panel : extensionManager.replacement(props.node.kind as "projects" | "sessions" | "todos" | "context")?.key
  const title = () => extensionManager.panels().find((item) => item.key === panel())?.title ?? paneDefinitions[props.node.kind].title
  return <PaneFrame kind={props.node.kind} focused={props.focused} title={title()}>
    <Show when={panel()} keyed fallback={<text wrapMode="word" fg={colors.muted}>{extensionManager.placeholder(props.node.kind as "projects" | "sessions" | "todos" | "context") ?? "Loading…"}</text>}>
      {(key) => <ExtensionPanel panelKey={key} width={props.width - (paneDefinitions[props.node.kind].selectable ? 4 : 2)}
        height={props.height - (paneDefinitions[props.node.kind].selectable ? 2 : 1)} paddingX={0} scrollRef={props.scrollRef} />}
    </Show>
  </PaneFrame>
}
function bundledInput(context: PaneInputContext): PaneInputHandle {
  const fallback = scrollInput(context)
  return { onKey: (key) => {
    const panel = context.node.kind === "extension" ? extensionManager.panels().find((entry) => entry.key === context.node.panel)
      : extensionManager.replacement(context.node.kind as "projects" | "sessions")
    if (panel?.keys) void extensionManager.key(panel.key, key)
    else if (context.node.kind !== "projects") fallback.onKey(key)
  } }
}
const renderers: Record<PaneKind, Component<PaneRenderProps>> = {
  conversation: () => <ConversationPane />,
  composer: (props) => <MessagePane width={props.width} />,
  activity: () => <WorkingIndicator />,
  projects: BundledPane, sessions: BundledPane, todos: BundledPane, context: BundledPane, extension: BundledPane,
  jobs: TextPane, status: TextPane, models: TextPane, workflows: TextPane, interactions: TextPane, api: TextPane,
  layout: LayoutPane,
}
/** Every pane uses the same registration and input ownership contract. */
export const paneRegistry: Record<PaneKind, RegisteredPane> = Object.fromEntries(
  paneKinds.map((kind) => [kind, {
    ...paneDefinitions[kind], render: renderers[kind],
    input: !paneDefinitions[kind].selectable || kind === "composer" || kind === "layout" ? undefined
      : bundledInput,
  }]),
) as Record<PaneKind, RegisteredPane>
