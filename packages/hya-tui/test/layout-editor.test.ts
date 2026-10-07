import { expect, test } from "bun:test"
import { createLayoutEditor, layoutEditorBack, layoutEditorChoose, layoutEditorKey, layoutEditorPaste, layoutEditorPreview, layoutEditorRows, layoutTreeRows, reconcileLayoutEditor, type LayoutEditorState } from "../src/state/layoutEditor"
import { closePane, defaultPaneLayout, openLayoutPane, paneLeaves, parsePaneLayout, resolvePaneNode, setPaneSize, type PaneLayout } from "../src/state/panes"

const key = (name: string, sequence = "") => ({ name, sequence, ctrl: false, meta: false, shift: false })
function editor(selected: string, layout = openLayoutPane(defaultPaneLayout())) {
  let state = createLayoutEditor(selected)
  const apply = (outcome: ReturnType<typeof layoutEditorKey>) => { state = outcome.state; layout = outcome.layout ?? layout; return outcome }
  return { get state() { return state }, get layout() { return layout },
    key: (name: string, sequence = "", shift = false) => apply(layoutEditorKey(layout, state, { ...key(name, sequence), shift })),
    choose: (id: string) => apply(layoutEditorChoose(layout, state, id)),
    paste: (text: string) => apply(layoutEditorPaste(state, text)),
  }
}

test("opening a layout pane adds one regular persisted leaf and reuses it", () => {
  const layout = openLayoutPane(defaultPaneLayout())
  expect(resolvePaneNode(layout, layout.active)).toMatchObject({ type: "pane", kind: "layout" })
  expect(parsePaneLayout(JSON.parse(JSON.stringify(layout)))).toEqual(layout)
  expect(paneLeaves(openLayoutPane(layout).root).filter((pane) => pane.kind === "layout")).toHaveLength(1)
  expect(layoutTreeRows(layout.root).some((row) => row.node.type === "pane" && row.node.kind === "todos")).toBe(true)
})

test("marking is independent of cursor navigation and Enter still opens cursor actions", () => {
  for (const mark of [{ ...key("return"), shift: true }, key("space"), key("linefeed"), { ...key("j"), ctrl: true }]) {
    const layout = openLayoutPane(defaultPaneLayout())
    const marked = layoutEditorKey(layout, createLayoutEditor("group-3"), mark).state
    expect(marked).toMatchObject({ selected: "group-3", marked: "group-3", stage: { type: "tree" } })
    const moved = layoutEditorKey(layout, marked, key("end")).state
    expect(moved).toMatchObject({ selected: "pane-5", marked: "group-3" })
    const actions = layoutEditorKey(layout, moved, key("return")).state
    expect(actions).toMatchObject({ selected: "pane-5", stage: { type: "actions" } })
    expect(layoutEditorKey(layout, marked, mark).state.marked).toBeUndefined()
    expect(layoutEditorKey(layout, marked, key("escape")).state.marked).toBeUndefined()
    expect(layoutEditorKey(layout, moved, mark).state.marked).toBe("pane-5")
  }
})

test("i inserts before the cursor despite another mark and selects the new pane", () => {
  const e = editor("pane-3")
  e.key("space"); e.key("down"); e.key("i")
  expect(layoutEditorPreview(e.state)).toBe("Insert here · before pane-4")
  const original = e.layout
  e.key("escape")
  expect(e.state).toMatchObject({ selected: "pane-4", marked: "pane-3", stage: { type: "tree" } })
  expect(e.layout).toBe(original)
  e.key("i"); e.choose("jobs")
  const group = resolvePaneNode(e.layout, "group-3")
  expect(group.type === "split" && group.children.map((child) => child.node.id)).toEqual(["pane-3", "pane-9", "pane-4", "pane-5"])
  expect(e.state).toMatchObject({ selected: "pane-9", marked: "pane-3" })
})

test("root insertion chooses a child position before choosing the job", () => {
  const e = editor("group-4")
  e.key("i")
  expect(e.state.stage.type).toBe("insert-position")
  e.choose("1")
  expect(layoutEditorPreview(e.state)).toBe("Insert here · group-4 position 1")
  e.choose("status")
  expect(e.state.selected).toBe("pane-9")
  const root = e.layout.root
  expect(root.type === "split" && root.children[1]?.node).toMatchObject({ id: "pane-9", kind: "status" })
})

