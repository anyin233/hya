import { TextAttributes, type SyntaxStyle } from "@opentui/core"
import type { ToolPart } from "@opencode-ai/sdk/v2"
import type { JSX } from "@opentui/solid"
import { createMemo, createSignal, For, Match, Show, Switch, type Accessor } from "solid-js"
import stripAnsi from "strip-ansi"

import {
  createSyntaxStyleMemo,
  DEFAULT_THEMES,
  generateSyntax,
  resolveTheme,
  useTheme,
  type Theme,
} from "../upstream/context/theme"
import { collapseToolOutput } from "../upstream/util/collapse-tool-output"
import { filetype } from "../upstream/util/filetype"

const MAX_TITLE_LENGTH = 8 * 1024
const MAX_PATH_LENGTH = 16 * 1024
const MAX_PATTERN_LENGTH = 16 * 1024
const MAX_COMMAND_LENGTH = 64 * 1024
const MAX_DISPLAY_TEXT_LENGTH = 512 * 1024
const MAX_DIFF_LENGTH = 1024 * 1024
const MAX_OUTPUT_LENGTH = 512 * 1024
const MAX_DIAGNOSTIC_LENGTH = 16 * 1024
const MAX_GREP_GROUPS = 200
const MAX_GREP_ROWS = 4_096
const MAX_DIAGNOSTICS = 200
const BACKEND_TRUNCATION_FLAGS = [
  "outputTruncated",
  "titleTruncated",
  "displayTruncated",
  "diffTruncated",
  "attachmentsTruncated",
  "diagnosticsTruncated",
  "warningsTruncated",
  "rowsTruncated",
  "groupsTruncated",
  "metadataTruncated",
  "unknownFieldsDropped",
  "envelopeTruncated",
] as const
const CODE_PREVIEW_LINES = 16
const OUTPUT_PREVIEW_LINES = 10
const MIN_PREVIEW_COLUMNS = 20
const PANEL_CHROME_COLUMNS = 6
const FALLBACK_THEME = resolveTheme(DEFAULT_THEMES.hya!, "dark")
const THEME_CONTEXT_ERROR = "Theme context must be used within a context provider"

type DiagnosticPoint = {
  line: number
  character: number
}

type CodingToolDiagnostic = {
  message: string
  severity?: number
  range?: {
    start: DiagnosticPoint
    end?: DiagnosticPoint
  }
}

type CodingToolGrepRow = {
  line: number
  text: string
  isMatch: boolean
}

type CodingToolGrepGroup = {
  path: string
  rows: CodingToolGrepRow[]
}

export type CodingToolView =
  | {
      kind: "read-code"
      title: string
      path: string
      text: string
      lineStart: number
      truncated: boolean
    }
  | {
      kind: "write-code"
      title: string
      path: string
      text: string
      lineStart: number
      truncated: boolean
      diagnostics: CodingToolDiagnostic[]
    }
  | {
      kind: "edit-diff"
      title: string
      path: string
      diff: string
      truncated: boolean
      diagnostics: CodingToolDiagnostic[]
    }
  | {
      kind: "grep-output"
      title: string
      pattern: string
      groups: CodingToolGrepGroup[]
      output: string
      truncated: boolean
    }
  | {
      kind: "shell-output"
      title: string
      command: string
      cwd?: string
      output: string
      exit?: number
      signal?: string
      timedOut: boolean
      truncated: boolean
      status: string
    }

type CodingToolPresentationProps = {
  /** A projected part used by direct callers; route callers pass the normalized view. */
  part?: ToolPart
  /** A view produced by the route's single presentation-adapter invocation. */
  view?: CodingToolView
  width: number
  diffStyle: "auto" | "stacked" | undefined
  diffWrapMode: "word" | "none"
}

type PresentationThemeState = {
  theme: Theme
  syntax?: Accessor<SyntaxStyle>
}

type CompletedEnvelope = {
  tool: "read" | "edit" | "write" | "grep" | "bash" | "shell"
  title: string
  input: Record<string, unknown>
  metadata: Record<string, unknown>
  output: string
}

