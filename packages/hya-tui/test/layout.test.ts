import { expect, test } from "bun:test"
import {
  layoutBreakpoints, parseSwitch,
  sidebarMinColumns, sidebarVisible, toggledSidebar, wrapLineCount,
} from "../src/state/layout"

import { createAppStore } from "../src/state/store"
test("the sidebar needs the breakpoint width; a pin cannot show it below", () => {
  // At 149 columns the default sidebar was 18 columns; its minimum is 60% wider (29) and it is closed there.
  expect(layoutBreakpoints.sidebar).toBe(150)
  expect(sidebarMinColumns).toBe(Math.ceil(18 * 1.6))
  expect(sidebarVisible("auto", 160)).toBe(true)
  expect(sidebarVisible("auto", 150)).toBe(true)
  expect(sidebarVisible("auto", 149)).toBe(false)
  expect(sidebarVisible("auto", 80)).toBe(false)
  expect(sidebarVisible("open", 149)).toBe(false)
  expect(sidebarVisible("open", 160)).toBe(true)
  expect(sidebarVisible("closed", 160)).toBe(false)
})

test("toggling flips what is visible on a wide terminal and changes nothing on a narrow one", () => {
  expect(toggledSidebar("auto", 149)).toBe("auto")
  expect(toggledSidebar("closed", 149)).toBe("closed")
  expect(toggledSidebar("auto", 160)).toBe("closed")
  expect(toggledSidebar("closed", 160)).toBe("auto")
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
  expect(store.state.sidebar).toBe("auto")
  store.setColumns(160)
  store.toggleSidebar()
  expect(store.state.sidebar).toBe("closed")
  store.toggleSidebar()
  expect(store.state.sidebar).toBe("auto")
})
