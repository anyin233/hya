/**
 * The tool-card view model: one `ToolCallPart` (docs/protocol/README.md
 * "Tool calls") becomes a `ToolCardView` — a state, the tool name, a
 * one-line summary, the duration once done, the error once failed, and the
 * body lines shown when the card is expanded (already clipped, each with a
 * tone the component maps to a color).
 *
 * Summaries read the canonical registry tools' real argument and output
 * fields (docs/architecture/agent-tool-surface.md): `bash` (command, exit),
 * `read` (path, line range), `edit` / `write` / `apply_patch` (path and a
 * diff), `grep` / `glob` / `find` / `ls` (pattern, scope, count), the
 * `todo__*` tools (the list), `webfetch` / `websearch`, `skill`, `ask_user`,
 * `task` (agent and description; see state/members.ts for the child), and
 * anything else (MCP `ns__tool` included) as the name and compact arguments.
 *
 * Pure TypeScript (no Solid).
 */
import type { ToolCallPart } from "../client"

export type ToolStatus = "pending" | "running" | "done" | "failed"

/** How a body line is colored (components/MessageView.tsx maps tones to theme colors). */
export type Tone = "fg" | "muted" | "add" | "remove" | "hunk" | "error"

export interface ToolLine {
  text: string
  tone: Tone
}

export interface TaskInfo {
  /** Subagent type (`subagent_type`, default `general`). */
  agent: string
  description: string
  /** Child session id, from the output's `metadata.sessionId`. */
  child?: string
}

export interface ToolCardView {
  tool: string
  status: ToolStatus
  /** One line; the component clips it to the width. */
  summary: string
  /** Formatted wall time, once done. */
  duration?: string
  /** The error message, once failed. */
  error?: string
  /** Expanded body, clipped to `toolBodyLines`. */
  body: ToolLine[]
  /** Shell command (bash), when known. */
  command?: string
  task?: TaskInfo
}

/** Most body lines an expanded card shows; longer bodies keep head and tail. */
export const toolBodyLines = 12
/** Longest compact-arguments summary for generic tools. */
const argsSummaryLimit = 160

type Json = Record<string, unknown>

function parse(text: string | undefined): unknown {
  if (!text) return undefined
  try {
    return JSON.parse(text) as unknown
  } catch {
    return undefined
  }
}

function record(value: unknown): Json {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Json : {}
}

function str(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined
}

function num(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined
}

function plural(count: number, word: string): string {
  return `${count} ${word}${count === 1 ? "" : word.endsWith("ch") ? "es" : "s"}`
}

/** `TOOL_EXECUTION_STATE_OK` → `done`, etc. */
export function toolStatus(state: string | undefined): ToolStatus {
  switch (state) {
    case "TOOL_EXECUTION_STATE_RUNNING": return "running"
    case "TOOL_EXECUTION_STATE_OK": return "done"
    case "TOOL_EXECUTION_STATE_ERROR": return "failed"
    default: return "pending"
  }
}

