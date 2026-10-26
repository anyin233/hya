/**
 * The TUI palette and its built-in themes (docs/tui.md "Themes").
 *
 * Every component reads colors from the reactive palette exported here:
 * `colors`, `toolColors`, `diffColors`, and `syntaxColors` are Solid stores
 * holding the current theme's values, so a read inside JSX, a memo, or an
 * effect (`fg={colors.muted}`) re-runs when `setTheme` switches the theme.
 * A read outside a tracking scope (a module-level constant) is a snapshot
 * and never updates — compute colors in a function or in JSX instead.
 *
 * `themeName()` is the reactive name of the theme in effect; Markdown
 * rebuilds its OpenTUI `SyntaxStyle` from `syntaxStylesFor(currentTheme())`
 * when it changes (components/Markdown.tsx).
 */
import { RGBA, type ColorInput } from "@opentui/core"
import { batch, createSignal } from "solid-js"
import { createStore } from "solid-js/store"

/** The base UI colors. */
export interface ThemeColors {
  /** ANSI default background: inherits the terminal color and transparency. */
  bg: ColorInput
  panel: ColorInput
  fg: string
  muted: string
  /** Keyboard focus, current input cursor, and highlighted choices only. */
  focus: string
  /** Structural headings; neutral text, distinguished by weight. */
  heading: string
  /** Action links; neutral text, distinguished by an underline. */
  link: string
  /** In-flight work, always accompanied by a spinner or status text. */
  activity: string
  /** A completed operation or successful result. */
  success: string
  border: string
  /** Failed turns and tool errors. */
  error: string
  /** Noteworthy finishes: length limit, cancelled. */
  warning: string
  /** Background of mouse-selected text (App.tsx paints it on every selectable text; the text keeps its color). */
  selection: string
}

/**
 * Tool call cards (components/MessageView.tsx): the state icon of a finished
 * call is `done` (the palette's semantic success color);
 * a failed one uses `colors.error`, a running spinner `colors.activity`, a
 * pending call `colors.muted`, one waiting for a permission answer
 * `colors.warning`.
 */
export interface ThemeToolColors {
  done: string
}

/** Diff rows in edit / write / patch cards: added, removed, hunk headers, context. */
export interface ThemeDiffColors {
  add: string
  remove: string
  hunk: string
  context: string
}

/** Token colors for Markdown and highlighted code blocks (tree-sitter capture names in `syntaxStylesFor`). */
export interface ThemeSyntaxColors {
  keyword: string
  string: string
  number: string
  comment: string
  function: string
  type: string
  operator: string
  inlineCode: string
}

/** One named theme: the full palette. */
export interface ThemeDefinition {
  /** Id stored in the preferences file (`theme` key) and listed by `/theme`. */
  name: string
  /** Display name. */
  label: string
  /** Preferred terminal background (`dark` or `light`), shown as the `/theme` row tag. */
  kind: "dark" | "light"
  description: string
  colors: ThemeColors
  toolColors: ThemeToolColors
  diffColors: ThemeDiffColors
  syntaxColors: ThemeSyntaxColors
}

/** An opaque default-color fill clears overlays without setting a terminal RGB background. */
export const terminalBackground = RGBA.defaultBackground()

