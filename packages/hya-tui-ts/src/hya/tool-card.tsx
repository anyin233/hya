import type { BoxRenderable, SyntaxStyle } from "@opentui/core"
import type { ToolPart } from "@opencode-ai/sdk/v2"
import { useRenderer, type JSX } from "@opentui/solid"
import { createMemo, createSignal, Show, type Accessor } from "solid-js"

import {
  createSyntaxStyleMemo,
  DEFAULT_THEMES,
  generateSyntax,
  resolveTheme,
  useTheme,
  type Theme,
} from "../upstream/context/theme"

/** Corner cells, border dashes, and the blank cell on each side of a border title. */
const TITLE_CHROME_COLUMNS = 6
/** Column budget for one argument value before the whole title is fitted to the card. */
const VALUE_COLUMNS = 96
/** Column budget for the assembled argument list of one call. */
const ARGUMENT_COLUMNS = 512
/** Character bound applied before collapsing whitespace so huge inputs stay cheap. */
const SOURCE_CHARS = 1024
const SHELL_TOOLS = new Set(["bash", "shell"])
/** Argument keys rendered through the session path formatter instead of verbatim. */
const PATH_KEYS = new Set(["path", "filePath", "cwd", "directory"])
/**
 * Failure markers that mean the call was refused rather than the tool failing.
 * Mirrors `PermissionError::Denied` in `crates/hya-tool/src/permission.rs`, which
 * covers both an explicit deny rule and a user reject.
 */
const DENIED_ERRORS = ["permission denied"]
const THEME_CONTEXT_ERROR = "Theme context must be used within a context provider"
const FALLBACK_THEME = resolveTheme(DEFAULT_THEMES.hya!, "dark")

export type ToolCardState = "pending" | "running" | "completed" | "error" | "denied"

export type ToolCardThemeState = {
  theme: Theme
  syntax?: Accessor<SyntaxStyle>
}

/**
 * Frame one tool call as a title bar plus padded content so it reads apart from
 * assistant prose. The title is fitted to the measured card width because the
 * renderer drops a border title that does not fit instead of clipping it.
 */
export function ToolCard(props: {
  title: string
  state: ToolCardState
  error?: string
  onClick?: () => void
  children?: JSX.Element
}): JSX.Element {
  const { theme } = useToolCardTheme()
  const renderer = useRenderer()
  const [columns, setColumns] = createSignal(0)
  const [hover, setHover] = createSignal(false)

  const title = createMemo(() => {
    const budget = columns() - TITLE_CHROME_COLUMNS
    if (budget <= 0) return undefined
    return ` ${truncateColumns(props.title, budget)} `
  })
  const borderColor = createMemo(() => {
    if (props.state === "error") return theme.error
    if (props.state === "pending" || props.state === "running") return theme.warning
    return theme.borderSubtle
  })
  const titleColor = createMemo(() => {
    if (props.state === "error") return theme.error
    if (props.state === "denied") return theme.textMuted
    return theme.accent
  })

  return (
    <box
      ref={(el: BoxRenderable) => {
        // The size hook runs inside the layout pass that resolves the width, so
        // the fitted title reaches the same frame that paints the border.
        el.onSizeChange = () => setColumns(el.width)
        setColumns(el.width)
      }}
      width="100%"
      flexShrink={0}
      marginTop={1}
      border={true}
      borderStyle="rounded"
      borderColor={borderColor()}
      title={title()}
      titleColor={titleColor()}
      backgroundColor={hover() && props.onClick ? theme.backgroundMenu : theme.backgroundPanel}
      paddingTop={1}
      paddingBottom={1}
      paddingLeft={2}
      paddingRight={2}
      gap={1}
      onMouseOver={() => props.onClick && setHover(true)}
      onMouseOut={() => setHover(false)}
      onMouseUp={() => {
        if (renderer.getSelection()?.getSelectedText()) return
        props.onClick?.()
      }}
    >
      {props.children}
      <Show when={props.error}>
        <text fg={props.state === "denied" ? theme.textMuted : theme.error} wrapMode="word" width="100%">
          {props.error}
        </text>
      </Show>
    </box>
  )
}