class InvalidCodingToolData extends Error {}

/** Normalize one completed SDK coding-tool part into a bounded allowlisted view. */
export function presentCodingTool(part: ToolPart): CodingToolView | undefined {
  try {
    const envelope = completedEnvelope(part)
    if (!envelope) return undefined

    switch (envelope.tool) {
      case "read":
        return presentRead(envelope)
      case "write":
        return presentWrite(envelope)
      case "edit":
        return presentEdit(envelope)
      case "grep":
        return presentGrep(envelope)
      case "bash":
      case "shell":
        return presentShell(envelope)
    }
  } catch (error) {
    if (error instanceof InvalidCodingToolData) return undefined
    throw error
  }
}

/** Render one valid completed coding-tool part without owning transport or replay state. */
export function CodingToolPresentation(props: CodingToolPresentationProps): JSX.Element {
  const view = createMemo(() => props.view ?? (props.part ? presentCodingTool(props.part) : undefined))
  return (
    <Switch>
      <Match when={view()?.kind === "read-code" ? (view() as Extract<CodingToolView, { kind: "read-code" }>) : undefined}>
        {(current) => <CodePresentation view={current()} width={props.width} />}
      </Match>
      <Match when={view()?.kind === "write-code" ? (view() as Extract<CodingToolView, { kind: "write-code" }>) : undefined}>
        {(current) => <CodePresentation view={current()} width={props.width} />}
      </Match>
      <Match when={view()?.kind === "edit-diff" ? (view() as Extract<CodingToolView, { kind: "edit-diff" }>) : undefined}>
        {(current) => (
          <EditPresentation
            view={current()}
            width={props.width}
            diffStyle={props.diffStyle}
            diffWrapMode={props.diffWrapMode}
          />
        )}
      </Match>
      <Match when={view()?.kind === "grep-output" ? (view() as Extract<CodingToolView, { kind: "grep-output" }>) : undefined}>
        {(current) => <GrepPresentation view={current()} />}
      </Match>
      <Match when={view()?.kind === "shell-output" ? (view() as Extract<CodingToolView, { kind: "shell-output" }>) : undefined}>
        {(current) => <ShellPresentation view={current()} width={props.width} />}
      </Match>
    </Switch>
  )
}

/** Render Read and Write source through the shared code and line-number primitives. */
function CodePresentation(props: {
  view: Extract<CodingToolView, { kind: "read-code" | "write-code" }>
  width: number
}): JSX.Element {
  const themeState = usePresentationTheme()
  const syntax = presentationSyntax(themeState)
  const [expanded, setExpanded] = createSignal(false)
  const maxChars = createMemo(() => CODE_PREVIEW_LINES * Math.max(MIN_PREVIEW_COLUMNS, props.width - PANEL_CHROME_COLUMNS))
  const collapsed = createMemo(() => collapseToolOutput(props.view.text, CODE_PREVIEW_LINES, maxChars()))
  const content = createMemo(() => (expanded() || !collapsed().overflow ? props.view.text : collapsed().output))
  const diagnostics = () => (props.view.kind === "write-code" ? props.view.diagnostics : [])

  return (
    <ToolPanel title={props.view.title} path={props.view.path}>
      <line_number
        fg={themeState.theme.textMuted}
        minWidth={3}
        paddingRight={1}
        lineNumberOffset={props.view.lineStart - 1}
        width="100%"
      >
        <code
          conceal={false}
          fg={themeState.theme.text}
          filetype={filetype(props.view.path)}
          syntaxStyle={syntax()}
          content={content()}
          wrapMode="word"
          width="100%"
        />
      </line_number>
      <For each={diagnostics()}>{(diagnostic) => <DiagnosticText diagnostic={diagnostic} />}</For>
      <Show when={props.view.truncated}>
        <text fg={themeState.theme.warning}>Result truncated</text>
      </Show>
      <Show when={collapsed().overflow}>
        <text fg={themeState.theme.textMuted} onMouseUp={() => setExpanded((value) => !value)}>
          {expanded() ? "Click to collapse" : "Click to expand"}
        </text>
      </Show>
    </ToolPanel>
  )
}

