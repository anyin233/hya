import { expect, test } from "bun:test"
import {
  layoutBreakpoints, parseSwitch,
  projectsSidebarVisible, projectsSidebarWidth, toggledProjectsSidebar,
  sidebarVisible, sidebarWidth, toggledSidebar,
} from "../src/state/layout"
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

test("the left Projects sidebar needs both sidebars and the chat column to fit, wider than the right sidebar alone", () => {
  expect(layoutBreakpoints.projectsSidebar).toBe(150)
  expect(layoutBreakpoints.projectsSidebar).toBeGreaterThan(layoutBreakpoints.sidebar)
  expect(projectsSidebarVisible("auto", 150)).toBe(true)
  expect(projectsSidebarVisible("auto", 149)).toBe(false)
  // At ~80 columns (a narrow terminal) it stays hidden even though the right sidebar's own threshold is lower.
  expect(projectsSidebarVisible("auto", 80)).toBe(false)
  expect(projectsSidebarVisible("open", 80)).toBe(true)
  expect(projectsSidebarVisible("closed", 200)).toBe(false)
})

test("toggling the left sidebar flips what is visible now, at any width", () => {
  expect(toggledProjectsSidebar("auto", 80)).toBe("open")
  expect(toggledProjectsSidebar("auto", 200)).toBe("closed")
  expect(toggledProjectsSidebar("open", 80)).toBe("closed")
  expect(toggledProjectsSidebar("closed", 200)).toBe("open")
})

test("the left sidebar is narrower than the right one", () => {
  expect(projectsSidebarWidth(200)).toBeLessThanOrEqual(28)
  expect(projectsSidebarWidth(200)).toBeLessThan(sidebarWidth(200))
  expect(projectsSidebarWidth(30)).toBeGreaterThanOrEqual(16)
})

test("the store tracks the left Projects sidebar mode and focus", () => {
  const store = createAppStore()
  expect(store.state.projectsSidebar).toBe("auto")
  expect(store.state.projectsSidebarFocus).toBe(false)
  store.setColumns(200)
  store.toggleProjectsSidebar()
  expect(store.state.projectsSidebar).toBe("closed")
  store.toggleProjectsSidebar()
  expect(store.state.projectsSidebar).toBe("open")
  store.setProjectsSidebarFocus(true)
  expect(store.state.projectsSidebarFocus).toBe(true)
  store.setProjectSidebarHighlight("prj_1")
  expect(store.state.projectSidebarHighlight).toBe("prj_1")
})
