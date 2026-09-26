/**
 * The Saved Rules view (`/rules`, docs/tui.md "Saved Rules"): pure state and
 * keys over `GET /v1/permissions/rules` / `DELETE /v1/permissions/rules/{id}`
 * (components/RulesView.tsx renders it; app/rules.ts owns the calls).
 *
 * One screen: the saved rules, newest first as the server returns them.
 * `d` asks to confirm, Enter deletes; `r` refreshes; Esc closes. While a call
 * runs (`busy`) Esc cancels it.
 */
import type { ProjectInfo, SavedRule } from "../client"
import { relativeTime } from "./catalog"
import type { KeyLike } from "../keys/bindings"
import { truncate } from "./format"

export interface RulesBusy {
  kind: "refresh" | "delete"
  label: string
  startedAt: number
}

export interface RulesNotice {
  text: string
  tone: "info" | "ok" | "error"
}

export interface RulesViewState {
  /** Highlighted rule id. */
  rule: string | undefined
  filter: string
  filtering: boolean
  /** Set while `d` asks to confirm a delete. */
  confirm?: string
  busy?: RulesBusy
  notice?: RulesNotice
}

export type RulesViewOutcome =
  | { type: "none" }
  | { type: "update"; view: RulesViewState }
  | { type: "close" }
  | { type: "cancelBusy" }
  | { type: "refresh" }
  | { type: "delete"; rule: string }

export interface RulesKeyRow {
  keys: string
  description: string
  hint?: string
}

export const rulesKeyRows: readonly RulesKeyRow[] = [
  { keys: "Up / Down", description: "Move the highlight over rules", hint: "↑↓ move" },
  { keys: "d", description: "Ask to delete the highlighted rule", hint: "d delete" },
  { keys: "Enter", description: "Confirm a pending delete" },
  { keys: "r", description: "Refresh the rule list", hint: "r refresh" },
  { keys: "/", description: "Filter the rows by typing; Enter keeps the filter, Esc clears it", hint: "/ filter" },
  { keys: "Esc", description: "Cancel a running call, clear the filter, cancel a pending delete, or close the view" },
]

/** `RulePermission` in words. */
export function permissionText(permission: string | undefined): string {
  const name = (permission ?? "").replace(/^RULE_PERMISSION_/, "")
  switch (name) {
    case "ALLOW": return "allow"
    case "ASK": return "ask"
    case "DENY": return "deny"
    default: return "—"
  }
}

/**
 * A saved timestamp as `Nm ago` / `Nh ago` / `Nd ago` (state/catalog.ts
 * `relativeTime`); `—` when the server left it unset (a rule saved before
 * creation times were recorded) or the timestamp does not parse.
 */
export function ruleTimeText(time: string | undefined, now: number = Date.now()): string {
  const rel = relativeTime(time, now)
  return rel ? `${rel} ago` : "—"
}

function cell(text: string, width: number): string {
  const cut = truncate(text, width)
  return cut + " ".repeat(Math.max(0, width - Bun.stringWidth(cut)))
}

/**
 * A rule's pattern column: the server leaves `pattern` empty for a tool-wide
 * grant (distinct from `*`, an action-wide grant across every tool), so an
 * empty pattern is shown as-is, not folded into `*`.
 */
function patternText(rule: SavedRule): string {
  return rule.pattern ?? ""
}

/**
 * A rule's project column: the Project's name when `projects` (`state.projects`)
 * has it, else the raw `projectId`; `"global"` for a rule scoped to every
 * Project (ADR-0026's `GLOBAL_PROJECT`, or an older/pre-ADR-0026 row with no
 * `projectId` at all).
 */
export function ruleProjectLabel(rule: SavedRule, projects: readonly ProjectInfo[] = []): string {
  const id = rule.projectId
  if (!id || id === "global") return "global"
  return projects.find((project) => project.id === id)?.name ?? id
}

const patternWidth = (width: number): number => (width > 60 ? 30 : 20)

/** One rule row: effect, tool, pattern, project, relative saved time (fits `width`), e.g. `allow  bash  git status  hya  · 2m ago`. */
export function ruleLine(rule: SavedRule, width: number, projects: readonly ProjectInfo[] = []): string {
  const line = `${cell(permissionText(rule.permission), 6)} ${cell(rule.tool || "*", 12)} ${cell(patternText(rule), patternWidth(width))} ${cell(ruleProjectLabel(rule, projects), 10)} · ${ruleTimeText(rule.timeCreated)}`
  return truncate(line.trimEnd(), width)
}