/** Render an Edit with the retained semantic diff primitive and responsive mode. */
function EditPresentation(props: {
  view: Extract<CodingToolView, { kind: "edit-diff" }>
  width: number
  diffStyle: CodingToolPresentationProps["diffStyle"]
  diffWrapMode: CodingToolPresentationProps["diffWrapMode"]
}): JSX.Element {
  const themeState = usePresentationTheme()
  const syntax = presentationSyntax(themeState)
  const diffView = createMemo<"unified" | "split">(() =>
    props.diffStyle === "stacked" || props.width <= 120 ? "unified" : "split",
  )

  return (
    <ToolPanel title={props.view.title} path={props.view.path}>
      <diff
        diff={props.view.diff}
        view={diffView()}
        filetype={filetype(props.view.path)}
        syntaxStyle={syntax()}
        showLineNumbers={true}
        width="100%"
        wrapMode={props.diffWrapMode}
        fg={themeState.theme.text}
        addedBg={themeState.theme.diffAddedBg}
        removedBg={themeState.theme.diffRemovedBg}
        contextBg={themeState.theme.diffContextBg}
        addedSignColor={themeState.theme.diffHighlightAdded}
        removedSignColor={themeState.theme.diffHighlightRemoved}
        lineNumberFg={themeState.theme.diffLineNumber}
        lineNumberBg={themeState.theme.diffContextBg}
        addedLineNumberBg={themeState.theme.diffAddedLineNumberBg}
        removedLineNumberBg={themeState.theme.diffRemovedLineNumberBg}
      />
      <For each={props.view.diagnostics}>{(diagnostic) => <DiagnosticText diagnostic={diagnostic} />}</For>
      <Show when={props.view.truncated}>
        <text fg={themeState.theme.warning}>Diff truncated</text>
      </Show>
    </ToolPanel>
  )
}

/** Render bounded Grep groups with explicit match markers and source line identity. */
function GrepPresentation(props: { view: Extract<CodingToolView, { kind: "grep-output" }> }): JSX.Element {
  const themeState = usePresentationTheme()
  const syntax = presentationSyntax(themeState)

  return (
    <ToolPanel title={props.view.title}>
      <Show when={!props.view.title.includes(props.view.pattern)}>
        <text fg={themeState.theme.textMuted}>Pattern: {props.view.pattern}</text>
      </Show>
      <Show when={props.view.groups.length > 0} fallback={<text fg={themeState.theme.textMuted}>No matches</text>}>
        <For each={props.view.groups}>
          {(group) => (
            <box gap={0} width="100%">
              <text fg={themeState.theme.textMuted} attributes={TextAttributes.BOLD} wrapMode="word" width="100%">
                {group.path}
              </text>
              <For each={group.rows}>
                {(row) => (
                  <box flexDirection="row" width="100%">
                    <text fg={row.isMatch ? themeState.theme.accent : themeState.theme.textMuted} width={2} flexShrink={0}>
                      {row.isMatch ? ">" : "·"}
                    </text>
                    <line_number
                      fg={themeState.theme.textMuted}
                      minWidth={3}
                      paddingRight={1}
                      lineNumberOffset={row.line - 1}
                      flexGrow={1}
                    >
                      <code
                        conceal={false}
                        fg={themeState.theme.text}
                        filetype={filetype(group.path)}
                        syntaxStyle={syntax()}
                        content={row.text}
                        wrapMode="word"
                        width="100%"
                      />
                    </line_number>
                  </box>
                )}
              </For>
            </box>
          )}
        </For>
      </Show>
      <Show when={props.view.truncated}>
        <text fg={themeState.theme.warning}>Results truncated</text>
      </Show>
    </ToolPanel>
  )
}

