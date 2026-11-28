/** Host validation and normalization for the bundle TUI extension protocol. */
import {
  BUNDLE_API_METHODS, MAX_CONTRIBUTIONS, PANE_SLOTS, RENDER_LIMITS,
  REPLACEABLE_SURFACES as replaceableSurfaces,
  buildBundleApiUrlPath, checkBundleConfinement, isTranscriptNode, isValidSessionId, validateTranscriptNode,
  type BundleApiCommand, type BundleApiMethod, type BundleScopedCapability,
  type Contributions as WireContributions, type ExtensionContext, type InterceptDecision,
  type JsonValue, type NodeAction, type PaneSize, type PaneSlot, type RenderNode,
  type ReplaceableSurface, type TextStyle, type TranscriptRenderNode,
  type TuiExtensionPermission as ExtensionPermission,
} from "./wire.generated"

export {
  BUNDLE_API_METHODS as bundleApiMethods,
  PANE_SLOTS as paneSlots,
  TUI_EXTENSION_API_VERSION as extensionApiVersion,
  TUI_EXTENSION_SDK_MAJOR as extensionSdkMajor,
  VM_STOPPED_ERROR_CODE as extensionStoppedCode,
  TUI_EXTENSION_PERMISSIONS as extensionPermissions,
  REPLACEABLE_PANES as replaceablePanes,
  REPLACEABLE_SURFACES as replaceableSurfaces,
  buildBundleApiUrlPath,
  checkBundleConfinement,
  isTranscriptNode,
  isValidSessionId,
  validateBundleApiPath,
  validateTranscriptNode,
} from "./wire.generated"
export type { ExtensionPermission }
export type {
  BundleApiCommand, BundleApiMethod, BundleScopedCapability, CommandResult, ExtensionAction,
  ExtensionContext, HostCommand, InterceptDecision, JsonValue, KeyEvent, NodeAction,
  PaneDescriptor, PaneHostCommand, PaneSize, PaneSlot, RenderNode, ReplaceablePane,
  ReplaceableSurface, SlashCommandDescriptor, TextStyle, TranscriptNodeProps, TranscriptRenderNode,
} from "./wire.generated"

export type PanelContribution = Omit<WireContributions["panels"][number], "refresh_ms" | "default_slot" | "default_size"> & {
  readonly defaultSlot?: PaneSlot
  readonly defaultSize?: PaneSize
  readonly refreshMs?: number
}
export type PaneContribution = Omit<WireContributions["panes"][number], "refresh_ms" | "default_slot" | "default_size"> & {
  readonly defaultSlot?: PaneSlot
  readonly defaultSize?: PaneSize
  readonly refreshMs?: number
}
export type SlashCommandContribution = Omit<WireContributions["commands"][number], "argument_hint"> & {
  readonly argumentHint?: string
}
export type StatusContribution = WireContributions["status_items"][number]
export type RendererContribution = WireContributions["renderers"][number]
export type FormatterContribution = WireContributions["formatters"][number]
export type InterceptorContribution = WireContributions["interceptors"][number]
export interface Contributions extends Omit<WireContributions, "panels" | "panes" | "commands" | "status_items"> {
  readonly panels: readonly PanelContribution[]
  readonly panes: readonly PaneContribution[]
  readonly commands: readonly SlashCommandContribution[]
  readonly statusItems: readonly StatusContribution[]
}

export const renderLimits = { ...RENDER_LIMITS, maxContributions: MAX_CONTRIBUTIONS } as const

