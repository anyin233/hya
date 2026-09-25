/** The TUI palette. Every component reads colors from here. */
export const colors = {
  bg: "#11151b",
  panel: "#1c2530",
  fg: "#e8edf3",
  muted: "#9caab9",
  accent: "#73c8e8",
  border: "#405366",
  /** Failed turns and tool errors. */
  error: "#f07878",
  /** Noteworthy finishes: length limit, cancelled. */
  warning: "#e5c07b",
} as const

/** Token colors for Markdown and highlighted code blocks (tree-sitter capture names in syntaxStyles). */
export const syntaxColors = {
  keyword: "#c792ea",
  string: "#a5d6a7",
  number: "#f78c6c",
  comment: "#7a8a9c",
  function: "#82aaff",
  type: "#ffcb6b",
  operator: "#89ddff",
  inlineCode: "#f2a97a",
} as const

export interface TextStyle {
  fg?: string
  bg?: string
  bold?: boolean
  italic?: boolean
  underline?: boolean
  dim?: boolean
}

/**
 * Styles by tree-sitter capture / Markdown scope name. A dotted name falls back
 * to its first segment (`keyword.return` → `keyword`), so the base names cover
 * most captures. Used to build the OpenTUI `SyntaxStyle` in components/Markdown.tsx.
 */
export const syntaxStyles: Record<string, TextStyle> = {
  default: { fg: colors.fg },
  conceal: { fg: colors.border },
  "markup.heading": { fg: colors.accent, bold: true },
  ...Object.fromEntries([1, 2, 3, 4, 5, 6].map((level) => [`markup.heading.${level}`, { fg: colors.accent, bold: true }])),
  "markup.strong": { fg: colors.fg, bold: true },
  "markup.italic": { fg: colors.fg, italic: true },
  "markup.strikethrough": { fg: colors.muted },
  "markup.raw": { fg: syntaxColors.inlineCode },
  "markup.raw.block": { fg: colors.fg },
  "markup.link": { fg: colors.muted },
  "markup.link.label": { fg: colors.accent, underline: true },
  "markup.link.url": { fg: colors.muted, underline: true },
  "markup.list": { fg: colors.accent },
  "markup.list.checked": { fg: colors.accent },
  "markup.list.unchecked": { fg: colors.muted },
  "markup.quote": { fg: colors.muted, italic: true },
  label: { fg: colors.muted },
  "punctuation.special": { fg: colors.muted },
  keyword: { fg: syntaxColors.keyword },
  string: { fg: syntaxColors.string },
  "string.escape": { fg: syntaxColors.operator },
  "string.special": { fg: syntaxColors.string },
  number: { fg: syntaxColors.number },
  boolean: { fg: syntaxColors.number },
  constant: { fg: syntaxColors.number },
  "constant.builtin": { fg: syntaxColors.number },
  comment: { fg: syntaxColors.comment, italic: true },
  function: { fg: syntaxColors.function },
  "function.call": { fg: syntaxColors.function },
  "function.method": { fg: syntaxColors.function },
  "function.method.call": { fg: syntaxColors.function },
  "function.builtin": { fg: syntaxColors.function },
  constructor: { fg: syntaxColors.type },
  type: { fg: syntaxColors.type },
  "type.builtin": { fg: syntaxColors.type },
  module: { fg: syntaxColors.type },
  operator: { fg: syntaxColors.operator },
  "keyword.operator": { fg: syntaxColors.operator },
  punctuation: { fg: colors.muted },
  "punctuation.bracket": { fg: colors.muted },
  "punctuation.delimiter": { fg: colors.muted },
  variable: { fg: colors.fg },
  "variable.builtin": { fg: syntaxColors.keyword },
  "variable.member": { fg: colors.fg },
  "variable.parameter": { fg: colors.fg },
  property: { fg: colors.fg },
  attribute: { fg: syntaxColors.type },
}
