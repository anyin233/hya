/**
 * The Skills view (`/skills`, docs/tui.md "Skills view"): pure state and keys
 * over `GET /v1/skills` (components/SkillsView.tsx renders it;
 * app/skills.ts owns the calls).
 *
 * Screens: `list` (every skill of the scope under a divider rule per source
 * group — `project .hya/skills`, `user ~/.codex/skills`, `builtin` — name,
 * source, and description) and `detail` (one skill: its file, description,
 * and its scrolling body, opened with Enter). `u` closes the view and puts
 * `/skill:<name> ` into the message input; `y` copies the skill file's path;
 * `r` re-reads the catalog.
 */
import type { SkillSummary } from "../client"
import type { KeyLike } from "../keys/bindings"
import { truncate } from "./format"
import { skillGroup } from "./startup"

export type SkillsScreen = "list" | "detail"

export interface SkillsBusy {
  label: string
  startedAt: number
}

export interface SkillsNotice {
  text: string
  tone: "info" | "ok" | "error"
}

export interface SkillsViewState {
  screen: SkillsScreen
  /** Highlighted skill name. */
  skill: string | undefined
  filter: string
  filtering: boolean
  /** First body line shown on the `detail` screen. */
  scroll: number
  busy?: SkillsBusy
  notice?: SkillsNotice
}

export type SkillsViewOutcome =
  | { type: "none" }
  | { type: "update"; view: SkillsViewState }
  | { type: "close" }
  | { type: "cancelBusy" }
  | { type: "refresh" }
  | { type: "use"; skill: string }
  | { type: "copy"; skill: string }

export interface SkillsKeyRow {
  keys: string
  description: string
  hint?: string
}

export const skillsKeyRows: readonly SkillsKeyRow[] = [
  { keys: "Up / Down", description: "Move the highlight over skills (divider rules are skipped); on the details, scroll the body", hint: "↑↓ move" },
  { keys: "PgUp / PgDn, Home / End", description: "Page through the skills or the body" },
  { keys: "Enter", description: "Open the highlighted skill's details: file, description, body", hint: "Enter details" },
  { keys: "u", description: "Close the view and put /skill:<name> into the message input", hint: "u use" },
  { keys: "y", description: "Copy the skill file's path (OSC 52)", hint: "y copy path" },
  { keys: "r", description: "Re-read the skill catalog", hint: "r refresh" },
  { keys: "/", description: "Filter the rows by name, description, or source; Enter keeps the filter, Esc clears it", hint: "/ filter" },
  { keys: "Esc", description: "Cancel a running call, back out of the details, clear the filter, then close the view" },
]

/** One line of the list: a source group's divider rule or a skill row. */
export type SkillsViewLine =
  | { kind: "divider"; title: string }
  | { kind: "skill"; skill: SkillSummary }

function cell(text: string, width: number): string {
  const cut = truncate(text, width)
  return cut + " ".repeat(Math.max(0, width - Bun.stringWidth(cut)))
}

function matches(haystack: string, filter: string): boolean {
  const words = filter.toLowerCase().split(/\s+/).filter(Boolean)
  const text = haystack.toLowerCase()
  return words.every((word) => text.includes(word))
}

/** The source column: `project`, `user`, `bundle`, or `builtin`. */
export function skillSourceText(skill: SkillSummary, env: Record<string, string | undefined> = process.env): string {
  return skillGroup(skill, env).split(" ")[0] || "builtin"
}

/** Skills shown now, in catalog order (first definition wins there), filtered by name, description, or group. */
export function shownSkills(view: Pick<SkillsViewState, "filter">, skills: readonly SkillSummary[], env: Record<string, string | undefined> = process.env): SkillSummary[] {
  if (!view.filter) return [...skills]
  return skills.filter((skill) => matches(`${skill.name}\n${skill.description ?? ""}\n${skillGroup(skill, env)}`, view.filter))
}

/** The list's lines: each group's divider rule, then its skills, groups in order of first appearance. */
export function skillsViewLines(view: Pick<SkillsViewState, "filter">, skills: readonly SkillSummary[], env: Record<string, string | undefined> = process.env): SkillsViewLine[] {
  const groups = new Map<string, SkillSummary[]>()
  for (const skill of shownSkills(view, skills, env)) {
    const group = skillGroup(skill, env)
    groups.set(group, [...groups.get(group) ?? [], skill])
  }
  return [...groups].flatMap(([title, members]): SkillsViewLine[] => [{ kind: "divider", title }, ...members.map((skill): SkillsViewLine => ({ kind: "skill", skill }))])
}