/** Render one Bash or Shell result with highlighted command and plain safe output. */
function ShellPresentation(props: {
  view: Extract<CodingToolView, { kind: "shell-output" }>
  width: number
}): JSX.Element {
  const themeState = usePresentationTheme()
  const syntax = presentationSyntax(themeState)
  const [expanded, setExpanded] = createSignal(false)
  const maxChars = createMemo(() => OUTPUT_PREVIEW_LINES * Math.max(MIN_PREVIEW_COLUMNS, props.width - PANEL_CHROME_COLUMNS))
  const collapsed = createMemo(() => collapseToolOutput(props.view.output, OUTPUT_PREVIEW_LINES, maxChars()))
  const output = createMemo(() => (expanded() || !collapsed().overflow ? props.view.output : collapsed().output))

  return (
    <ToolPanel title={props.view.title}>
      <box flexDirection="row" width="100%">
        <text fg={themeState.theme.accent} flexShrink={0}>$ </text>
        <code
          conceal={false}
          fg={themeState.theme.accent}
          filetype="bash"
          syntaxStyle={syntax()}
          content={props.view.command}
          wrapMode="word"
          width="100%"
        />
      </box>
      <Show when={props.view.cwd}>
        <text fg={themeState.theme.textMuted} width="100%">cwd {props.view.cwd}</text>
      </Show>
      <Show when={output()}>
        <text fg={themeState.theme.text} wrapMode="word" width="100%">
          {output()}
        </text>
      </Show>
      <text fg={props.view.exit === 0 && !props.view.timedOut ? themeState.theme.success : themeState.theme.warning}>
        {props.view.status}
      </text>
      <Show when={collapsed().overflow}>
        <text fg={themeState.theme.textMuted} onMouseUp={() => setExpanded((value) => !value)}>
          {expanded() ? "Click to collapse" : "Click to expand"}
        </text>
      </Show>
    </ToolPanel>
  )
}

/** Provide a compact borderless semantic surface for one coding-tool result. */
function ToolPanel(props: { title: string; path?: string; children: JSX.Element }): JSX.Element {
  const { theme } = usePresentationTheme()

  return (
    <box
      backgroundColor={theme.backgroundPanel}
      paddingTop={1}
      paddingBottom={1}
      paddingLeft={1}
      paddingRight={1}
      gap={1}
      width="100%"
    >
      <box flexDirection="row" gap={1} width="100%">
        <text fg={theme.textMuted} attributes={TextAttributes.BOLD} wrapMode="word">
          {props.title}
        </text>
        <Show when={props.path && !props.title.includes(props.path)}>
          <text fg={theme.textMuted} wrapMode="word">
            {props.path}
          </text>
        </Show>
      </box>
      {props.children}
    </box>
  )
}

/** Render one positioned severity-one diagnostic with one-based coordinates. */
function DiagnosticText(props: { diagnostic: CodingToolDiagnostic }): JSX.Element {
  const { theme } = usePresentationTheme()
  const location = () => {
    const start = props.diagnostic.range?.start
    return start ? ` [${start.line + 1}:${start.character + 1}]` : ""
  }
  return (
    <text fg={theme.error}>
      Error{location()} {props.diagnostic.message}
    </text>
  )
}

/** Use the live Theme context, with the shipped semantic hya theme for isolated renderer tests. */
function usePresentationTheme(): PresentationThemeState {
  try {
    return useTheme()
  } catch (error) {
    if (!(error instanceof Error) || error.message !== THEME_CONTEXT_ERROR) throw error
    return { theme: FALLBACK_THEME }
  }
}

/** Return the live syntax accessor or create a renderer-owned fallback style. */
function presentationSyntax(state: PresentationThemeState): Accessor<SyntaxStyle> {
  return state.syntax ?? createSyntaxStyleMemo(() => generateSyntax(state.theme))
}

