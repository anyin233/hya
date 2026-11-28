import { TextAttributes } from "@opentui/core"
import { For, type JSX } from "solid-js"
import { colors } from "../theme"
import { extensionManager, type HookTree, type HookTrees } from "./manager"
import { isTranscriptNode, type ExtensionAction, type RenderNode, type TranscriptRenderNode } from "./wire"

/** How a validated tree meets the host: what fills its slot, what a click does, and how host-virtualized transcripts render. */
export interface TreeHost {
  readonly slot?: () => JSX.Element
  readonly onAction?: (action: ExtensionAction, point: { readonly x: number; readonly y: number }) => void
  /** Host-virtualized streaming transcript renderer for `{ type: "transcript", sessionId }` nodes. */
  readonly transcript?: (node: TranscriptRenderNode) => JSX.Element
}

/** True when `node` or any descendant is a host-virtualized transcript node (which owns its own scrollbox). */
export function containsTranscript(node: RenderNode | undefined): boolean {
  if (!node) return false
  if (isTranscriptNode(node)) return true
  if ("children" in node && Array.isArray(node.children)) {
    return node.children.some((child) => containsTranscript(child))
  }
  return false
}

const themed = (color: string | undefined): string | undefined => {
  if (color === undefined || color.startsWith("#")) return color
  if (color === "accent") return colors.focus // Legacy extension spelling.
  return colors[color as "fg" | "focus" | "heading" | "link" | "activity" | "muted" | "error" | "warning" | "success" | "border"]
}

/**
 * Render an already validated tree (extensions/wire.ts `validateRenderNode`)
 * with OpenTUI primitives. Text wraps at words, except the segments of a
 * `row`, which is one line: its segments keep their width and the line is
 * clipped at the edge.
 */
export function RenderTree(props: { node: RenderNode; host?: TreeHost; inRow?: boolean }): JSX.Element {
  const node = props.node
  if (isTranscriptNode(node)) {
    if (props.host?.transcript) return props.host.transcript(node)
    const mode = node.follow === false
      ? `scroll:${node.scrollOffset ?? 0}`
      : "follow"
    return (
      <box flexDirection="column" width="100%">
        <text wrapMode="none" fg={colors.muted}>{`[transcript: ${node.sessionId} · ${mode}${node.compact ? " · compact" : ""}]`}</text>
      </box>
    )
  }
  const child = (item: RenderNode) => <RenderTree node={item} {...(props.host ? { host: props.host } : {})} inRow={node.kind === "row"} />
  switch (node.kind) {
    case "slot": return props.host?.slot?.() ?? null
    case "text": {
      const action = node.action
      const onAction = props.host?.onAction
      // An unstyled clickable text is marked as a link; a styled one is drawn exactly as styled.
      const link = (action !== undefined && node.style === undefined) || node.style?.color === "link"
      return (
        <text
          {...(props.inRow ? { wrapMode: "none" as const, flexShrink: 0 } : { wrapMode: "word" as const })}
          fg={themed(node.style?.color) ?? (link ? colors.link : colors.fg)}
          {...(node.style?.background ? { bg: themed(node.style.background) } : {})}
          attributes={(node.style?.bold ? TextAttributes.BOLD : 0) | (node.style?.italic ? TextAttributes.ITALIC : 0) | (node.style?.underline || link ? TextAttributes.UNDERLINE : 0)}
          {...(action && onAction ? { onMouseDown: (event: { button: number; x: number; y: number; stopPropagation(): void }) => { if (event.button === 0 || event.button === 2) { event.stopPropagation(); onAction({ ...action, data: action.data ?? null, button: event.button === 2 ? "right" : "left" }, { x: event.x, y: event.y }) } } } : {})}
        >{node.text}</text>
      )
    }
    case "row":
      return <box flexDirection="row" gap={node.gap ?? 0} width="100%" flexShrink={0}><For each={node.children}>{child}</For></box>
    case "column":
      return <box flexDirection="column" gap={node.gap ?? 0} width="100%" {...(containsTranscript(node) ? { flexGrow: 1, height: "100%" } : {})}><For each={node.children}>{child}</For></box>
    case "box":
      return (
        <box flexDirection="column" width="100%" padding={node.padding ?? 0}>
          {node.title ? <text height={1} flexShrink={0} wrapMode="none" fg={colors.heading}>{node.title}</text> : null}
          <For each={node.children}>{child}</For>
        </box>
      )
    case "table": {
      const widths = node.columns.map((column, index) => Math.max(column.length, ...node.rows.map((row) => row[index]!.length)))
      const line = (cells: readonly string[]) => cells.map((cell, index) => cell.padEnd(widths[index]!)).join("  ").trimEnd()
      return (
        <box flexDirection="column" width="100%">
          <text wrapMode="none" fg={colors.muted} attributes={TextAttributes.BOLD}>{line(node.columns)}</text>
          <For each={node.rows}>{(row) => <text wrapMode="none" fg={colors.fg}>{line(row)}</text>}</For>
        </box>
      )
    }
    case "progress": {
      const share = node.total > 0 ? Math.max(0, Math.min(1, node.value / node.total)) : 0
      const filled = Math.round(share * 20)
      return <text wrapMode="none" fg={colors.fg}>{`${node.label ? `${node.label} ` : ""}${"█".repeat(filled)}${"░".repeat(20 - filled)} ${Math.round(share * 100)}%`}</text>
    }
  }
}

/**
 * The built-in rendering `base` under extension renderers: a replacement tree
 * (if any) instead of it, then each decorator around what is inside it.
 * `base` is called at most once, so a stateful built-in keeps its renderables.
 */
export function hooked(trees: HookTrees, base: () => JSX.Element): JSX.Element {
  const tree = (hook: HookTree, slot?: () => JSX.Element) => (
    <RenderTree node={hook.node} host={{ ...(slot ? { slot } : {}), onAction: (action, point) => void extensionManager.action(hook.extension, { kind: "renderer", id: hook.renderer }, action, point) }} />
  )
  let content = trees.replace ? () => tree(trees.replace!) : base
  for (const decorator of trees.decorators) {
    const slot = content
    content = () => tree(decorator, slot)
  }
  return content()
}
