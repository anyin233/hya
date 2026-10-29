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
} from "../src/theme"

afterEach(() => { setTheme(defaultThemeName) })

const hex = /^#[0-9a-f]{6}$/

test("the default theme inherits the terminal background and preserves text colors", () => {
  expect(defaultThemeName).toBe("hya")
  expect(themeName()).toBe("hya")
  expect({ ...colors }).toEqual({
    bg: terminalBackground, panel: terminalBackground, fg: "#e8edf3", muted: "#9caab9", focus: "#73c8e8", heading: "#e8edf3", link: "#e8edf3", activity: "#9caab9", success: "#a5d6a7", border: "#535353", error: "#f07878", warning: "#e5c07b", selection: "#2f4d6b",
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
    for (const group of ["colors", "toolColors", "diffColors", "syntaxColors"] as const) {
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
    const [red, green, blue] = [ui.border.slice(1, 3), ui.border.slice(3, 5), ui.border.slice(5, 7)]
    expect(red).toBe(green)
    expect(green).toBe(blue)
    expect(ui.border).not.toBe(ui.focus)
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
