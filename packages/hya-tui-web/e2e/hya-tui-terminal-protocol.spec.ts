// Real PTY terminal escape protocol seam checks:
// 1. OSC 52 clipboard (`/copy` and mouse drag selection) + OSC 9 desktop
//    notifications gated by CSI 1004 terminal focus reporting.
// 2. OSC 0 terminal title (`●` running, `✓` unseen until focus, `?` ask).
// 3. `COLORFGBG` auto-light theme detection + real PTY bracketed paste chips
//    and pasted image path round-trip.
// Plus `@browser-only` WebUI host mappings for `Notification` and `document.title`.

import { mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { terminalDriver, type Tui } from "./harness"
import { expect, hyaTui, test, textStep, toolStep } from "./hya"

async function captureOsc52(term: Tui): Promise<void> {
  await term.inspect((terminal, state) => {
    state.osc52 = []
    terminal.parser.registerOscHandler(52, (data: string) => {
      state.osc52!.push(data)
      return true
    })
  })
}

async function copied(term: Tui): Promise<string[]> {
  const payloads = await term.inspect((_terminal, state) => state.osc52 ?? [])
  return payloads.map((payload) => Buffer.from(payload.slice(payload.indexOf(";") + 1), "base64").toString("utf8"))
}

async function captureOsc9(term: Tui): Promise<void> {
  await term.inspect((terminal, state) => {
    state.osc9 = []
    terminal.parser.registerOscHandler(9, (data: string) => {
      state.osc9!.push(data)
      return true
    })
  })
}

async function osc9Payloads(term: Tui): Promise<string[]> {
  return term.inspect((_terminal, state) => state.osc9 ?? [])
}

async function captureTitles(term: Tui): Promise<void> {
  await term.inspect((terminal, state) => {
    const view = state as { titles?: string[] }
    view.titles = []
    terminal.onTitleChange((title: string) => { view.titles!.push(title) })
  })
}

async function titles(term: Tui): Promise<string[]> {
  return term.inspect((_terminal, state) => (state as { titles?: string[] }).titles ?? [])
}

async function lastTitle(term: Tui): Promise<string> {
  return (await titles(term)).at(-1) ?? ""
}

async function cellPoint(term: Tui, row: number, col: number): Promise<{ x: number; y: number }> {
  const box = await term.screenBox()
  const { cols, rows } = await term.size()
  return { x: box.x + ((col + 0.5) / cols) * box.width, y: box.y + ((row + 0.5) / rows) * box.height }
}

async function prompt(term: Tui, text: string, { blur = false } = {}): Promise<void> {
  await term.focus(true)
  await term.type(text)
  await term.press("Enter")
  if (blur) await term.focus(false)
}

function pngBytes(size = 2048): Buffer {
  const signature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])
  return Buffer.concat([signature, Buffer.alloc(size - signature.length, 1)])
}

test.describe("OSC 52 clipboard and OSC 9 focus-gated notifications", () => {
  test.use({
    model: {
      steps: [
        textStep("Alpha bravo charlie delta echo.", { chunkSize: 8, delayMs: 15 }),
        toolStep("bash", { command: "echo notified" }),
        textStep("Ran notified."),
      ],
    },
  })

  test("emits real OSC 52 on /copy and mouse drag, and OSC 9 only when terminal is unfocused", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    await captureOsc52(term)
    await captureOsc9(term)

    // Focused turn: no OSC 9 notification.
    await prompt(term, "say it")
    await term.waitForText("Alpha bravo charlie delta echo.", 20_000)
    await term.waitForIdle()
    expect(await osc9Payloads(term)).toEqual([])

    // `/copy` emits OSC 52.
    await prompt(term, "/copy")
    await expect.poll(() => copied(term)).toEqual(["Alpha bravo charlie delta echo."])

    // Drag selection highlights with `#2f4d6b` and emits OSC 52 on release.
    await captureOsc52(term)
    const at = (await term.find("bravo charlie"))!
    const start = await cellPoint(term, at.row, at.col)
    const end = await cellPoint(term, at.row, at.col + "bravo charlie".length - 1)
    await term.mouse.move(start.x, start.y)
    await term.mouse.down()
    await term.mouse.move((start.x + end.x) / 2, end.y, { steps: 3 })
    await term.mouse.move(end.x, end.y, { steps: 3 })
    await expect.poll(async () => (await term.cell(at.row, at.col + 2))?.bg).toBe("#2f4d6b")
    await term.mouse.up()
    await expect.poll(() => copied(term)).toEqual(["bravo charlie"])

    // Unfocused permission ask emits OSC 9 over CSI 1004 focus reporting.
    await captureOsc9(term)
    await prompt(term, "run command", { blur: true })
    await expect.poll(() => osc9Payloads(term), { timeout: 20_000 }).toEqual(["Permission needed: bash echo notified"])
    await term.focus(true)
    await term.press("1")
    await term.waitForText("Ran notified.", 20_000)
  })
})

