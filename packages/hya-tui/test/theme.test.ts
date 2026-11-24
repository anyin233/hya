import { afterEach, expect, test } from "bun:test"
import { createEffect, createRoot } from "solid-js"
import {
  colors,
  defaultThemeName,
  diffColors,
  setTheme,
  syntaxColors,
  syntaxStylesFor,
  themeName,
  themes,
  terminalBackground,
  toolColors,
  type ThemeDefinition,
  autoThemeFor,
  resolveThemeSetting,
  setTerminalThemeMode,
  setThemeSetting,
  terminalModeFromEnv,
  themeSetting,
} from "../src/theme"

afterEach(() => { setTheme(defaultThemeName) })

const hex = /^#[0-9a-f]{6}$/

test("the default theme inherits the terminal background and preserves text colors", () => {
  expect(defaultThemeName).toBe("hya")
  expect(themeName()).toBe("hya")
  expect({ ...colors }).toEqual({
    bg: terminalBackground, panel: terminalBackground, fg: "#e8edf3", muted: "#9caab9", focus: "#73c8e8", heading: "#e8edf3", link: "#e8edf3", activity: "#9caab9", success: "#a5d6a7", border: "#405366", error: "#f07878", warning: "#e5c07b", selection: "#2f4d6b",
  })
  expect({ ...toolColors }).toEqual({ done: "#a5d6a7" })
  expect({ ...diffColors }).toEqual({ add: "#a5d6a7", remove: "#f07878", hunk: "#e8edf3", context: "#9caab9" })
  expect({ ...syntaxColors }).toEqual({
    keyword: "#c792ea", string: "#d6bb8d", number: "#d6bb8d", comment: "#7a8a9c", function: "#e8edf3", type: "#e8edf3", operator: "#e8edf3", inlineCode: "#d6bb8d",
  })
})

test("built-ins: hya, one light theme, and two more dark themes, each defining every palette key", () => {
  const list = Object.values(themes) as ThemeDefinition[]
  expect(list.map((theme) => theme.name)).toContain("hya")
  expect(list.filter((theme) => theme.kind === "light").length).toBe(1)
  expect(list.filter((theme) => theme.kind === "dark").length).toBe(3)
  const reference = themes.hya
  for (const theme of list) {
    expect(theme.label.length).toBeGreaterThan(0)
    expect(theme.description.length).toBeGreaterThan(0)
    for (const group of ["colors", "toolColors", "diffColors", "syntaxColors", "attachColors"] as const) {
      expect(Object.keys(theme[group]).sort()).toEqual(Object.keys(reference[group]).sort())
      for (const [key, value] of Object.entries(theme[group])) {
        if (group === "colors" && (key === "bg" || key === "panel")) expect(value).toMatchObject({ intent: "default" })
        else expect(value).toMatch(hex)
      }
    }
  }
  // The themes really differ from each other.
  expect(new Set(list.map((theme) => theme.colors.fg)).size).toBe(list.length)
})

test("setTheme switches the reactive palette; effects reading it re-run", () => {
  const light = (Object.values(themes) as ThemeDefinition[]).find((theme) => theme.kind === "light")!
  const seen: string[] = []
  const names: string[] = []
  const dispose = createRoot((dispose) => {
    createEffect(() => { seen.push(colors.fg) })
    createEffect(() => { names.push(themeName()) })
    return dispose
  })
  expect(setTheme(light.name)).toBe(true)
  expect(themeName()).toBe(light.name)
  expect(colors.bg).toBe(light.colors.bg)
  expect(colors.fg).toBe(light.colors.fg)
  expect(diffColors.add).toBe(light.diffColors.add)
  expect(syntaxColors.keyword).toBe(light.syntaxColors.keyword)
  expect(toolColors.done).toBe(light.toolColors.done)
  expect(seen).toEqual(["#e8edf3", light.colors.fg])
  expect(names).toEqual(["hya", light.name])
  dispose()
})

test("an unknown theme name is rejected and the palette stays", () => {
  expect(setTheme("no-such-theme")).toBe(false)
  expect(themeName()).toBe("hya")
  expect(colors.bg).toBe(terminalBackground)
})

test("syntax styles are derived from a theme's palette", () => {
  const styles = syntaxStylesFor(themes.hya)
  expect(styles.default).toEqual({ fg: "#e8edf3" })
  expect(styles["markup.heading.2"]).toEqual({ fg: "#e8edf3", bold: true })
  expect(styles.keyword).toEqual({ fg: "#c792ea" })
  expect(styles.comment).toEqual({ fg: "#7a8a9c", italic: true })
  const light = (Object.values(themes) as ThemeDefinition[]).find((theme) => theme.kind === "light")!
  expect(syntaxStylesFor(light).keyword).toEqual({ fg: light.syntaxColors.keyword })
  expect(syntaxStylesFor(light).default).toEqual({ fg: light.colors.fg })
})

