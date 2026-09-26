/**
 * The `/` command menu: merges the local command registry with the backend
 * command catalog (`ListCommands`, which already includes skills and custom
 * commands — see `crates/hya-server/src/support/command_catalog.rs`) into one
 * deduplicated, sorted list, and fuzzy-filters it as the user types a command
 * name.
 */
import type { CommandSummary } from "../client"
import type { CommandSpec } from "./registry"

/** Where a menu row came from. Local (TUI-registered) names win name clashes. */
export type CommandSource = "local" | "command" | "skill"

export interface CommandEntry {
  /** Including the leading slash, e.g. `/model`. */
  name: string
  description: string
  argumentHint?: string
  source: CommandSource
}

/** Rows shown at once; the rest are still reachable by typing a longer query. */
export const commandSuggestionLimit = 8

/**
 * Merge the local registry with the backend catalog into one list, sorted by
 * name. A backend command whose name clashes with a local command is dropped
 * (local takes precedence — the local command is what actually runs, since
 * the registry checks its own names before falling back to the backend).
 */
export function mergeCommandEntries(local: CommandSpec[], backend: CommandSummary[]): CommandEntry[] {
  const byName = new Map<string, CommandEntry>()
  for (const spec of local) {
    byName.set(spec.name, { name: spec.name, description: spec.description, argumentHint: spec.argumentHint, source: "local" })
  }
  for (const command of backend) {
    const name = `/${command.name}`
    if (byName.has(name)) continue
    byName.set(name, {
      name,
      description: command.description ?? "",
      ...(command.argumentHint ? { argumentHint: command.argumentHint } : {}),
      source: command.source === "skill" ? "skill" : "command",
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
 * command menu's Enter runs the command as is. Any other hint (`/open
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
