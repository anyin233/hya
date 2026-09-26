/**
 * One transcript message, from its `MessageView` (state/messages.ts):
 *
 * - user: a panel-colored block with a heavy accent bar on the left; queued
 *   prompts use a muted bar and text and a `queued` tag.
 * - assistant (and other roles): an `● agent · provider/model` header, the
 *   blocks (Markdown text, collapsible reasoning, tool call cards — a `task`
 *   card links to its subagent), and at most one finish notice (error,
 *   cancelled, length limit).
 *
 * Blocks are keyed by part id, so a streaming delta updates the existing
 * Markdown renderable instead of rebuilding it.
 */
import { TextAttributes } from "@opentui/core"
import { createMemo, For, Match, Show, Switch, type JSX } from "solid-js"
import { useApp } from "../app/context"
import { formatBytes } from "../composer/attachments"
import { childStatus, taskLink, type ChildStatus } from "../state/members"
import { waitingKind } from "../state/prompts"
import { reasoningExpanded, reasoningLabel, toolExpanded, type Block, type MessageView } from "../state/messages"
import type { TaskInfo, Tone, ToolStatus } from "../state/tools"
import { colors, diffColors, toolColors } from "../theme"
import { Markdown } from "./Markdown"
import { useSpinner } from "./Spinner"

/** `<For>` over items keyed by id: children stay mounted while their item changes. */
export function KeyedFor<T extends { id: string }>(props: { each: T[]; children: (item: () => T, index: () => number) => JSX.Element }) {
  const ids = createMemo(() => props.each.map((item) => item.id), [], {
    equals: (a, b) => a.length === b.length && a.every((id, index) => id === b[index]),
  })
  const byId = createMemo(() => new Map(props.each.map((item) => [item.id, item])))
  return (
    <For each={ids()}>
      {(id, index) => {
        let last: T | undefined
        const item = () => (last = byId().get(id) ?? last)!
        return props.children(item, index)
      }}
    </For>
  )
}

export function MessageItem(props: { view: MessageView; first: boolean }) {
  return (
    <box width="100%" flexDirection="column" flexShrink={0} marginTop={props.first ? 0 : 1}>
      <Switch>
        <Match when={props.view.role === "user"}>
          <UserMessage view={props.view} />
        </Match>
        <Match when={props.view.role === "system" || props.view.role === "divider"}>
          <NoticeMessage view={props.view} />
        </Match>
        <Match when={props.view.role !== "user" && props.view.role !== "system" && props.view.role !== "divider"}>
          <AssistantMessage view={props.view} />
        </Match>
      </Switch>
    </box>
  )
}

/**
 * An engine system message (e.g. `TEAM QUIESCED …`) or a `CompactionApplied`
 * divider: a muted notice line, not an assistant header block (E24).
 */
function NoticeMessage(props: { view: MessageView }) {
  const text = () => props.view.blocks.map((block) => (block.kind === "text" ? block.text : "")).filter(Boolean).join("\n")
  return <text width="100%" wrapMode="word" fg={props.view.tone === "warning" ? colors.warning : colors.muted}>{text()}</text>
}

/** `↳ attachment · name · mime · 240 KB` (mime/size omitted when unknown): shared by user messages (the prompt's own attachments) and assistant blocks. */
function attachmentText(block: Extract<Block, { kind: "attachment" }>): string {
  const bytes = block.size !== undefined ? Number(block.size) : undefined
  const detail = [block.mime, bytes !== undefined && Number.isFinite(bytes) ? formatBytes(bytes) : undefined].filter(Boolean).join(" · ")
  return `↳ attachment · ${block.name}${detail ? ` · ${detail}` : ""}`
}

function UserMessage(props: { view: MessageView }) {
  const text = () => props.view.blocks.map((block) => (block.kind === "text" ? block.text : "")).filter(Boolean).join("\n")
  const attachments = () => props.view.blocks.filter((block): block is Extract<Block, { kind: "attachment" }> => block.kind === "attachment")
  return (
    <box
      width="100%"
      flexDirection="column"
      border={["left"]}
      borderStyle="heavy"
      borderColor={props.view.queued ? colors.muted : colors.accent}
      backgroundColor={colors.panel}
      paddingLeft={1}
      paddingRight={1}
    >
      <box width="100%" flexDirection="row">
        <text flexGrow={1} wrapMode="word" fg={props.view.queued ? colors.muted : colors.fg}>{text()}</text>
        <Show when={props.view.queued}>
          <text flexShrink={0} fg={colors.muted}> queued</text>
        </Show>
      </box>
      <For each={attachments()}>
        {(block) => <text wrapMode="word" fg={colors.muted}>{attachmentText(block)}</text>}
      </For>
    </box>
  )
}

