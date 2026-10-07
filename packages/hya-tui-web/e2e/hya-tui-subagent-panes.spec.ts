import { writeFile } from "node:fs/promises"
import { join } from "node:path"
import { expect, hangStep, hyaTui, test, textStep, toolStep, toolsStep } from "./hya"
import type { Tui } from "./harness"

async function command(term: Tui, text: string) {
  if (text.startsWith("/")) {
    await term.press("Control+x")
    await term.type("/")
    await term.type(text.slice(1))
  } else await term.type(text)
  await term.press("Enter")
}

test.describe("subagent panes", () => {
  test.use({ model: { steps: [] } })
  for (const viewport of [{ width: 1100, height: 640 }, { width: 690, height: 640 }]) {
    test(`preview, pin and restore without changing the parent at ${viewport.width}px`, async ({ tui, backend, fakeModel }, testInfo) => {
      test.setTimeout(60_000)
      await writeFile(join(backend.dir, "notes.txt"), "child file marker\n")
      fakeModel!.route("NEVER call `report`", [
        toolStep("task", { description: "survey files", prompt: "inspect notes", subagent_type: "hya-task" }),
        textStep("Parent stays here."),
      ])
      fakeModel!.route("Finish your task with `report`", [toolStep("read", { path: "notes.txt" }), hangStep(40_000)])
      const argv = hyaTui(backend)
      let term = await tui(argv, { viewport })
      await term.waitForText("Message, !shell, or @file · / commands")
      await command(term, "delegate")
      await term.waitForText("Parent stays here.", 20_000)
      // Remove unrelated sidebars to make room for all three transcript/editor regions.
      await command(term, "/layout close projects")
      await command(term, "/layout close sessions")
      await command(term, "/layout close todos")
      await command(term, "/layout close context")
      await command(term, "/subagents")
      await term.waitForText("Subagents")
      await term.waitForText("Subagent · pane-9 · following")
      await term.waitForText("inspect notes", 15_000)
      await term.press("Enter")
      await term.waitForText("Subagent · pane-9 · pinned")
      // The selector owns ordinary input. Typing does not alter the parent's draft.
      await term.type("zzz")
      expect(await term.find("zzz")).toBeNull()
      await command(term, "/layout focus pane-1")
      await term.type("draft42")
      await term.waitForText("draft42")
      await command(term, "/subagents follow pane-9")
      await term.waitForText("Subagent · pane-9 · following")
      await term.waitForText("draft42")
      await command(term, "/layout focus pane-8")
      await term.press("Enter")
      await term.waitForText("Subagent · pane-9 · pinned")
      await term.press("n")
      await term.waitForText("Subagent · pane-10 · pinned")
      await term.attach(testInfo, `multiple-${viewport.width}`)
      await command(term, "/layout close pane-10")
      await expect.poll(() => term.find("Subagent · pane-10")).toBeNull()
      term = await tui(argv, { viewport })
      await term.waitForText("Subagent · pane-9 · pinned")
      await term.waitForText("inspect notes", 15_000)
      await command(term, "/layout close subagents")
      await command(term, "/layout close subagent-viewer")
      await expect.poll(() => term.find("Subagent · pane-9")).toBeNull()
      await term.waitForText("Parent stays here.")
    })
  }
  test("a child streams in its viewer while the parent remains open", async ({ tui, backend, fakeModel }, testInfo) => {
    fakeModel!.route("NEVER call `report`", [
      toolStep("task", { description: "streaming helper", prompt: "child prompt", subagent_type: "hya-task" }),
      textStep("Parent live marker."),
    ])
    fakeModel!.route("Finish your task with `report`", [
      textStep("child-live-start " + "progress ".repeat(80) + "child-live-tail", { chunkSize: 40, delayMs: 400 }), hangStep(40_000),
    ])
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    await command(term, "/layout close projects")
    await command(term, "/layout close sessions")
    await command(term, "/layout close todos")
    await command(term, "/layout close context")
    await command(term, "/subagents")
    await command(term, "/layout focus pane-1")
    await command(term, "delegate live")
    await term.waitForText("Parent live marker.", 20_000)
    await term.waitForText("child-live-start")
    expect(await term.find("child-live-tail")).toBeNull()
    await term.waitForText("child-live-tail", 20_000)
    await term.waitForText("Parent live marker.")
    expect(await term.find("Viewing subagent")).toBeNull()
    await term.attach(testInfo, "live-child")
  })

  test("a pin stays on one child while the selector previews and opens another", async ({ tui, backend, fakeModel }) => {
    fakeModel!.route("NEVER call `report`", [toolsStep([
      { name: "task", arguments: { description: "Alpha helper", prompt: "alpha-child-prompt", subagent_type: "hya-task" } },
      { name: "task", arguments: { description: "Beta helper", prompt: "beta-child-prompt", subagent_type: "hya-task" } },
    ]), textStep("Parent two helpers.")])
    fakeModel!.route("Finish your task with `report`", [hangStep(40_000), hangStep(40_000)])
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    await command(term, "delegate two")
    await term.waitForText("Parent two helpers.", 20_000)
    for (const pane of ["projects", "sessions", "todos", "context"]) await command(term, `/layout close ${pane}`)
    await command(term, "/subagents")
    await term.waitForText(/(?:alpha|beta)-child-prompt/)
    const first = (await term.text()).includes("alpha-child-prompt") ? "alpha-child-prompt" : "beta-child-prompt"
    const second = first === "alpha-child-prompt" ? "beta-child-prompt" : "alpha-child-prompt"
    await term.press("Enter")
    await term.waitForText("Subagent · pane-9 · pinned")
    await term.press("ArrowDown")
    await term.waitForText(first)
    expect(await term.find(second)).toBeNull()
    await term.press("n")
    await term.waitForText("Subagent · pane-10 · pinned")
    await term.waitForText(first)
    await term.waitForText(second)
    await term.waitForText("Parent two helpers.")
  })

})