test("w r/c wraps a frozen marked target, defaults after, and Tab toggles before", () => {
  for (const [direction, chord] of [["row", "r"], ["column", "c"]] as const) {
    for (const before of [false, true]) {
      const e = editor("pane-5")
      e.key("return", "", true); e.key("home"); e.key("w")
      expect(e.state.stage).toEqual({ type: "wrap", target: "pane-5" })
      e.key("x"); expect(e.state.stage.type).toBe("wrap")
      e.key(chord)
      if (before) e.key("tab")
      expect(layoutEditorPreview(e.state)).toContain(`new pane ${before ? "before" : "after"}`)
      e.choose("jobs")
      const pane = layoutTreeRows(e.layout.root).find((row) => row.id === "pane-5")!
      expect(pane.parent?.direction).toBe(direction)
      const siblings = pane.parent!.children.map((child) => child.node.id)
      expect(siblings.indexOf("pane-9") - siblings.indexOf("pane-5")).toBe(before ? -1 : 1)
      expect(e.state.selected).toBe("pane-9")
    }
  }
  const e = editor("pane-5")
  e.key("w"); e.key("escape")
  expect(e.state.stage.type).toBe("tree")
  expect(paneLeaves(e.layout.root)).toHaveLength(8)
})

test("a marked conversation group wraps together while its original leaves and cursor survive", () => {
  const e = editor("group-2")
  const ids = paneLeaves(resolvePaneNode(e.layout, "group-2")).map((pane) => pane.id)
  e.key("space"); e.key("end"); e.key("w"); e.key("r"); e.choose("jobs")
  const group = layoutTreeRows(e.layout.root).find((row) => row.id === "group-2")!
  expect(paneLeaves(group.node).map((pane) => pane.id)).toEqual(ids)
  expect(group.parent?.children[group.index + 1]?.node).toMatchObject({ kind: "jobs", id: "pane-9" })
  expect(e.state).toMatchObject({ selected: "pane-9", marked: "group-2" })
})

test("direct removal follows sibling order, honors a remote mark, and clears deleted marks", () => {
  const e = editor("pane-4")
  e.key("space"); e.key("home"); e.key("backspace")
  expect(e.state).toMatchObject({ selected: "pane-5", stage: { type: "tree" } })
  expect(e.state.marked).toBeUndefined()
  expect(paneLeaves(e.layout.root).some((pane) => pane.id === "pane-4")).toBe(false)
  e.key("delete")
  expect(e.state.selected).toBe("pane-3")
  e.key("delete")
  expect(paneLeaves(e.layout.root).some((pane) => ["pane-3", "pane-4", "pane-5"].includes(pane.id))).toBe(false)
  expect(paneNodesForTest(e.layout).includes(e.state.selected)).toBe(true)
})

function paneNodesForTest(layout: PaneLayout): string[] { return layoutTreeRows(layout.root).map((row) => row.id) }

test("group deletion confirms with Cancel selected and protects viewer/editor groups", () => {
  const e = editor("group-3")
  e.key("space"); e.key("home"); e.key("delete")
  expect(e.state.stage).toEqual({ type: "remove", target: "group-3", direct: true })
  expect(layoutEditorRows(e.layout, e.state)[1]?.detail).toContain("3 pane(s)")
  e.key("return") // Cancel; no tree mutation.
  expect(resolvePaneNode(e.layout, "group-3")).toBeDefined()
  e.key("delete"); e.key("down"); e.key("return")
  expect(paneNodesForTest(e.layout)).not.toContain("group-3")
  expect(e.state.marked).toBeUndefined()
  for (const target of ["pane-1", "pane-6", "group-2", "root"]) {
    const protectedEditor = editor(target === "root" ? "group-4" : target)
    const before = protectedEditor.layout
    protectedEditor.key("delete")
    expect(protectedEditor.state.error).toContain("Cannot remove")
    expect(protectedEditor.layout).toBe(before)
  }
})

