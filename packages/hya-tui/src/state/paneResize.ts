import type { LayoutDirection, Rect } from "./panes"

/** A boundary between adjacent visible siblings, indexed into the saved tree. */
export interface ResizeBoundary {
  id: string
  index: number
  secondIndex: number
  direction: LayoutDirection
  first: Rect
  second: Rect
  container: Rect
}

/** Hover and drag share the same two-cell hit area; the innermost split wins. */
export function resizeBoundaryAt(boundaries: readonly ResizeBoundary[], x: number, y: number): ResizeBoundary | undefined {
  let found: ResizeBoundary | undefined
  let smallest = Infinity
  for (const edge of boundaries) {
    const row = edge.direction === "row"
    const boundary = row ? edge.second.left : edge.second.top
    const coordinate = row ? x : y, cross = row ? y : x
    const low = row ? edge.container.top : edge.container.left, high = row ? edge.container.bottom : edge.container.right
    if ((coordinate !== boundary && coordinate !== boundary - 1) || cross < low || cross >= high) continue
    const area = (edge.container.right - edge.container.left) * (edge.container.bottom - edge.container.top)
    if (area > 0 && area < smallest) { found = edge; smallest = area }
  }
  return found
}

/** A one-cell overlay on the actual divider; arrows describe its movement. */
export function resizeGuide(edge: ResizeBoundary): { rect: Rect; text: string; cursor: "ew-resize" | "ns-resize" } {
  const vertical = edge.direction === "row"
  const rect = vertical
    ? { left: edge.second.left, right: edge.second.left + 1, top: edge.container.top, bottom: edge.container.bottom }
    : { left: edge.container.left, right: edge.container.right, top: edge.second.top, bottom: edge.second.top + 1 }
  const length = Math.max(0, vertical ? rect.bottom - rect.top : rect.right - rect.left)
  const cells = Array.from({ length }, (_, index) => index === Math.floor(length / 2) ? (vertical ? "↔" : "↕") : (vertical ? "│" : "─"))
  return { rect, text: cells.join(vertical ? "\n" : ""), cursor: vertical ? "ew-resize" : "ns-resize" }
}