test.describe("OSC 0 terminal title", () => {
  test.use({
    model: {
      steps: [
        textStep("Slow title reply.", { chunkSize: 3, delayMs: 40 }),
        textStep("Unseen reply.", { chunkSize: 4, delayMs: 25 }),
        toolStep("bash", { command: "echo titled" }),
        textStep("Ran titled."),
      ],
    },
  })

  test("marks running turn (●), unseen outcome (✓) until focus, and permission ask (?)", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    await captureTitles(term)

    await prompt(term, "say it slowly")
    await expect.poll(async () => (await titles(term)).some((title) => title.startsWith("● ")), { timeout: 20_000 }).toBe(true)
    await term.waitForText("Slow title reply.", 20_000)
    await term.waitForIdle()
    await expect.poll(() => lastTitle(term)).toMatch(/^[^●?✓✗]/)

    await prompt(term, "say it while away", { blur: true })
    await term.waitForText("Unseen reply.", 20_000)
    await expect.poll(() => lastTitle(term), { timeout: 20_000 }).toMatch(/^✓ /)
    await term.focus(true)
    await expect.poll(() => lastTitle(term)).toMatch(/^[^●?✓✗]/)

    await prompt(term, "run titled")
    await expect.poll(() => lastTitle(term), { timeout: 20_000 }).toMatch(/^\? /)
  })
})

test.describe("COLORFGBG auto theme and PTY bracketed paste", () => {
  test.use({ model: { steps: [textStep("got the paste"), textStep("got the image")] } })

  test("starts light under COLORFGBG=0;15 and turns bracketed pastes and pasted image paths into chips sent to the model", async ({ tui, backend, fakeModel }) => {
    const prefsDir = await mkdtemp(join(tmpdir(), "hya-tui-proto-"))
    try {
      await writeFile(join(backend.dir, "shot.png"), pngBytes())
      const term = await tui(hyaTui(backend), { env: { HYA_TUI_CONFIG: join(prefsDir, "tui.json"), COLORFGBG: "0;15" } })
      await term.waitForText("Message, !shell, or @file · / commands")

      // Light theme auto-detected from COLORFGBG (focus border = `#0b6f94`).
      const border = (await term.find("┌"))!
      await expect.poll(async () => (await term.cell(border.row, border.col))?.fg).toBe("#0b6f94")

      // Bracketed 30-line paste becomes a chip and is sent wrapped in `<attachment>`.
      const thirty = Array.from({ length: 30 }, (_, i) => `const value${i + 1} = compute(${i + 1})`).join("\n")
      await term.paste(thirty)
      await term.waitForText("[paste #1 · 30 lines]")
      await term.press("Enter")
      await term.waitForText("got the paste", 20_000)
      const firstReq = fakeModel!.requests().at(-1) as { messages?: { role?: string; content?: unknown }[] }
      const firstContent = firstReq.messages?.findLast((m) => m.role === "user")?.content
      expect(String(firstContent)).toContain('<attachment kind="paste" id="1" lines="30">')

      // Bracketed pasted image path becomes an image chip and sends an `image_url` part.
      await term.paste(`${backend.dir}/shot.png`)
      await term.waitForText("[image #2 shot.png]")
      await term.press("Enter")
      await term.waitForText("got the image", 20_000)
      const secondReq = fakeModel!.requests().at(-1) as { messages?: { role?: string; content?: unknown }[] }
      const secondContent = secondReq.messages?.findLast((m) => m.role === "user")?.content as { type?: string }[]
      expect(secondContent.some((part) => part.type === "image_url")).toBe(true)
    } finally {
      await rm(prefsDir, { recursive: true, force: true })
    }
  })
})

test.describe("WebUI browser Notification and document.title", { tag: "@browser-only" }, () => {
  test.skip(terminalDriver === "pty", "Browser Notification API and document.title require Chromium")
  test.use({ model: { steps: [textStep("Notify me please.", { chunkSize: 4, delayMs: 30 })] } })

  test("maps OSC 9/777 to one deduplicated browser Notification while hidden and mirrors OSC 0 to document.title", async ({ tui, backend, page }) => {
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
    await term.waitForText("Message, !shell, or @file · / commands")

    await page.evaluate(() => Object.defineProperty(document, "hidden", { value: true, configurable: true }))
    await prompt(term, "say it", { blur: true })
    await expect.poll(() => page.title(), { timeout: 20_000 }).toMatch(/^[●✓] /)
    await term.waitForText("Notify me please.", 20_000)
    await expect.poll(() => page.evaluate(() => (window as unknown as { notifications?: unknown[] }).notifications?.length ?? 0)).toBe(1)
    await page.waitForTimeout(300)
    expect(await page.evaluate(() => (window as unknown as { notifications?: unknown[] }).notifications?.length ?? 0)).toBe(1)
  })
})