test("direct shortcuts stay inactive in forms and stale marks/targets reconcile after reload", () => {
  const e = editor("pane-5")
  e.key("space"); e.key("return"); e.choose("weight")
  e.key("i", "i"); e.key("w", "w"); e.key("delete")
  expect(e.state.stage).toMatchObject({ type: "weight", value: "i" })
  expect(paneLeaves(e.layout.root)).toHaveLength(8)
  e.key("escape"); e.key("escape"); e.key("w"); e.key("r")
  const changed = closePane(e.layout, "pane-5")
  expect(reconcileLayoutEditor(changed, e.state)).toMatchObject({ marked: undefined, stage: { type: "tree" } })
  const mark = layoutEditorKey(e.layout, createLayoutEditor("pane-4"), key("space")).state
  expect(reconcileLayoutEditor(closePane(e.layout, "pane-4"), mark).marked).toBeUndefined()
  const cursorElsewhere = layoutEditorKey(e.layout, mark, key("home")).state
  const pending = layoutEditorKey(e.layout, cursorElsewhere, key("w")).state
  expect(reconcileLayoutEditor(closePane(e.layout, "pane-4"), pending)).toMatchObject({
    selected: e.layout.root.id, marked: undefined, stage: { type: "tree" }, error: "The target no longer exists",
  })
})

test("tree navigation selects passive/hidden nodes without changing workspace focus", () => {
  const e = editor("pane-1")
  const active = e.layout.active
  e.key("left")
  expect(e.state.selected).toBe("group-2")
  e.key("right")
  expect(e.state.selected).toBe("pane-6")
  e.key("down")
  expect(e.state.selected).toBe("pane-7")
  e.key("end")
  expect(e.state.selected).toBe("pane-5")
  expect(e.layout.active).toBe(active)
  e.key("return")
  expect(e.state.stage.type).toBe("actions")
  e.key("escape")
  expect(e.state.stage.type).toBe("tree")
})

test("insert before/after and add child share ordered tree operations", () => {
  for (const [action, expected] of [["before", 1], ["after", 2]] as const) {
    const e = editor("pane-7")
    e.key("return"); e.choose(action); e.choose("jobs")
    const parent = resolvePaneNode(e.layout, "group-2")
    expect(parent.type === "split" && parent.children.findIndex((child) => child.node.type === "pane" && child.node.kind === "jobs")).toBe(expected)
    expect(parsePaneLayout(e.layout)).toEqual(e.layout)
  }
  const e = editor("group-3")
  e.key("return"); e.choose("child"); e.choose("status")
  const parent = resolvePaneNode(e.layout, "group-3")
  expect(parent.type === "split" && parent.children.at(-1)?.node).toMatchObject({ kind: "status" })
})

test("weight changes retain sibling slots, reject invalid input and allow column content", () => {
  const e = editor("pane-3")
  const before = resolvePaneNode(e.layout, "group-3")
  e.key("return"); e.choose("weight"); e.paste("2.5")
  expect(e.state.stage).toMatchObject({ type: "weight", value: "2.5" })
  e.key("return")
  const after = resolvePaneNode(e.layout, "group-3")
  expect(after.type === "split" && after.children[0]?.size).toEqual({ mode: "weight", value: 2.5 })
  expect(after.type === "split" && after.children.slice(1)).toEqual(before.type === "split" && before.children.slice(1))
  const valid = e.layout
  e.key("return"); e.choose("weight"); e.paste("0"); e.key("return")
  expect(e.state.error).toContain("positive finite")
  expect(e.layout).toBe(valid)
  e.key("backspace"); e.paste("content"); e.key("return")
  expect(layoutTreeRows(e.layout.root).find((row) => row.id === "pane-3")?.size).toEqual({ mode: "content" })
  expect(() => setPaneSize(e.layout, "pane-2", { mode: "content" })).toThrow("only in a column")
  expect(() => setPaneSize(e.layout, "root", { mode: "weight", value: 1 })).toThrow("root")
  expect(() => setPaneSize(e.layout, "pane-3", { mode: "weight", value: Infinity })).toThrow("positive finite")
  const huge = setPaneSize(e.layout, "pane-3", { mode: "weight", value: Number.MAX_VALUE })
  expect(() => setPaneSize(huge, "pane-4", { mode: "weight", value: Number.MAX_VALUE })).toThrow("Total")
})

test("move offers valid destinations and indexes after source removal", () => {
  const e = editor("pane-4")
  e.key("return"); e.choose("move")
  expect(layoutEditorRows(e.layout, e.state).some((row) => row.id === "group-3")).toBe(true)
  e.choose("group-3")
  expect(layoutEditorRows(e.layout, e.state).map((row) => row.id)).toEqual(["0", "1", "2"])
  e.choose("0")
  expect(layoutTreeRows(e.layout.root).find((row) => row.id === "pane-4")?.index).toBe(0)
  const container = editor("group-3")
  container.key("return"); container.choose("move")
  expect(layoutEditorRows(container.layout, container.state).some((row) => row.id === "group-3")).toBe(false)
})

