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

test("the default theme is hya and uses Sublime Monokai with separate focus, headings and activity", () => {
  expect(defaultThemeName).toBe("hya")
  expect(themeName()).toBe("hya")
  expect({ ...colors }).toEqual({
    bg: "#272822", panel: "#34352e", fg: "#f8f8f2", muted: "#aaa99f", accent: "#e6db74", border: "#75715e", heading: "#fd971f", activity: "#66d9ef", error: "#f92672", warning: "#fd971f", selection: "#49483e",
  })
  expect({ ...toolColors }).toEqual({ done: "#a6e22e" })
  expect({ ...diffColors }).toEqual({ add: "#a6e22e", remove: "#f92672", hunk: "#66d9ef", context: "#aaa99f" })
  expect({ ...syntaxColors }).toEqual({
    keyword: "#f92672", string: "#e6db74", number: "#ae81ff", comment: "#75715e", function: "#a6e22e", type: "#66d9ef", operator: "#f92672", inlineCode: "#a6e22e",
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
  expect(seen).toEqual(["#272822", light.colors.bg])
  expect(names).toEqual(["hya", light.name])
  dispose()
})

test("an unknown theme name is rejected and the palette stays", () => {
  expect(setTheme("no-such-theme")).toBe(false)
  expect(themeName()).toBe("hya")
  expect(colors.bg).toBe("#272822")
})

test("syntax styles are derived from a theme's palette", () => {
  const styles = syntaxStylesFor(themes.hya)
  expect(styles.default).toEqual({ fg: "#f8f8f2" })
  expect(styles["markup.heading.2"]).toEqual({ fg: "#fd971f", bold: true })
  expect(styles.keyword).toEqual({ fg: "#f92672" })
  expect(styles.comment).toEqual({ fg: "#75715e", italic: true })
  const light = (Object.values(themes) as ThemeDefinition[]).find((theme) => theme.kind === "light")!
  expect(syntaxStylesFor(light).keyword).toEqual({ fg: light.syntaxColors.keyword })
  expect(syntaxStylesFor(light).default).toEqual({ fg: light.colors.fg })
})

/** WCAG luminance of an sRGB palette color; verify readable UI text on both surfaces. */
function luminance(hex: string): number {
  const linear = [1, 3, 5].map((offset) => {
    const channel = parseInt(hex.slice(offset, offset + 2), 16) / 255
    return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4
  })
  return linear[0]! * 0.2126 + linear[1]! * 0.7152 + linear[2]! * 0.0722
}

test("Monokai body text, secondary text, focus and activity remain readable on screen and panels", () => {
  const palette = themes.hya.colors
  for (const surface of [palette.bg, palette.panel]) {
    for (const ink of [palette.fg, palette.muted, palette.accent, palette.activity, palette.heading]) {
      const values = [luminance(ink), luminance(surface)].sort((a, b) => b - a)
      expect((values[0]! + 0.05) / (values[1]! + 0.05)).toBeGreaterThanOrEqual(4.5)
    }
  }
  expect(new Set([palette.accent, palette.heading, palette.activity]).size).toBe(3)
})
