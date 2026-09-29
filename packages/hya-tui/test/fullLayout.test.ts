import { expect, test } from "bun:test"
import { defaultPaneLayout, isDefaultPaneTree, paneLeaves, paneRects, parsePaneLayout, resizePane, setPaneKind, visiblePaneLayout } from "../src/state/panes"

test("default layout owns the Projects, Conversation, Sessions, Todos, and Context rectangles", () => {
  const layout = defaultPaneLayout()
  expect(isDefaultPaneTree(layout)).toBe(true)
  expect(paneLeaves(layout.root).map((pane) => pane.kind)).toEqual(["projects", "conversation", "sessions", "todos", "context"])
  const rects = paneRects(layout.root)
  const projects = rects.get("pane-2")!
  const conversation = rects.get("pane-1")!
  const sessions = rects.get("pane-3")!
  const todos = rects.get("pane-4")!
  const context = rects.get("pane-5")!
  expect(projects.right).toBeLessThanOrEqual(conversation.left)
  expect(conversation.right).toBeLessThanOrEqual(sessions.left)
  expect(sessions.bottom).toBeLessThanOrEqual(todos.top)
  expect(todos.bottom).toBeLessThanOrEqual(context.top)
  expect(paneLeaves(visiblePaneLayout(layout, 80, "auto", "auto").root).map((pane) => pane.kind)).toEqual(["conversation"])
  expect(paneLeaves(visiblePaneLayout(layout, 120, "auto", "auto").root).map((pane) => pane.kind)).toEqual(["conversation", "sessions", "todos", "context"])
})

test("side panes can be selected, resized, reassigned, and remain visible after reload", () => {
  const original = defaultPaneLayout()
  const projects = { ...original, active: "pane-2" }
  const resized = resizePane(projects, 0.05)
  expect(isDefaultPaneTree(resized)).toBe(false)
  expect(paneRects(resized.root).get("pane-2")!.right).toBeGreaterThan(paneRects(original.root).get("pane-2")!.right)
  const sessions = setPaneKind({ ...resized, active: "pane-3" }, "jobs")
  expect(paneLeaves(parsePaneLayout(JSON.parse(JSON.stringify(sessions)))!.root).map((pane) => pane.kind)).toEqual(["projects", "conversation", "jobs", "todos", "context"])
})

test("version 1 center-only layouts migrate into the complete workspace", () => {
  const old = { version: 1, active: "pane-2", root: { type: "split", axis: "vertical", weight: 0.5,
    first: { type: "pane", id: "pane-1", kind: "conversation" }, second: { type: "pane", id: "pane-2", kind: "jobs" } } }
  const migrated = parsePaneLayout(old)!
  expect(migrated.version).toBe(2)
  expect(migrated.active).toBe("pane-2")
  expect(paneLeaves(migrated.root).map((pane) => pane.kind)).toEqual(["projects", "conversation", "jobs", "sessions", "todos", "context"])
})