/** Decode the common completed ToolPart envelope and reject compacted or unsupported state. */
function completedEnvelope(part: ToolPart): CompletedEnvelope | undefined {
  const rawPart = object(part, "part")
  if (rawPart.type !== "tool") return undefined
  const tool = rawPart.tool
  if (tool !== "read" && tool !== "edit" && tool !== "write" && tool !== "grep" && tool !== "bash" && tool !== "shell") {
    return undefined
  }

  const state = object(rawPart.state, "part.state")
  if (state.status !== "completed") return undefined
  if (state.time !== undefined) {
    const time = object(state.time, "part.state.time")
    if (optionalBoolean(time.compacted, "part.state.time.compacted") === true) return undefined
  }

  return {
    tool,
    title: boundedString(state.title, "part.state.title", MAX_TITLE_LENGTH),
    input: object(state.input, "part.state.input"),
    metadata: state.metadata === undefined ? {} : object(state.metadata, "part.state.metadata"),
    output: boundedString(state.output, "part.state.output", MAX_OUTPUT_LENGTH),
  }
}

/** Normalize one completed Read file payload. */
function presentRead(envelope: CompletedEnvelope): Extract<CodingToolView, { kind: "read-code" }> {
  assertKnownKeys(envelope.input, "read.input", ["path", "filePath", "offset", "limit", "raw"])
  readInputPath(envelope.input)
  optionalInteger(envelope.input.offset, "read.input.offset", 0)
  optionalInteger(envelope.input.limit, "read.input.limit", 1)
  optionalBoolean(envelope.input.raw, "read.input.raw")

  const display = object(envelope.metadata.display, "read.metadata.display")
  if (display.type !== "file") invalid("read.metadata.display.type: expected file")
  const displayTruncated = optionalBoolean(display.truncated, "read.metadata.display.truncated")
  const metadataTruncated = combinedTruncation(envelope.metadata, "read.metadata")

  return {
    kind: "read-code",
    title: envelope.title,
    path: nonEmptyBoundedString(display.path, "read.metadata.display.path", MAX_PATH_LENGTH),
    text: boundedString(display.text, "read.metadata.display.text", MAX_DISPLAY_TEXT_LENGTH),
    lineStart: positiveInteger(display.lineStart, "read.metadata.display.lineStart"),
    truncated: displayTruncated === true || metadataTruncated,
  }
}

/** Normalize one completed Write request and authoritative final preview when present. */
function presentWrite(envelope: CompletedEnvelope): Extract<CodingToolView, { kind: "write-code" }> {
  assertKnownKeys(envelope.input, "write.input", ["path", "content"])
  const inputPath = nonEmptyBoundedString(envelope.input.path, "write.input.path", MAX_PATH_LENGTH)
  const inputText = boundedString(envelope.input.content, "write.input.content", MAX_DISPLAY_TEXT_LENGTH)
  const display = optionalObject(envelope.metadata.display, "write.metadata.display")
  if (display && display.type !== "file") invalid("write.metadata.display.type: expected file")

  const path = display
    ? nonEmptyBoundedString(display.path, "write.metadata.display.path", MAX_PATH_LENGTH)
    : inputPath
  const text = display ? boundedString(display.text, "write.metadata.display.text", MAX_DISPLAY_TEXT_LENGTH) : inputText
  const lineStart = display?.lineStart === undefined ? 1 : positiveInteger(display.lineStart, "write.metadata.display.lineStart")
  const displayTruncated = display ? optionalBoolean(display.truncated, "write.metadata.display.truncated") : undefined
  const metadataTruncated = combinedTruncation(envelope.metadata, "write.metadata")

  return {
    kind: "write-code",
    title: envelope.title,
    path,
    text,
    lineStart,
    truncated: displayTruncated === true || metadataTruncated,
    diagnostics: diagnostics(envelope.metadata.diagnostics, path, "write.metadata.diagnostics"),
  }
}

/** Normalize one completed Edit diff and its allowlisted diagnostics. */
function presentEdit(envelope: CompletedEnvelope): Extract<CodingToolView, { kind: "edit-diff" }> {
  assertKnownKeys(envelope.input, "edit.input", ["path", "edits"])
  const inputPath = nonEmptyBoundedString(envelope.input.path, "edit.input.path", MAX_PATH_LENGTH)
  const display = optionalObject(envelope.metadata.display, "edit.metadata.display")
  if (display && display.type !== "file") invalid("edit.metadata.display.type: expected file")
  const path = display
    ? nonEmptyBoundedString(display.path, "edit.metadata.display.path", MAX_PATH_LENGTH)
    : inputPath
  const edits = array(envelope.input.edits, "edit.input.edits")
  for (const [index, edit] of edits.entries()) object(edit, `edit.input.edits[${index}]`)

  return {
    kind: "edit-diff",
    title: envelope.title,
    path,
    diff: boundedString(envelope.metadata.diff, "edit.metadata.diff", MAX_DIFF_LENGTH),
    truncated:
      combinedTruncation(envelope.metadata, "edit.metadata") ||
      optionalBoolean(display?.truncated, "edit.metadata.display.truncated") === true,
    diagnostics: diagnostics(envelope.metadata.diagnostics, path, "edit.metadata.diagnostics"),
  }
}