function AssistantMessage(props: { view: MessageView }) {
  const name = () => props.view.role === "assistant" ? props.view.agent || "assistant" : props.view.role
  const model = () => props.view.role === "assistant" ? props.view.model : ""
  // E21: while the message streams with no body content yet (the working
  // indicator line has not shown any of it as a card or text yet), the
  // header's marker spins in place of the static bullet.
  const waitingForBody = () => props.view.streaming && props.view.blocks.length === 0
  return (
    <box width="100%" flexDirection="column">
      <text height={1} wrapMode="none">
        <Show when={waitingForBody()} fallback={<span style={{ fg: colors.accent }}>● </span>}>
          <RunningIcon />
          <span style={{ fg: colors.accent }}> </span>
        </Show>
        <b style={{ fg: colors.accent }}>{name()}</b>
        <span style={{ fg: colors.muted }}>{model() ? ` · ${model()}` : ""}</span>
      </text>
      <KeyedFor each={props.view.blocks}>
        {(block, index) => (
          <box width="100%" flexDirection="column" marginTop={cardGap(props.view.blocks[index() - 1], block()) ? 1 : 0}>
            <BlockView block={block()} streaming={props.view.streaming} />
          </box>
        )}
      </KeyedFor>
      <Show when={props.view.notice}>
        {(notice) => (
          <text wrapMode="word" fg={notice().kind === "error" ? colors.error : colors.warning}>{notice().text}</text>
        )}
      </Show>
    </box>
  )
}

/** A blank row between a run of tool cards and the text before or after it. */
function cardGap(previous: Block | undefined, block: Block): boolean {
  if (!previous) return false
  return (previous.kind === "tool") !== (block.kind === "tool") && (previous.kind === "text" || block.kind === "text")
}

function BlockView(props: { block: Block; streaming: boolean }) {
  return (
    <Switch>
      <Match when={props.block.kind === "text" && props.block}>
        {(block) => <Markdown text={block().text} streaming={props.streaming} />}
      </Match>
      <Match when={props.block.kind === "reasoning" && props.block}>
        {(block) => <Reasoning block={block()} />}
      </Match>
      <Match when={props.block.kind === "tool" && props.block}>
        {(block) => <ToolCard block={block()} />}
      </Match>
      <Match when={props.block.kind === "attachment" && props.block}>
        {(block) => <text fg={colors.muted}>{attachmentText(block())}</text>}
      </Match>
    </Switch>
  )
}

/** A reasoning part: one muted `▸ Thinking` line; expanded, the text in muted italics beside a bar. */
function Reasoning(props: { block: Extract<Block, { kind: "reasoning" }> }) {
  const { store } = useApp()
  const expanded = () => reasoningExpanded(store.state, props.block.id)
  return (
    <box width="100%" flexDirection="column" marginBottom={expanded() ? 1 : 0} onMouseDown={() => store.toggleReasoning(props.block.id)}>
      <text height={1} wrapMode="none" fg={colors.muted}>{reasoningLabel(props.block, expanded())}</text>
      <Show when={expanded()}>
        <box width="100%" border={["left"]} borderColor={colors.border} paddingLeft={1}>
          <text wrapMode="word" fg={colors.muted} attributes={TextAttributes.ITALIC}>{props.block.text}</text>
        </box>
      </Show>
    </box>
  )
}

/** Color of a body line's tone. */
export function toneColor(tone: Tone): string {
  switch (tone) {
    case "add": return diffColors.add
    case "remove": return diffColors.remove
    case "hunk": return diffColors.hunk
    case "error": return colors.error
    case "fg": return colors.fg
    default: return colors.muted
  }
}

/** The state icon of a card: a spinner while running, ✓ done, ✗ failed, ○ pending, ◌ waiting for a permission answer. */
function StatusIcon(props: { status: ToolStatus | "waiting" }) {
  return (
    <Switch>
      <Match when={props.status === "running"}>
        <RunningIcon />
      </Match>
      <Match when={props.status !== "running"}>
        <span style={{ fg: iconColor(props.status) }}>{iconGlyph(props.status)}</span>
      </Match>
    </Switch>
  )
}

function RunningIcon() {
  const frame = useSpinner()
  return <span style={{ fg: colors.accent }}>{frame()}</span>
}

function iconGlyph(status: ToolStatus | "waiting"): string {
  return status === "done" ? "✓" : status === "failed" ? "✗" : status === "waiting" ? "◌" : "○"
}

function iconColor(status: ToolStatus | "waiting"): string {
  return status === "done" ? toolColors.done : status === "failed" ? colors.error : status === "waiting" ? colors.warning : colors.muted
}

/**
 * A tool call card: one header line (state icon, tool name, muted summary,
 * duration on the right), the error line of a failed call, and, when
 * expanded, the body lines beside a bar. A click on the card toggles it; a
 * `task` card opens its child session instead (see `TaskCard`).
 */
