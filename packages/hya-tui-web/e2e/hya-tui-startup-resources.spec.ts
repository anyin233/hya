// The startup list (ADR-0036 `startup`, docs/tui-config.md "Startup list"):
// what was loaded, at the start of the transcript, in a new and a resumed
// session and at 80 columns; `quiet` keeps only problems (a failing MCP
// server); `/resources` lists everything grouped by source.

import { writeFile } from "node:fs/promises"
import { join } from "node:path"
import { backendConfigDir, expect, hyaTui, test, textStep } from "./hya"
import type { Tui } from "./harness"

const input = "Message, !shell, or @file · / commands"
const widthFor = (columns: number) => Math.ceil(columns * 8.43 + 15)

async function startupRows(term: Tui): Promise<string[]> {
  return (await term.lines()).filter((line) => /^\s*\[(Context|Skills|Slash commands|MCP|Bundles|Extensions|Config)\]/.test(line)).map((line) => line.trim())
}

test.describe("startup list", () => {
  test.use({ model: { steps: [textStep("first reply"), textStep("second reply")] } })

  test("a new session starts with one line per loaded section; a resumed one keeps it at the top", async ({ tui, backend }, testInfo) => {
    let term = await tui(hyaTui(backend), { viewport: { width: widthFor(120), height: 640 } })
    await term.waitForText(input)
    await term.waitForText(/\[Skills\] \S/)
    await term.waitForText(/\[Bundles\] .*hya\//)
    await term.waitForText(/\[Extensions\] hya\/basic-tui-components/)
    // No MCP server is configured: no [MCP] line at startup.
    expect((await startupRows(term)).some((row) => row.startsWith("[MCP]"))).toBe(false)
    const skills = (await startupRows(term)).find((row) => row.startsWith("[Skills]"))!
    expect(skills.length).toBeLessThanOrEqual(118)
    // The section label in the heading color, the names muted (hya theme).
    const label = (await term.find("[Skills]"))!
    expect((await term.cell(label.row, label.col))?.fg).toBe("#e8edf3")
    expect((await term.cell(label.row, label.col + "[Skills] ".length))?.fg).toBe("#9caab9")
    await term.attach(testInfo, "startup-new")
    await term.type("hello")
    await term.press("Enter")
    await term.waitForText("first reply", 20_000)
    // The list stays first, above the messages.
    const lines = await term.lines()
    expect(lines.findIndex((line) => line.includes("[Skills]"))).toBeLessThan(lines.findIndex((line) => line.includes("┃ hello")))

    term = await tui([...hyaTui(backend), "--continue"], { viewport: { width: widthFor(80), height: 640 } })
    await term.waitForText("first reply")
    await term.waitForText(/\[Bundles\]/)
    for (const row of await startupRows(term)) expect(row.length).toBeLessThanOrEqual(78)
    await term.waitForText(/\[Bundles\] .*, \+\d+/)
    await term.attach(testInfo, "startup-resumed-80")
  })

  test("[Context] names the AGENTS.md a session in the directory reads", async ({ tui, backend }) => {
    await writeFile(join(backend.dir, "AGENTS.md"), "# Rules\nBe brief.\n")
    const term = await tui(hyaTui(backend), { viewport: { width: widthFor(120), height: 640 } })
    await term.waitForText(input)
    await term.waitForText(/\[Context\] \S*work\/AGENTS\.md/)
    await term.type("/resources")
    await term.press("Enter")
    await term.waitForText(/^\s+project\s*$/m)
    await term.waitForText(/work\/AGENTS\.md · 18 B/)
  })

  test("/resources lists every section grouped by source", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: { width: widthFor(120), height: 960 } })
    await term.waitForText(input)
    await term.waitForText(/\[Bundles\]/)
    await term.type("/resources")
    await term.press("Enter")
    await term.waitForText(/^\s*\[MCP\]\s*$/m)
    await term.waitForText(/^\s+first_party\s*$/m)
    await term.waitForText(/hya\/basic-tui-components · \S+ · running/)
  })
})

test.describe("quiet startup", () => {
  test.use({ mcpServers: { broken: { command: ["/nonexistent/mcp-server-binary"] } } })

  test("quiet = true shows only problems: a failed MCP server", async ({ tui, backend }, testInfo) => {
    await writeFile(join(backendConfigDir(backend), "tui.toml"), "[startup]\nquiet = true\n")
    const term = await tui(hyaTui(backend))
    await term.waitForText(input)
    await term.waitForText(/\[MCP\] broken: failed.* · \/mcp to fix/, 20_000)
    expect(await term.find("[Skills]")).toBeNull()
    expect(await term.find("[Bundles]")).toBeNull()
    await term.attach(testInfo, "startup-quiet")
  })
})
