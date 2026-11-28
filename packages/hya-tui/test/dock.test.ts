import { expect, test } from "bun:test"
import { builtinPreset } from "../src/config/presets"
import { dockRowCount, subagentDockRows, todoDockRows } from "../src/state/dock"
import { chatPaneLayout, defaultPaneLayout } from "../src/state/panes"
import { bandRule, statusLineShown } from "../src/state/statusline"
import { createAppStore } from "../src/state/store"

function dockStore() {
  const store = createAppStore()
  store.setStructure(builtinPreset("dock"))
  store.setPaneLayout(chatPaneLayout())
  store.openSession({ id: "hysec_1", agent: "hya-main", workdir: "/w" })
  return store
}

const todo = (id: string, content: string, status: string) => ({ id, content, status: `TODO_STATUS_${status.toUpperCase()}` })

test("open todos: in progress first, at most max_rows rows, the rest folded with the done count", () => {
  const store = dockStore()
  expect(todoDockRows(store.state, 120)).toEqual([])
  store.setTodos([todo("1", "write tests", "pending"), todo("2", "fix the bug", "in_progress"), todo("3", "ship", "completed"), todo("4", "docs", "pending"), todo("5", "blocked one", "blocked")])
  expect(todoDockRows(store.state, 120).map((row) => `${row.glyph} ${row.text} ${row.tone}`)).toEqual([
    "◐ fix the bug activity",
    "○ write tests muted",
    "… +2 more · 1 done · /todos muted",
  ])
  store.setTodos([todo("1", "only", "pending"), todo("2", "ship", "completed")])
  expect(todoDockRows(store.state, 120).map((row) => row.text)).toEqual(["only  (1 done)"])
  store.setTodos([todo("1", "ship", "completed")])
  expect(todoDockRows(store.state, 120)).toEqual([])
})

test("the todo rows yield to a visible Todos pane and are off outside dock and workspace", () => {
  const store = dockStore()
  store.setTodos([todo("1", "write tests", "pending")])
  store.setStructure(builtinPreset("workspace"))
  store.setPaneLayout(defaultPaneLayout())
  // 190 columns: the sidebar's Todos pane shows them; 120: the sidebar is hidden, the dock takes over.
  expect(todoDockRows(store.state, 190)).toEqual([])
  expect(todoDockRows(store.state, 120)).toHaveLength(1)
  store.setStructure(builtinPreset("statusline"))
  expect(todoDockRows(store.state, 120)).toEqual([])
})

test("running subagents: one row each, waiting ones in the warning tone, at most three", () => {
  const store = dockStore()
  const child = (n: number, busy = true) => ({ id: `hysec_c${n}`, parent: "hysec_1", agent: "scout", title: `hya-scout-${n}`, workdir: "/w", busy })
  store.setSessions([store.state.selected!, child(1, true), child(2, false)])
  expect(subagentDockRows(store.state, 120).map((row) => `${row.glyph} ${row.text}`)).toEqual(["↳ hya-scout-1"])
  store.setSessions([store.state.selected!, child(1), child(3), child(4), child(5)])
  expect(subagentDockRows(store.state, 120).map((row) => row.text)).toEqual(["hya-scout-1", "hya-scout-3", "+2 more · /subagents"])
  expect(dockRowCount(store.state, 120)).toBe(3)
  store.setStructure({ ...builtinPreset("dock"), subagents: { enabled: false } })
  expect(subagentDockRows(store.state, 120)).toEqual([])
})

test("a band status line needs the band input; the band rule carries border info", () => {
  const store = dockStore()
  expect(statusLineShown(store.state, 120, "band")).toBe(true)
  expect(statusLineShown(store.state, 120, "below")).toBe(false)
  // A boxed input with placement = "band": the line goes below instead.
  store.setStructure({ ...builtinPreset("dock"), composer: { ...builtinPreset("dock").composer, style: "box" } })
  expect(statusLineShown(store.state, 120, "below")).toBe(true)
  store.setStructure({ ...builtinPreset("dock"), statusline: { ...builtinPreset("dock").statusline, enabled: false }, composer: { ...builtinPreset("dock").composer, border_info: ["mode"] } })
  expect(bandRule(store.state, 20, {})).toBe("─────────── manual ─")
  expect(Bun.stringWidth(bandRule(store.state, 20, {}))).toBe(20)
})
