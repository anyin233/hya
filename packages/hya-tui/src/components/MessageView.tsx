/**
 * One transcript message, from its `MessageView` (state/messages.ts):
 *
 * - user: a panel-colored block with a heavy accent bar on the left; queued
 *   prompts use a muted bar and text and a `queued` tag.
 * - assistant (and other roles): an `● agent · provider/model` header, the
 *   blocks (Markdown text, collapsible reasoning, one-line tool calls with a
 *   shell command and its output indented below when known), and at most one
 *   finish notice (error, cancelled, length limit).
 *
 * Blocks are keyed by part id, so a streaming delta updates the existing
 * Markdown renderable instead of rebuilding it.
 */
import { TextAttributes } from "@opentui/core"
import { createMemo, For, Match, Show, Switch, type JSX } from "solid-js"
import { useApp } from "../app/context"
import { reasoningExpanded, reasoningLabel, type Block, type MessageView } from "../state/messages"
import { colors } from "../theme"
import { Markdown } from "./Markdown"

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
        <Match when={props.view.role !== "user"}>
          <AssistantMessage view={props.view} />
        </Match>
      </Switch>
    </box>
  )
}

function UserMessage(props: { view: MessageView }) {
  const text = () => props.view.blocks.map((block) => (block.kind === "text" ? block.text : "")).filter(Boolean).join("\n")
  return (
    <box
      width="100%"
      flexDirection="row"
      border={["left"]}
      borderStyle="heavy"
      borderColor={props.view.queued ? colors.muted : colors.accent}
      backgroundColor={colors.panel}
      paddingLeft={1}
      paddingRight={1}
    >
      <text flexGrow={1} wrapMode="word" fg={props.view.queued ? colors.muted : colors.fg}>{text()}</text>
      <Show when={props.view.queued}>
        <text flexShrink={0} fg={colors.muted}> queued</text>
      </Show>
    </box>
  )
}

function AssistantMessage(props: { view: MessageView }) {
  const name = () => props.view.role === "assistant" ? props.view.agent || "assistant" : props.view.role
  const model = () => props.view.role === "assistant" ? props.view.model : ""
  return (
    <box width="100%" flexDirection="column">
      <text height={1} wrapMode="none">
        <span style={{ fg: colors.accent }}>● </span>
        <b style={{ fg: colors.accent }}>{name()}</b>
        <span style={{ fg: colors.muted }}>{model() ? ` · ${model()}` : ""}</span>
      </text>
      <KeyedFor each={props.view.blocks}>
        {(block) => <BlockView block={block()} streaming={props.view.streaming} />}
      </KeyedFor>
      <Show when={props.view.notice}>
        {(notice) => (
          <text wrapMode="word" fg={notice().kind === "error" ? colors.error : colors.warning}>{notice().text}</text>
        )}
      </Show>
    </box>
  )
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
        {(block) => <ToolLine block={block()} />}
      </Match>
      <Match when={props.block.kind === "attachment" && props.block}>
        {(block) => <text fg={colors.muted}>{`↳ attachment · ${block().name}`}</text>}
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

/** `↳ tool · state`; a shell command (`$ command`) and its output text indented below when known. */
function ToolLine(props: { block: Extract<Block, { kind: "tool" }> }) {
  const block = () => props.block
  return (
    <box width="100%" flexDirection="column">
      <text wrapMode="word" fg={block().state === "error" ? colors.error : colors.muted}>
        {`↳ ${block().tool} · ${block().state || "pending"}${block().error ? `: ${block().error}` : ""}`}
      </text>
      <Show when={block().command !== undefined || block().output !== undefined}>
        <box width="100%" flexDirection="column" paddingLeft={2}>
          <Show when={block().command !== undefined}>
            <text wrapMode="word" fg={colors.fg}>{`$ ${block().command}`}</text>
          </Show>
          <Show when={block().output !== undefined}>
            <text wrapMode="word" fg={colors.muted}>{block().output}</text>
          </Show>
        </box>
      </Show>
    </box>
  )
}