// CSI/OSC/other escape sequences, then remaining C0/C1 controls except tab and newline.
const escapes = /[\u001B\u009B][[\]()#;?]*(?:(?:[a-zA-Z\d]*(?:;[-a-zA-Z\d/#&.:=?%@~_]+)*)?\u0007|(?:(?:\d{1,4}(?:;\d{0,4})*)?[\dA-PR-T-Zcf-nq-uy=><~]))|[\u0000-\u0008\u000B-\u001F\u007F-\u009F]/g
const idPattern = /^[a-z0-9][a-z0-9._-]{0,63}$/
const commandNamePattern = /^[a-z0-9][a-z0-9._:-]{0,63}$/
const percentPattern = /^([1-9][0-9]?|100)%$/
const colorPattern = /^(#[0-9a-fA-F]{6}|fg|focus|heading|link|activity|accent|muted|error|warning|success|border)$/
const encoder = new TextEncoder()

/** Terminal-safe extension text, or `undefined` when it is not a string or too large. */
export function safeText(value: unknown): string | undefined {
  return typeof value === "string" && encoder.encode(value).byteLength <= renderLimits.maxStringBytes ? value.replace(escapes, "") : undefined
}

export function isJson(value: unknown, depth = 0): value is JsonValue {
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

export function validatePaneSize(value: unknown): PaneSize | string {
  if (!value || typeof value !== "object" || Array.isArray(value)) return "size must be an object"
  const raw = value as Record<string, unknown>
  if (!only(raw, ["width", "height", "min_width", "min_height"])) return "size: unknown field"
  const dim = (val: unknown): boolean =>
    val === undefined
    || (Number.isSafeInteger(val) && (val as number) >= 1 && (val as number) <= 1000)
    || (typeof val === "string" && percentPattern.test(val))
  const minDim = (val: unknown): boolean =>
    val === undefined || (Number.isSafeInteger(val) && (val as number) >= 1 && (val as number) <= 1000)
  if (!dim(raw.width)) return "size.width must be an integer 1..1000 or percentage string"
  if (!dim(raw.height)) return "size.height must be an integer 1..1000 or percentage string"
  if (!minDim(raw.min_width)) return "size.min_width must be an integer 1..1000"
  if (!minDim(raw.min_height)) return "size.min_height must be an integer 1..1000"
  return {
    ...(raw.width !== undefined ? { width: raw.width as number | string } : {}),
    ...(raw.height !== undefined ? { height: raw.height as number | string } : {}),
    ...(raw.min_width !== undefined ? { min_width: raw.min_width as number } : {}),
    ...(raw.min_height !== undefined ? { min_height: raw.min_height as number } : {}),
  }
}

export type SlotRule = "forbid" | "require"

/**
 * Validate an untrusted render tree. `slot`: `require` (decorating renderer)
 * demands exactly one `{ kind: "slot" }`, `forbid` none. Actions survive only
 * with `actions` (the `tui.action` permission); otherwise the text stays, inert.
 * Host-virtualized `{ type: "transcript", sessionId }` nodes require
 * `transcriptEmbed: true` (`tui.transcript.embed`).
 */
export function validateRenderNode(
  value: unknown,
  options: { slot: SlotRule; actions: boolean; transcriptEmbed?: boolean },
): RenderNode | string {
  let nodes = 0
  let slots = 0
  const visit = (raw: unknown, depth: number): RenderNode | string => {
    if (depth > renderLimits.maxDepth) return `render tree deeper than ${renderLimits.maxDepth}`
    if (++nodes > renderLimits.maxNodes) return `render tree larger than ${renderLimits.maxNodes} nodes`
    if (!raw || typeof raw !== "object" || Array.isArray(raw)) return "render node must be an object"
    const node = raw as Record<string, unknown>
    if (node.type === "transcript" || node.kind === "transcript") {
      return validateTranscriptNode(node, { allowed: options.transcriptEmbed === true })
    }
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
  if (isTranscriptNode(node)) return `[transcript:${node.sessionId}]`
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
  const list = (key: Exclude<keyof WireContributions, "commands">): Record<string, unknown>[] | string => {
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
  const panes = list("panes"); if (typeof panes === "string") return panes
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
    if (panel.default_slot !== undefined && !PANE_SLOTS.includes(panel.default_slot as PaneSlot)) return `panel ${String(panel.id)}: invalid default_slot ${String(panel.default_slot)}`
    let defaultSize: PaneSize | undefined
    if (panel.default_size !== undefined) {
      const checkedSize = validatePaneSize(panel.default_size)
      if (typeof checkedSize === "string") return `panel ${String(panel.id)}: ${checkedSize}`
      defaultSize = checkedSize
    }
    if (panel.keys !== undefined && panel.keys !== true) return `panel ${String(panel.id)}: keys must be true`
    if (panel.keys === true && !permissions.includes("tui.keys")) warnings.push(`panel ${String(panel.id)} keys ignored: permission tui.keys not declared`)
    if (panel.refresh_ms !== undefined && (!Number.isSafeInteger(panel.refresh_ms) || (panel.refresh_ms as number) < 1000)) return `panel ${String(panel.id)}: refresh_ms must be >= 1000`
    checkedPanels.push({
      id: panel.id as string, title, placement: panel.placement,
      ...(panel.replaces ? { replaces: panel.replaces as ReplaceableSurface } : {}),
      ...(panel.default_slot ? { defaultSlot: panel.default_slot as PaneSlot } : {}),
      ...(defaultSize ? { defaultSize } : {}),
      ...(panel.refresh_ms ? { refreshMs: panel.refresh_ms as number } : {}),
      ...(panel.keys === true && permissions.includes("tui.keys") ? { keys: true as const } : {}),
    })
  }

  const checkedPanes: PaneContribution[] = []
  for (const pane of panes) {
    const title = safeText(pane.title)
    if (!title || title.length > 64) return `pane ${String(pane.id)}: title must be 1-64 characters`
    if (pane.default_slot !== undefined && !PANE_SLOTS.includes(pane.default_slot as PaneSlot)) return `pane ${String(pane.id)}: invalid default_slot ${String(pane.default_slot)}`
    let defaultSize: PaneSize | undefined
    if (pane.default_size !== undefined) {
      const checkedSize = validatePaneSize(pane.default_size)
      if (typeof checkedSize === "string") return `pane ${String(pane.id)}: ${checkedSize}`
      defaultSize = checkedSize
    }
    if (pane.pinnable !== undefined && typeof pane.pinnable !== "boolean") return `pane ${String(pane.id)}: pinnable must be boolean`
    if (pane.closable !== undefined && typeof pane.closable !== "boolean") return `pane ${String(pane.id)}: closable must be boolean`
    if (pane.keys !== undefined && pane.keys !== true) return `pane ${String(pane.id)}: keys must be true`
    if (pane.keys === true && !permissions.includes("tui.keys")) warnings.push(`pane ${String(pane.id)} keys ignored: permission tui.keys not declared`)
    if (pane.refresh_ms !== undefined && (!Number.isSafeInteger(pane.refresh_ms) || (pane.refresh_ms as number) < 1000)) return `pane ${String(pane.id)}: refresh_ms must be >= 1000`
    checkedPanes.push({
      id: pane.id as string,
      title,
      ...(pane.default_slot ? { defaultSlot: pane.default_slot as PaneSlot } : {}),
      ...(defaultSize ? { defaultSize } : {}),
      ...(pane.pinnable !== undefined ? { pinnable: pane.pinnable as boolean } : {}),
      ...(pane.closable !== undefined ? { closable: pane.closable as boolean } : {}),
      ...(pane.refresh_ms ? { refreshMs: pane.refresh_ms as number } : {}),
      ...(pane.keys === true && permissions.includes("tui.keys") ? { keys: true as const } : {}),
    })
  }

  const rawCommands = raw.commands ?? []
  if (!Array.isArray(rawCommands) || rawCommands.length > renderLimits.maxContributions || rawCommands.some((item) => !item || typeof item !== "object")) {
    return `contributions.commands must be at most ${renderLimits.maxContributions} objects`
  }
  const seenCommands = new Set<string>()
  const checkedCommands: SlashCommandContribution[] = []
  for (const item of rawCommands as Record<string, unknown>[]) {
    if (typeof item.name !== "string" || !commandNamePattern.test(item.name) || seenCommands.has(item.name)) {
      return `contributions.commands: invalid or duplicate name ${JSON.stringify(item.name)}`
    }
    if (item.name.startsWith("skill:")) return `command ${item.name}: skill: prefix is reserved`
    seenCommands.add(item.name)
    const description = safeText(item.description)
    if (!description || !description.trim() || description.length > 160) return `command ${item.name}: description must be 1-160 characters`
    const argumentHint = item.argument_hint === undefined ? undefined : safeText(item.argument_hint)
    if (item.argument_hint !== undefined && (argumentHint === undefined || argumentHint.length > 80)) {
      return `command ${item.name}: argument_hint must be at most 80 characters`
    }
    checkedCommands.push({
      name: item.name,
      description,
      ...(argumentHint !== undefined ? { argumentHint } : {}),
    })
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
      panes: gate(checkedPanes, "tui.panel", "panes"),
      commands: gate(checkedCommands, "tui.action", "commands"),
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

/** Confinement options for `scopeContext`. */
export interface ScopeContextOptions {
  /** Authoritative bundle ID of the extension receiving the context. */
  readonly bundleId?: string
  /** Optional explicit allowlist of additional bundle IDs authorized for `extension_state`. */
  readonly authorizedBundles?: readonly string[]
}

/**
 * The context sections a permission set may read.
 * `extension_state` is gated by `tui.extension_state.read` and strictly confined
 * to `options.bundleId` (plus any explicitly authorized bundles); fail-closed if
 * no `bundleId` or `authorizedBundles` is supplied.
 */
export function scopeContext(
  context: ExtensionContext,
  permissions: readonly ExtensionPermission[],
  options?: ScopeContextOptions,
): ExtensionContext {
  let extensionState: Record<string, JsonValue> | undefined
  if (context.extension_state && permissions.includes("tui.extension_state.read")) {
    const allowed = new Set<string>(options?.authorizedBundles ?? [])
    if (options?.bundleId) allowed.add(options.bundleId)
    const confined: Record<string, JsonValue> = {}
    for (const [bundleId, value] of Object.entries(context.extension_state)) {
      if (allowed.has(bundleId)) confined[bundleId] = value
    }
    extensionState = confined
  }
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
    ...(extensionState !== undefined ? { extension_state: extensionState } : {}),
  }
}

/** Validated, bundle-confined `bundle.api` request ready to forward to the backend. */
export interface ResolvedBundleApiRequest {
  readonly bundleId: string
  readonly method: BundleApiMethod
  readonly path: string
  /** Canonical backend URL path (`/v2/bundles/<bundleId>/api<path>` or session-scoped variant, plus encoded query). */
  readonly urlPath: string
  readonly body?: JsonValue
  readonly query?: Readonly<Record<string, string>>
  readonly sessionId?: string
}

/** Validate and confine a `bundle.api` request to the caller's own `bundleId` (unless explicitly authorized). */
export function resolveBundleApiRequest(
  callerBundleId: string,
  raw: Readonly<Record<string, unknown>>,
  authorize?: (targetBundleId: string) => boolean,
): ResolvedBundleApiRequest | string {
  const method = raw.method
  if (typeof method !== "string" || !BUNDLE_API_METHODS.includes(method as BundleApiMethod)) {
    return `bundle.api method must be one of ${BUNDLE_API_METHODS.join(", ")}`
  }
  const requestedBundle = raw.bundleId === undefined ? undefined : typeof raw.bundleId === "string" ? raw.bundleId : null
  if (requestedBundle === null) return "bundle.api bundleId must be a string"
  const confined = checkBundleConfinement(callerBundleId, requestedBundle, "bundle_api", authorize)
  if (!confined.ok) return confined.error
  if (raw.sessionId !== undefined && !isValidSessionId(raw.sessionId)) return "bundle.api sessionId is invalid"
  if (raw.body !== undefined && !isJson(raw.body)) return "bundle.api body must be JSON-safe"
  const query = raw.query as Readonly<Record<string, string>> | undefined
  const built = buildBundleApiUrlPath(confined.bundleId, raw.path, {
    ...(raw.sessionId !== undefined ? { sessionId: raw.sessionId as string } : {}),
    ...(query !== undefined ? { query } : {}),
  })
  if (!built.ok) return built.error
  return {
    bundleId: confined.bundleId,
    method: method as BundleApiMethod,
    path: built.normalizedPath,
    urlPath: built.urlPath,
    ...(raw.body !== undefined ? { body: raw.body as JsonValue } : {}),
    ...(query !== undefined ? { query } : {}),
    ...(raw.sessionId !== undefined ? { sessionId: raw.sessionId as string } : {}),
  }
}
