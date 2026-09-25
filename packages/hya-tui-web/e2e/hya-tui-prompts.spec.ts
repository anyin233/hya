// Permission and question prompts (docs/tui.md "Permission and question
// prompts"): the prompt docked above the composer under the default
// permission model (bash, edit, and write ask), its option keys (1/2/3,
// arrows + Enter, Esc denies), Always allow, queued asks (`1 of 2`), question
// options / free text / reject from `ask_user`, a subagent's ask shown in the
// parent — all driven by the fake model — and a `!command` shell turn that
// never asks.

import { writeFile } from "node:fs/promises"
import { join } from "node:path"
import type { Tui } from "./harness"
import { expect, hangStep, hyaTui, test, textStep, toolStep } from "./hya"
import { startProxy } from "./proxy"

const colors = { fg: "#e8edf3", muted: "#9caab9", accent: "#73c8e8", error: "#f07878", warning: "#e5c07b", add: "#a5d6a7", remove: "#f07878" }

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.press("Enter")
}

async function at(term: Tui, needle: string) {
  const found = await term.find(needle)
  expect(found, `screen shows ${needle}`).not.toBeNull()
  return found!
}

/** The prompt box is shown (its title) and sits above the input. */
async function promptShown(term: Tui, title: RegExp | string = "Permission"): Promise<void> {
  await term.waitForText(title, 20_000)
  const box = typeof title === "string" ? await at(term, title) : undefined
  const input = await at(term, "Message, /command, !shell, or @file")
  if (box) expect(box.row).toBeLessThan(input.row)
}

async function promptGone(term: Tui): Promise<void> {
  await expect.poll(async () => /asked by /.test(await term.text()), { timeout: 20_000 }).toBe(false)
}

test.describe("bash permission prompt", () => {
  test.use({ model: { steps: [toolStep("bash", { command: "echo prompt-once" }), textStep("Ran it once.")] } })

  test("shows the command and who asks; 1 allows once and the card completes", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "run the command")
    await promptShown(term)
    await term.waitForText("asked by build")
    await term.waitForText("│ $ echo prompt-once")
    await term.waitForText(/▸ 1 {2}Allow once/)
    await term.waitForText(/2 {2}Always allow {2}bash: /)
    await term.waitForText(/3 {2}Deny/)
    await term.waitForText(/1-3 or ↑↓ Enter · Esc denies · perm_\w+ · mode manual/)
    // The box is drawn in the warning color; the highlighted option in the accent color.
    const box = await at(term, "Permission")
    expect((await term.cell(box.row, box.col - 1))?.fg).toBe(colors.warning)
    const first = await at(term, "▸ 1  Allow once")
    expect((await term.cell(first.row, first.col))?.fg).toBe(colors.accent)
    // The tool card waits meanwhile.
    await term.waitForText(/◌ bash\s+echo prompt-once · awaiting approval/)
    await term.attach(testInfo, "bash-prompt")

    await term.press("1")
    await term.waitForText("Ran it once.", 20_000)
    await term.waitForText(/✓ bash\s+echo prompt-once/)
    await promptGone(term)
    await term.waitForText(/Allowed once · bash|Ready/)
  })

  test("with text in the input, digits type; the text stays after answering with arrows + Enter", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "run the command")
    await promptShown(term)
    await term.type("draft 3")
    await term.waitForText("Clear the input to answer with 1-3")
    await term.waitForText("│ draft 3")
    // The prompt is still there: 3 went into the input, it did not deny.
    await term.waitForText("asked by build")
    for (let index = 0; index < 7; index++) await term.press("Backspace")
    await term.waitForText(/1-3 or ↑↓ Enter/)
    await term.press("ArrowDown")
    await term.press("ArrowUp")
    await term.waitForText("▸ 1  Allow once")
    await term.press("Enter")
    await term.waitForText("Ran it once.", 20_000)
    await promptGone(term)
  })
})

test.describe("always allow", () => {
  test.use({
    model: {
      steps: [
        toolStep("bash", { command: "echo always-allowed" }), textStep("First run."),
        toolStep("bash", { command: "echo always-allowed" }), textStep("Second run."),
      ],
    },
  })

  test("2 always allows; a second identical call runs without asking", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "run it")
    await promptShown(term)
    await term.waitForText("$ echo always-allowed")
    await term.press("2")
    await term.waitForText("First run.", 20_000)
    await promptGone(term)
    // The same command again: no prompt (the turn would block on it), the reply arrives.
    await prompt(term, "run it again")
    await term.waitForText("Second run.", 20_000)
    const cards = (await term.text()).match(/✓ bash\s+echo always-allowed/g) ?? []
    expect(cards.length).toBe(2)
  })
})