export function ruleHeaderLine(width: number): string {
  return truncate(`${cell("EFFECT", 6)} ${cell("TOOL", 12)} ${cell("PATTERN", patternWidth(width))} ${cell("PROJECT", 10)} · SAVED`, width)
}

function matches(haystack: string, filter: string): boolean {
  const words = filter.toLowerCase().split(/\s+/).filter(Boolean)
  const text = haystack.toLowerCase()
  return words.every((word) => text.includes(word))
}

/** The rules shown now, filtered by tool, pattern, or effect. */
export function shownRules(view: Pick<RulesViewState, "filter">, rules: readonly SavedRule[]): SavedRule[] {
  if (!view.filter) return [...rules]
  return rules.filter((rule) => matches(`${rule.tool ?? ""}\n${rule.pattern ?? ""}\n${permissionText(rule.permission)}`, view.filter))
}

export function initialRulesView(rules: readonly SavedRule[]): RulesViewState {
  return { rule: shownRules({ filter: "" }, rules)[0]?.id, filter: "", filtering: false }
}

/** Keep the highlight on its row after a reload (the first shown row when it is gone). */
export function settleRulesView(view: RulesViewState, rules: readonly SavedRule[]): RulesViewState {
  const rows = shownRules(view, rules)
  return rows.some((row) => row.id === view.rule) || !rows.length ? view : { ...view, rule: rows[0]!.id }
}

const printable = (key: KeyLike): boolean => !key.ctrl && !key.meta && key.sequence.length === 1 && key.sequence >= " " && key.sequence !== "\x7f"
const isEnter = (key: KeyLike): boolean => !key.meta && (key.name === "return" || key.name === "enter" || key.name === "kpenter")

function move(view: RulesViewState, rules: readonly SavedRule[], step: number): RulesViewState {
  const rows = shownRules(view, rules)
  if (!rows.length) return view
  const at = rows.findIndex((row) => row.id === view.rule)
  return { ...view, rule: rows[(at + step + rows.length) % rows.length]!.id }
}

/** One key while the Saved Rules view is open. */
export function rulesViewKey(view: RulesViewState, key: KeyLike, rules: readonly SavedRule[]): RulesViewOutcome {
  if (view.busy) return key.name === "escape" ? { type: "cancelBusy" } : { type: "none" }
  if (view.confirm) {
    if (key.name === "escape") return { type: "update", view: { ...view, confirm: undefined } }
    return isEnter(key) ? { type: "delete", rule: view.confirm } : { type: "none" }
  }
  if (key.name === "up") return { type: "update", view: move(view, rules, -1) }
  if (key.name === "down") return { type: "update", view: move(view, rules, 1) }
  if (view.filtering) {
    if (key.name === "escape") return { type: "update", view: settleRulesView({ ...view, filtering: false, filter: "" }, rules) }
    if (isEnter(key)) return { type: "update", view: { ...view, filtering: false } }
    if (key.name === "backspace") return { type: "update", view: settleRulesView({ ...view, filter: view.filter.slice(0, -1) }, rules) }
    if (printable(key)) return { type: "update", view: settleRulesView({ ...view, filter: view.filter + key.sequence }, rules) }
    return { type: "none" }
  }
  if (key.name === "escape") {
    if (view.filter) return { type: "update", view: settleRulesView({ ...view, filter: "" }, rules) }
    return { type: "close" }
  }
  if (key.ctrl || key.meta) return { type: "none" }
  if (key.sequence === "/") return { type: "update", view: { ...view, filtering: true } }
  if (key.sequence === "r") return { type: "refresh" }
  if (key.sequence === "d") {
    if (!view.rule) return { type: "update", view: { ...view, notice: { tone: "info", text: "No rule selected" } } }
    return { type: "update", view: { ...view, notice: undefined, confirm: view.rule } }
  }
  return { type: "none" }
}

/** The footer hint for the current screen, filter, or confirm. */
export function rulesViewHint(view: RulesViewState): string {
  if (view.busy) return `${view.busy.label}… · Esc cancels`
  if (view.confirm) return "Enter deletes · Esc cancels"
  if (view.filtering) return "Type to filter · Enter keeps the filter · Esc clears it"
  return [...rulesKeyRows.filter((row) => row.hint).map((row) => row.hint!), "Esc close"].join(" · ")
}
