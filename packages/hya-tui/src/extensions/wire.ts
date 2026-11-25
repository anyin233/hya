/** Host validation and normalization for the bundle TUI extension protocol. */
import {
  MAX_CONTRIBUTIONS, RENDER_LIMITS, REPLACEABLE_SURFACES as replaceableSurfaces,
  type Contributions as WireContributions, type InterceptDecision, type ExtensionContext,
  type TuiExtensionPermission as ExtensionPermission, type JsonValue, type NodeAction,
  type RenderNode, type ReplaceableSurface, type TextStyle,
} from "./wire.generated"

export {
  TUI_EXTENSION_API_VERSION as extensionApiVersion,
  TUI_EXTENSION_SDK_MAJOR as extensionSdkMajor,
  VM_STOPPED_ERROR_CODE as extensionStoppedCode,
  TUI_EXTENSION_PERMISSIONS as extensionPermissions,
  REPLACEABLE_PANES as replaceablePanes,
  REPLACEABLE_SURFACES as replaceableSurfaces,
} from "./wire.generated"
export type { ExtensionPermission }
export type {
  CommandResult, ExtensionAction, ExtensionContext, HostCommand, InterceptDecision, JsonValue,
  KeyEvent, NodeAction, RenderNode, ReplaceablePane, ReplaceableSurface, TextStyle,
} from "./wire.generated"

export type PanelContribution = Omit<WireContributions["panels"][number], "refresh_ms"> & { readonly refreshMs?: number }
export type StatusContribution = WireContributions["status_items"][number]
export type RendererContribution = WireContributions["renderers"][number]
export type FormatterContribution = WireContributions["formatters"][number]
export type InterceptorContribution = WireContributions["interceptors"][number]
export interface Contributions extends Omit<WireContributions, "panels" | "status_items"> {
  readonly panels: readonly PanelContribution[]
  readonly statusItems: readonly StatusContribution[]
}

export const renderLimits = { ...RENDER_LIMITS, maxContributions: MAX_CONTRIBUTIONS } as const

