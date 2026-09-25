/**
 * `@file` references. An `@` at the start of the input or after white space,
 * followed by at least one character up to the cursor, is a mention: its text
 * is looked up with `FindFiles` and a chosen path replaces the token as
 * `@<relative path> `. Nothing is attached; the prompt carries the text.
 */
export interface MentionToken {
  /** Offset of the `@`. */
  start: number
  /** End of the token (the next white space after the cursor, or the end). */
  end: number
  /** The text between `@` and the cursor. */
  query: string
}

/** The mention the cursor is in, or `undefined`. Slash-command lines have none. */
export function mentionAt(text: string, cursor: number): MentionToken | undefined {
  if (text.startsWith("/")) return undefined
  let at = cursor - 1
  while (at >= 0 && !/\s/.test(text[at]!) && text[at] !== "@") at--
  if (at < 0 || text[at] !== "@") return undefined
  if (at > 0 && !/\s/.test(text[at - 1]!)) return undefined
  const query = text.slice(at + 1, cursor)
  if (!query) return undefined
  let end = cursor
  while (end < text.length && !/\s/.test(text[end]!)) end++
  return { start: at, end, query }
}

/** Replace the token with `@path` and one space after it (unless white space follows already); the cursor goes after what was inserted. */
export function insertMention(text: string, token: MentionToken, path: string): { text: string; cursor: number } {
  const rest = text.slice(token.end)
  const inserted = `@${path}${/^\s/.test(rest) ? "" : " "}`
  return { text: text.slice(0, token.start) + inserted + rest, cursor: token.start + inserted.length }
}

/** The `FindFiles` glob for a query: any path containing it (`**` + `/*query*`). Matching is case-sensitive. */
export function findPattern(query: string): string {
  return `**/*${query}*`
}

/** Best matches first: file name starts with the query, then contains it, then the path does; shorter paths first. */
export function rankPaths(paths: string[], query: string, limit = 8): string[] {
  const needle = query.toLowerCase()
  const tier = (path: string): number => {
    const name = (path.split("/").at(-1) ?? path).toLowerCase()
    if (name.startsWith(needle)) return 0
    if (name.includes(needle)) return 1
    return 2
  }
  return [...new Set(paths)]
    .map((path) => ({ path, tier: tier(path) }))
    .sort((a, b) => a.tier - b.tier || a.path.length - b.path.length || (a.path < b.path ? -1 : a.path > b.path ? 1 : 0))
    .slice(0, limit)
    .map((item) => item.path)
}
