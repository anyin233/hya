import { writeFile } from "node:fs/promises"
import { join } from "node:path"
import { expect, hyaTui, test, textStep, toolStep, wideViewport } from "./hya"

// These scenarios exercise the tiled workspace (ADR-0036: the workspace preset).
test.use({ preset: "workspace" })

test.describe("hya TUI in the browser", () => {
  test("connects to the backend and lays out the main column and the sidebar", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: wideViewport })
    await term.waitForText("Message, !shell, or @file · / commands")
    // One main column (transcript, status, composer, footer) and the sidebar boxes;
    // the old Chat and Pending panels are gone.
    for (const title of ["Sessions", "Todos", "Context"]) await term.waitForText(title)
    await term.waitForText("No messages yet. Type a prompt below.")
    const text = await term.text()
    expect(text).not.toContain("Chat")
    expect(text).not.toContain("Pending")
    expect(text).not.toMatch(/backend \d+\.\d+\.\d+ ≠ tui \d+\.\d+\.\d+/)
  })

  test("admits a prompt and shows the offline model's reply", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: wideViewport })
    await term.waitForText("Message, !shell, or @file · / commands")
    await term.type("hello from the browser")
    await term.press("Enter")
    await term.waitForText("● hya-main · hya/offline", 20_000)
    await term.waitForText("No live provider is available")
    const text = await term.text()
    // The user's prompt is a block with the neutral bar, not a `user · stop` header.
    const prompt = (await term.find("┃ hello from the browser"))!
    expect((await term.cell(prompt.row, prompt.col))?.fg).toBe("#405366")
    expect(text).not.toContain("user · stop")
    // The offline model echoes the prompt back, so it shows up in both messages.
    expect(text.match(/hello from the browser/g)?.length ?? 0).toBeGreaterThanOrEqual(2)
    expect(text).not.toContain("hya ·")
  })

  test("Tab completes slash commands and /exit quits cleanly", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: wideViewport })
    await term.waitForText("Message, !shell, or @file · / commands")
    await term.type("/workf")
    await term.press("Tab")
    await term.waitForText("/workflow")
    await term.press("Control+a")
    await term.press("Control+k")
    await term.type("/exit")
    await term.waitForText("▸ /exit")
    await term.press("Enter")
    expect(await term.waitForExit(15_000)).toBe(0)
  })
})

test.describe("hya TUI golden path with fake model", () => {
  test.use({
    model: {
      steps: [
        toolStep("read", { path: "note.txt" }),
        toolStep("bash", { command: "echo golden-ok" }),
        textStep("## Summary\n\nRead **note.txt** and ran `echo golden-ok`.\n", { chunkSize: 12, delayMs: 10 }),
      ],
    },
  })

  test("streams Markdown, renders a real tool card, approves a real permission ask, and finishes cleanly", async ({ tui, backend, fakeModel }) => {
    await writeFile(join(backend.dir, "note.txt"), "golden-content\n")
    const term = await tui(hyaTui(backend), { viewport: wideViewport })
    await term.waitForText("Message, !shell, or @file · / commands")
    await term.type("inspect note.txt and echo")
    await term.press("Enter")
    // Real read tool card runs automatically; real bash tool asks for permission.
    await term.waitForText("Permission", 20_000)
    await term.waitForText("echo golden-ok")
    await term.press("1")
    await term.waitForText("Summary", 20_000)
    await term.waitForText("echo golden-ok")
    await term.waitForIdle()
    expect(fakeModel!.requests().length).toBe(3)
  })
})

test.describe("hya TUI in --bare mode (zero built-in bundles)", () => {
  test.use({ bare: true, preset: "workspace" })

  test("boots cleanly with zero TUI extensions, collapses bundled sidebars, and completes a prompt", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: wideViewport })
    await term.waitForText("Message, !shell, or @file · / commands")
    await term.waitForText("No messages yet. Type a prompt below.")
    const initial = await term.text()
    expect(initial).not.toContain("Loading…")
    expect(initial).not.toContain("Sessions")
    expect(initial).not.toContain("Todos")
    expect(initial).not.toContain("Projects")
    await term.type("hello in bare mode")
    await term.press("Enter")
    await term.waitForText("● hya-bare · hya/offline", 20_000)
    await term.waitForText("No live provider is available")
    await term.type("/exit")
    await term.waitForText("▸ /exit")
    await term.press("Enter")
    expect(await term.waitForExit(15_000)).toBe(0)
  })
})
