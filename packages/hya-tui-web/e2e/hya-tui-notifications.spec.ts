// Desktop notifications (docs/tui.md "Desktop notifications", docs/tui-web.md
// "Desktop notifications", ADR-0021): the TUI writes an OSC 9 sequence when a
// turn finishes or a permission/question ask arrives for the open session,
// only while the terminal is unfocused and `/notifications` is on. This spec
// captures the OSC 9 payload the same way e2e/hya-tui-clipboard.spec.ts
// captures OSC 52, and separately checks the WebUI host's generic mapping
// from OSC 9 / OSC 777 to a browser `Notification` (web/client.ts).
//
// Typing requires real DOM focus (Playwright's keyboard events go to
// whatever element the browser considers focused), so every scenario types
// and submits its prompt while focused, then blurs the terminal — the
// realistic order: the user sends a prompt, then looks away.

import type { Tui } from "./harness"
import { expect, hyaTui, test, textStep, toolStep } from "./hya"

declare global {
  interface Window {
    osc9?: string[]
  }
}

/** Record every OSC 9 payload the terminal receives from now on. */
async function captureOsc9(term: Tui): Promise<void> {
  await term.page.evaluate(() => {
    window.osc9 = []
    window.hyaTerm.term.parser.registerOscHandler(9, (data: string) => {
      window.osc9!.push(data)
      return true
    })
  })
}

async function osc9Payloads(term: Tui): Promise<string[]> {
  return term.page.evaluate(() => window.osc9 ?? [])
}

/** xterm.js only reports terminal focus to the program when the running program asked for it (CSI ?1004h) — the TUI does. */
async function blurTerminal(term: Tui): Promise<void> {
  await term.page.evaluate(() => document.querySelector<HTMLElement>(".xterm-helper-textarea")?.blur())
}

async function focusTerminal(term: Tui): Promise<void> {
  await term.page.evaluate(() => document.querySelector<HTMLElement>(".xterm-helper-textarea")?.focus())
}

/** Type and submit while focused (required for keys to reach the terminal at all), then blur. */
async function promptThenBlur(term: Tui, text: string): Promise<void> {
  await focusTerminal(term)
  await term.type(text)
  await term.press("Enter")
  await blurTerminal(term)
}

test.describe("desktop notifications: turn end", () => {
  test.use({ model: { steps: [textStep("Notify me please.", { chunkSize: 4, delayMs: 30 }), textStep("Notify me please.", { chunkSize: 4, delayMs: 30 }), textStep("Notify me please.", { chunkSize: 4, delayMs: 30 })] } })

  test("notifies over OSC 9 only while unfocused, and not when /notifications is off", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")

    // Focused throughout: no notification.
    await captureOsc9(term)
    await focusTerminal(term)
    await term.type("say it")
    await term.press("Enter")
    await term.waitForText("Notify me please.", 20_000)
    expect(await osc9Payloads(term)).toEqual([])

    // Submitted focused, then unfocused before the turn finishes: notifies.
    await captureOsc9(term)
    await promptThenBlur(term, "say it again")
    await term.waitForText("Notify me please.", 20_000)
    // A fresh session has no title yet (the backend titles it in the background), so the body is bare.
    await expect.poll(() => osc9Payloads(term)).toEqual(["Turn finished"])

    // Off: no notification even while unfocused.
    await focusTerminal(term)
    await term.type("/notifications off")
    await term.press("Enter")
    await term.waitForText("Desktop notifications off")
    await captureOsc9(term)
    await promptThenBlur(term, "say it once more")
    await term.waitForText("Notify me please.", 20_000)
    expect(await osc9Payloads(term)).toEqual([])
  })
})

test.describe("desktop notifications: permission ask", () => {
  test.use({ model: { steps: [toolStep("bash", { command: "echo notified" }), textStep("Ran it.")] } })

  test("notifies over OSC 9 while unfocused", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await captureOsc9(term)
    await promptThenBlur(term, "run the command")
    await expect.poll(() => osc9Payloads(term), { timeout: 20_000 }).toEqual(["Permission needed: bash echo notified"])
    await focusTerminal(term)
    await term.press("1")
    await term.waitForText("Ran it.", 20_000)
  })
})

test.describe("desktop notifications: browser Notification", () => {
  test.use({ model: { steps: [textStep("Notify me please.", { chunkSize: 4, delayMs: 30 })] } })

  test("the host shows exactly one browser Notification per event, only while the page is hidden or unfocused", async ({ tui, backend, page }) => {
    await page.addInitScript(() => {
      class FakeNotification {
        static permission = "granted"
        static requestPermission(): Promise<string> { return Promise.resolve("granted") }
        constructor(title: string, options?: { body?: string; tag?: string }) {
          ;(window as unknown as { notifications: Array<{ title: string; body?: string; tag?: string }> }).notifications ??= []
          ;(window as unknown as { notifications: Array<{ title: string; body?: string; tag?: string }> }).notifications.push({ title, body: options?.body, tag: options?.tag })
        }
      }
      Object.defineProperty(window, "Notification", { value: FakeNotification, writable: true })
    })
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")

    // Focused: no Notification.
    await focusTerminal(term)
    await term.type("say it")
    await term.press("Enter")
    await term.waitForText("Notify me please.", 20_000)
    expect(await page.evaluate(() => (window as unknown as { notifications?: unknown[] }).notifications ?? [])).toEqual([])

    // Hidden (document.hidden true) and submitted, then blurred: shows one —
    // not two, even though the TUI sends both OSC 9 and OSC 777 for it.
    await page.evaluate(() => Object.defineProperty(document, "hidden", { value: true, configurable: true }))
    await promptThenBlur(term, "say it again")
    await term.waitForText("Notify me please.", 20_000)
    await expect.poll(() => page.evaluate(() => (window as unknown as { notifications?: unknown[] }).notifications?.length ?? 0)).toBe(1)
    // Stays one: the second OSC sequence for the same event does not sneak in late.
    await page.waitForTimeout(300)
    expect(await page.evaluate(() => (window as unknown as { notifications?: unknown[] }).notifications?.length ?? 0)).toBe(1)
    expect(await page.evaluate(() => (window as unknown as { notifications: Array<{ tag?: string }> }).notifications[0]!.tag)).toBeTruthy()
  })
})
