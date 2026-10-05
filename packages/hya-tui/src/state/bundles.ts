/**
 * The Bundles view (`/bundles`, docs/tui.md "Bundles"): pure state and keys
 * over `GET /v1/bundles` merged with the TUI's own extension state
 * (extensions/manager.ts). components/BundlesView.tsx renders it;
 * app/bundles.ts owns the calls and preferences.
 *
 * Screens: `list` (every bundle of the scope: id, version, scope, state, what
 * it contributes, its TUI extension) and `detail` (one bundle's components,
 * permissions, and extension state, opened with Enter). A bundle id can be
 * listed twice (a project bundle shadowing an installed one), so rows are
 * keyed by scope and id.
 *
 * Pop-ups: `install` asks for a package path, then, when the scope has a
 * directory, whether to install for the user or into the directory's
 * `.hya/bundles`; `uninstall` asks for confirmation.
 */
import type { BundleSummary } from "../client"
import type { ExtensionInfo } from "../extensions/manager"
import type { KeyLike } from "../keys/bindings"
import { truncate } from "./format"

export type BundleScreen = "list" | "detail"

/** One listed bundle with the TUI's state of its extension (if it declares one and the catalog publishes it). */
export interface BundleRow {
  /** `<scope>:<id>`: unique even when a project bundle shadows an installed one. */
  readonly key: string
  readonly bundle: BundleSummary
  readonly extension?: Pick<ExtensionInfo, "state" | "jit" | "isolated" | "reason">
}

export interface BundleBusy {
  label: string
  startedAt: number
}

export interface BundleNotice {
  text: string
  tone: "info" | "ok" | "error"
}

export type BundlePopup =
  | { kind: "install"; step: "path"; path: string; error?: string }
  | { kind: "install"; step: "target"; path: string; project: boolean }
  | { kind: "uninstall"; key: string; bundleId: string; project: boolean }

export interface BundleViewState {
  screen: BundleScreen
  /** Highlighted row key. */
  selected: string | undefined
  filter: string
  filtering: boolean
  /** The scope has a directory: installs may target its `.hya/bundles`. */
  projectTarget: boolean
  popup?: BundlePopup
  busy?: BundleBusy
  notice?: BundleNotice
}

export type BundleCommand =
  | { kind: "install"; path: string; project: boolean }
  | { kind: "uninstall"; bundleId: string; project: boolean }
  | { kind: "setEnabled"; bundleId: string; enabled: boolean; tui: boolean }
  | { kind: "setTrusted"; bundleId: string; trusted: boolean }

export type BundleViewOutcome =
  | { type: "none" }
  | { type: "update"; view: BundleViewState }
  | { type: "close" }
  | { type: "cancelBusy" }
  | { type: "refresh" }
  | { type: "command"; command: BundleCommand }

export interface BundleKeyRow {
  keys: string
  description: string
  hint?: string
  screens: readonly BundleScreen[]
}

export const bundleKeyRows: readonly BundleKeyRow[] = [
  { keys: "Up / Down", description: "Move the highlight over bundles", hint: "↑↓ move", screens: ["list", "detail"] },
  { keys: "Enter", description: "Show the highlighted bundle's components and TUI extension", hint: "Enter details", screens: ["list"] },
  { keys: "i", description: "Install a .hyabundle package: its path, then user or project scope", hint: "i install", screens: ["list", "detail"] },
  { keys: "x", description: "Uninstall the highlighted bundle (asks first; first-party bundles cannot be removed)", hint: "x uninstall", screens: ["list", "detail"] },
  { keys: "e", description: "Enable or disable the highlighted bundle: a disabled bundle publishes nothing, TUI extension included", hint: "e enable/disable", screens: ["list", "detail"] },
  { keys: "t", description: "Trust or untrust the bundle's TUI extension: trusted extensions run on the JIT (no VM memory cap)", hint: "t trust", screens: ["list", "detail"] },
  { keys: "r", description: "Reload the bundle list", hint: "r reload", screens: ["list", "detail"] },
  { keys: "/", description: "Filter the rows by typing; Enter keeps the filter, Esc clears it", hint: "/ filter", screens: ["list"] },
  { keys: "Esc", description: "Cancel a running call, close a pop-up, back out of the filter, back to the list, close the view", screens: ["list", "detail"] },
]

