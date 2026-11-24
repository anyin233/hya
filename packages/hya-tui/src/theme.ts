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
 * The saved setting may be `auto` (the default): the theme follows the
 * terminal's light or dark background (`setThemeSetting`,
 * `setTerminalThemeMode`).
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

/**
 * Identity colors of the composer's chips and attachment rows
 * (composer/chips.ts): a paste, a file, and an image token and its row share
 * one color, distinct from the focus color.
 */
export interface ThemeAttachColors {
  paste: string
  file: string
  image: string
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
  attachColors: ThemeAttachColors
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
    attachColors: { paste: "#c3a6f0", file: "#7fd1c4", image: "#f0a6c8" },
  },
  light: {
    name: "light",
    label: "Light",
    kind: "light",
    description: "Terminal background with dark text, for bright terminals",
    colors: { bg: terminalBackground, panel: terminalBackground, fg: "#1f2933", muted: "#4f5f6f", focus: "#0b6f94", heading: "#1f2933", link: "#1f2933", activity: "#4f5f6f", success: "#276b2b", border: "#7f8e9d", error: "#c23b3b", warning: "#8a5a00", selection: "#b7d5ea" },
    toolColors: { done: "#276b2b" },
    diffColors: { add: "#276b2b", remove: "#c23b3b", hunk: "#1f2933", context: "#4f5f6f" },
    syntaxColors: { keyword: "#8839c9", string: "#805f20", number: "#805f20", comment: "#5f6f80", function: "#1f2933", type: "#1f2933", operator: "#1f2933", inlineCode: "#805f20" },
    attachColors: { paste: "#6a4bb5", file: "#17706a", image: "#b0407a" },
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
    attachColors: { paste: "#d7b8ff", file: "#5cffe0", image: "#ff9ed8" },
  },
  ember: {
    name: "ember",
    label: "Ember",
    kind: "dark",
    description: "Terminal background with warm text and blue focus highlights",
    colors: { bg: terminalBackground, panel: terminalBackground, fg: "#f1e6d8", muted: "#b3a393", focus: "#8fc9d9", heading: "#f1e6d8", link: "#f1e6d8", activity: "#b3a393", success: "#a9c77a", border: "#5c4a3d", error: "#f07468", warning: "#e6b450", selection: "#2c4c55" },
    toolColors: { done: "#a9c77a" },
    diffColors: { add: "#a9c77a", remove: "#f07468", hunk: "#f1e6d8", context: "#b3a393" },
    syntaxColors: { keyword: "#c8a6dd", string: "#d7c28a", number: "#d7c28a", comment: "#85766a", function: "#f1e6d8", type: "#f1e6d8", operator: "#f1e6d8", inlineCode: "#d7c28a" },
    attachColors: { paste: "#c8a6dd", file: "#8fc9b0", image: "#e8a0a0" },
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
const [attachStore, setAttachColors] = createStore<ThemeAttachColors>({ ...themes.hya.attachColors })

/** The base UI colors of the theme in effect (reactive). */
export const colors: Readonly<ThemeColors> = colorStore
/** Tool card state colors of the theme in effect (reactive). */
export const toolColors: Readonly<ThemeToolColors> = toolStore
/** Diff row colors of the theme in effect (reactive). */
export const diffColors: Readonly<ThemeDiffColors> = diffStore
/** Syntax token colors of the theme in effect (reactive). */
export const syntaxColors: Readonly<ThemeSyntaxColors> = syntaxStore
/** Chip and attachment-row identity colors of the theme in effect (reactive). */
export const attachColors: Readonly<ThemeAttachColors> = attachStore

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
    setAttachColors({ ...theme.attachColors })
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

/**
 * `auto` (the default when no `theme` is saved): follow the terminal's
 * background — `light` on a light terminal, `hya` on a dark one. The terminal
 * reports its background to OpenTUI (OSC 11 at start, DEC 2031 when it
 * changes; app/run.tsx feeds `setTerminalThemeMode`); before that, or when a
 * terminal does not answer, `COLORFGBG` decides, else dark.
 */
export const autoThemeSetting = "auto"
export type TerminalThemeMode = "dark" | "light"

const [setting, setSetting] = createSignal<string>(autoThemeSetting)
const [terminalMode, setTerminalMode] = createSignal<TerminalThemeMode | null>(null)

/** The built-in theme `auto` picks for a terminal background. */
export function autoThemeFor(mode: TerminalThemeMode | null): ThemeName {
  return mode === "light" ? "light" : defaultThemeName
}

/**
 * The terminal background from `COLORFGBG` (`fg;bg` or `fg;default;bg`, ANSI
 * color indexes, set by rxvt, Konsole, iTerm2, and others): bg 7 or 15 (and
 * 9–14) is light, 0–6 and 8 dark; `undefined`/other: unknown.
 */
export function terminalModeFromEnv(env: Record<string, string | undefined>): TerminalThemeMode | null {
  const bg = Number((env.COLORFGBG ?? "").split(";").at(-1))
  if (!(env.COLORFGBG ?? "").includes(";") || !Number.isInteger(bg)) return null
  return bg === 7 || (bg >= 9 && bg <= 15) ? "light" : bg >= 0 && bg <= 8 ? "dark" : null
}

/** The saved setting: `auto` or a built-in theme name (reactive). */
export function themeSetting(): string {
  return setting()
}

/** The terminal background mode known so far (reactive; `null` before it answered). */
export function terminalThemeMode(): TerminalThemeMode | null {
  return terminalMode()
}

/** The theme a setting means now: `auto` resolves through the terminal mode. */
export function resolveThemeSetting(value: string): string {
  return value === autoThemeSetting ? autoThemeFor(terminalMode()) : value
}

/** Use `value` (`auto` or a theme name); `false` (and no change) for an unknown name. */
export function setThemeSetting(value: string): boolean {
  if (value !== autoThemeSetting && !findTheme(value)) return false
  setSetting(value)
  return setTheme(resolveThemeSetting(value))
}

/** The terminal reported its background (or `COLORFGBG` guessed it): an `auto` setting follows. */
export function setTerminalThemeMode(mode: TerminalThemeMode | null): void {
  if (mode === terminalMode()) return
  setTerminalMode(mode)
  if (setting() === autoThemeSetting) setTheme(autoThemeFor(mode))
}