/** `42ms`, `1.5s`, `12s`, `1m 5s`. */
export function formatDuration(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`
  if (ms < 10_000) return `${(ms / 1000).toFixed(1)}s`
  if (ms < 60_000) return `${Math.round(ms / 1000)}s`
  const seconds = Math.round(ms / 1000)
  return `${Math.floor(seconds / 60)}m ${seconds % 60}s`
}

/** More than `max` lines: the head, a `… N lines hidden` marker, and the tail (the tail is usually what matters). */
export function clipLines(lines: ToolLine[], max = toolBodyLines): ToolLine[] {
  if (lines.length <= max) return lines
  const tail = Math.ceil((max - 1) / 2)
  const head = max - 1 - tail
  return [...lines.slice(0, head), { text: `… ${lines.length - head - tail} lines hidden`, tone: "muted" }, ...lines.slice(lines.length - tail)]
}

function textLines(text: string, tone: Tone = "muted"): ToolLine[] {
  const trimmed = text.replace(/\s+$/, "")
  return trimmed ? trimmed.split("\n").map((line) => ({ text: line, tone })) : []
}

/**
 * A string field of arguments that are still streaming (not valid JSON yet):
 * `{"command":"ls -la /tm` → `ls -la /tm`. Undefined until the value started.
 */
export function partialField(text: string, key: string): string | undefined {
  const match = new RegExp(`"${key}"\\s*:\\s*"((?:[^"\\\\]|\\\\.)*)`).exec(text)
  if (!match) return undefined
  const raw = match[1]!.replace(/\\$/, "")
  try {
    return JSON.parse(`"${raw}"`) as string
  } catch {
    return raw
  }
}

/** Readable output text: a JSON string, else its `output` / `stdout` / `text` / `content` string, else pretty JSON. */
function outputText(output: unknown): string {
  if (output === undefined || output === null) return ""
  if (typeof output === "string") return output
  const fields = record(output)
  const field = ["output", "stdout", "text", "content"].map((name) => fields[name]).find((item) => typeof item === "string")
  return typeof field === "string" ? field : JSON.stringify(output, null, 2)
}

/** Unified-diff rows, file headers skipped: `+` add, `-` remove, `@@` hunk, the rest context. */
export function diffLines(diff: string): ToolLine[] {
  const lines: ToolLine[] = []
  for (const line of diff.replace(/\n$/, "").split("\n")) {
    if (line.startsWith("+++") || line.startsWith("---") || line.startsWith("diff ") || line.startsWith("index ")) continue
    if (line.startsWith("@@")) lines.push({ text: line, tone: "hunk" })
    else if (line.startsWith("+")) lines.push({ text: `+ ${line.slice(1)}`, tone: "add" })
    else if (line.startsWith("-")) lines.push({ text: `- ${line.slice(1)}`, tone: "remove" })
    else if (line.startsWith("\\")) lines.push({ text: line, tone: "muted" })
    else lines.push({ text: `  ${line.startsWith(" ") ? line.slice(1) : line}`, tone: "muted" })
  }
  return lines
}

function counted(lines: ToolLine[]): string {
  const add = lines.filter((line) => line.tone === "add").length
  const remove = lines.filter((line) => line.tone === "remove").length
  return `+${add} -${remove}`
}

function split(text: string): string[] {
  return text.replace(/\n$/, "").split("\n")
}

/** Diff rows derived from edit arguments: hashline `edits[]` or compat old/new strings. */
function editArgLines(input: Json): ToolLine[] {
  const lines: ToolLine[] = []
  const pair = (oldText: string | undefined, newText: string | undefined): void => {
    if (oldText) for (const line of split(oldText)) lines.push({ text: `- ${line}`, tone: "remove" })
    if (newText) for (const line of split(newText)) lines.push({ text: `+ ${line}`, tone: "add" })
  }
  const edits = Array.isArray(input.edits) ? input.edits.map(record) : [input]
  for (const edit of edits) {
    const oldText = str(edit.oldText) ?? str(edit.oldString) ?? str(edit.old_string)
    const newText = str(edit.newText) ?? str(edit.newString) ?? str(edit.new_string)
    if (oldText !== undefined || newText !== undefined) pair(oldText, newText)
    else if (Array.isArray(edit.lines)) for (const line of edit.lines) lines.push({ text: `+ ${String(line)}`, tone: "add" })
  }
  return lines
}

/** `apply_patch` envelope rows: file headers as hunks, `+` / `-` / context lines. */
function patchLines(patch: string): { files: string[]; lines: ToolLine[] } {
  const files: string[] = []
  const lines: ToolLine[] = []
  for (const line of patch.replace(/\r\n?/g, "\n").split("\n")) {
    const header = /^\*\*\* (Add|Update|Delete) File: (.+)$/.exec(line.trim())
    if (header) {
      const [, kind, path] = header
      files.push(path!)
      lines.push({ text: kind === "Update" ? path! : `${path} (${kind === "Add" ? "new" : "deleted"})`, tone: "hunk" })
      continue
    }
    const moved = /^\*\*\* Move to: (.+)$/.exec(line.trim())
    if (moved) {
      lines.push({ text: `→ ${moved[1]}`, tone: "hunk" })
      continue
    }
    if (line.startsWith("***") || line === "") continue
    if (line.startsWith("@@")) lines.push({ text: line, tone: "hunk" })
    else if (line.startsWith("+")) lines.push({ text: `+ ${line.slice(1)}`, tone: "add" })
    else if (line.startsWith("-")) lines.push({ text: `- ${line.slice(1)}`, tone: "remove" })
    else lines.push({ text: `  ${line.startsWith(" ") ? line.slice(1) : line}`, tone: "muted" })
  }
  return { files, lines }
}

const todoGlyphs: Record<string, string> = { pending: "☐", in_progress: "▸", blocked: "!", completed: "✓", cancelled: "✗" }

function todoLines(items: unknown[]): ToolLine[] {
  return items.map(record).map((item) => {
    const status = (str(item.status) ?? "pending").replace(/^[A-Z_]*STATUS_/, "").toLowerCase()
    return { text: `${todoGlyphs[status] ?? "·"} ${str(item.content) ?? ""}`, tone: status === "completed" ? "muted" : "fg" }
  })
}

function scope(pattern: string | undefined, path: string | undefined, quoted = false): string {
  const shown = pattern === undefined ? "" : quoted ? `"${pattern}"` : pattern
  return [shown, path && path !== "." ? `in ${path}` : ""].filter(Boolean).join(" ")
}

interface Summary {
  summary: string
  body: ToolLine[]
  command?: string
  task?: TaskInfo
}

/** The input's main string field while the arguments still stream. */
const streamingFields = ["command", "path", "filePath", "pattern", "url", "query", "name", "description"]

function describe(tool: string, input: Json, output: unknown, raw: string, shellCommand: string | undefined): Summary {
  const out = record(output)
  const meta = record(out.metadata)
  const text = outputText(output)
  switch (tool) {
    case "bash":
    case "shell": {
      const command = str(input.command) ?? shellCommand ?? ""
      const exit = num(meta.exit)
      const flags = [exit !== undefined && exit !== 0 ? `exit ${exit}` : "", meta.timedOut === true ? "timed out" : ""].filter(Boolean)
      const firstLine = command.split("\n")[0] ?? ""
      const body: ToolLine[] = command ? [{ text: `$ ${command}`, tone: "fg" }] : []
      body.push(...clipLines(textLines(text), toolBodyLines - body.length - flags.length))
      for (const flag of flags) body.push({ text: flag, tone: "error" })
      return {
        summary: [`${firstLine}${command.includes("\n") ? " …" : ""}`, ...flags].filter(Boolean).join(" · "),
        body,
        ...(command ? { command } : {}),
      }
    }
    case "read": {
      const path = str(input.path) ?? str(input.filePath) ?? ""
      const display = record(meta.display)
      const start = num(display.lineStart)
      const end = num(display.lineEnd)
      const total = num(display.totalLines)
      const offset = num(input.offset)
      const limit = num(input.limit)
      const range = start !== undefined && end !== undefined
        ? `lines ${start}-${end}${total !== undefined ? ` of ${total}` : ""}`
        : offset !== undefined && limit !== undefined ? `lines ${offset}-${offset + limit - 1}`
        : offset !== undefined ? `from line ${offset}` : ""
      const content = str(display.text) ?? str(out.content)
      let body: ToolLine[]
      if (content !== undefined && display.type !== "directory") {
        const rows = split(content)
        const first = start ?? 1
        const width = String(first + rows.length - 1).length
        body = rows.map((row, index) => ({ text: `${String(first + index).padStart(width)}  ${row}`, tone: "muted" as const }))
      } else body = textLines(content ?? text)
      return { summary: [path, range].filter(Boolean).join(" · "), body: clipLines(body) }
    }
    case "edit":
    case "multiedit": {
      const path = str(input.path) ?? str(input.filePath) ?? str(input.file_path) ?? ""
      const diff = str(meta.diff)
      const lines = diff ? diffLines(diff) : editArgLines(input)
      return { summary: [path, lines.length ? counted(lines) : ""].filter(Boolean).join(" · "), body: clipLines(lines) }
    }
    case "write": {
      const path = str(input.path) ?? str(input.filePath) ?? ""
      const content = str(input.content)
      const lines = content === undefined ? [] : split(content).map((line) => ({ text: `+ ${line}`, tone: "add" as const }))
      return { summary: [path, content === undefined ? "" : plural(lines.length, "line")].filter(Boolean).join(" · "), body: clipLines(lines) }
    }
    case "apply_patch":
    case "patch": {
      const { files, lines } = patchLines(str(input.patchText) ?? str(input.patch) ?? "")
      return { summary: [files.join(", "), lines.length ? counted(lines) : ""].filter(Boolean).join(" · "), body: clipLines(lines) }
    }
    case "grep": {
      const matches = Array.isArray(out.matches) ? out.matches.map(record) : []
      const count = num(meta.matches) ?? num(out.total) ?? (output === undefined ? undefined : matches.length)
      const where = scope(str(input.pattern), str(input.path), true) + (str(input.glob) ? ` (${str(input.glob)})` : "")
      const body = matches.length
        ? matches.map((match) => ({ text: `${str(match.file) ?? ""}:${num(match.line) ?? ""}: ${str(match.text) ?? ""}`, tone: "muted" as const }))
        : textLines(text)
      return { summary: [where, count === undefined ? "" : plural(count, "match")].filter(Boolean).join(" · "), body: clipLines(body) }
    }
    case "glob":
    case "find": {
      const paths = Array.isArray(out.paths) ? out.paths.map((item) => typeof item === "string" ? item : str(record(item).path) ?? "") : []
      const count = num(meta.count) ?? num(out.total) ?? (output === undefined ? undefined : paths.length)
      const body = paths.length ? paths.map((path) => ({ text: path, tone: "muted" as const })) : textLines(text)
      return { summary: [scope(str(input.pattern), str(input.path)), count === undefined ? "" : plural(count, "file")].filter(Boolean).join(" · "), body: clipLines(body) }
    }
    case "ls": {
      const body = textLines(text)
      return { summary: [str(input.path) ?? ".", output === undefined ? "" : plural(body.length, "entry").replace("entrys", "entries")].filter(Boolean).join(" · "), body: clipLines(body) }
    }
    case "lsp": {
      const where = [str(input.filePath) ?? str(input.path), num(input.line), num(input.character)].filter((part) => part !== undefined).join(":")
      return { summary: [str(input.operation), where, str(input.query) ? `"${str(input.query)}"` : ""].filter(Boolean).join(" "), body: clipLines(textLines(text)) }
    }
    case "webfetch":
    case "fetch":
      return { summary: str(input.url) ?? "", body: clipLines(textLines(text)) }
    case "websearch":
    case "search":
      return { summary: str(input.query) !== undefined ? `"${str(input.query)}"` : "", body: clipLines(textLines(text)) }
    case "skill":
      return { summary: str(input.name) ?? "", body: [] }
    case "ask_user":
    case "question": {
      const first = record(Array.isArray(input.questions) ? input.questions[0] : undefined)
      const question = [str(first.header), str(first.question)].filter(Boolean).join(": ")
      return { summary: question, body: clipLines(textLines(text)) }
    }
    case "task": {
      const members = Array.isArray(input.members) ? input.members.map(record) : []
      const agent = str(input.subagent_type) || str(members[0]?.subagent_type) || str(meta.subagent_type) || "general"
      const description = str(input.description) ?? str(members[0]?.description) ?? str(out.title) ?? ""
      const child = str(meta.sessionId)
      return {
        summary: [agent, description, members.length > 1 ? `${members.length} members` : ""].filter(Boolean).join(" · "),
        body: [],
        task: { agent, description, ...(child ? { child } : {}) },
      }
    }
  }
  if (tool.startsWith("todo")) {
    const items = [meta.todos, out.todos, input.todos].find(Array.isArray) as unknown[] | undefined
    const done = items?.map(record).filter((item) => /completed/i.test(str(item.status) ?? "")).length ?? 0
    return { summary: items ? `${plural(items.length, "todo")} · ${done} done` : "", body: clipLines(todoLines(items ?? [])) }
  }
  const args = Object.keys(input).length ? JSON.stringify(input) : raw
  return {
    summary: args.length > argsSummaryLimit ? `${args.slice(0, argsSummaryLimit - 1)}…` : args,
    body: clipLines(textLines(text)),
  }
}

/** The card for one tool call. `options.command`: the command of this TUI's own shell turn (no input on the part yet). */
export function toolCard(call: ToolCallPart, options: { command?: string } = {}): ToolCardView {
  const tool = call.tool || "tool"
  const status = toolStatus(call.state)
  const raw = call.inputJson ?? ""
  let input = record(parse(raw))
  if (raw && !Object.keys(input).length && parse(raw) === undefined) {
    // Arguments still streaming: pick the main string fields out of the fragments.
    input = Object.fromEntries(streamingFields.map((key) => [key, partialField(raw, key)]).filter(([, value]) => value !== undefined))
  }
  const output = call.outputJson ? parse(call.outputJson) ?? call.outputJson : undefined
  const described = describe(tool, input, output, raw, options.command)
  const ms = call.durationMs === undefined ? undefined : Number(call.durationMs)
  const card: ToolCardView = { tool, status, summary: described.summary, body: described.body }
  if (status === "done" && ms !== undefined && Number.isFinite(ms)) card.duration = formatDuration(ms)
  if (status === "failed") card.error = call.errorMessage || "failed"
  if (described.command !== undefined) card.command = described.command
  if (described.task) card.task = described.task
  return card
}