/** The built-in themes, in `/theme` order. `hya` is the default. */
export const themes = {
  hya: {
    name: "hya",
    label: "hya",
    kind: "dark",
    description: "Terminal background with light text and blue focus highlights",
    colors: { bg: terminalBackground, panel: terminalBackground, fg: "#e8edf3", muted: "#9caab9", focus: "#73c8e8", heading: "#e8edf3", link: "#e8edf3", activity: "#9caab9", success: "#a5d6a7", border: "#405366", error: "#f07878", warning: "#e5c07b", selection: "#2f4d6b" },
    toolColors: { done: "#a5d6a7" },
    diffColors: { add: "#a5d6a7", remove: "#f07878", hunk: "#e8edf3", context: "#9caab9" },
    syntaxColors: { keyword: "#c792ea", string: "#d6bb8d", number: "#d6bb8d", comment: "#7a8a9c", function: "#e8edf3", type: "#e8edf3", operator: "#e8edf3", inlineCode: "#d6bb8d" },
  },
  light: {
    name: "light",
    label: "Light",
    kind: "light",
    description: "Terminal background with dark text, for bright terminals",
    colors: { bg: terminalBackground, panel: terminalBackground, fg: "#1f2933", muted: "#5b6b7b", focus: "#0b6f94", heading: "#1f2933", link: "#1f2933", activity: "#5b6b7b", success: "#2e7d32", border: "#a9b6c3", error: "#c23b3b", warning: "#946200", selection: "#b7d5ea" },
    toolColors: { done: "#2e7d32" },
    diffColors: { add: "#2e7d32", remove: "#c23b3b", hunk: "#1f2933", context: "#5b6b7b" },
    syntaxColors: { keyword: "#8839c9", string: "#805f20", number: "#805f20", comment: "#6a7a8a", function: "#1f2933", type: "#1f2933", operator: "#1f2933", inlineCode: "#805f20" },
  },
  contrast: {
    name: "contrast",
    label: "High contrast",
    kind: "dark",
    description: "Terminal background with white text and high-contrast focus highlights",
    colors: { bg: terminalBackground, panel: terminalBackground, fg: "#ffffff", muted: "#cccccc", focus: "#00e5ff", heading: "#ffffff", link: "#ffffff", activity: "#cccccc", success: "#5cff7a", border: "#8a8a8a", error: "#ff5c5c", warning: "#ffd400", selection: "#1f4f8a" },
    toolColors: { done: "#5cff7a" },
    diffColors: { add: "#5cff7a", remove: "#ff5c5c", hunk: "#ffffff", context: "#cccccc" },
    syntaxColors: { keyword: "#ff79ff", string: "#ffe6a7", number: "#ffe6a7", comment: "#a8a8a8", function: "#ffffff", type: "#ffffff", operator: "#ffffff", inlineCode: "#ffe6a7" },
  },
  ember: {
    name: "ember",
    label: "Ember",
    kind: "dark",
    description: "Terminal background with warm text and blue focus highlights",
    colors: { bg: terminalBackground, panel: terminalBackground, fg: "#f1e6d8", muted: "#b3a393", focus: "#8fc9d9", heading: "#f1e6d8", link: "#f1e6d8", activity: "#b3a393", success: "#a9c77a", border: "#5c4a3d", error: "#e8665a", warning: "#e6b450", selection: "#2c4c55" },
    toolColors: { done: "#a9c77a" },
    diffColors: { add: "#a9c77a", remove: "#e8665a", hunk: "#f1e6d8", context: "#b3a393" },
    syntaxColors: { keyword: "#c8a6dd", string: "#d7c28a", number: "#d7c28a", comment: "#85766a", function: "#f1e6d8", type: "#f1e6d8", operator: "#f1e6d8", inlineCode: "#d7c28a" },
  },
} satisfies Record<string, ThemeDefinition>

export type ThemeName = keyof typeof themes

export const defaultThemeName: ThemeName = "hya"

/** The built-in theme named `name`, if any. */
export function findTheme(name: string): ThemeDefinition | undefined {
  return Object.hasOwn(themes, name) ? (themes as Record<string, ThemeDefinition>)[name] : undefined
}

const [name, setName] = createSignal<string>(defaultThemeName)
const [colorStore, setColors] = createStore<ThemeColors>({ ...themes.hya.colors })
const [toolStore, setToolColors] = createStore<ThemeToolColors>({ ...themes.hya.toolColors })
const [diffStore, setDiffColors] = createStore<ThemeDiffColors>({ ...themes.hya.diffColors })
const [syntaxStore, setSyntaxColors] = createStore<ThemeSyntaxColors>({ ...themes.hya.syntaxColors })

