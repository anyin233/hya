/**
 * The command pane's suggestions: merges the local command registry with the backend
 * command catalog (`ListCommands`, which already includes skills and custom
 * commands — see `crates/hya-server/src/support/command_catalog.rs`) into one
 * deduplicated, sorted list, and fuzzy-filters it as the user types a command
 * name.
 */
import type { CommandSummary } from "../client"
import type { CommandSpec, Completion } from "./registry"

/** Where a menu row came from. Local (TUI-registered) names win name clashes. */
export type CommandSource = "local" | "command" | "skill" | "extension"

export interface CommandEntry {
  /** Including the leading slash, e.g. `/model`. */
  name: string
  description: string
  argumentHint?: string
  source: CommandSource
  /** A backend row's `scope` (`project`, `user`, `builtin`, a skill's `bundle`, or an extension bundle id): the menu tags it `[skill · project]`. */
  scope?: string
}

/** Extension-contributed slash command entry merged into the command menu. */
export interface ExtensionMenuCommand {
  readonly name: string
  readonly description: string
  readonly argumentHint?: string
  readonly extension: string
}

/** What the menu lists besides the local commands. */
export interface MergeOptions {
  /** `false` (`tui.toml` `[commands] skill_commands = false`): no `/skill:*` rows; typing one still runs it. */
  skillCommands?: boolean
  /** Running TUI extension slash commands (`extensionManager.commands()`). */
  extensionCommands?: readonly ExtensionMenuCommand[]
}

/** A selectable row in the command pane, regardless of argument depth. */
export interface CommandSuggestion {
  label: string
  replacement: string
  kind: "command" | "argument"
  runOnEnter: boolean
}

/** Rows shown at once; navigation still visits the complete suggestion list. */
export const commandSuggestionLimit = 12

/**
 * Merge the local registry with the backend catalog into one list, sorted by
 * name. A backend command whose name clashes with a local command is dropped
 * (local takes precedence — the local command is what actually runs, since
 * the registry checks its own names before falling back to the backend).
 */
export function mergeCommandEntries(local: CommandSpec[], backend: CommandSummary[], options: MergeOptions = {}): CommandEntry[] {
  const byName = new Map<string, CommandEntry>()
  for (const ext of options.extensionCommands ?? []) {
    const name = ext.name.startsWith("/") ? ext.name : `/${ext.name}`
    if (byName.has(name)) continue
    byName.set(name, {
      name,
      description: ext.description,
      ...(ext.argumentHint ? { argumentHint: ext.argumentHint } : {}),
      source: "extension",
      scope: ext.extension,
    })
  }
  for (const spec of local) {
    if (byName.has(spec.name)) continue
    byName.set(spec.name, { name: spec.name, description: spec.description, argumentHint: spec.argumentHint, source: "local" })
  }
  for (const command of backend) {
    const name = `/${command.name}`
    if (byName.has(name)) continue
    const source = command.source === "skill" ? "skill" : "command"
    if (source === "skill" && options.skillCommands === false) continue
    byName.set(name, {
      name,
      description: command.description ?? "",
      ...(command.argumentHint ? { argumentHint: command.argumentHint } : {}),
      source,
      ...(command.scope ? { scope: command.scope } : {}),
    })
  }
  return [...byName.values()].sort((a, b) => a.name.localeCompare(b.name))
}

/** Score a candidate name against a lower-cased query; `undefined` means no match. */
function matchScore(name: string, query: string): number | undefined {
  if (!query) return 0
  if (name === query) return 3
  if (name.startsWith(query)) return 2
  if (name.includes(query)) return 1
  // Subsequence match: every query character appears in name, in order.
  let index = 0
  for (const char of query) {
    index = name.indexOf(char, index)
    if (index < 0) return undefined
    index++
  }
  return 0
}

