import { afterEach, expect, test } from "bun:test"
import { TextAttributes, type RGBA } from "@opentui/core"
import { testRender } from "@opentui/solid"
import { PaneFrame } from "../src/components/PaneFrame"
import { RenderTree } from "../src/extensions/renderTree"
import { validateRenderNode } from "../src/extensions/wire"
import { colors, defaultThemeName, setTheme } from "../src/theme"

let setup: Awaited<ReturnType<typeof testRender>> | undefined
afterEach(() => { setup?.renderer.destroy(); setup = undefined; setTheme(defaultThemeName) })
const hex = (color: RGBA | undefined) => color ? `#${[color.r, color.g, color.b].map((n) => Math.round(n * 255).toString(16).padStart(2, "0")).join("")}` : undefined
const span = (text: string) => setup?.captureSpans().lines.flatMap((line) => line.spans).find((item) => item.text.includes(text))

test("a focused frame has a blue border and an independently neutral title", async () => {
  setup = await testRender(() => <PaneFrame kind="subagents" title="Window title" focused><text>body</text></PaneFrame>, { width: 40, height: 6 })
  await setup.renderOnce()
  expect(hex(span("┌")?.fg)).toBe(colors.focus)
  expect(hex(span("Window title")?.fg)).toBe(colors.heading)
  setTheme("light")
  await setup.renderOnce()
  expect(hex(span("┌")?.fg)).toBe(colors.focus)
  expect(hex(span("Window title")?.fg)).toBe(colors.heading)
})

test("extension roles validate and render with the same meanings; legacy accent means focus", async () => {
  const roles = ["fg", "muted", "focus", "heading", "link", "activity", "success", "warning", "error", "border", "accent"] as const
  const tree = { kind: "column" as const, children: roles.map((role) => ({ kind: "text" as const, text: `role-${role}`, style: { color: role } })) }
  expect(validateRenderNode(tree, { slot: "forbid", actions: false })).toEqual(tree)
  setup = await testRender(() => <RenderTree node={tree} />, { width: 40, height: 15 })
  for (const theme of ["hya", "light", "ember"]) {
    setTheme(theme)
    await setup.renderOnce()
    for (const role of roles) expect(hex(span(`role-${role}`)?.fg)).toBe(role === "accent" ? colors.focus : colors[role])
    expect((span("role-link")?.attributes ?? 0) & TextAttributes.UNDERLINE).toBe(TextAttributes.UNDERLINE)
  }
})