/** The skill rows in display order (grouped), the order Up/Down walk. */
export function orderedSkills(view: Pick<SkillsViewState, "filter">, skills: readonly SkillSummary[], env: Record<string, string | undefined> = process.env): SkillSummary[] {
  return skillsViewLines(view, skills, env).flatMap((line) => line.kind === "skill" ? [line.skill] : [])
}

/** The name column: the longest name up to 28, at least 12. */
function nameColumn(skills: readonly SkillSummary[]): number {
  return Math.max(12, Math.min(28, Math.max(0, ...skills.map((skill) => Bun.stringWidth(skill.name)))))
}

/**
 * One skill row fitting `width`: name, source, description. Below about 60
 * columns the source column goes (the divider above already says it).
 */
export function skillLine(skill: SkillSummary, width: number, skills: readonly SkillSummary[] = [skill], env: Record<string, string | undefined> = process.env): string {
  const name = cell(skill.name, nameColumn(skills))
  const source = width >= 60 ? `${cell(skillSourceText(skill, env), 8)} ` : ""
  return truncate(`${name} ${source}${skill.description ?? ""}`.trimEnd(), width)
}

export function skillHeaderLine(width: number, skills: readonly SkillSummary[]): string {
  return truncate(`${cell("SKILL", nameColumn(skills))} ${width >= 60 ? `${cell("SOURCE", 8)} ` : ""}DESCRIPTION`, width)
}

/** A group's divider rule, `── Title ───…`, filling `width`. */
export function groupRule(title: string, width: number): string {
  const head = `── ${title} `
  return truncate(head + "─".repeat(Math.max(0, width - Bun.stringWidth(head))), width)
}

/**
 * The skill's file in words: its path, or for a built-in skill
 * `<built-in> (hya/core-skills)`.
 */
export function skillLocationText(skill: SkillSummary): string {
  const location = skill.location ?? ""
  if (!location || location === "<built-in>") return "<built-in> (hya/core-skills)"
  return location
}

/** Whether the skill has a file whose path `y` can copy. */
export function hasSkillFile(skill: SkillSummary): boolean {
  const location = skill.location ?? ""
  return location !== "" && !location.startsWith("<")
}

/**
 * The body as display lines at `width`: each source line, word-wrapped
 * (a word longer than the width is cut); blank lines are kept.
 */
export function skillBodyLines(content: string, width: number): string[] {
  const columns = Math.max(1, width)
  const out: string[] = []
  for (const line of content.replace(/\s+$/, "").split("\n")) {
    if (Bun.stringWidth(line) <= columns) {
      out.push(line)
      continue
    }
    let current = ""
    for (const word of line.split(/(\s+)/)) {
      if (!word) continue
      if (Bun.stringWidth(current + word) <= columns) {
        current += word
        continue
      }
      if (current.trim()) out.push(current.trimEnd())
      current = word.trimStart()
      while (Bun.stringWidth(current) > columns) {
        out.push(current.slice(0, columns))
        current = current.slice(columns)
      }
    }
    out.push(current.trimEnd())
  }
  return out
}

/** Open on the first skill (or the first match of `filter`, the argument of `/skills <filter>`). */
export function initialSkillsView(skills: readonly SkillSummary[], filter = ""): SkillsViewState {
  const view: SkillsViewState = { screen: "list", skill: undefined, filter, filtering: false, scroll: 0 }
  return { ...view, skill: orderedSkills(view, skills)[0]?.name }
}

/** Keep the highlight on its row after a reload or filter change (the first shown row when it is gone). */
export function settleSkillsView(view: SkillsViewState, skills: readonly SkillSummary[]): SkillsViewState {
  const rows = orderedSkills(view, skills)
  if (rows.some((row) => row.name === view.skill)) return view
  return { ...view, screen: "list", scroll: 0, skill: rows[0]?.name }
}

const printable = (key: KeyLike): boolean => !key.ctrl && !key.meta && key.sequence.length === 1 && key.sequence >= " " && key.sequence !== "\x7f"
const isEnter = (key: KeyLike): boolean => !key.meta && (key.name === "return" || key.name === "enter" || key.name === "kpenter")

function move(view: SkillsViewState, skills: readonly SkillSummary[], step: number, wrap: boolean): SkillsViewState {
  const rows = orderedSkills(view, skills)
  if (!rows.length) return view
  const at = Math.max(0, rows.findIndex((row) => row.name === view.skill))
  const next = wrap ? (at + step + rows.length) % rows.length : Math.max(0, Math.min(rows.length - 1, at + step))
  return { ...view, skill: rows[next]!.name }
}

