import { expect, test } from "./harness"

test("reports the command's exit code and last output", async ({ tui }) => {
  const term = await tui(["sh", "-c", "printf 'done\\n'; exit 3"])
  expect(await term.waitForExit()).toBe(3)
  await term.waitForText("done")
  await term.waitForText("[process exited with code 3]")
})

test("each browser connection gets its own process", async ({ tui, page }) => {
  const term = await tui(["sh", "-c", "echo pid:$$; sleep 30"])
  await term.waitForText(/pid:\d+/)
  const first = /pid:(\d+)/.exec(await term.text())![1]
  await page.reload()
  await expect.poll(() => page.evaluate(() => window.hyaTerm?.connected ?? false)).toBe(true)
  await term.waitForText(/pid:\d+/)
  expect(/pid:(\d+)/.exec(await term.text())![1]).not.toBe(first)
})