test.describe("deny", () => {
  test.use({ model: { steps: [toolStep("bash", { command: "echo never" }), textStep("It was denied.")] } })

  test("3 denies: the card fails and the model continues", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "try it")
    await promptShown(term)
    await term.press("3")
    await term.waitForText("It was denied.", 20_000)
    await term.waitForText(/✗ bash\s+echo never/)
    const failed = await at(term, "✗ bash")
    expect((await term.cell(failed.row, failed.col))?.fg).toBe(colors.error)
    await promptGone(term)
    await term.attach(testInfo, "denied")
  })

  test("Esc denies (it never approves)", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "try it")
    await promptShown(term)
    await term.press("Escape")
    await term.waitForText("It was denied.", 20_000)
    await term.waitForText(/✗ bash\s+echo never/)
    await promptGone(term)
  })
})

test.describe("edit permission prompt", () => {
  test.use({
    model: {
      steps: [
        toolStep("edit", { path: "poem.txt", edits: [{ op: "replace_text", oldText: "two", newText: "TWO" }] }),
        textStep("Edited the poem."),
      ],
    },
  })

  test("shows the path and a colored diff", async ({ tui, backend }, testInfo) => {
    await writeFile(join(backend.dir, "poem.txt"), "one\ntwo\nthree\n")
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "edit the poem")
    await promptShown(term)
    await term.waitForText(/edit .*poem\.txt/)
    const removed = await at(term, "│ - two")
    expect((await term.cell(removed.row, removed.col + 2))?.fg).toBe(colors.remove)
    const added = await at(term, "│ + TWO")
    expect((await term.cell(added.row, added.col + 2))?.fg).toBe(colors.add)
    await term.attach(testInfo, "edit-prompt")
    await term.press("1")
    await term.waitForText("Edited the poem.", 20_000)
    await term.waitForText(/✓ edit\s+poem\.txt/)
  })
})

test.describe("queued asks", () => {
  test.use({ model: { steps: [] } })

  // The session's own tool calls ask one after another; a subagent's ask can wait at the same time.
  test("two pending asks show one at a time with 1 of 2", async ({ tui, backend, fakeModel }, testInfo) => {
    fakeModel!.route("NEVER call `report`", [
      toolStep("task", { description: "helper", prompt: "run a command", subagent_type: "general" }),
      toolStep("bash", { command: "echo parent-ask" }),
      textStep("Parent done."),
    ])
    fakeModel!.route("Finish your task with `report`", [toolStep("bash", { command: "echo child-ask" }), hangStep(20_000)])
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "run both")
    await term.waitForText("Permission · 1 of 2", 20_000)
    await term.attach(testInfo, "queued")
    const first = /\$ echo (parent|child)-ask/.exec(await term.text())![1]
    const other = first === "parent" ? "child" : "parent"
    await term.press("1")
    await term.waitForText(new RegExp(`\\$ echo ${other}-ask`), 20_000)
    await expect.poll(() => term.find("1 of 2")).toBeNull()
    await term.press("1")
    await promptGone(term)
    await term.waitForText("Parent done.", 20_000)
    await term.waitForText(/✓ bash\s+echo parent-ask/)
  })
})

const question = {
  questions: [{
    header: "Color",
    question: "Which color do you want?",
    options: [{ label: "red", description: "warm" }, { label: "blue", description: "cool" }],
  }],
}

test.describe("question prompt", () => {
  test.use({ model: { permission: "allow", steps: [toolStep("ask_user", question), textStep("Noted your answer.")] } })

  async function asked(term: Tui): Promise<void> {
    await term.waitForText("Connected to hya")
    await prompt(term, "ask me")
    await promptShown(term, "Question")
    await term.waitForText("Color: Which color do you want?")
    await term.waitForText(/▸ 1 {2}red/)
    await term.waitForText(/2 {2}blue/)
    await term.waitForText(/3 {2}Other… {2}type the answer in the input, Enter sends/)
    await term.waitForText(/4 {2}Reject/)
  }

  test("selecting an option answers it", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await asked(term)
    await term.attach(testInfo, "question")
    await term.press("ArrowDown")
    await term.waitForText("▸ 2  blue")
    await term.press("Enter")
    await term.waitForText("Noted your answer.", 20_000)
    await promptGone(term)
    await prompt(term, "/tools on")
    await term.waitForText(/"Which color do you want\?"="blue"/)
  })

  test("a free-text answer typed into the input", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await asked(term)
    await term.press("3")
    await term.waitForText("Type the answer in the input · Enter sends it")
    await term.type("green")
    await term.waitForText("Enter sends the input as the answer")
    await term.press("Enter")
    await term.waitForText("Noted your answer.", 20_000)
    await promptGone(term)
    await prompt(term, "/tools on")
    await term.waitForText(/"Which color do you want\?"="green"/)
  })

  test("reject", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await asked(term)
    await term.press("4")
    await term.waitForText("Noted your answer.", 20_000)
    await promptGone(term)
    await prompt(term, "/tools on")
    await term.waitForText(/"Which color do you want\?"="Unanswered"/)
  })
})

