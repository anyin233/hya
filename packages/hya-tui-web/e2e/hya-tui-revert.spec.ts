// `/undo`, `/redo`, and `/fork` (docs/tui.md "Undo, redo, and fork"):
// a model `write` call creates a file in the backend's workdir; `/undo`
// hides the turn, deletes the file, and puts the prompt back in the input;
// `/redo` brings both back; a new prompt makes the revert permanent; `/fork`
// copies the session at the head or before a picked prompt; a revert is
// refused while a turn runs.

import { existsSync } from "node:fs"
import { readFile } from "node:fs/promises"
import { join } from "node:path"
import type { Tui } from "./harness"
import { expect, hangStep, hyaTui, test, textStep, toolStep } from "./hya"

const warning = "#e5c07b"

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.press("Enter")
}

/** Empty the input (the first Ctrl+C clears it). */
async function clearInput(term: Tui): Promise<void> {
  await term.press("Control+c")
  await term.waitForText("Press Ctrl+C again to quit")
}

test.describe("undo and redo", () => {
  test.use({
    model: {
      permission: "allow",
      steps: [
        toolStep("write", { path: "notes.txt", content: "alpha\n" }),
        textStep("Wrote the notes."),
        textStep("Second reply."),
      ],
    },
  })

  test("/undo hides the turn, deletes the written file, and refills the input; /redo restores; a new prompt commits", async ({ tui, backend }, testInfo) => {
    const file = join(backend.dir, "notes.txt")
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "write notes")
    await term.waitForText("Wrote the notes.", 20_000)
    await expect.poll(() => existsSync(file)).toBe(true)

    await prompt(term, "/undo")
    await term.waitForText("Reverted · 1 deleted · the prompt is back in the input")
    await expect.poll(() => term.find("Wrote the notes.")).toBeNull()
    await expect.poll(() => existsSync(file)).toBe(false)
    // The pending-revert line ends the transcript, in the warning color.
    await term.waitForText(/↶ \d+ messages reverted · \/redo or Ctrl\+X R restores them/)
    const line = (await term.find("↶"))!
    expect((await term.cell(line.row, line.col))?.fg).toBe(warning)
    // The only "write notes" left on screen is the input.
    const input = await term.find("write notes")
    expect(input).not.toBeNull()
    expect(input!.row).toBeGreaterThan(line.row)
    await term.attach(testInfo, "after-undo")

    // Ctrl+X R redoes without clearing the prefilled input first; the untouched prefill is emptied.
    await term.press("Control+x")
    await term.waitForText("Ctrl+X · Ctrl+E opens the external editor · U undo · R redo · F fork")
    await term.press("r")
    await term.waitForText("Restored · 1 file restored")
    await term.waitForText("Wrote the notes.")
    await expect.poll(() => term.find("write notes")).not.toBeNull()
    expect(await term.find("Message, /command")).not.toBeNull()
    await expect.poll(() => existsSync(file)).toBe(true)
    expect(await readFile(file, "utf8")).toBe("alpha\n")
    await expect.poll(() => term.find("↶")).toBeNull()

    // Undo again with Ctrl+X U, then a new prompt makes it permanent: /redo is refused.
    await term.press("Control+x")
    await term.press("u")
    await term.waitForText("Reverted · 1 deleted · the prompt is back in the input")
    await clearInput(term)
    await prompt(term, "something else")
    await term.waitForText("Second reply.", 20_000)
    await expect.poll(() => term.find("↶")).toBeNull()
    await prompt(term, "/redo")
    await term.waitForText("Nothing to redo · /redo works after /undo, until the next prompt")
    expect(await term.find("Wrote the notes.")).toBeNull()
    expect(existsSync(file)).toBe(false)
  })
})

test.describe("undo while busy", () => {
  test.use({ model: { permission: "allow", steps: [hangStep(20_000)] } })

  test("/undo while a turn runs shows the refusal and changes nothing", async ({ tui, backend, fakeModel }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "take your time")
    await expect.poll(() => fakeModel!.pendingHangs()).toBe(1)
    await prompt(term, "/undo")
    await term.waitForText("Undo refused: a turn is running · wait for it to finish or press Esc to cancel it")
    expect(await term.find("↶")).toBeNull()
    await term.waitForText("take your time")
    fakeModel!.release()
  })
})

test.describe("fork", () => {
  test.use({ model: { permission: "allow", steps: [textStep("First reply."), textStep("Second reply.")] } })

  test("/fork before a picked prompt opens a new session with the earlier messages and the prompt in the input; /fork at the head copies everything", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "alpha question")
    await term.waitForText("First reply.", 20_000)
    await prompt(term, "beta question")
    await term.waitForText("Second reply.", 20_000)

    await prompt(term, "/fork")
    await term.waitForText("Fork · the new session ends before the picked prompt")
    await term.waitForText("Fork at the latest message")
    await term.waitForText(/beta question\s+\[#2\]/)
    await term.waitForText(/alpha question\s+\[#1\]/)
    await term.attach(testInfo, "fork-picker")
    // Newest first: one Down highlights "beta question".
    await term.press("ArrowDown")
    await term.press("Enter")
    await term.waitForText("Forked before “beta question” · the prompt is in the input")
    await term.waitForText("First reply.")
    await expect.poll(() => term.find("Second reply.")).toBeNull()
    // The sidebar names the source session.
    await term.waitForText(/Forked\s+from hysec_/)
    const input = await term.find("beta question")
    expect(input).not.toBeNull()
    expect(input!.row).toBeGreaterThan((await term.find("First reply."))!.row)
    await term.attach(testInfo, "fork-at-message")

    await clearInput(term)
    await prompt(term, "/fork")
    await term.waitForText("Fork at the latest message")
    await term.press("Enter")
    await term.waitForText("Forked at the latest message")
    await term.waitForText("alpha question")
    await term.waitForText("First reply.")
    expect(await term.find("beta question")).toBeNull()
  })

  test("at about 80 columns the fork picker and the pending-revert line fit", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: { width: 690, height: 640 } })
    await term.waitForText("Connected to hya")
    expect((await term.size()).cols).toBeLessThanOrEqual(82)
    await prompt(term, "alpha question")
    await term.waitForText("First reply.", 20_000)
    await prompt(term, "/undo")
    await term.waitForText("Reverted · no file changes · the prompt is back in the input")
    await term.waitForText("↶ 2 messages reverted · /redo or Ctrl+X R restores")
    // The line wraps at the width instead of running off the edge.
    await term.waitForText(/the\s+next\s+prompt\s+makes\s+it\s+permanent/)
    await term.attach(testInfo, "narrow-after-undo")
    await clearInput(term)
    await prompt(term, "/redo")
    await term.waitForText("Restored · no file changes")
    // Ctrl+X F opens the fork picker.
    await term.press("Control+x")
    await term.press("f")
    await term.waitForText("Fork at the latest message")
    await term.waitForText(/alpha question\s+\[#1\]/)
    await term.press("Escape")
    await expect.poll(() => term.find("Fork at the latest message")).toBeNull()
  })
})
