import { expect, hyaTui, test } from "./hya"

test("command dropdown shows twelve choices and fits a short terminal", async ({ tui, backend }, testInfo) => {
  const term = await tui(hyaTui(backend))
  await term.waitForText("Message, !shell, or @file · / commands")
  await term.type("/")
  await term.waitForText("▸ /agent")
  const recommendations = async () => {
    const lines = await term.lines()
    const top = lines.findIndex((row) => row.includes("─Commands"))
    const left = lines[top]!.indexOf("┌")
    const bottom = lines.findIndex((row, index) => index > top && row[left] === "└")
    return bottom - top - 3 // borders, input, and hint surround the choices
  }
  await expect.poll(recommendations).toBe(12)
  await term.attach(testInfo, "twelve-choices")
  await term.resize(700, 230)
  await term.waitForText("Commands")
  const { rows } = await term.size()
  await expect.poll(recommendations).toBe(Math.max(1, rows - 6))
  await term.press("ArrowUp")
  await term.press("ArrowDown")
  await term.waitForText("▸ /agent")
  await term.attach(testInfo, "short-dropdown")
  await term.press("Escape")
  await expect.poll(() => term.find("Commands")).toBeNull()
})