/** Merge the backend's bundles with the TUI's extension states. */
export function bundleRows(bundles: readonly BundleSummary[], extensions: readonly ExtensionInfo[]): BundleRow[] {
  return bundles.map((bundle) => {
    const extension = bundle.components?.tui && bundle.state === "active" ? extensions.find((row) => row.id === bundle.id) : undefined
    return { key: `${bundle.scope ?? ""}:${bundle.id}`, bundle, ...(extension ? { extension } : {}) }
  })
}

const scopeLabel = (scope: string | undefined): string => scope === "first_party" ? "first-party" : scope ?? "—"

function plural(count: number, word: string): string {
  return `${count} ${word}${count === 1 ? "" : "s"}`
}

/** What a bundle contributes, in words (`2 agents · 1 skill · TUI`). */
export function contentsText(bundle: BundleSummary): string {
  const c = bundle.components ?? {}
  const parts = [
    c.agents?.length ? plural(c.agents.length, "agent") : "",
    c.skills?.length ? plural(c.skills.length, "skill") : "",
    c.tools?.length ? plural(c.tools.length, "tool") : "",
    c.mcpServers?.length ? `${c.mcpServers.length} MCP` : "",
    c.workflows?.length ? plural(c.workflows.length, "workflow") : "",
    c.apis?.length ? plural(c.apis.length, "API") : "",
    c.hooks ? plural(c.hooks, "hook") : "",
    c.tui ? "TUI" : "",
  ]
  return parts.filter(Boolean).join(" · ") || "—"
}

/** The TUI extension column: tier and run state (`JIT running`, `VM blocked`), `—` without one. */
export function extensionText(row: BundleRow): string {
  if (!row.bundle.components?.tui) return "—"
  if (!row.extension) return row.bundle.state === "active" ? "not loaded" : "off"
  return `${row.extension.jit ? "JIT" : "VM"} ${row.extension.state}`
}

function cell(text: string, width: number): string {
  const cut = truncate(text, width)
  return cut + " ".repeat(Math.max(0, width - Bun.stringWidth(cut)))
}

export function bundleHeaderLine(width: number): string {
  return truncate(`${cell("BUNDLE", 30)} ${cell("VERSION", 9)} ${cell("SCOPE", 11)} ${cell("STATE", 9)} ${cell("TUI", 12)} CONTENTS`, width)
}

/** One list row (fits `width`). */
export function bundleLine(row: BundleRow, width: number): string {
  const { bundle } = row
  const line = `${cell(bundle.id, 30)} ${cell(bundle.version ?? "", 9)} ${cell(scopeLabel(bundle.scope), 11)} ${cell(bundle.state ?? "", 9)} ${cell(extensionText(row), 12)} ${contentsText(bundle)}`
  return truncate(line.trimEnd(), width)
}

/** The detail screen's lines for one bundle. */
export function bundleDetailLines(row: BundleRow): string[] {
  const { bundle } = row
  const c = bundle.components ?? {}
  const list = (label: string, ids: readonly string[] | undefined): string[] => ids?.length ? [`${label}: ${ids.join(", ")}`] : []
  const tui: string[] = c.tui
    ? [
      `TUI extension: ${row.extension ? `${row.extension.state} · ${row.extension.jit ? "JIT (trusted)" : "VM"}${row.extension.isolated ? " + OS sandbox" : ""}${row.extension.reason ? ` · ${row.extension.reason}` : ""}` : extensionText(row)}`,
      `TUI permissions: ${c.tuiPermissions?.join(", ") || "none"}`,
    ]
    : ["TUI extension: none"]
  return [
    `${bundle.id} ${bundle.version ?? ""} · ${scopeLabel(bundle.scope)} · ${bundle.state ?? ""}${bundle.kind ? ` · ${bundle.kind}` : ""}`,
    ...(bundle.description ? [bundle.description] : []),
    ...(bundle.publisher ? [`Publisher: ${bundle.publisher}`] : []),
    ...(bundle.preparedDigest ? [`Digest: ${bundle.preparedDigest}`] : []),
    ...(bundle.error ? [`Error: ${bundle.error}`] : []),
    ...list("Agents", c.agents),
    ...list("Skills", c.skills),
    ...list("Tools", c.tools),
    ...list("MCP servers", c.mcpServers),
    ...list("Workflows", c.workflows),
    ...list("Permission modes", c.permissionModes),
    ...list("APIs", c.apis),
    ...(c.hooks ? [`Hooks: ${c.hooks}`] : []),
    ...tui,
    bundle.removable ? "Removable: yes" : "Removable: no (ships with hya)",
  ]
}

