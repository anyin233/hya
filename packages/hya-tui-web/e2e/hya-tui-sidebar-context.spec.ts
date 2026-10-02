import type { Tui } from "./harness"
import { expect, hyaTui, statusSessionId, test, wideViewport } from "./hya"

async function rightClick(term: Tui, text: string) {
  await term.waitForText(text)
  const at = (await term.find(text))!
  const box = (await term.page.locator(".xterm-screen").boundingBox())!
  const { cols, rows } = await term.size()
  await term.page.mouse.click(box.x + (at.col + .5) / cols * box.width, box.y + (at.row + .5) / rows * box.height, { button: "right" })
}

test("sidebar context menus coexist with strict pane focus and the keybinding columns", async ({ tui, backend }, testInfo) => {
  const term = await tui(hyaTui(backend), { viewport: wideViewport })
  await term.waitForText("Message, !shell")
  await statusSessionId(term)
  await term.type("preserved draft")

  await rightClick(term, "work (1)")
  await term.waitForText("Click an action · Esc closes")
  await term.waitForText("Open")
  await term.waitForText("Delete")
  expect(await term.find("Filter")).toBeNull()
  await term.attach(testInfo, "project-context")
  await term.press("Escape")
  await expect.poll(() => term.find("Click an action")).toBeNull()

  await rightClick(term, "1. ")
  await term.waitForText("Click an action · Esc closes")
  await term.waitForText("Archive")
  await term.attach(testInfo, "session-context")
  await term.press("Escape")
  await expect.poll(() => term.find("Click an action")).toBeNull()

  await term.type("no leak")
  await term.type("/keybind list")
  await term.press("Enter")
  await term.waitForText("Keybindings")
  await term.waitForText(/Shortcut\s+Action \/ command\s+Scope/)
  await term.press("Escape")
  await expect.poll(() => term.find("Keybindings")).toBeNull()
  await term.type("/layout focus pane-1")
  await term.press("Enter")
  await expect.poll(() => term.find("Commands")).toBeNull()
  await term.type(" intact")
  await term.waitForText("preserved draft intact")
  expect(await term.find("no leak")).toBeNull()
})