test("every theme reserves focus for navigation and keeps status and structure distinct", () => {
  for (const theme of Object.values(themes)) {
    const ui = theme.colors
    for (const role of ["heading", "link", "activity", "success", "warning", "error"] as const) expect(ui[role]).not.toBe(ui.focus)
    expect(ui.heading).toBe(ui.fg)
    expect(ui.activity).toBe(ui.muted)
    expect(theme.toolColors.done).toBe(ui.success)
    expect(theme.diffColors.add).toBe(ui.success)
    expect(theme.diffColors.remove).toBe(ui.error)
    expect(theme.diffColors.hunk).toBe(ui.heading)
    expect(theme.syntaxColors.string).toBe(theme.syntaxColors.number)
    expect(theme.syntaxColors.inlineCode).toBe(theme.syntaxColors.string)
    for (const role of ["function", "type", "operator"] as const) expect(theme.syntaxColors[role]).toBe(ui.fg)
    const styles = syntaxStylesFor(theme)
    expect(styles["markup.heading"]).toEqual({ fg: ui.heading, bold: true })
    expect(styles["markup.link.label"]).toEqual({ fg: ui.link, underline: true })
    expect(styles["markup.list"]).toEqual({ fg: ui.fg })
    expect(styles["markup.list.checked"]).toEqual({ fg: ui.success })
  }
})

/** WCAG relative-luminance contrast of two `#rrggbb` colors. */
function contrast(a: string, b: string): number {
  const luminance = (hex: string) => {
    const [r, g, bl] = [1, 3, 5].map((at) => parseInt(hex.slice(at, at + 2), 16) / 255).map((v) => v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4)
    return 0.2126 * r! + 0.7152 * g! + 0.0722 * bl!
  }
  const [high, low] = [luminance(a), luminance(b)].sort((x, y) => y - x)
  return (high! + 0.05) / (low! + 0.05)
}

test("text stays readable on the terminal backgrounds each theme is for; borders stay visible", () => {
  // Typical terminal backgrounds: pure, a common gray, and a tinted one.
  const backgrounds = { light: ["#ffffff", "#f5f5f5", "#fdf6e3"], dark: ["#000000", "#1e1e1e", "#282c34"] }
  for (const theme of Object.values(themes) as ThemeDefinition[]) {
    for (const bg of backgrounds[theme.kind]) {
      for (const role of ["fg", "muted", "focus", "heading", "success", "error", "warning"] as const) {
        expect({ theme: theme.name, role, bg, ok: contrast(theme.colors[role] as string, bg) >= 4.5 }).toEqual({ theme: theme.name, role, bg, ok: true })
      }
      for (const [role, color] of Object.entries(theme.attachColors)) {
        expect({ theme: theme.name, role, bg, ok: contrast(color, bg) >= 4.5 }).toEqual({ theme: theme.name, role, bg, ok: true })
      }
    }
    if (theme.kind === "light") for (const bg of backgrounds.light.slice(0, 2)) expect(contrast(theme.colors.border, bg)).toBeGreaterThanOrEqual(3)
  }
})

test("auto follows the terminal background; COLORFGBG is the first guess; an explicit theme stays", () => {
  expect(terminalModeFromEnv({ COLORFGBG: "0;15" })).toBe("light")
  expect(terminalModeFromEnv({ COLORFGBG: "0;default;7" })).toBe("light")
  expect(terminalModeFromEnv({ COLORFGBG: "15;0" })).toBe("dark")
  expect(terminalModeFromEnv({ COLORFGBG: "15" })).toBeNull()
  expect(terminalModeFromEnv({})).toBeNull()
  expect(autoThemeFor("light")).toBe("light")
  expect(autoThemeFor("dark")).toBe("hya")
  expect(autoThemeFor(null)).toBe("hya")
  try {
    expect(setThemeSetting("auto")).toBe(true)
    setTerminalThemeMode("light")
    expect(themeName()).toBe("light")
    expect(themeSetting()).toBe("auto")
    setTerminalThemeMode("dark")
    expect(themeName()).toBe("hya")
    expect(setThemeSetting("ember")).toBe(true)
    setTerminalThemeMode("light")
    expect(themeName()).toBe("ember")
    expect(setThemeSetting("no-such-theme")).toBe(false)
    expect(themeSetting()).toBe("ember")
    expect(resolveThemeSetting("auto")).toBe("light")
  } finally {
    setTerminalThemeMode(null)
    setThemeSetting("auto")
  }
  expect(themeName()).toBe("hya")
})