function matches(row: BundleRow, filter: string): boolean {
  const text = `${row.bundle.id}\n${row.bundle.scope ?? ""}\n${row.bundle.state ?? ""}\n${contentsText(row.bundle)}`.toLowerCase()
  return filter.toLowerCase().split(/\s+/).filter(Boolean).every((word) => text.includes(word))
}

/** Rows shown on the list screen, filtered. */
export function shownBundles(view: Pick<BundleViewState, "filter">, rows: readonly BundleRow[]): BundleRow[] {
  return view.filter ? rows.filter((row) => matches(row, view.filter)) : [...rows]
}

export function initialBundlesView(rows: readonly BundleRow[], projectTarget: boolean): BundleViewState {
  return { screen: "list", selected: rows[0]?.key, filter: "", filtering: false, projectTarget }
}

/** Keep the highlight on a shown row after a reload or a filter change. */
export function settleBundlesView(view: BundleViewState, rows: readonly BundleRow[]): BundleViewState {
  const shown = shownBundles(view, rows)
  if (shown.some((row) => row.key === view.selected) || !shown.length) return view
  return { ...view, selected: shown[0]!.key }
}

const printable = (key: KeyLike): boolean => !key.ctrl && !key.meta && key.sequence.length === 1 && key.sequence >= " " && key.sequence !== "\x7f"
const isEnter = (key: KeyLike): boolean => !key.meta && (key.name === "return" || key.name === "enter" || key.name === "kpenter")

function move(view: BundleViewState, rows: readonly BundleRow[], step: number): BundleViewState {
  const shown = shownBundles(view, rows)
  if (!shown.length) return view
  const at = shown.findIndex((row) => row.key === view.selected)
  return { ...view, selected: shown[(at + step + shown.length) % shown.length]!.key }
}

/** One key in a pop-up. */
function popupKey(view: BundleViewState, popup: BundlePopup, key: KeyLike): BundleViewOutcome {
  if (key.name === "escape") return { type: "update", view: { ...view, popup: undefined } }
  if (popup.kind === "uninstall") {
    return isEnter(key) ? { type: "command", command: { kind: "uninstall", bundleId: popup.bundleId, project: popup.project } } : { type: "none" }
  }
  if (popup.step === "path") {
    if (isEnter(key)) {
      const path = popup.path.trim()
      if (!path) return { type: "update", view: { ...view, popup: { ...popup, error: "Type the path of a .hyabundle package" } } }
      if (!view.projectTarget) return { type: "command", command: { kind: "install", path, project: false } }
      return { type: "update", view: { ...view, popup: { kind: "install", step: "target", path, project: false } } }
    }
    if (key.name === "backspace") return { type: "update", view: { ...view, popup: { kind: "install", step: "path", path: popup.path.slice(0, -1) } } }
    if (printable(key)) return { type: "update", view: { ...view, popup: { kind: "install", step: "path", path: popup.path + key.sequence } } }
    return { type: "none" }
  }
  if (isEnter(key)) return { type: "command", command: { kind: "install", path: popup.path, project: popup.project } }
  if (key.sequence === "u" || key.sequence === "p") return { type: "update", view: { ...view, popup: { ...popup, project: key.sequence === "p" } } }
  if (key.name === "tab" || key.name === "up" || key.name === "down") return { type: "update", view: { ...view, popup: { ...popup, project: !popup.project } } }
  return { type: "none" }
}