/** Compose one title bar: the command itself for shells, else the tool name and its call arguments. */
export function toolCardTitle(tool: string, input: unknown, formatPath?: (value: string) => string): string {
  const record = plainRecord(input)
  if (SHELL_TOOLS.has(tool)) {
    const command = typeof record?.command === "string" ? collapseWhitespace(record.command) : ""
    return command ? `$ ${command}` : tool
  }
  const args = record ? formatArguments(record, formatPath) : ""
  return args ? `${tool} [${args}]` : tool
}

/** Reduce one tool part to the state the card renders, keeping denial distinct from failure. */
export function toolCardState(part: ToolPart): ToolCardState {
  if (part.state.status === "pending") return "pending"
  if (part.state.status === "running") return "running"
  if (part.state.status === "completed") return "completed"
  const error = part.state.error ?? ""
  return DENIED_ERRORS.some((marker) => error.includes(marker)) ? "denied" : "error"
}

/** Read the failure text of one tool part, if it has one. */
export function toolCardError(part: ToolPart): string | undefined {
  return part.state.status === "error" ? part.state.error : undefined
}

/** Fit text to a column budget, marking the cut with an ellipsis. */
export function truncateColumns(text: string, columns: number): string {
  if (columns <= 0) return ""
  if (Bun.stringWidth(text) <= columns) return text
  let width = 0
  let fitted = ""
  for (const char of text) {
    const next = width + Bun.stringWidth(char)
    if (next > columns - 1) break
    width = next
    fitted += char
  }
  return `${fitted}…`
}

/** Use the live theme, falling back to the shipped hya theme for isolated renderer tests. */
export function useToolCardTheme(): ToolCardThemeState {
  try {
    return useTheme()
  } catch (error) {
    if (!(error instanceof Error) || error.message !== THEME_CONTEXT_ERROR) throw error
    return { theme: FALLBACK_THEME }
  }
}

/** Return the live syntax accessor or create a renderer-owned fallback style. */
export function toolCardSyntax(state: ToolCardThemeState): Accessor<SyntaxStyle> {
  return state.syntax ?? createSyntaxStyleMemo(() => generateSyntax(state.theme))
}

/** Render the call arguments of one tool as a single `key=value` line. */
function formatArguments(record: Record<string, unknown>, formatPath?: (value: string) => string): string {
  const entries: string[] = []
  let columns = 0
  for (const [key, value] of Object.entries(record)) {
    const formatted = formatValue(value, formatPath && PATH_KEYS.has(key) ? formatPath : undefined)
    if (formatted === undefined) continue
    entries.push(`${key}=${formatted}`)
    columns += key.length + formatted.length + 3
    if (columns >= ARGUMENT_COLUMNS) break
  }
  return entries.join(", ")
}

/** Flatten one argument value onto a single bounded line, or drop it when absent. */
function formatValue(value: unknown, formatPath?: (value: string) => string): string | undefined {
  if (value === undefined) return undefined
  if (value === null) return "null"
  if (typeof value === "string") {
    const text = formatPath ? formatPath(value) || value : value
    return truncateColumns(collapseWhitespace(text), VALUE_COLUMNS)
  }
  if (typeof value === "number" || typeof value === "boolean") return String(value)
  if (Array.isArray(value)) return `[${value.length} item${value.length === 1 ? "" : "s"}]`
  if (typeof value === "object") return "{…}"
  return undefined
}

/** Collapse a bounded prefix of one value into a single line. */
function collapseWhitespace(text: string): string {
  return text.slice(0, SOURCE_CHARS).replace(/\s+/g, " ").trim()
}

/** Require a plain argument record. */
function plainRecord(value: unknown): Record<string, unknown> | undefined {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return undefined
  return value as Record<string, unknown>
}