/** Normalize bounded per-file Grep display metadata. */
function presentGrep(envelope: CompletedEnvelope): Extract<CodingToolView, { kind: "grep-output" }> {
  assertKnownKeys(envelope.input, "grep.input", ["pattern", "path", "glob", "ignoreCase", "literal", "context", "limit"])
  const pattern = boundedString(envelope.input.pattern, "grep.input.pattern", MAX_PATTERN_LENGTH)
  optionalBoundedString(envelope.input.path, "grep.input.path", MAX_PATH_LENGTH)
  optionalBoundedString(envelope.input.glob, "grep.input.glob", MAX_PATTERN_LENGTH)
  optionalBoolean(envelope.input.ignoreCase, "grep.input.ignoreCase")
  optionalBoolean(envelope.input.literal, "grep.input.literal")
  optionalInteger(envelope.input.context, "grep.input.context", 0, 5)
  optionalInteger(envelope.input.limit, "grep.input.limit", 1, 200)

  const display = object(envelope.metadata.display, "grep.metadata.display")
  const rawGroups = array(display.groups, "grep.metadata.display.groups")
  if (rawGroups.length > MAX_GREP_GROUPS) invalid("grep.metadata.display.groups: too many groups")

  let rowCount = 0
  let displayLength = 0
  const groups = rawGroups.map((value, groupIndex) => {
    const group = object(value, `grep.metadata.display.groups[${groupIndex}]`)
    const groupPath = nonEmptyBoundedString(group.path, `grep.metadata.display.groups[${groupIndex}].path`, MAX_PATH_LENGTH)
    const rawRows = array(group.rows, `grep.metadata.display.groups[${groupIndex}].rows`)
    rowCount += rawRows.length
    displayLength += groupPath.length
    if (displayLength > MAX_DISPLAY_TEXT_LENGTH) invalid("grep.metadata.display.groups: display text exceeds limit")
    if (rowCount > MAX_GREP_ROWS) invalid("grep.metadata.display.groups: too many rows")
    return {
      path: groupPath,
      rows: rawRows.map((value, rowIndex) => {
        const rowPath = `grep.metadata.display.groups[${groupIndex}].rows[${rowIndex}]`
        const row = object(value, rowPath)
        const text = boundedString(row.text, `${rowPath}.text`, MAX_DISPLAY_TEXT_LENGTH)
        displayLength += text.length
        if (displayLength > MAX_DISPLAY_TEXT_LENGTH) invalid("grep.metadata.display.groups: display text exceeds limit")
        return {
          line: positiveInteger(row.line, `${rowPath}.line`),
          text,
          isMatch: boolean(row.isMatch, `${rowPath}.isMatch`),
        }
      }),
    }
  })

  return {
    kind: "grep-output",
    title: envelope.title,
    pattern,
    groups,
    output: envelope.output,
    truncated:
      combinedTruncation(envelope.metadata, "grep.metadata") ||
      optionalBoolean(display.truncated, "grep.metadata.display.truncated") === true,
  }
}