/** One key while the Bundles view is open. */
export function bundlesViewKey(view: BundleViewState, key: KeyLike, rows: readonly BundleRow[]): BundleViewOutcome {
  if (view.busy) return key.name === "escape" ? { type: "cancelBusy" } : { type: "none" }
  if (view.popup) return popupKey(view, view.popup, key)
  if (view.filtering) {
    if (key.name === "escape") return { type: "update", view: settleBundlesView({ ...view, filtering: false, filter: "" }, rows) }
    if (isEnter(key)) return { type: "update", view: { ...view, filtering: false } }
    if (key.name === "backspace") return { type: "update", view: settleBundlesView({ ...view, filter: view.filter.slice(0, -1) }, rows) }
    if (printable(key)) return { type: "update", view: settleBundlesView({ ...view, filter: view.filter + key.sequence }, rows) }
    return { type: "none" }
  }
  if (key.name === "up") return { type: "update", view: move(view, rows, -1) }
  if (key.name === "down") return { type: "update", view: move(view, rows, 1) }
  if (key.name === "escape" || (key.name === "left" && view.screen === "detail")) {
    if (view.screen === "detail") return { type: "update", view: { ...view, screen: "list" } }
    if (view.filter) return { type: "update", view: settleBundlesView({ ...view, filter: "" }, rows) }
    return key.name === "escape" ? { type: "close" } : { type: "none" }
  }
  if (key.ctrl || key.meta) return { type: "none" }
  const row = rows.find((candidate) => candidate.key === view.selected)
  if ((isEnter(key) || key.name === "right") && view.screen === "list") return row ? { type: "update", view: { ...view, screen: "detail" } } : { type: "none" }
  if (key.sequence === "/" && view.screen === "list") return { type: "update", view: { ...view, filtering: true } }
  if (key.sequence === "r") return { type: "refresh" }
  if (key.sequence === "i") return { type: "update", view: { ...view, notice: undefined, popup: { kind: "install", step: "path", path: "" } } }
  if (!"xet".includes(key.sequence) || key.sequence.length !== 1) return { type: "none" }
  if (!row) return { type: "update", view: { ...view, notice: { tone: "info", text: "No bundle selected" } } }
  const { bundle } = row
  const notice = (text: string): BundleViewOutcome => ({ type: "update", view: { ...view, notice: { tone: "info", text } } })
  switch (key.sequence) {
    case "x":
      if (!bundle.removable) return notice(`${bundle.id} ships with hya and cannot be uninstalled; e disables it`)
      return { type: "update", view: { ...view, notice: undefined, popup: { kind: "uninstall", key: row.key, bundleId: bundle.id, project: bundle.scope === "project" } } }
    case "e":
      return { type: "command", command: { kind: "setEnabled", bundleId: bundle.id, enabled: bundle.enabled !== true, tui: bundle.components?.tui === true } }
    case "t":
      if (!bundle.components?.tui) return notice(`${bundle.id} has no TUI extension to trust`)
      return { type: "command", command: { kind: "setTrusted", bundleId: bundle.id, trusted: row.extension?.jit !== true } }
    default:
      return { type: "none" }
  }
}

/** The footer hint for the current screen, filter, or pop-up. */
export function bundlesViewHint(view: BundleViewState): string {
  const popup = view.popup
  if (popup?.kind === "uninstall") return "Enter uninstalls · Esc cancels"
  if (popup?.kind === "install") return popup.step === "path" ? "Type the package path · Enter next · Esc cancels" : "u user · p project · Enter installs · Esc cancels"
  if (view.busy) return `${view.busy.label}… · Esc cancels`
  if (view.filtering) return "Type to filter · Enter keeps the filter · Esc clears it"
  const keys = bundleKeyRows.filter((row) => row.hint && row.screens.includes(view.screen)).map((row) => row.hint!)
  return [...keys, view.screen === "detail" ? "Esc back" : "Esc close"].join(" · ")
}
