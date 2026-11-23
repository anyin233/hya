// Built-in structure presets (ADR-0036, docs/tui-config.md): which regions
// `minimal`, `statusline` (the default), and `workspace` draw at 80, 120, and
// 190 columns, and the status line's segments and elision order.

import { writeFile } from "node:fs/promises"
import { dirname, join } from "node:path"
import { expect, hyaTui, test, textStep } from "./hya"
import type { Tui } from "./harness"

/** CSS width whose PTY conversion (tui-exp/driver.ts `viewportCells`) gives exactly `columns`. */
const widthFor = (columns: number) => Math.ceil(columns * 8.43 + 15)
const input = "Message, !shell, or @file"

/** The row right below the input box's bottom border, or `undefined`. */
async function belowInput(term: Tui): Promise<string | undefined> {
  const lines = await term.lines()
  const at = lines.findIndex((line) => line.includes(input))
  if (at < 0) return undefined
  return lines[at + 2]
}

async function regions(term: Tui) {
  const lines = await term.lines()
  const text = lines.join("\n")
  return {
    projects: /─Projects─/.test(text),
    sessions: /─Sessions─/.test(text),
    context: lines.some((line) => /^\s*Context\s*$/.test(line.slice(-31))),
  }
}

test.describe("statusline (default)", () => {
  test.use({ model: { steps: [textStep("hello back")], contextLimit: 100_000 } })

  for (const columns of [80, 120, 190]) {
    test(`one status row below the input and no sidebars at ${columns} columns`, async ({ tui, backend, fakeModel }, testInfo) => {
      fakeModel!.setUsage({ prompt: 42_000, completion: 300, reasoning: 0 })
      const term = await tui(hyaTui(backend), { viewport: { width: widthFor(columns), height: 640 } })
      await term.waitForText(input)
      await expect.poll(async () => (await belowInput(term)) ?? "", { timeout: 15_000 }).toMatch(/model · manual/)
      expect(await regions(term)).toEqual({ projects: false, sessions: false, context: false })
      await term.type("hi")
      await term.press("Enter")
      await term.waitForText("hello back", 20_000)
      await expect.poll(async () => (await belowInput(term)) ?? "", { timeout: 15_000 }).toMatch(/ctx ━━─── 42% · 42\.3k tok/)
      const status = (await belowInput(term))!
      // At 80 columns the work dir and the session title are the first to go.
      if (columns >= 120) expect(status).toMatch(/work/)
      await term.attach(testInfo, `statusline-${columns}`)
    })
  }
})

test.describe("status line elision", () => {
  test.use({ model: { steps: [textStep("hello back")], contextLimit: 100_000 } })

  test("the session title goes first, then tokens; model, mode, and ctx stay", async ({ tui, backend, fakeModel }, testInfo) => {
    fakeModel!.setUsage({ prompt: 42_000, completion: 300, reasoning: 0 })
    await writeFile(join(dirname(backend.dir), "config", "hya", "tui.toml"), '[statusline]\nsegments = ["model", "mode", "agent", "cwd", "ctx", "tokens", "session"]\n')
    const term = await tui(hyaTui(backend), { viewport: { width: widthFor(120), height: 640 } })
    await term.waitForText(input)
    await term.type("hi")
    await term.press("Enter")
    await term.waitForText("hello back", 20_000)
    // 120 columns: everything fits, the session id at the right end.
    await expect.poll(async () => (await belowInput(term)) ?? "", { timeout: 15_000 }).toMatch(/^ model · manual · hya-main · work · ctx ━━─── 42% · 42\.3k tok {2,}hysec_\w+ $/)
    await term.resize(widthFor(80), 640)
    await expect.poll(async () => (await belowInput(term)) ?? "").toMatch(/^ model · manual · hya-main · work · ctx ━━─── 42% · 42\.3k tok\s*$/)
    await term.resize(widthFor(40), 640)
    await expect.poll(async () => (await belowInput(term)) ?? "").toMatch(/^ model · manual · ctx ━━─── 42%\s*$/)
    await term.attach(testInfo, "elision-40")
  })
})

test.describe("minimal", () => {
  test.use({ preset: "minimal" })

  test("no status row; model and mode on the input border", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: { width: widthFor(120), height: 640 } })
    await term.waitForText(input)
    const top = async () => {
      const lines = await term.lines()
      const at = lines.findIndex((line) => line.includes(input))
      return at > 0 ? lines[at - 1]! : ""
    }
    await expect.poll(top, { timeout: 15_000 }).toMatch(/─ .+ · manual ─/)
    expect((await belowInput(term))?.trim() ?? "").toBe("")
    expect(await regions(term)).toEqual({ projects: false, sessions: false, context: false })
    await term.attach(testInfo, "minimal-120")
  })
})

test.describe("workspace", () => {
  test.use({ preset: "workspace" })

  for (const columns of [80, 190]) {
    test(`sidebars from 150 columns; the status line only while Context is hidden (${columns} columns)`, async ({ tui, backend }, testInfo) => {
      const term = await tui(hyaTui(backend), { viewport: { width: widthFor(columns), height: 640 } })
      await term.waitForText(input)
      const wide = columns >= 150
      await expect.poll(async () => (await regions(term)).sessions, { timeout: 15_000 }).toBe(wide)
      if (wide) expect(await belowInput(term) ?? "").not.toMatch(/manual/)
      else await expect.poll(async () => (await belowInput(term)) ?? "").toMatch(/manual/)
      await term.attach(testInfo, `workspace-${columns}`)
    })
  }
})

test.describe("session layer", () => {
  test("/set preset workspace shows the sidebars, /set sidebar off returns, and tui.toml is not written", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: { width: widthFor(190), height: 640 } })
    await term.waitForText(input)
    await term.type("preserved draft")
    await term.press("Control+x")
    await term.type("/set preset workspace")
    await term.press("Enter")
    await expect.poll(async () => (await regions(term)).sessions, { timeout: 15_000 }).toBe(true)
    await term.waitForText("preserved draft")
    await term.press("Control+x")
    await term.type("/set sidebar off")
    await term.press("Enter")
    await expect.poll(async () => (await regions(term)).sessions).toBe(false)
    await term.waitForText("preserved draft")
    await term.attach(testInfo, "session-layer")
    const toml = join(dirname(backend.dir), "config", "hya", "tui.toml")
    expect(await Bun.file(toml).exists()).toBe(false)
  })

  test("a saved tiled layout without tui.toml says once that the default changed", async ({ tui, backend }) => {
    const prefs = join(dirname(backend.dir), "config", "hya", "tui.json")
    const leaf = (id: number, kind: string) => ({ node: { type: "pane", id: `pane-${id}`, kind }, size: { mode: "weight", value: 1 } })
    await writeFile(prefs, JSON.stringify({ paneLayout: { version: 4, active: "pane-1", root: { type: "split", id: "group-1", direction: "column", children: [leaf(6, "conversation"), leaf(1, "composer")] } } }))
    let term = await tui(hyaTui(backend))
    await term.waitForText("The default layout is now statusline")
    expect(JSON.parse(await Bun.file(prefs).text()).seenNotices).toEqual(["statusline-default"])
    term = await tui(hyaTui(backend))
    await term.waitForText(input)
    await term.waitForText(/manual/)
    expect(await term.find("The default layout is now")).toBeNull()
  })
})