/** Normalize Bash and hidden Shell alias data without retaining environment fields. */
function presentShell(envelope: CompletedEnvelope): Extract<CodingToolView, { kind: "shell-output" }> {
  assertKnownKeys(envelope.input, "shell.input", ["command", "env", "timeout", "cwd", "pty"])
  const command = nonEmptyBoundedString(envelope.input.command, "shell.input.command", MAX_COMMAND_LENGTH)
  const cwd = optionalBoundedString(envelope.input.cwd, "shell.input.cwd", MAX_PATH_LENGTH)
  optionalFiniteNumber(envelope.input.timeout, "shell.input.timeout", 0)
  optionalBoolean(envelope.input.pty, "shell.input.pty")
  validateEnvironment(envelope.input.env)

  const exit = nullableInteger(envelope.metadata.exit, "shell.metadata.exit", 0)
  const signal = optionalBoundedString(envelope.metadata.signal, "shell.metadata.signal", 256)
  const timedOut = optionalBoolean(envelope.metadata.timedOut, "shell.metadata.timedOut") ?? false
  const truncated = combinedTruncation(envelope.metadata, "shell.metadata")

  return {
    kind: "shell-output",
    title: envelope.title,
    command,
    cwd,
    output: stripAnsi(envelope.output),
    exit,
    signal,
    timedOut,
    truncated,
    status: shellStatus(exit, signal, timedOut, truncated),
  }
}


/** Validate an optional environment map without retaining any key or value. */
function validateEnvironment(value: unknown): void {
  if (value === undefined) return
  const environment = object(value, "shell.input.env")
  for (const environmentValue of Object.values(environment)) {
    if (typeof environmentValue !== "string") invalid("shell.input.env: expected string values")
  }
}

/** Derive a compact status while preserving timeout, signal, and truncation facts. */
function shellStatus(exit: number | undefined, signal: string | undefined, timedOut: boolean, truncated: boolean): string {
  const base = timedOut
    ? "Timed out"
    : signal
      ? `Terminated · ${signal}`
      : exit === undefined
        ? "Completed"
        : exit === 0
          ? "Completed · exit 0"
          : `Failed · exit ${exit}`
  return truncated ? `${base} · output truncated` : base
}


/** Reject fields outside one tool's documented model-facing input schema. */
function assertKnownKeys(input: Record<string, unknown>, path: string, allowed: readonly string[]): void {
  for (const key of Object.keys(input)) {
    if (!allowed.includes(key)) invalid(`${path}.${key}: unsupported field`)
  }
}

/** Combine the envelope truncation bit with every backend-owned result-cap flag. */
function combinedTruncation(metadata: Record<string, unknown>, path: string): boolean {
  let truncated = optionalBoolean(metadata.truncated, `${path}.truncated`) === true
  for (const flag of BACKEND_TRUNCATION_FLAGS) {
    truncated = optionalBoolean(metadata[flag], `${path}.${flag}`) === true || truncated
  }
  return truncated
}

/** Decode the nullable process exit code used for timeout and signal completion. */
function nullableInteger(value: unknown, path: string, minimum: number): number | undefined {
  return value === null ? undefined : optionalInteger(value, path, minimum)
}
/** Resolve canonical and legacy Read path spellings without copying either into the view. */
function readInputPath(input: Record<string, unknown>): string {
  const canonical = optionalBoundedString(input.path, "read.input.path", MAX_PATH_LENGTH)
  const legacy = optionalBoundedString(input.filePath, "read.input.filePath", MAX_PATH_LENGTH)
  const canonicalValue = canonical && canonical.length > 0 ? canonical : undefined
  const legacyValue = legacy && legacy.length > 0 ? legacy : undefined
  if (canonicalValue && legacyValue && canonicalValue !== legacyValue) invalid("read.input: conflicting path values")
  const value = canonicalValue ?? legacyValue
  if (!value) invalid("read.input.path: expected non-empty string")
  return value
}

/** Normalize diagnostic arrays or the retained path-keyed diagnostic envelope. */
function diagnostics(value: unknown, path: string, valuePath: string): CodingToolDiagnostic[] {
  if (value === undefined) return []
  let values: unknown[]
  if (Array.isArray(value)) {
    values = value
  } else {
    const byPath = object(value, valuePath)
    const selected = byPath[path]
    if (selected === undefined) return []
    values = array(selected, `${valuePath}.${path}`)
  }
  if (values.length > MAX_DIAGNOSTICS) invalid(`${valuePath}: too many diagnostics`)

  const result: CodingToolDiagnostic[] = []
  for (const [index, entry] of values.entries()) {
    const diagnosticValue = diagnostic(entry, `${valuePath}[${index}]`)
    if (diagnosticValue.severity !== 1 || diagnosticValue.range?.start === undefined) continue
    if (result.length < 3) result.push(diagnosticValue)
  }
  return result
}

