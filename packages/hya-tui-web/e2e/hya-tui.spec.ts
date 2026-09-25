import { expect, hyaTui, test } from "./hya"

test.describe("hya TUI in the browser", () => {
  test("connects to the backend and lays out its panels", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText(/Connected to hya \d+\.\d+\.\d+/)
    for (const title of ["Sessions", "Chat", "Pending"]) await term.waitForText(title)
    await term.waitForText("Enter a prompt · /new creates a session · /help lists commands")
  })

  test("admits a prompt and shows the offline model's reply", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await term.type("hello from the browser")
    await term.press("Enter")
    await term.waitForText("assistant · stop", 20_000)
    await term.waitForText("No live provider is available")
    const text = await term.text()
    expect(text).toContain("user · stop")
    // The offline model echoes the prompt back, so it shows up in both messages.
    expect(text.match(/hello from the browser/g)?.length ?? 0).toBeGreaterThanOrEqual(2)
    expect(text).toMatch(/hya · hysec_\w+ · build hya\/offline/)
  })

  test("Tab completes slash commands", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await term.type("/workf")
    await term.press("Tab")
    await term.waitForText("/workflow")
  })
})
