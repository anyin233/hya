import { writeFile } from "node:fs/promises"
import { join } from "node:path"
import { api, expect, hangStep, hyaTui, statusSessionId, test, textStep, toolStep, toolsStep, tuiMain } from "./hya"
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

  test("PgUp/PgDn and Home/End in the selector scroll the previewed viewer, not the parent", async ({ tui, backend, fakeModel }) => {
    const rows = Array.from({ length: 80 }, (_, at) => `child row ${at + 1}.`).join("\n\n")
    fakeModel!.route("NEVER call `report`", [
      toolStep("task", { description: "long helper", prompt: "long child prompt", subagent_type: "hya-task" }),
      textStep("Parent scroll marker."),
    ])
    fakeModel!.route("Finish your task with `report`", [textStep(rows), hangStep(40_000)])
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    for (const pane of ["projects", "sessions", "todos", "context"]) await command(term, `/layout close ${pane}`)
    await command(term, "/subagents")
    await command(term, "/layout focus pane-1")
    await command(term, "delegate long")
    await term.waitForText("Parent scroll marker.", 20_000)
    await term.waitForText("child row 80.", 20_000)
    await command(term, "/layout focus pane-8")

    await term.press("PageUp")
    await expect.poll(() => term.find("child row 80.")).toBeNull()
    await term.press("Home")
    await term.waitForText("child row 1.")
    await term.press("PageDown")
    await expect.poll(() => term.find("child row 1.")).toBeNull()
    await term.press("End")
    await term.waitForText("child row 80.")
    // The parent's transcript did not move.
    await term.waitForText("Parent scroll marker.")
  })

  test("x interrupts, s steers, and d archives the selected subagent", async ({ tui, backend, fakeModel }) => {
    test.setTimeout(60_000)
    fakeModel!.route("NEVER call `report`", [
      toolStep("task", { description: "steerable helper", prompt: "steer-child-prompt", subagent_type: "hya-task" }),
      textStep("Parent left it running."),
      textStep("Parent synthesis one."),
      textStep("Parent synthesis two."),
      textStep("Parent synthesis three."),
    ])
    fakeModel!.route("Finish your task with `report`", [hangStep(40_000), textStep("child-heard-the-user"), hangStep(40_000)])
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    for (const pane of ["projects", "sessions", "todos", "context"]) await command(term, `/layout close ${pane}`)
    await command(term, "/subagents")
    await command(term, "/layout focus pane-1")
    await command(term, "delegate steerable")
    await term.waitForText("Parent left it running.", 20_000)
    await expect.poll(() => fakeModel!.pendingHangs(), { timeout: 20_000 }).toBe(1)
    await command(term, "/layout focus pane-8")
    await term.waitForText("x interrupt · s steer")

    // x: the child's turn is cancelled; the parent is untouched.
    await term.press("x")
    await term.waitForText("! Cancelled", 10_000)
    await expect.poll(() => fakeModel!.pendingHangs(), { timeout: 10_000 }).toBe(0)
    await term.waitForText("Parent left it running.")

    // s: the command pane opens prefilled; the message wakes the idle child.
    await term.press("s")
    await term.waitForText(/\/subagents steer \S+/)
    await term.type("steer-marker please")
    await term.press("Enter")
    await term.waitForText("child-heard-the-user", 20_000)
    await term.waitForText("[user] steer-marker please")

    // d asks first; any other key keeps the child, Enter archives it.
    await term.press("d")
    await term.waitForText(/Archive .*\? Enter confirms/)
    await term.press("ArrowDown")
    await expect.poll(() => term.find("Enter confirms")).toBeNull()
    await term.press("d")
    await term.waitForText("Enter confirms")
    await term.press("Enter")
    await term.waitForText("No active subagents in this conversation.", 15_000)
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

  test.describe("actor lifecycle", () => {
    test.use({ model: { steps: [], permission: "allow" } })
    for (const protocol of ["http", "grpc"] as const) {
      test(`archival hides the selector row and pinned transcript after reopening and revival restores them over ${protocol}`, async ({ tui, backend, fakeModel }) => {
        test.setTimeout(60_000)
        const wake = { channel: "", body: "wake-archived-helper" }
        const report = { channel: "", body: "report now" }
        fakeModel!.route("NEVER call `report`", [
          toolStep("task", { description: "helper", prompt: "archive-child-prompt", subagent_type: "hya-task" }),
          toolStep("wait", { timeout_secs: 60 }), toolStep("send", report),
          toolStep("wait", { timeout_secs: 60 }), hangStep(40_000),
          toolStep("send", wake), textStep("Parent woke helper."), hangStep(40_000),
        ])
        fakeModel!.route("Finish your task with `report`", [
          hangStep(40_000), toolStep("report", { result: "archive-report-marker", outcome: "done" }), hangStep(40_000),
        ])
        const argv = protocol === "http" ? hyaTui(backend)
          : ["bun", tuiMain, "--grpc", new URL(backend.url).host, "--dir", backend.dir]
        let term = await tui(argv)
        await term.waitForText("Message, !shell, or @file · / commands")
        const rootId = await statusSessionId(term)
        for (const pane of ["projects", "sessions", "todos", "context"]) await command(term, `/layout close ${pane}`)
        await command(term, "/subagents")
        await term.waitForText("Subagent · pane-9 · following")
        await command(term, "/layout focus pane-1")
        await command(term, "delegate archive check")
        await term.waitForText("archive-child-prompt", 20_000)
        await command(term, "/layout focus pane-8")
        await term.press("Enter")
        await term.waitForText("Subagent · pane-9 · pinned")
        await expect.poll(() => fakeModel!.pendingHangs()).toBe(1)
        const before = await api<{ members: { handle: string }[] }>(backend, "GET", `/v1/sessions/${rootId}`)
        report.channel = before.members[0]!.handle
        fakeModel!.release()
        await term.waitForText("No active subagents in this conversation.")
        await term.waitForText("Pinned subagent unavailable in this conversation.")
        expect(await term.find("helper ·")).toBeNull()
        expect(await term.find("archive-child-prompt")).toBeNull()
        type Session = { id: string; parent?: string; busy?: boolean; members?: { child: string; handle: string; archived?: boolean }[] }
        const read = () => api<Session>(backend, "GET", `/v1/sessions/${rootId}`)
        const snapshot = await read()
        expect(snapshot.members?.[0]?.archived).toBe(true)
        wake.channel = snapshot.members![0]!.handle
        // The archive key belongs to the selector, never the composer.
        await command(term, "/layout focus pane-1")
        await term.press("Control+a")
        expect(await term.find("helper · done · archived")).toBeNull()
        await command(term, "/layout focus pane-8")
        await term.press("Control+a")
        await term.waitForText("helper · done · archived")
        await term.waitForText("archive-child-prompt")
        await term.waitForText("hide archived")
        // Disabled keys must remain removable through the existing keybind system.
        await command(term, "/keybind unset Ctrl+A")
        await term.waitForText("Keybind unset")
        await term.press("Escape")
        await expect.poll(() => term.find("Keybind unset")).toBeNull()
        await term.press("Control+a")
        expect(await term.find("helper · done · archived")).not.toBeNull()
        await command(term, "/keybind reset Ctrl+A")
        await term.waitForText("Keybind reset")
        await term.press("Escape")
        await expect.poll(() => term.find("Keybind reset")).toBeNull()
        await term.press("Control+a")
        await term.waitForText("No active subagents in this conversation.")
        await term.waitForText("Pinned subagent unavailable in this conversation.")
        // Runtime visibility is not written to the persisted viewer layout.
        await term.press("Control+a")
        await term.waitForText("helper · done · archived")
        // Keep the parent turn in flight until both the live and restored rows were checked.
        await expect.poll(() => fakeModel!.pendingHangs()).toBe(1)
        await term.close()
        term = await tui([...argv, "--session", rootId])
        await term.waitForText("No active subagents in this conversation.")
        await term.waitForText("Pinned subagent unavailable in this conversation.")
        expect(await term.find("helper ·")).toBeNull()
        fakeModel!.release()
        // An unread report may wake the parent automatically; otherwise request
        // the next turn explicitly. Both paths use the real send/revival tools.
        await expect.poll(async () => {
          const current = await read()
          return current.members?.[0]?.archived === false || !current.busy
        }).toBe(true)
        if ((await read()).members?.[0]?.archived) {
          await api(backend, "POST", `/v1/sessions/${rootId}/turns`, { prompt: { text: "wake the helper" } }).catch((error: unknown) => {
            if (!String(error).includes("HTTP 409")) throw error
          })
        }
        await expect.poll(async () => (await read()).members?.[0]?.archived, { timeout: 15_000 }).toBe(false)
        await term.waitForText("Parent woke helper.", 15_000)
        await term.waitForText("helper ·")
        await expect.poll(() => term.find("No active subagents in this conversation.")).toBeNull()
        await term.waitForText("wake-archived-helper", 15_000)
      })
    }
  })
})