/** Copy only supported fields from one diagnostic object. */
function diagnostic(value: unknown, path: string): CodingToolDiagnostic {
  const input = object(value, path)
  const result: CodingToolDiagnostic = {
    message: boundedString(input.message, `${path}.message`, MAX_DIAGNOSTIC_LENGTH),
  }
  const severity = optionalInteger(input.severity, `${path}.severity`, 0)
  if (severity !== undefined) result.severity = severity
  if (input.range !== undefined) {
    const range = object(input.range, `${path}.range`)
    result.range = {
      start: diagnosticPoint(range.start, `${path}.range.start`),
    }
  }
  return result
}


/** Decode one diagnostic line and character point. */
function diagnosticPoint(value: unknown, path: string): DiagnosticPoint {
  const point = object(value, path)
  return {
    line: nonNegativeInteger(point.line, `${path}.line`),
    character: nonNegativeInteger(point.character, `${path}.character`),
  }
}

/** Require a non-array object. */
function object(value: unknown, path: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) invalid(`${path}: expected object`)
  return value as Record<string, unknown>
}

/** Decode an optional non-array object. */
function optionalObject(value: unknown, path: string): Record<string, unknown> | undefined {
  return value === undefined ? undefined : object(value, path)
}

/** Require an array. */
function array(value: unknown, path: string): unknown[] {
  if (!Array.isArray(value)) invalid(`${path}: expected array`)
  return value
}

/** Require a bounded string. */
function boundedString(value: unknown, path: string, maximum: number): string {
  if (typeof value !== "string") invalid(`${path}: expected string`)
  if (value.length > maximum) invalid(`${path}: string exceeds display limit`)
  return value
}

/** Require a non-empty bounded string. */
function nonEmptyBoundedString(value: unknown, path: string, maximum: number): string {
  const result = boundedString(value, path, maximum)
  if (result.length === 0) invalid(`${path}: expected non-empty string`)
  return result
}

/** Decode an optional bounded string. */
function optionalBoundedString(value: unknown, path: string, maximum: number): string | undefined {
  return value === undefined ? undefined : boundedString(value, path, maximum)
}

/** Require a boolean. */
function boolean(value: unknown, path: string): boolean {
  if (typeof value !== "boolean") invalid(`${path}: expected boolean`)
  return value
}

/** Decode an optional boolean. */
function optionalBoolean(value: unknown, path: string): boolean | undefined {
  return value === undefined ? undefined : boolean(value, path)
}

/** Require a non-negative safe integer. */
function nonNegativeInteger(value: unknown, path: string): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) invalid(`${path}: expected non-negative integer`)
  return value
}

/** Require a positive safe integer. */
function positiveInteger(value: unknown, path: string): number {
  const result = nonNegativeInteger(value, path)
  if (result === 0) invalid(`${path}: expected positive integer`)
  return result
}

/** Decode an optional safe integer in an inclusive range. */
function optionalInteger(value: unknown, path: string, minimum: number, maximum = Number.MAX_SAFE_INTEGER): number | undefined {
  if (value === undefined) return undefined
  const result = nonNegativeInteger(value, path)
  if (result < minimum || result > maximum) invalid(`${path}: integer outside supported range`)
  return result
}

/** Decode an optional finite number with a lower bound. */
function optionalFiniteNumber(value: unknown, path: string, minimum: number): number | undefined {
  if (value === undefined) return undefined
  if (typeof value !== "number" || !Number.isFinite(value) || value < minimum) invalid(`${path}: expected finite number`)
  return value
}

/** Stop normalization with a typed validation failure. */
function invalid(message: string): never {
  throw new InvalidCodingToolData(message)
}
