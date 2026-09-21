/**
 * Minimal frontmatter parser for Claude Code markdown resources
 * (`agents/*.md`, per-skill `SKILL.md` files, `commands/*.md`).
 *
 * Claude Code uses YAML frontmatter with a small, flat key set; the parser
 * accepts `key: value` lines plus simple inline lists (`tools: a, b`) and
 * block lists (`tools:\n  - a`), which covers the documented agent/skill/
 * command fields without pulling in a YAML dependency.
 */

export type Frontmatter = {
  /** Scalar/string values keyed by their lowercased field name. */
  readonly scalars: Readonly<Record<string, string>>
  /** List values (inline comma lists or block `- item` lists). */
  readonly lists: Readonly<Record<string, readonly string[]>>
  /** The markdown body after the closing `---`. */
  readonly body: string
}

export type AgentMeta = {
  readonly name: string
  readonly description: string
  readonly tools: readonly string[]
  readonly model: string
}

/** Parse `---\\n…\\n---\\n<body>`; files without frontmatter yield an empty meta. */
export function parseFrontmatter(text: string): Frontmatter {
  const normalized = text.replace(/\r\n/g, "\n")
  if (!normalized.startsWith("---")) {
    return { scalars: {}, lists: {}, body: normalized }
  }
  const firstNewline = normalized.indexOf("\n")
  if (firstNewline < 0) {
    return { scalars: {}, lists: {}, body: "" }
  }
  const closing = findClosingFence(normalized, firstNewline + 1)
  if (closing < 0) {
    return { scalars: {}, lists: {}, body: normalized }
  }
  const header = normalized.slice(firstNewline + 1, closing)
  const body = normalized.slice(closing + lineLength(normalized, closing) + 1)
  return { ...parseHeader(header), body }
}

/** Extract the documented agent fields, defaulting like Claude Code does. */
export function agentMetaFrom(text: string, fallbackName: string): AgentMeta {
  const frontmatter = parseFrontmatter(text)
  const name = firstNonEmpty(frontmatter.scalars["name"] ?? "", fallbackName)
  const tools = frontmatter.lists["tools"] ?? []
  return {
    name,
    description: frontmatter.scalars["description"] ?? "",
    tools,
    model: frontmatter.scalars["model"] ?? "inherit",
  }
}

function parseHeader(header: string): {
  scalars: Record<string, string>
  lists: Record<string, readonly string[]>
} {
  const scalars: Record<string, string> = {}
  const lists: Record<string, Record<string, readonly string[]>> = { __all: {} }
  let currentList: string | undefined
  for (const rawLine of header.split("\n")) {
    const line = rawLine.trimEnd()
    if (line.trim() === "" || line.trim().startsWith("#")) {
      continue
    }
    const blockItem = /^-\s+(.*)$/.exec(line.trim())
    if (blockItem !== null && currentList !== undefined) {
      const existing = lists.__all[currentList] ?? []
      lists.__all[currentList] = [...existing, blockItem[1].trim()]
      continue
    }
    const colon = line.indexOf(":")
    if (colon <= 0) {
      continue
    }
    const key = line.slice(0, colon).trim().toLowerCase()
    const value = line.slice(colon + 1).trim()
    currentList = key
    if (value === "") {
      lists.__all[key] = []
      continue
    }
    if (value.startsWith("[") && value.endsWith("]")) {
      const inner = value.slice(1, -1)
      lists.__all[key] = inner === "" ? [] : splitList(inner)
      continue
    }
    if (value.includes(",")) {
      lists.__all[key] = splitList(value)
      continue
    }
    scalars[key] = stripQuotes(value)
  }
  const finalLists: Record<string, readonly string[]> = {}
  for (const [key, value] of Object.entries(lists.__all)) {
    if (value.length > 0) {
      finalLists[key] = value
    }
  }
  return { scalars, lists: finalLists }
}

function splitList(value: string): readonly string[] {
  return value
    .split(",")
    .map((entry) => stripQuotes(entry.trim()))
    .filter((entry) => entry.length > 0)
}

function stripQuotes(value: string): string {
  if (
    (value.startsWith("\"") && value.endsWith("\"") && value.length >= 2) ||
    (value.startsWith("'") && value.endsWith("'") && value.length >= 2)
  ) {
    return value.slice(1, -1)
  }
  return value
}

function findClosingFence(text: string, from: number): number {
  let index = from
  while (index < text.length) {
    const newline = text.indexOf("\n", index)
    const lineEnd = newline < 0 ? text.length : newline
    if (text.slice(index, lineEnd).trim() === "---") {
      return index
    }
    if (newline < 0) {
      return -1
    }
    index = newline + 1
  }
  return -1
}

function lineLength(text: string, offset: number): number {
  const newline = text.indexOf("\n", offset)
  return newline < 0 ? text.length - offset : newline - offset
}

function firstNonEmpty(...values: readonly string[]): string {
  for (const value of values) {
    if (value.length > 0) {
      return value
    }
  }
  return ""
}
