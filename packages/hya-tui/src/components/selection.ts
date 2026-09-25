/**
 * Mouse selection (docs/tui.md "Copy"): OpenTUI's text renderables are
 * selectable; dragging over the transcript (or any text) highlights it, and
 * on release App.tsx copies the selected text with OSC 52.
 *
 * The highlight color is per renderable (`selectionBg`), and Markdown and
 * code blocks create their text renderables themselves, so instead of
 * passing it to every `<text>`, `paintSelection` sets the theme's
 * `colors.selection` on every renderable under the root that has a
 * selection color — on each left mouse press, before the drag repaints the
 * highlight. The text keeps its own foreground color.
 */
import type { Renderable } from "@opentui/core"

const painted = new WeakMap<Renderable, string>()

/** Set `selectionBg = color` on `root` and every descendant that has one (skips nodes already painted with it). */
export function paintSelection(root: Renderable, color: string): void {
  const stack: Renderable[] = [root]
  while (stack.length) {
    const node = stack.pop()!
    if ("selectionBg" in node && painted.get(node) !== color) {
      ;(node as Renderable & { selectionBg: string | undefined }).selectionBg = color
      painted.set(node, color)
    }
    for (const child of node.getChildren()) stack.push(child)
  }
}