test("wrap and assign retain ids; removal requires explicit choice and protects the editor", () => {
  const e = editor("pane-5")
  e.key("return"); e.choose("row"); e.choose("models")
  const context = layoutTreeRows(e.layout.root).find((row) => row.id === "pane-5")!
  expect(context.parent?.direction).toBe("row")
  e.key("return"); e.choose("assign"); e.choose("jobs")
  expect(resolvePaneNode(e.layout, "pane-5")).toMatchObject({ kind: "jobs" })
  e.key("return"); e.choose("remove"); e.key("return")
  expect(resolvePaneNode(e.layout, "pane-5")).toBeDefined() // Cancel is the default.
  e.key("return"); e.choose("remove"); e.choose("remove")
  expect(paneLeaves(e.layout.root).some((pane) => pane.id === "pane-5")).toBe(false)
  const protectedEditor = editor("pane-1")
  protectedEditor.key("return"); protectedEditor.choose("remove")
  expect(protectedEditor.state.error).toContain("Cannot remove")
  expect(protectedEditor.state.stage.type).toBe("actions")
})

test("external edits repair removed selections and pending destinations; forms cancel without edits", () => {
  const layout = openLayoutPane(defaultPaneLayout())
  const state: LayoutEditorState = { ...createLayoutEditor("pane-2"), stage: { type: "position", destination: "group-3" } }
  expect(reconcileLayoutEditor(closePane(layout, "group-3"), state).stage.type).toBe("tree")
  expect(reconcileLayoutEditor(closePane(layout, "pane-2"), state).selected).toBe(layout.root.id)
  const e = editor("pane-5", layout)
  e.key("return"); e.choose("weight"); e.paste("5")
  e.key("escape")
  expect(e.layout).toBe(layout)
  expect(e.state.stage.type).toBe("actions")
  expect(layoutEditorBack(e.state).stage.type).toBe("tree")
  expect(layoutEditorKey(layout, e.state, { ...key("x", "x"), ctrl: true }).state).toBe(e.state)
})

test("Shift arrows bubble the cursor within row/column siblings, preserving the mark and editor focus", () => {
  for (const [id, before, after] of [["pane-4", "pane-3", "pane-5"], ["group-2", "pane-2", "group-3"]]) {
    const e = editor(id!)
    const original = layoutTreeRows(e.layout.root).find((row) => row.id === id)!
    const slot = original.parent!.children[original.index]
    const active = e.layout.active
    e.key("space"); e.key("up") // Mark target but put cursor elsewhere.
    const cursor = e.state.selected
    e.key("down") // Cursor returns to target.
    expect(e.state.selected).toBe(id)
    e.key("up", "", true)
    let row = layoutTreeRows(e.layout.root).find((row) => row.id === id)!
    expect(row.parent!.children[row.index + 1]?.node.id).toBe(before)
    expect(row.parent!.children[row.index]).toBe(slot)
    expect(e.state.selected).toBe(id)
    expect(e.state.marked).toBe(id)
    expect(e.layout.active).toBe(active)
    e.key("down", "", true); e.key("down", "", true)
    row = layoutTreeRows(e.layout.root).find((row) => row.id === id)!
    expect(row.parent!.children[row.index - 1]?.node.id).toBe(after)
    expect(parsePaneLayout(JSON.parse(JSON.stringify(e.layout)))).toEqual(e.layout)
    expect(cursor).not.toBe(id)
  }
})

test("bubble menu matches direct keys; edge/root moves do nothing and an unrelated mark does not move", () => {
  const e = editor("pane-3")
  e.key("space"); e.key("down")
  e.key("up", "", true)
  expect(e.state).toMatchObject({ selected: "pane-4", marked: "pane-3", stage: { type: "tree" } })
  let row = layoutTreeRows(e.layout.root).find((row) => row.id === "pane-4")!
  expect(row.index).toBe(0)
  expect(e.key("up", "", true).layout).toBeUndefined()
  e.key("return")
  expect(layoutEditorRows(e.layout, e.state).map((row) => row.id)).toContain("bubble-next")
  e.choose("bubble-next")
  row = layoutTreeRows(e.layout.root).find((row) => row.id === "pane-4")!
  expect(row.index).toBe(1)
  e.key("home")
  expect(e.key("down", "", true).layout).toBeUndefined()
  expect(e.state.error).toBeUndefined()
})
