import { expect, test } from "bun:test"
import { layoutBreakpoints, parseSwitch, sidebarVisible, sidebarWidth, toggledSidebar, wrapLineCount } from "../src/state/layout"
import { createAppStore } from "../src/state/store"

test("the sidebar follows the terminal width until it is toggled", () => {
  expect(layoutBreakpoints.sidebar).toBe(110)
  expect(sidebarVisible("auto", 130)).toBe(true)
  expect(sidebarVisible("auto", 110)).toBe(true)
  expect(sidebarVisible("auto", 109)).toBe(false)
  expect(sidebarVisible("auto", 80)).toBe(false)
  expect(sidebarVisible("open", 80)).toBe(true)
  expect(sidebarVisible("closed", 130)).toBe(false)
})

test("toggling flips what is visible now, at any width", () => {
  expect(toggledSidebar("auto", 80)).toBe("open")
  expect(toggledSidebar("auto", 130)).toBe("closed")
  expect(toggledSidebar("open", 80)).toBe("closed")
  expect(toggledSidebar("closed", 130)).toBe("open")
})

test("the sidebar is at most 32 columns and never takes more than 40% of a narrow screen", () => {
  expect(sidebarWidth(130)).toBe(32)
  expect(sidebarWidth(80)).toBe(32)
  expect(sidebarWidth(60)).toBe(24)
  expect(sidebarWidth(30)).toBe(20)
})

test("on/off arguments set a switch; no argument toggles it", () => {
  expect(parseSwitch(undefined, true)).toBe(false)
  expect(parseSwitch("", false)).toBe(true)
  expect(parseSwitch("on", true)).toBe(true)
  expect(parseSwitch("off", false)).toBe(false)
  expect(parseSwitch("show", false)).toBe(true)
  expect(parseSwitch("hide", true)).toBe(false)
  expect(() => parseSwitch("maybe", true)).toThrow("Usage")
})

test("wrapLineCount counts the rows word-wrapped text takes at a width", () => {
  expect(wrapLineCount("↑↓ move · Esc back", 76)).toBe(1)
  expect(wrapLineCount("", 76)).toBe(1)
  expect(wrapLineCount("one two three", 0)).toBe(1)
  // "one two three" is 13 chars; at width 7 "one two" fits (7), "three" wraps.
  expect(wrapLineCount("one two three", 7)).toBe(2)
  // A run of short hint segments that together exceed a narrow width wraps
  // more than once, never merging words across the break.
  expect(wrapLineCount("aaaa bbbb cccc dddd", 9)).toBe(2)
})

test("the store tracks the sidebar mode and the terminal width", () => {
  const store = createAppStore()
  expect(store.state.sidebar).toBe("auto")
  store.setColumns(80)
  expect(store.state.columns).toBe(80)
  store.toggleSidebar()
  expect(store.state.sidebar).toBe("open")
  store.toggleSidebar()
  expect(store.state.sidebar).toBe("closed")
  store.setSidebar("auto")
  expect(store.state.sidebar).toBe("auto")
})