/**
 * A `[bracketed]` argument hint (`/new [agent] [model]`) is optional — the
 * command pane's Enter runs the command as is. Any other hint (`/open
 * <id|number>`, `/answer <interaction id> <text>`) names a required first
 * argument — Enter only completes the name and waits, the same as Tab.
 */
export function requiresArgument(hint: string | undefined): boolean {
  return hint !== undefined && !hint.startsWith("[")
}

/**
 * Fuzzy-filter `entries` by `query` (the text typed after `/`, no leading
 * slash). Ranked exact > prefix > substring > subsequence match, ties broken
 * alphabetically; an empty query returns every entry, alphabetically.
 */
export function filterCommands(entries: CommandEntry[], query: string): CommandEntry[] {
  const q = query.toLowerCase()
  const scored = entries
    .map((entry) => ({ entry, score: matchScore(entry.name.slice(1).toLowerCase(), q) }))
    .filter((row): row is { entry: CommandEntry; score: number } => row.score !== undefined)
  return scored
    .sort((a, b) => b.score - a.score || a.entry.name.localeCompare(b.entry.name))
    .map((row) => row.entry)
}

/** Build the same selectable rows for a command name or any nested argument. */
export function suggestCommandInput(input: string, entries: () => CommandEntry[], complete: (input: string) => Completion[]): CommandSuggestion[] {
  if (!input.startsWith("/")) return []
  if (/\s/.test(input)) {
    return complete(input)
      .map((item) => typeof item === "string" ? { label: item, replacement: item } : item)
      .filter((item) => item.replacement !== input)
      .map(({ label, replacement }) => ({ label, replacement, kind: "argument", runOnEnter: false }))
  }
  return filterCommands(entries(), input.slice(1))
    .map((entry) => ({
      label: `${entry.name}${entry.argumentHint ? ` ${entry.argumentHint}` : ""}  ${entry.description}  [${entry.source}${entry.scope ? ` · ${entry.scope}` : ""}]`,
      replacement: entry.name,
      kind: "command",
      runOnEnter: !requiresArgument(entry.argumentHint),
    }))
}

/** The reserved skill namespace of the backend catalog: the skill `reviewer` runs as `/skill:reviewer`. */
export const skillCommandPrefix = "skill:"

function editDistance(a: string, b: string): number {
  let previous = Array.from({ length: b.length + 1 }, (_, index) => index)
  for (let i = 1; i <= a.length; i++) {
    const current = [i]
    for (let j = 1; j <= b.length; j++) {
      current[j] = Math.min(previous[j]! + 1, current[j - 1]! + 1, previous[j - 1]! + (a[i - 1] === b[j - 1] ? 0 : 1))
    }
    previous = current
  }
  return previous[b.length]!
}

/**
 * The `did you mean` for an unknown command `name` (no slash) among `names`
 * (no slashes): the skill of that very name first (`reviewer` →
 * `skill:reviewer`, the bare skill commands of older releases), then the
 * closest name within a typo's distance, else one that starts with it.
 */
export function closestCommand(name: string, names: readonly string[]): string | undefined {
  const lower = name.toLowerCase()
  if (!lower) return undefined
  if (names.includes(`${skillCommandPrefix}${name}`)) return `${skillCommandPrefix}${name}`
  const limit = Math.max(1, Math.floor(lower.length / 3))
  let best: { name: string; distance: number } | undefined
  for (const candidate of names) {
    const distance = editDistance(lower, candidate.toLowerCase())
    if (distance <= limit && (!best || distance < best.distance || (distance === best.distance && candidate < best.name))) best = { name: candidate, distance }
  }
  if (best) return best.name
  return [...names].sort().find((candidate) => candidate.toLowerCase().startsWith(lower))
}

/**
 * An unknown command line whose first word holds another `/` (`/usr/lib/x
 * fails to load`) reads as a path, not a command: it goes to the agent as
 * a prompt instead of being refused.
 */
export function looksLikePath(name: string): boolean {
  return /^\/[^/\s]*\/\S*/.test(name)
}
