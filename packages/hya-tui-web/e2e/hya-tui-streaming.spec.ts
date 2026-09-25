// Streaming render, the client-side prompt queue, and the turn status line,
// driven against the scripted fake model (see docs/tui.md "Streaming,
// queued prompts, and turn status").

import type { Tui } from "./harness"
import { expect, hangStep, httpErrorStep, hyaTui, test, textStep } from "./hya"

const muted = "#9caab9"

function count(text: string, needle: string): number {
  return text.split(needle).length - 1
}

/** The status line reads exactly `Ready` (the sidebar may share its screen row). */
const readyLine = /^Ready(?! ·)/m

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.press("Enter")
}

test.describe("streamed reply", () => {
  const reply = "alpha bravo charlie delta echo foxtrot"
  test.use({ model: { steps: [textStep(reply, { chunkSize: 4, delayMs: 150 })] } })

  test("shows a slow chunked reply while it streams, then exactly once", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "stream please")
    // Partial text: the first chunks are on screen before the reply is complete.
    await term.waitForText("alpha br", 20_000)
    const partial = await term.text()
    expect(partial).not.toContain(reply)
    // The working line shows the run; the status line does not repeat it as `Running · msg_…`.
    expect(partial).toContain("Esc to interrupt")
    expect(partial).not.toMatch(/Running · msg_/)
    await term.attach(testInfo, "streaming-screen")
    await term.waitForText(reply)
    await term.waitForText("● build · fake/model")
    await term.waitForText(readyLine)
    const final = await term.text()
    expect(count(final, reply)).toBe(1)
    expect(count(final, "● build · fake/model")).toBe(1)
    expect(final).not.toContain("turn_state_running")
  })
})

test.describe("queued prompt", () => {
  test.use({ model: { steps: [hangStep(), textStep("second reply marker q2")] } })

  test("queues a prompt during a running turn and sends it after the turn ends", async ({ tui, backend, fakeModel }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "first prompt")
    await expect.poll(() => fakeModel!.pendingHangs(), { timeout: 20_000 }).toBe(1)
    await prompt(term, "second prompt")
    // The queued prompt is a dimmed user block with a `queued` tag at its right.
    await term.waitForText(/second prompt\s+queued/)
    // The working line counts it (the status line no longer repeats `Running · … · 1 queued`).
    await term.waitForText("Queued 1")
    const queuedPrompt = (await term.find("second prompt"))!
    expect((await term.cell(queuedPrompt.row, queuedPrompt.col))?.fg).toBe(muted)
    expect((await term.cell(queuedPrompt.row, queuedPrompt.col - 2))?.fg).toBe(muted)
    const label = (await term.lines())[queuedPrompt.row]!.lastIndexOf("queued")
    expect((await term.cell(queuedPrompt.row, label))?.fg).toBe(muted)
    expect(fakeModel!.requests().length).toBe(1)
    await term.attach(testInfo, "queued-screen")

    fakeModel!.release()
    await term.waitForText("second reply marker q2", 20_000)
    await term.waitForText(readyLine)
    const text = await term.text()
    expect(text).not.toContain("queued")
    expect(count(text, "second prompt")).toBe(1)
    expect(text.indexOf("first prompt")).toBeLessThan(text.indexOf("second prompt"))
    expect(text.indexOf("second prompt")).toBeLessThan(text.indexOf("second reply marker q2"))
    expect(fakeModel!.requests().length).toBe(2)
  })
})

test.describe("failed turn", () => {
  test.use({ model: { steps: [httpErrorStep(400)] } })

  test("shows the provider error in the status line and the transcript", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "please fail")
    await term.waitForText("Error · provider_error: http status 400", 20_000)
    // The failed assistant message carries the error line under its header.
    await term.waitForText("✗ provider_error: http status 400")
    const header = (await term.find("● build · fake/model"))!
    expect((await term.find("✗ provider_error"))!.row).toBe(header.row + 1)
    expect(await term.text()).not.toContain("Running ·")
  })
})
