import { describe, expect, test } from "bun:test"
import type { CapturedFrame } from "@opentui/core"
import type { JSX } from "@opentui/solid"
import { testRender } from "@opentui/solid"

import { ToolCard, toolCardTitle } from "../src/hya/tool-card"

/** Flatten a captured frame into trailing-space-free terminal lines. */
function frameLines(frame: CapturedFrame): string[] {
  return frame.lines
    .map((line) => line.spans.map((span) => span.text).join("").replace(/\s+$/, ""))
    .filter((line) => line.length > 0)
}

/** Render one card at a fixed width and return its settled terminal lines. */
async function renderCard(element: () => JSX.Element, width: number, ready: string) {
  const setup = await testRender(element, { width, height: 16, footerHeight: 0 })
  try {
    await setup.waitForFrame((frame) => frame.includes(ready))
    await setup.waitForVisualIdle()
    return frameLines(setup.captureSpans())
  } finally {
    setup.renderer.destroy()
  }
}

describe("toolCardTitle", () => {
  test("shows the executed command for shell tools", () => {
    expect(toolCardTitle("bash", { command: "cargo test --workspace", cwd: "/work" })).toBe(
      "$ cargo test --workspace",
    )
    expect(toolCardTitle("shell", { command: "ls\n  -la" })).toBe("$ ls -la")
  })

  test("falls back to the tool name when no command is available yet", () => {
    expect(toolCardTitle("bash", {})).toBe("bash")
    expect(toolCardTitle("bash", undefined)).toBe("bash")
  })

  test("shows the tool name and one-line arguments for other tools", () => {
    expect(toolCardTitle("read", { path: "src/main.ts", offset: 12, limit: 40 })).toBe(
      "read [path=src/main.ts, offset=12, limit=40]",
    )
    expect(toolCardTitle("glob", { pattern: "src/**/*.ts", caseSensitive: false })).toBe(
      "glob [pattern=src/**/*.ts, caseSensitive=false]",
    )
    expect(toolCardTitle("todowrite", {})).toBe("todowrite")
  })

  test("collapses structured and multi-line argument values onto one line", () => {
    expect(toolCardTitle("edit", { path: "a.ts", edits: [{ old: "1" }, { old: "2" }] })).toBe(
      "edit [path=a.ts, edits=[2 items]]",
    )
    expect(toolCardTitle("task", { prompt: "line one\nline two", options: { deep: true } })).toBe(
      "task [prompt=line one line two, options={…}]",
    )
  })

  test("caps one oversized value without dropping later arguments", () => {
    const title = toolCardTitle("custom", { blob: "x".repeat(4096), after: 7 })
    expect(title.startsWith("custom [blob=x")).toBe(true)
    expect(title.endsWith("…, after=7]")).toBe(true)
    expect(title.length).toBeLessThan(160)
  })
})

describe("ToolCard", () => {
  test("frames the body with a title bar and one padding row above and below", async () => {
    const lines = await renderCard(
      () => (
        <ToolCard title="$ echo hi" state="completed">
          <text fg="#EEEEEE">CARD_BODY</text>
        </ToolCard>
      ),
      48,
      "CARD_BODY",
    )

    expect(lines[0]).toStartWith("╭─ $ echo hi ─")
    expect(lines[0]).toEndWith("╮")
    expect(lines.at(-1)).toMatch(/^╰─+╯$/)

    const body = lines.findIndex((line) => line.includes("CARD_BODY"))
    expect(body).toBe(2)
    expect(lines[1]).toMatch(/^│\s*│$/)
    expect(lines[body + 1]).toMatch(/^│\s*│$/)
    expect(lines).toHaveLength(5)
  })

  test("truncates an overlong title instead of dropping it", async () => {
    const lines = await renderCard(
      () => (
        <ToolCard title="$ rg --no-heading --line-number 'needle' packages/hya-tui-ts/src | sort -u" state="running">
          <text fg="#EEEEEE">CARD_BODY</text>
        </ToolCard>
      ),
      48,
      "CARD_BODY",
    )

    expect(lines[0]).toStartWith("╭─ $ rg --no-heading")
    expect(lines[0]).toContain("…")
    expect(lines[0]).toEndWith("╮")
    for (const line of lines) expect(Bun.stringWidth(line)).toBe(48)
  })

  test("carries the title in the frame that first paints the body", async () => {
    // A static transcript repaints only on demand, so a title that lands a frame
    // after the body leaves the card permanently untitled on screen.
    const setup = await testRender(
      () => (
        <ToolCard title="$ echo hi" state="completed">
          <text fg="#EEEEEE">CARD_BODY</text>
        </ToolCard>
      ),
      { width: 48, height: 16, footerHeight: 0 },
    )
    try {
      const frame = await setup.waitForFrame((value) => value.includes("CARD_BODY"))
      expect(frame).toContain("$ echo hi")
    } finally {
      setup.renderer.destroy()
    }
  })
})
