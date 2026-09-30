import { expect, test } from "bun:test"
import type { CommandSummary } from "../src/client"
import { filterCommands, mergeCommandEntries, requiresArgument, suggestCommandInput, type CommandEntry } from "../src/commands/menu"
import { createCommandRegistry } from "../src/commands/native"

test("merges the local registry with the backend catalog, sorted by name", () => {
  const backend: CommandSummary[] = [
    { name: "init", description: "Bootstrap the project", source: "command" },
    { name: "review", description: "Review the diff", argumentHint: "[path]", source: "command" },
    { name: "changelog", description: "Draft a changelog entry", source: "skill" },
  ]
  const entries = mergeCommandEntries(createCommandRegistry().list(), backend)
  const byName = new Map(entries.map((entry) => [entry.name, entry]))
  expect(byName.get("/init")).toEqual({ name: "/init", description: "Bootstrap the project", source: "command" })
  expect(byName.get("/review")).toEqual({ name: "/review", description: "Review the diff", argumentHint: "[path]", source: "command" })
  expect(byName.get("/changelog")).toEqual({ name: "/changelog", description: "Draft a changelog entry", source: "skill" })
  // Sorted by name.
  expect(entries.map((entry) => entry.name)).toEqual([...entries.map((entry) => entry.name)].sort())
})

test("a local command wins a name clash with a backend command", () => {
  const backend: CommandSummary[] = [{ name: "help", description: "Backend's own /help", source: "command" }]
  const entries = mergeCommandEntries(createCommandRegistry().list(), backend)
  const help = entries.find((entry) => entry.name === "/help")
  expect(help?.source).toBe("local")
  expect(help?.description).not.toBe("Backend's own /help")
})

const entries: CommandEntry[] = [
  { name: "/help", description: "a", source: "local" },
  { name: "/hidden", description: "b", source: "local" },
  { name: "/history", description: "c", source: "command" },
  { name: "/models", description: "d", source: "local" },
]

test("suggestions retain every command and nested argument beyond the twelve visible rows", () => {
  const names = Array.from({ length: 32 }, (_, index) => `/choice-${String(index).padStart(2, "0")}`)
  const catalog = names.map((name): CommandEntry => ({ name, description: "", source: "local" }))
  expect(suggestCommandInput("/", () => catalog, () => []).map((row) => row.replacement)).toEqual(names)
  const replacements = names.map((name) => `/layout assign ${name.slice(1)}`)
  expect(suggestCommandInput("/layout assign ", () => [], () => replacements).map((row) => row.replacement)).toEqual(replacements)
  const titled = names.map((name) => ({ label: `${name} (session title)`, replacement: `/open ${name.slice(1)}` }))
  const suggestions = suggestCommandInput("/open ", () => [], () => titled)
  expect(suggestions.map((row) => row.label)).toEqual(titled.map((row) => row.label))
  expect(suggestions.map((row) => row.replacement)).toEqual(titled.map((row) => row.replacement))
})

test("filterCommands ranks exact, then prefix, then substring, then subsequence matches", () => {
  expect(filterCommands(entries, "help").map((entry) => entry.name)).toEqual(["/help"])
  expect(filterCommands(entries, "hi").map((entry) => entry.name)).toEqual(["/hidden", "/history"])
  expect(filterCommands(entries, "story").map((entry) => entry.name)).toEqual(["/history"])
  // "hp" is a subsequence of "help" only.
  expect(filterCommands(entries, "hp").map((entry) => entry.name)).toEqual(["/help"])
})

test("filterCommands drops non-matching entries and is case-insensitive", () => {
  expect(filterCommands(entries, "HELP").map((entry) => entry.name)).toEqual(["/help"])
  expect(filterCommands(entries, "zzz")).toEqual([])
})

test("filterCommands with an empty query returns every entry", () => {
  expect(filterCommands(entries, "")).toHaveLength(entries.length)
})

test("requiresArgument: no hint or a [bracketed] hint is optional; anything else is required", () => {
  expect(requiresArgument(undefined)).toBe(false)
  expect(requiresArgument("[agent] [model]")).toBe(false)
  expect(requiresArgument("[on|off]")).toBe(false)
  expect(requiresArgument("<id|number>")).toBe(true)
  expect(requiresArgument("<title>")).toBe(true)
  expect(requiresArgument("set|remove <provider>")).toBe(true)
  expect(requiresArgument("select <name> | run [name]")).toBe(true)
})
