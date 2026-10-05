import { TextAttributes } from "@opentui/core"
import { For, type JSX } from "solid-js"
import { colors, toolColors } from "../theme"
import { extensionManager, type HookTree, type HookTrees } from "./manager"
import type { ExtensionAction, RenderNode } from "./wire"

/** How a validated tree meets the host: what fills its slot, what a click does. */
export interface TreeHost {
  readonly slot?: () => JSX.Element
  readonly onAction?: (action: ExtensionAction, point: { readonly x: number; readonly y: number }) => void
}

const themed = (color: string | undefined): string | undefined => {
  if (color === undefined || color.startsWith("#")) return color
  if (color === "success") return toolColors.done
  if (color === "border") return colors.border
  return colors[color as "fg" | "accent" | "muted" | "error" | "warning"]
}

/**
 * Render an already validated tree (extensions/wire.ts `validateRenderNode`)
 * with OpenTUI primitives. Text wraps at words, except the segments of a
 * `row`, which is one line: its segments keep their width and the line is
 * clipped at the edge.
 */
export function RenderTree(props: { node: RenderNode; host?: TreeHost; inRow?: boolean }): JSX.Element {
  const node = props.node
  const child = (item: RenderNode) => <RenderTree node={item} {...(props.host ? { host: props.host } : {})} inRow={node.kind === "row"} />
  switch (node.kind) {
    case "slot": return props.host?.slot?.() ?? null
    case "text": {
      const action = node.action
      const onAction = props.host?.onAction
      // An unstyled clickable text is marked as a link; a styled one is drawn exactly as styled.
      const link = action !== undefined && node.style === undefined
      return (
        <text
          {...(props.inRow ? { wrapMode: "none" as const, flexShrink: 0 } : { wrapMode: "word" as const })}
          fg={themed(node.style?.color) ?? (link ? colors.accent : colors.fg)}
          {...(node.style?.background ? { bg: themed(node.style.background) } : {})}
          attributes={(node.style?.bold ? TextAttributes.BOLD : 0) | (node.style?.italic ? TextAttributes.ITALIC : 0) | (node.style?.underline || link ? TextAttributes.UNDERLINE : 0)}
          {...(action && onAction ? { onMouseDown: (event: { button: number; x: number; y: number; stopPropagation(): void }) => { if (event.button === 0 || event.button === 2) { event.stopPropagation(); onAction({ ...action, button: event.button === 2 ? "right" : "left" }, { x: event.x, y: event.y }) } } } : {})}
        >{node.text}</text>
      )
    }
    case "row": case "column":
      return <box flexDirection={node.kind} gap={node.gap ?? 0} width="100%"><For each={node.children}>{child}</For></box>
    case "box":
      return (
        <box flexDirection="column" width="100%" border={node.border ?? false} borderColor={colors.border} padding={node.padding ?? 0} {...(node.title ? { title: node.title } : {})}>
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