test.describe("subagent asks", () => {
  test.use({ model: { steps: [] } })

  test("a subagent's ask shows in the parent, labelled with the subagent; its card and the sidebar say it waits", async ({ tui, backend, fakeModel }, testInfo) => {
    fakeModel!.route("NEVER call `report`", [
      toolStep("task", { description: "survey the repo", prompt: "list the files", subagent_type: "general" }),
      textStep("Spawned a helper."),
    ])
    fakeModel!.route("Finish your task with `report`", [toolStep("bash", { command: "echo from-child" }), hangStep(20_000)])
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "delegate the survey")
    await term.waitForText("Spawned a helper.", 20_000)
    await promptShown(term)
    await term.waitForText(/asked by subagent general/)
    await term.waitForText("│ $ echo from-child")
    await term.waitForText("◌ waiting for approval", 15_000)
    await term.waitForText(/↳ 2\. general · ◌ waiting/)
    await term.attach(testInfo, "subagent-ask")
    await term.press("1")
    await promptGone(term)
    await term.waitForText(/↳ bash echo from-child/, 15_000)
    expect(await term.find("waiting for approval")).toBeNull()
  })

  test("a subagent's ask arrives on the parent's stream (includeDescendants), not by polling the interactions listing", async ({ tui, backend, fakeModel }) => {
    fakeModel!.route("NEVER call `report`", [
      toolStep("task", { description: "survey the repo", prompt: "list the files", subagent_type: "general" }),
      textStep("Spawned a helper."),
    ])
    fakeModel!.route("Finish your task with `report`", [toolStep("bash", { command: "echo from-child" }), hangStep(20_000)])
    const proxy = await startProxy(backend.url)
    const term = await tui(hyaTui({ ...backend, url: proxy.url }))
    await term.waitForText("Connected to hya")
    await prompt(term, "delegate the survey")
    await promptShown(term)
    await term.waitForText(/asked by subagent general/)
    const shown = Date.now()
    const turn = proxy.log.findIndex((entry) => entry.method === "POST" && /\/turns$/.test(entry.path))
    expect(turn).toBeGreaterThan(0)
    // The stream was subscribed with the opt-in before the turn was admitted.
    const stream = proxy.log.findLast((entry, index) => index < turn && entry.path.includes("/events/stream"))
    expect(stream?.path).toContain("includeDescendants=true")
    // Between admitting the turn and showing the child's ask, the listing was never read.
    const listed = proxy.log.filter((entry, index) => index > turn && entry.at <= shown && entry.method === "GET" && entry.path.startsWith("/v1/interactions"))
    expect(listed).toEqual([])
    await term.press("1")
    await promptGone(term)
  })
})

test.describe("shell turns", () => {
  test("the user's own !command never shows a prompt", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "!echo shell-prompt")
    await term.waitForText("│ $ echo shell-prompt", 20_000)
    await term.waitForText(/✓ bash\s+echo shell-prompt/, 20_000)
    await term.waitForText("│ shell-prompt")
    expect(await term.text()).not.toMatch(/asked by /)
  })
})

test.describe("narrow terminal", () => {
  test.use({ model: { steps: [toolStep("bash", { command: "echo narrow-prompt" }), textStep("Narrow done.")] } })

  test("the prompt fits about 80 columns", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: { width: 690, height: 480 } })
    expect((await term.size()).cols).toBeLessThanOrEqual(84)
    await term.waitForText("Connected to hya")
    await prompt(term, "run it")
    await promptShown(term)
    await term.waitForText("│ $ echo narrow-prompt")
    await term.waitForText(/3 {2}Deny/)
    await term.attach(testInfo, "narrow-prompt")
    await term.press("1")
    await term.waitForText("Narrow done.", 20_000)
  })
})