/** The base UI colors of the theme in effect (reactive). */
export const colors: Readonly<ThemeColors> = colorStore
/** Tool card state colors of the theme in effect (reactive). */
export const toolColors: Readonly<ThemeToolColors> = toolStore
/** Diff row colors of the theme in effect (reactive). */
export const diffColors: Readonly<ThemeDiffColors> = diffStore
/** Syntax token colors of the theme in effect (reactive). */
export const syntaxColors: Readonly<ThemeSyntaxColors> = syntaxStore

/** Name of the theme in effect (reactive). */
export function themeName(): string {
  return name()
}

/** The definition of the theme in effect (reactive through `themeName`). */
export function currentTheme(): ThemeDefinition {
  return findTheme(name()) ?? themes.hya
}

/** Switch every component to the built-in theme `next`; `false` (and no change) when no theme has that name. */
export function setTheme(next: string): boolean {
  const theme = findTheme(next)
  if (!theme) return false
  // One batch: effects run once, after the whole palette and the name changed.
  batch(() => {
    setColors({ ...theme.colors })
    setToolColors({ ...theme.toolColors })
    setDiffColors({ ...theme.diffColors })
    setSyntaxColors({ ...theme.syntaxColors })
    setName(theme.name)
  })
  return true
}

export interface TextStyle {
  fg?: string
  bg?: string
  bold?: boolean
  italic?: boolean
  underline?: boolean
  dim?: boolean
}

/**
 * Styles by tree-sitter capture / Markdown scope name for `theme`. A dotted
 * name falls back to its first segment (`keyword.return` → `keyword`), so the
 * base names cover most captures. Used to build the OpenTUI `SyntaxStyle` in
 * components/Markdown.tsx.
 */
export function syntaxStylesFor(theme: ThemeDefinition): Record<string, TextStyle> {
  const { colors: ui, syntaxColors: syntax } = theme
  return {
    default: { fg: ui.fg },
    conceal: { fg: ui.border },
    "markup.heading": { fg: ui.heading, bold: true },
    ...Object.fromEntries([1, 2, 3, 4, 5, 6].map((level) => [`markup.heading.${level}`, { fg: ui.heading, bold: true }])),
    "markup.strong": { fg: ui.fg, bold: true },
    "markup.italic": { fg: ui.fg, italic: true },
    "markup.strikethrough": { fg: ui.muted },
    "markup.raw": { fg: syntax.inlineCode },
    "markup.raw.block": { fg: ui.fg },
    "markup.link": { fg: ui.muted },
    "markup.link.label": { fg: ui.link, underline: true },
    "markup.link.url": { fg: ui.muted, underline: true },
    "markup.list": { fg: ui.fg },
    "markup.list.checked": { fg: ui.success },
    "markup.list.unchecked": { fg: ui.muted },
    "markup.quote": { fg: ui.muted, italic: true },
    label: { fg: ui.muted },
    "punctuation.special": { fg: ui.muted },
    keyword: { fg: syntax.keyword },
    string: { fg: syntax.string },
    "string.escape": { fg: syntax.operator },
    "string.special": { fg: syntax.string },
    number: { fg: syntax.number },
    boolean: { fg: syntax.number },
    constant: { fg: syntax.number },
    "constant.builtin": { fg: syntax.number },
    comment: { fg: syntax.comment, italic: true },
    function: { fg: syntax.function },
    "function.call": { fg: syntax.function },
    "function.method": { fg: syntax.function },
    "function.method.call": { fg: syntax.function },
    "function.builtin": { fg: syntax.function },
    constructor: { fg: syntax.type },
    type: { fg: syntax.type },
    "type.builtin": { fg: syntax.type },
    module: { fg: syntax.type },
    operator: { fg: syntax.operator },
    "keyword.operator": { fg: syntax.operator },
    punctuation: { fg: ui.muted },
    "punctuation.bracket": { fg: ui.muted },
    "punctuation.delimiter": { fg: ui.muted },
    variable: { fg: ui.fg },
    "variable.builtin": { fg: syntax.keyword },
    "variable.member": { fg: ui.fg },
    "variable.parameter": { fg: ui.fg },
    property: { fg: ui.fg },
    attribute: { fg: syntax.type },
  }
}
