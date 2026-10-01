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
  toolColors,
  type ThemeDefinition,
} from "../src/theme"

afterEach(() => { setTheme(defaultThemeName) })

const hex = /^#[0-9a-f]{6}$/

test("the default theme is hya and keeps the historical palette", () => {
  expect(defaultThemeName).toBe("hya")
  expect(themeName()).toBe("hya")
  expect({ ...colors }).toEqual({
    bg: "#11151b", panel: "#1c2530", fg: "#e8edf3", muted: "#9caab9", accent: "#73c8e8", border: "#405366", error: "#f07878", warning: "#e5c07b", selection: "#2f4d6b",
  })
  expect({ ...toolColors }).toEqual({ done: "#a5d6a7" })
  expect({ ...diffColors }).toEqual({ add: "#a5d6a7", remove: "#f07878", hunk: "#82aaff", context: "#9caab9" })
  expect({ ...syntaxColors }).toEqual({
    keyword: "#c792ea", string: "#a5d6a7", number: "#f78c6c", comment: "#7a8a9c", function: "#82aaff", type: "#ffcb6b", operator: "#89ddff", inlineCode: "#f2a97a",
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
      for (const value of Object.values(theme[group])) expect(value).toMatch(hex)
    }
  }
  // The themes really differ from each other.
  expect(new Set(list.map((theme) => theme.colors.bg)).size).toBe(list.length)
})

test("setTheme switches the reactive palette; effects reading it re-run", () => {
  const light = (Object.values(themes) as ThemeDefinition[]).find((theme) => theme.kind === "light")!
  const seen: string[] = []
  const names: string[] = []
  const dispose = createRoot((dispose) => {
    createEffect(() => { seen.push(colors.bg) })
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
  expect(seen).toEqual(["#11151b", light.colors.bg])
  expect(names).toEqual(["hya", light.name])
  dispose()
})

test("an unknown theme name is rejected and the palette stays", () => {
  expect(setTheme("no-such-theme")).toBe(false)
  expect(themeName()).toBe("hya")
  expect(colors.bg).toBe("#11151b")
})

test("syntax styles are derived from a theme's palette", () => {
  const styles = syntaxStylesFor(themes.hya)
  expect(styles.default).toEqual({ fg: "#e8edf3" })
  expect(styles["markup.heading.2"]).toEqual({ fg: "#73c8e8", bold: true })
  expect(styles.keyword).toEqual({ fg: "#c792ea" })
  expect(styles.comment).toEqual({ fg: "#7a8a9c", italic: true })
  const light = (Object.values(themes) as ThemeDefinition[]).find((theme) => theme.kind === "light")!
  expect(syntaxStylesFor(light).keyword).toEqual({ fg: light.syntaxColors.keyword })
  expect(syntaxStylesFor(light).default).toEqual({ fg: light.colors.fg })
})
