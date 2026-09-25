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
import { batch, createSignal } from "solid-js"
import { createStore } from "solid-js/store"

/** The base UI colors. */
export interface ThemeColors {
  bg: string
  panel: string
  fg: string
  muted: string
  accent: string
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
 * call is `done` (the palette's green, the same as the string token color);
 * a failed one uses `colors.error`, a running spinner `colors.accent`, a
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
  /** `dark` or `light` background (shown as the `/theme` row tag). */
  kind: "dark" | "light"
  description: string
  colors: ThemeColors
  toolColors: ThemeToolColors
  diffColors: ThemeDiffColors
  syntaxColors: ThemeSyntaxColors
}

/** The built-in themes, in `/theme` order. `hya` is the default. */
export const themes = {
  hya: {
    name: "hya",
    label: "hya",
    kind: "dark",
    description: "Default dark theme: slate background, cyan accent",
    colors: { bg: "#11151b", panel: "#1c2530", fg: "#e8edf3", muted: "#9caab9", accent: "#73c8e8", border: "#405366", error: "#f07878", warning: "#e5c07b", selection: "#2f4d6b" },
    toolColors: { done: "#a5d6a7" },
    diffColors: { add: "#a5d6a7", remove: "#f07878", hunk: "#82aaff", context: "#9caab9" },
    syntaxColors: { keyword: "#c792ea", string: "#a5d6a7", number: "#f78c6c", comment: "#7a8a9c", function: "#82aaff", type: "#ffcb6b", operator: "#89ddff", inlineCode: "#f2a97a" },
  },
  light: {
    name: "light",
    label: "Light",
    kind: "light",
    description: "Light background with dark text, for bright terminals",
    colors: { bg: "#f7f9fb", panel: "#e6ecf2", fg: "#1f2933", muted: "#5b6b7b", accent: "#0b6f94", border: "#a9b6c3", error: "#c23b3b", warning: "#946200", selection: "#b7d5ea" },
    toolColors: { done: "#2e7d32" },
    diffColors: { add: "#2e7d32", remove: "#c23b3b", hunk: "#3a5fcd", context: "#5b6b7b" },
    syntaxColors: { keyword: "#8839c9", string: "#2e7d32", number: "#b5520f", comment: "#6a7a8a", function: "#3a5fcd", type: "#8a5a00", operator: "#0e7490", inlineCode: "#b4491f" },
  },
  contrast: {
    name: "contrast",
    label: "High contrast",
    kind: "dark",
    description: "Black background, white text, saturated accents",
    colors: { bg: "#000000", panel: "#141414", fg: "#ffffff", muted: "#cccccc", accent: "#00e5ff", border: "#8a8a8a", error: "#ff5c5c", warning: "#ffd400", selection: "#1f4f8a" },
    toolColors: { done: "#5cff7a" },
    diffColors: { add: "#5cff7a", remove: "#ff5c5c", hunk: "#7aa2ff", context: "#cccccc" },
    syntaxColors: { keyword: "#ff79ff", string: "#5cff7a", number: "#ffa057", comment: "#a8a8a8", function: "#7aa2ff", type: "#ffe066", operator: "#66f0ff", inlineCode: "#ffb86c" },
  },
  ember: {
    name: "ember",
    label: "Ember",
    kind: "dark",
    description: "Warm dark theme: brown background, amber accent",
    colors: { bg: "#1a1512", panel: "#2a211c", fg: "#f1e6d8", muted: "#b3a393", accent: "#f0a35e", border: "#5c4a3d", error: "#e8665a", warning: "#e6b450", selection: "#5a3f28" },
    toolColors: { done: "#a9c77a" },
    diffColors: { add: "#a9c77a", remove: "#e8665a", hunk: "#8fb3c9", context: "#b3a393" },
    syntaxColors: { keyword: "#e0879a", string: "#a9c77a", number: "#f29e6d", comment: "#85766a", function: "#e6b86a", type: "#d7c28a", operator: "#e8a87c", inlineCode: "#f2b880" },
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
    "markup.heading": { fg: ui.accent, bold: true },
    ...Object.fromEntries([1, 2, 3, 4, 5, 6].map((level) => [`markup.heading.${level}`, { fg: ui.accent, bold: true }])),
    "markup.strong": { fg: ui.fg, bold: true },
    "markup.italic": { fg: ui.fg, italic: true },
    "markup.strikethrough": { fg: ui.muted },
    "markup.raw": { fg: syntax.inlineCode },
    "markup.raw.block": { fg: ui.fg },
    "markup.link": { fg: ui.muted },
    "markup.link.label": { fg: ui.accent, underline: true },
    "markup.link.url": { fg: ui.muted, underline: true },
    "markup.list": { fg: ui.accent },
    "markup.list.checked": { fg: ui.accent },
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
