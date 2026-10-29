import { expect, test } from "bun:test"
import { resizeBoundaryAt, resizeGuide, type ResizeBoundary } from "../src/state/paneResize"

const outer: ResizeBoundary = {
  id: "root", index: 0, secondIndex: 2, direction: "row",
  first: { left: 0, top: 0, right: 20, bottom: 10 },
  second: { left: 20, top: 0, right: 40, bottom: 10 },
  container: { left: 0, top: 0, right: 40, bottom: 10 },
}
const inner: ResizeBoundary = {
  id: "nested", index: 1, secondIndex: 3, direction: "column",
  first: { left: 20, top: 0, right: 40, bottom: 5 },
  second: { left: 20, top: 5, right: 40, bottom: 10 },
  container: { left: 20, top: 0, right: 40, bottom: 10 },
}

test("hover and drag hit the same boundary cells, with the innermost split winning intersections", () => {
  expect(resizeBoundaryAt([outer, inner], 19, 2)).toBe(outer)
  expect(resizeBoundaryAt([outer, inner], 20, 2)).toBe(outer)
  expect(resizeBoundaryAt([outer, inner], 18, 2)).toBeUndefined()
  expect(resizeBoundaryAt([outer, inner], 20, 5)).toBe(inner)
  expect(resizeBoundaryAt([inner, outer], 20, 4)).toBe(inner)
  expect(resizeBoundaryAt([outer, inner], 19, 10)).toBeUndefined()
  expect(resizeBoundaryAt([outer, inner], 40, 5)).toBeUndefined()
  expect(resizeBoundaryAt([], 20, 2)).toBeUndefined()
})

test("guides show the resize axis and only span their affected container", () => {
  const vertical = resizeGuide(outer)
  expect(vertical.rect).toEqual({ left: 20, top: 0, right: 21, bottom: 10 })
  expect(vertical.cursor).toBe("ew-resize")
  expect(vertical.text.split("\n")).toEqual(["│", "│", "│", "│", "│", "↔", "│", "│", "│", "│"])
  const horizontal = resizeGuide(inner)
  expect(horizontal.rect).toEqual({ left: 20, top: 5, right: 40, bottom: 6 })
  expect(horizontal.cursor).toBe("ns-resize")
  expect(horizontal.text).toBe("──────────↕─────────")
  expect(outer.secondIndex).toBe(2)
  expect(inner.secondIndex).toBe(3)
})
