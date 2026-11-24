// WebUI uploads (ADR-0037, docs/tui-web.md "Pasting and dropping files"): a
// file pasted into the browser tab goes to the host as an `upload` frame, the
// host stores it, and the page pastes the stored path into the terminal; the
// TUI then treats it like a dragged file (an image chip). Browser-only: the PTY
// driver has no page to paste into.

import { terminalDriver } from "./harness"
import { expect, hyaTui, test, textStep } from "./hya"

const browserOnly = terminalDriver === "pty" ? test.skip : test

test.describe("WebUI upload", () => {
  test.use({ model: { steps: [textStep("got the upload")] } })

  browserOnly("a pasted image file is uploaded, pasted as its path, and sent as an image", { tag: "@browser-only" }, async ({ tui, backend, fakeModel }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    await term.page.evaluate(() => {
      const png = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, ...new Array(56).fill(1)])
      const data = new DataTransfer()
      data.items.add(new File([png], "pasted shot.png", { type: "image/png" }))
      document.getElementById("terminal")!.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }))
    })
    await term.waitForText("[image #1 pasted shot.png]")
    await term.attach(testInfo, "uploaded-chip")
    await term.press("Enter")
    await term.waitForText("got the upload", 20_000)
    const request = fakeModel!.requests().at(-1) as { messages?: { role?: string; content?: unknown }[] }
    const content = request.messages?.findLast((message) => message.role === "user")?.content as { type?: string }[]
    expect(content.some((part) => part.type === "image_url")).toBe(true)
  })
})