/** What the `detail` screen can scroll: its body's line count and the rows it shows at once. */
export interface SkillsScroll {
  lines: number
  page: number
}

function scrollTo(view: SkillsViewState, scroll: number, limits: SkillsScroll): SkillsViewState {
  const max = Math.max(0, limits.lines - limits.page)
  return { ...view, scroll: Math.max(0, Math.min(max, scroll)) }
}

const pageStep = (limits: SkillsScroll): number => Math.max(1, limits.page - 1)

/** One key while the Skills view is open. */
export function skillsViewKey(view: SkillsViewState, key: KeyLike, skills: readonly SkillSummary[], limits: SkillsScroll = { lines: 0, page: 10 }): SkillsViewOutcome {
  if (view.busy) return key.name === "escape" ? { type: "cancelBusy" } : { type: "none" }
  const update = (next: SkillsViewState): SkillsViewOutcome => ({ type: "update", view: next })
  if (view.screen === "detail") {
    switch (key.name) {
      case "up": return update(scrollTo(view, view.scroll - 1, limits))
      case "down": return update(scrollTo(view, view.scroll + 1, limits))
      case "pageup": return update(scrollTo(view, view.scroll - pageStep(limits), limits))
      case "pagedown": return update(scrollTo(view, view.scroll + pageStep(limits), limits))
      case "home": return update(scrollTo(view, 0, limits))
      case "end": return update(scrollTo(view, limits.lines, limits))
      case "escape": case "left": return update({ ...view, screen: "list", scroll: 0, notice: undefined })
    }
    if (key.ctrl || key.meta || !view.skill) return { type: "none" }
    if (key.sequence === "u") return { type: "use", skill: view.skill }
    if (key.sequence === "y") return { type: "copy", skill: view.skill }
    return { type: "none" }
  }
  if (key.name === "up") return update(move(view, skills, -1, true))
  if (key.name === "down") return update(move(view, skills, 1, true))
  if (key.name === "pageup") return update(move(view, skills, -pageStep(limits), false))
  if (key.name === "pagedown") return update(move(view, skills, pageStep(limits), false))
  if (view.filtering) {
    if (key.name === "escape") return update(settleSkillsView({ ...view, filtering: false, filter: "" }, skills))
    if (isEnter(key)) return update({ ...view, filtering: false })
    if (key.name === "backspace") return update(settleSkillsView({ ...view, filter: view.filter.slice(0, -1) }, skills))
    if (printable(key)) return update(settleSkillsView({ ...view, filter: view.filter + key.sequence }, skills))
    return { type: "none" }
  }
  if (key.name === "home") return update(move(view, skills, -orderedSkills(view, skills).length, false))
  if (key.name === "end") return update(move(view, skills, orderedSkills(view, skills).length, false))
  if (key.name === "escape") {
    if (view.filter) return update(settleSkillsView({ ...view, filter: "" }, skills))
    return { type: "close" }
  }
  if (key.ctrl || key.meta) return { type: "none" }
  if (key.sequence === "/") return update({ ...view, filtering: true })
  if (key.sequence === "r") return { type: "refresh" }
  if (!view.skill || !orderedSkills(view, skills).some((row) => row.name === view.skill)) {
    return isEnter(key) || key.sequence === "u" || key.sequence === "y" ? update({ ...view, notice: { tone: "info", text: "No skill selected" } }) : { type: "none" }
  }
  if (isEnter(key)) return update({ ...view, screen: "detail", scroll: 0, notice: undefined })
  if (key.sequence === "u") return { type: "use", skill: view.skill }
  if (key.sequence === "y") return { type: "copy", skill: view.skill }
  return { type: "none" }
}

/** The footer hint for the current screen, filter, or busy call. */
export function skillsViewHint(view: SkillsViewState): string {
  if (view.busy) return `${view.busy.label}… · Esc cancels`
  if (view.filtering) return "Type to filter · Enter keeps the filter · Esc clears it"
  if (view.screen === "detail") return "↑↓ PgUp PgDn scroll · u use · y copy path · Esc back"
  return [...skillsKeyRows.filter((row) => row.hint).map((row) => row.hint!), "Esc close"].join(" · ")
}

/** `6 skills`. */
export function skillsCountLine(skills: readonly SkillSummary[]): string {
  return `${skills.length} skill${skills.length === 1 ? "" : "s"}`
}