function ToolCard(props: { block: Extract<Block, { kind: "tool" }> }) {
  const { store } = useApp()
  const card = () => props.block.card
  const expanded = () => toolExpanded(store.state, props.block)
  const waiting = () => card().status !== "done" && card().status !== "failed" && props.block.callId !== undefined
    && store.state.interactions.some((item) => item.payload?.callId === props.block.callId)
  return (
    <Switch>
      <Match when={card().task}>
        {(task) => <TaskCard block={props.block} task={task()} />}
      </Match>
      <Match when={!card().task}>
        <box width="100%" flexDirection="column" onMouseDown={() => store.toggleTool(props.block.id, expanded())}>
          <CardHeader status={waiting() ? "waiting" : card().status} tool={card().tool} summary={`${card().summary}${waiting() ? " · awaiting approval" : ""}`} duration={card().duration} />
          <Show when={card().error}>
            <box width="100%" paddingLeft={2}>
              <text width="100%" wrapMode="word" fg={colors.error}>{card().error}</text>
            </box>
          </Show>
          <Show when={expanded() && card().body.length > 0}>
            <box width="100%" flexDirection="column" border={["left"]} borderColor={colors.border} paddingLeft={1}>
              <For each={card().body}>
                {(line) => <text width="100%" height={1} wrapMode="none" fg={toneColor(line.tone)}>{line.text || " "}</text>}
              </For>
            </box>
          </Show>
        </box>
      </Match>
    </Switch>
  )
}

function CardHeader(props: { status: ToolStatus | "waiting"; tool: string; summary: string; duration?: string | undefined }) {
  return (
    <box width="100%" height={1} flexDirection="row">
      <text flexShrink={0} height={1} wrapMode="none">
        <StatusIcon status={props.status} />
        <span style={{ fg: colors.fg }}> </span>
        <b style={{ fg: colors.fg }}>{props.tool}</b>
        <span style={{ fg: colors.fg }}>{"  "}</span>
      </text>
      <text flexGrow={1} flexShrink={1} height={1} wrapMode="none" fg={colors.muted}>{props.summary}</text>
      <text flexShrink={0} height={1} wrapMode="none" fg={colors.muted}>{props.duration ? ` ${props.duration}` : ""}</text>
    </box>
  )
}

const childLabels: Record<ChildStatus, string> = {
  starting: "starting", running: "running", idle: "idle", done: "done", failed: "failed", cancelled: "cancelled",
}

/**
 * A `task` card: the header (`task  agent · description`) and, always shown,
 * the child's status with its latest activity (or the member's finish
 * summary) and how to open it. A click opens the child session read-only.
 */
function TaskCard(props: { block: Extract<Block, { kind: "tool" }>; task: TaskInfo }) {
  const { store, controller } = useApp()
  const link = () => taskLink({ ...(props.block.callId ? { callId: props.block.callId } : {}), ...(props.task.child ? { child: props.task.child } : {}) }, store.state.members)
  const child = () => {
    const id = link().child
    return id ? store.state.children.get(id) : undefined
  }
  const status = () => childStatus(link().member, child())
  /** The child waits for the user (a pending ask of its session): shown instead of `running`. */
  const waiting = () => {
    const id = link().child
    return id ? waitingKind(store.state.interactions, id) : undefined
  }
  const detail = () => link().member?.summary || child()?.activity
  const open = () => {
    const id = link().child
    if (id) void controller.openSession(id).catch((error: unknown) => store.setStatus(`Open failed: ${String(error)}`))
  }
  const statusColor = () => {
    switch (status()) {
      case "running": return colors.accent
      case "idle": case "done": return toolColors.done
      case "failed": return colors.error
      case "cancelled": return colors.warning
      default: return colors.muted
    }
  }
  return (
    <box width="100%" flexDirection="column" onMouseDown={open}>
      <CardHeader status={props.block.card.status} tool={props.block.card.tool} summary={props.block.card.summary} duration={props.block.card.duration} />
      <Show when={props.block.card.error}>
        <box width="100%" paddingLeft={2}>
              <text width="100%" wrapMode="word" fg={colors.error}>{props.block.card.error}</text>
            </box>
      </Show>
      <Show when={props.block.card.status !== "failed"}>
        <box width="100%" flexDirection="column" border={["left"]} borderColor={colors.border} paddingLeft={1}>
          <text width="100%" height={1} wrapMode="none">
            <Show when={!waiting()} fallback={<span style={{ fg: colors.warning }}>◌</span>}>
              <Show when={status() === "running"} fallback={<span style={{ fg: statusColor() }}>{status() === "failed" ? "✗" : status() === "cancelled" ? "!" : status() === "starting" ? "○" : "✓"}</span>}>
                <RunningIcon />
              </Show>
            </Show>
            <span style={{ fg: waiting() ? colors.warning : statusColor() }}>{waiting() ? ` waiting for ${waiting() === "approval" ? "approval" : "an answer"}` : ` ${childLabels[status()]}`}</span>
            <span style={{ fg: colors.muted }}>{detail() ? `  ↳ ${detail()}` : ""}</span>
          </text>
          <Show when={link().child}>
            {(id) => <text width="100%" height={1} wrapMode="none" fg={colors.muted}>{`click to view · /open ${id()}`}</text>}
          </Show>
        </box>
      </Show>
    </box>
  )
}