// CSI/OSC/other escape sequences, then remaining C0/C1 controls except tab and newline.
const escapes = /[\u001B\u009B][[\]()#;?]*(?:(?:[a-zA-Z\d]*(?:;[-a-zA-Z\d/#&.:=?%@~_]+)*)?\u0007|(?:(?:\d{1,4}(?:;\d{0,4})*)?[\dA-PR-T-Zcf-nq-uy=><~]))|[\u0000-\u0008\u000B-\u001F\u007F-\u009F]/g
const idPattern = /^[a-z0-9][a-z0-9._-]{0,63}$/
const colorPattern = /^(#[0-9a-fA-F]{6}|fg|focus|heading|link|activity|accent|muted|error|warning|success|border)$/
const encoder = new TextEncoder()

/** Terminal-safe extension text, or `undefined` when it is not a string or too large. */
export function safeText(value: unknown): string | undefined {
  return typeof value === "string" && encoder.encode(value).byteLength <= renderLimits.maxStringBytes ? value.replace(escapes, "") : undefined
}

function isJson(value: unknown, depth = 0): value is JsonValue {
  if (depth > renderLimits.maxDepth) return false
  if (value === null || typeof value === "string" || typeof value === "boolean") return true
  if (typeof value === "number") return Number.isFinite(value)
  if (Array.isArray(value)) return value.every((item) => isJson(item, depth + 1))
  return typeof value === "object" && Object.values(value as object).every((item) => isJson(item, depth + 1))
}

function only(node: object, keys: readonly string[]): boolean {
  return Object.keys(node).every((key) => keys.includes(key))
}

function smallInt(value: unknown): value is number | undefined {
  return value === undefined || (Number.isSafeInteger(value) && (value as number) >= 0 && (value as number) <= 100)
}

export type SlotRule = "forbid" | "require"

/**
 * Validate an untrusted render tree. `slot`: `require` (decorating renderer)
 * demands exactly one `{ kind: "slot" }`, `forbid` none. Actions survive only
 * with `actions` (the `tui.action` permission); otherwise the text stays, inert.
 */
export function validateRenderNode(value: unknown, options: { slot: SlotRule; actions: boolean }): RenderNode | string {
  let nodes = 0
  let slots = 0
  const visit = (raw: unknown, depth: number): RenderNode | string => {
    if (depth > renderLimits.maxDepth) return `render tree deeper than ${renderLimits.maxDepth}`
    if (++nodes > renderLimits.maxNodes) return `render tree larger than ${renderLimits.maxNodes} nodes`
    if (!raw || typeof raw !== "object" || Array.isArray(raw)) return "render node must be an object"
    const node = raw as Record<string, unknown>
    const children = (): RenderNode[] | string => {
      if (!Array.isArray(node.children)) return `${String(node.kind)}.children must be an array`
      const out: RenderNode[] = []
      for (const child of node.children) {
        const checked = visit(child, depth + 1)
        if (typeof checked === "string") return checked
        out.push(checked)
      }
      return out
    }
    switch (node.kind) {
      case "slot":
        if (!only(node, ["kind"])) return "slot takes no fields"
        slots += 1
        return { kind: "slot" }
      case "text": {
        if (!only(node, ["kind", "text", "style", "action"])) return "text: unknown field"
        const text = safeText(node.text)
        if (text === undefined) return "text.text must be a string within limits"
        let style: TextStyle | undefined
        if (node.style !== undefined) {
          const raw = node.style as Record<string, unknown>
          if (!raw || typeof raw !== "object" || !only(raw, ["color", "background", "bold", "italic", "underline"])) return "text.style: unknown field"
          for (const key of ["color", "background"] as const) if (raw[key] !== undefined && (typeof raw[key] !== "string" || !colorPattern.test(raw[key] as string))) return `text.style.${key} must be #rrggbb or a theme token`
          for (const key of ["bold", "italic", "underline"] as const) if (raw[key] !== undefined && typeof raw[key] !== "boolean") return `text.style.${key} must be boolean`
          style = raw as TextStyle
        }
        let action: NodeAction | undefined
        if (node.action !== undefined) {
          const raw = node.action as Record<string, unknown>
          if (!raw || typeof raw !== "object" || !only(raw, ["name", "data"]) || typeof raw.name !== "string" || !idPattern.test(raw.name) || (raw.data !== undefined && !isJson(raw.data))) return "text.action must be { name, data? } with a lowercase name"
          if (options.actions) action = { name: raw.name, data: (raw.data ?? null) as JsonValue }
        }
        return { kind: "text", text, ...(style ? { style } : {}), ...(action ? { action } : {}) }
      }
      case "row": case "column": {
        if (!only(node, ["kind", "children", "gap"]) || !smallInt(node.gap)) return `${node.kind}: invalid field`
        const kids = children()
        return typeof kids === "string" ? kids : { kind: node.kind, children: kids, ...(node.gap === undefined ? {} : { gap: node.gap as number }) }
      }
      case "box": {
        if (!only(node, ["kind", "children", "title", "border", "padding"]) || !smallInt(node.padding) || (node.border !== undefined && typeof node.border !== "boolean")) return "box: invalid field"
        const title = node.title === undefined ? undefined : safeText(node.title)
        if (node.title !== undefined && title === undefined) return "box.title must be a string"
        const kids = children()
        if (typeof kids === "string") return kids
        return { kind: "box", children: kids, ...(title === undefined ? {} : { title }), ...(node.border === undefined ? {} : { border: node.border as boolean }), ...(node.padding === undefined ? {} : { padding: node.padding as number }) }
      }
      case "table": {
        if (!only(node, ["kind", "columns", "rows"]) || !Array.isArray(node.columns) || !Array.isArray(node.rows)) return "table needs columns and rows arrays"
        const columns = node.columns.map(safeText)
        if (columns.some((cell) => cell === undefined)) return "table.columns must be strings"
        const rows: string[][] = []
        for (const row of node.rows) {
          if (!Array.isArray(row) || row.length !== columns.length) return "table rows must match the column count"
          const cells = row.map(safeText)
          if (cells.some((cell) => cell === undefined)) return "table cells must be strings"
          rows.push(cells as string[])
          nodes += 1
          if (nodes > renderLimits.maxNodes) return `render tree larger than ${renderLimits.maxNodes} nodes`
        }
        return { kind: "table", columns: columns as string[], rows }
      }
      case "progress": {
        if (!only(node, ["kind", "value", "total", "label"]) || typeof node.value !== "number" || !Number.isFinite(node.value) || typeof node.total !== "number" || !Number.isFinite(node.total) || node.total < 0) return "progress needs finite value and total >= 0"
        const label = node.label === undefined ? undefined : safeText(node.label)
        if (node.label !== undefined && label === undefined) return "progress.label must be a string"
        return { kind: "progress", value: node.value, total: node.total, ...(label === undefined ? {} : { label }) }
      }
      default: return `unknown render node kind ${JSON.stringify(node.kind)}`
    }
  }
  const root = visit(value, 0)
  if (typeof root === "string") return root
  if (options.slot === "require" && slots !== 1) return "a decorating render tree needs exactly one slot"
  if (options.slot === "forbid" && slots !== 0) return "slot is only allowed in decorating renderers"
  return root
}

/** Plain text of a tree (status items, narrow fallbacks). */
export function nodeText(node: RenderNode): string {
  switch (node.kind) {
    case "text": return node.text
    case "row": return node.children.map(nodeText).join(" ")
    case "column": case "box": return node.children.map(nodeText).join("\n")
    case "table": return [node.columns.join(" | "), ...node.rows.map((row) => row.join(" | "))].join("\n")
    case "progress": return `${node.label ? `${node.label} ` : ""}${node.value}/${node.total}`
    case "slot": return ""
  }
}

export interface CheckedContributions { readonly contributions: Contributions; readonly warnings: string[] }

/**
 * Validate `tui/activate`'s contributions against the declared permissions.
 * Malformed entries fail the whole activation; entries whose permission is
 * missing are dropped with a warning (the extension still runs).
 */
export function validateContributions(value: unknown, permissions: readonly ExtensionPermission[]): CheckedContributions | string {
  if (!value || typeof value !== "object") return "contributions must be an object"
  const raw = value as Record<string, unknown>
  const warnings: string[] = []
  const list = (key: keyof WireContributions): Record<string, unknown>[] | string => {
    const items = raw[key] ?? []
    if (!Array.isArray(items) || items.length > renderLimits.maxContributions || items.some((item) => !item || typeof item !== "object")) return `contributions.${key} must be at most ${renderLimits.maxContributions} objects`
    const ids = new Set<string>()
    for (const item of items as Record<string, unknown>[]) {
      if (typeof item.id !== "string" || !idPattern.test(item.id) || ids.has(item.id)) return `contributions.${key}: invalid or duplicate id ${JSON.stringify(item.id)}`
      ids.add(item.id)
      if (item.priority !== undefined && !Number.isSafeInteger(item.priority)) return `contributions.${key}.${item.id}: priority must be an integer`
    }
    return items as Record<string, unknown>[]
  }
  const gate = <T>(items: T[], permission: ExtensionPermission, what: string): T[] => {
    if (items.length && !permissions.includes(permission)) {
      warnings.push(`${what} ignored: permission ${permission} not declared`)
      return []
    }
    return items
  }
  const panels = list("panels"); if (typeof panels === "string") return panels
  const statuses = list("status_items"); if (typeof statuses === "string") return statuses
  const renderers = list("renderers"); if (typeof renderers === "string") return renderers
  const formatters = list("formatters"); if (typeof formatters === "string") return formatters
  const interceptors = list("interceptors"); if (typeof interceptors === "string") return interceptors
  const checkedPanels: PanelContribution[] = []
  for (const panel of panels) {
    const title = safeText(panel.title)
    if (!title || title.length > 64) return `panel ${String(panel.id)}: title must be 1-64 characters`
    if (panel.placement !== "sidebar" && panel.placement !== "pane") return `panel ${String(panel.id)}: placement must be sidebar or pane`
    if (panel.replaces !== undefined && !replaceableSurfaces.includes(panel.replaces as ReplaceableSurface)) return `panel ${String(panel.id)}: cannot replace ${String(panel.replaces)}`
    if (panel.keys !== undefined && panel.keys !== true) return `panel ${String(panel.id)}: keys must be true`
    if (panel.keys === true && !permissions.includes("tui.keys")) warnings.push(`panel ${String(panel.id)} keys ignored: permission tui.keys not declared`)
    if (panel.refresh_ms !== undefined && (!Number.isSafeInteger(panel.refresh_ms) || (panel.refresh_ms as number) < 1000)) return `panel ${String(panel.id)}: refresh_ms must be >= 1000`
    checkedPanels.push({ id: panel.id as string, title, placement: panel.placement, ...(panel.replaces ? { replaces: panel.replaces as ReplaceableSurface } : {}), ...(panel.refresh_ms ? { refreshMs: panel.refresh_ms as number } : {}), ...(panel.keys === true && permissions.includes("tui.keys") ? { keys: true as const } : {}) })
  }
  const checkedStatuses: StatusContribution[] = []
  for (const item of statuses) {
    const label = safeText(item.label)
    if (!label || label.length > 24) return `status item ${String(item.id)}: label must be 1-24 characters`
    const priority = item.priority ?? 7
    if (typeof priority !== "number" || priority < 1 || priority > 9) return `status item ${String(item.id)}: priority must be 1..9`
    checkedStatuses.push({ id: item.id as string, label, priority })
  }
  const checkedRenderers: RendererContribution[] = []
  for (const renderer of renderers) {
    if (renderer.target !== "tool_call" && renderer.target !== "composer") return `renderer ${String(renderer.id)}: unknown target ${String(renderer.target)}`
    if (renderer.mode !== "replace" && renderer.mode !== "decorate") return `renderer ${String(renderer.id)}: mode must be replace or decorate`
    if (renderer.target === "composer" && renderer.mode === "replace") return `renderer ${String(renderer.id)}: the composer can only be decorated`
    checkedRenderers.push({ id: renderer.id as string, target: renderer.target, mode: renderer.mode, priority: (renderer.priority as number | undefined) ?? 0 })
  }
  for (const interceptor of interceptors) if (interceptor.target !== "submit") return `interceptor ${String(interceptor.id)}: unknown target ${String(interceptor.target)}`
  return {
    contributions: {
      panels: gate(checkedPanels, "tui.panel", "panels"),
      statusItems: gate(checkedStatuses, "tui.status_item", "status items"),
      renderers: gate(checkedRenderers, "tui.render", "renderers"),
      formatters: gate(formatters.map((item) => ({ id: item.id as string, priority: (item.priority as number | undefined) ?? 0 })), "tui.render", "formatters"),
      interceptors: gate(interceptors.map((item) => ({ id: item.id as string, target: "submit" as const, priority: (item.priority as number | undefined) ?? 0 })), "tui.action", "interceptors"),
    },
    warnings,
  }
}

/** Validate a `tui/intercept` result; anything malformed continues unchanged. */
export function validateDecision(value: unknown): InterceptDecision {
  const raw = (value ?? {}) as Record<string, unknown>
  if (raw.decision === "replace") {
    const text = safeText(raw.text)
    if (text !== undefined && text.trim()) return { decision: "replace", text }
  }
  if (raw.decision === "block") return { decision: "block", message: safeText(raw.message)?.slice(0, 200) || "blocked by an extension" }
  return { decision: "continue" }
}

/** The context sections a permission set may read. */
export function scopeContext(context: ExtensionContext, permissions: readonly ExtensionPermission[]): ExtensionContext {
  return {
    terminal: context.terminal,
    ...(context.session && permissions.includes("tui.session.read") ? { session: context.session } : {}),
    ...(context.transcript && permissions.includes("tui.transcript.read") ? { transcript: context.transcript } : {}),
    ...(context.workspace && permissions.includes("tui.workspace.read") ? { workspace: context.workspace } : {}),
    ...(context.git && permissions.includes("workspace.git.read") ? { git: context.git } : {}),
    ...(context.sessions && permissions.includes("tui.sessions.read") ? { sessions: context.sessions } : {}),
    ...(context.projects && permissions.includes("tui.projects.read") ? { projects: context.projects } : {}),
    ...(context.todos && permissions.includes("tui.todos.read") ? { todos: context.todos } : {}),
    ...(context.status && permissions.includes("tui.status.read") ? { status: context.status } : {}),
  }
}
