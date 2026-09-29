import { expect, test } from "bun:test"
import {
  closePane, movePaneFocus, parsePaneLayout, paneLeaves,
  resizePane, setPaneKind, splitPane, type PaneLayout,
} from "../src/state/panes"

const singlePaneLayout = (): PaneLayout => ({ version: 2, active: "pane-1", root: { type: "pane", id: "pane-1", kind: "conversation" } })

test("nested splits keep one conversation and focus the nearest pane in each direction", () => {
  const leftRight = splitPane(singlePaneLayout(), "vertical", "jobs")
  expect(paneLeaves(leftRight.root).map((pane) => pane.kind)).toEqual(["conversation", "jobs"])
  const nested = splitPane(leftRight, "horizontal", "todos")
  expect(paneLeaves(nested.root).map((pane) => pane.kind)).toEqual(["conversation", "jobs", "todos"])
  expect(movePaneFocus(nested, "left").active).toBe("pane-1")
  expect(movePaneFocus(nested, "up").active).toBe("pane-2")
  expect(movePaneFocus({ ...nested, active: "pane-1" }, "right").active).toBe("pane-2")
})

test("assignment and close preserve the conversation; invalid saved layouts are refused", () => {
  const split = splitPane(singlePaneLayout(), "vertical", "jobs")
  const models = setPaneKind(split, "models")
  expect(paneLeaves(models.root).map((pane) => pane.kind)).toEqual(["conversation", "models"])
  const moved = setPaneKind(models, "conversation")
  expect(paneLeaves(moved.root).map((pane) => pane.kind)).toEqual(["models", "conversation"])
  expect(paneLeaves(closePane(models).root).map((pane) => pane.kind)).toEqual(["conversation"])
  expect(() => closePane(moved)).toThrow("conversation")
  expect(parsePaneLayout(JSON.parse(JSON.stringify(models)))).toEqual(models)
  expect(parsePaneLayout({ ...models, active: "missing" })).toBeUndefined()
  expect(parsePaneLayout({ ...models, root: { ...models.root, weight: 0 } })).toBeUndefined()
})

test("resize changes only the focused split and closes a nested auxiliary pane", () => {
  const nested = splitPane(splitPane(singlePaneLayout(), "vertical", "jobs"), "horizontal", "todos")
  const resized = resizePane(nested, 0.1)
  expect(resized.root.type).toBe("split")
  if (resized.root.type !== "split" || resized.root.second.type !== "split") return
  expect(resized.root.weight).toBe(0.5)
  expect(resized.root.second.weight).toBeCloseTo(0.4)
  const closed = closePane(resized)
  expect(paneLeaves(closed.root).map((pane) => pane.kind)).toEqual(["conversation", "jobs"])
  expect(parsePaneLayout(closed)).toEqual(closed)
})
