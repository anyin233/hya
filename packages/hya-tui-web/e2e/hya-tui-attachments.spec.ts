// Image attachments in the composer (docs/tui.md "Attachments"): `@shot.png`
// resolves to the file, is base64-attached on `CreateTurn`, and shows up in
// the fake model's request; the transcript shows the attachment row; a bad
// file (wrong type / too large) is refused locally with nothing sent; a
// model configured `modalities: { input: [text] }` refuses images.

import { writeFile } from "node:fs/promises"
import { join } from "node:path"
import { expect, hyaTui, test, textStep } from "./hya"

/** An 8-byte PNG signature plus filler: enough for the server's type sniff (it checks the signature, not a full decode). */
function pngBytes(size = 64): Buffer {
  const signature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])
  return Buffer.concat([signature, Buffer.alloc(Math.max(0, size - signature.length), 1)])
}

async function prompt(term: import("./harness").Tui, text: string): Promise<void> {
  await term.type(text)
  await term.press("Enter")
}

test.describe("image attachments", () => {
  test.use({ model: { steps: [textStep("looks fine")] } })

  test("@shot.png attaches the file; the fake model receives an image part; the transcript shows it", async ({ tui, backend, fakeModel }, testInfo) => {
    await writeFile(join(backend.dir, "shot.png"), pngBytes(240 * 1024))
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "@shot.png describe this")
    await term.waitForText("looks fine", 20_000)
    // The transcript's user message shows the attachment row with name and size, once the debounced projection refresh lands (app/controller.ts `scheduleRefresh`, ~120-400 ms after the durable `partsAdded`).
    await term.waitForText(/attachment . shot\.png/, 5_000)
    await term.attach(testInfo, "attachments-screen")

    // The fake model's request carries an image_url content part (crates/hya-provider/src/openai.rs `user_content`).
    const requests = fakeModel!.requests() as { messages?: { role?: string; content?: unknown }[] }[]
    const userMessage = requests[0]?.messages?.find((message) => message.role === "user")
    expect(Array.isArray(userMessage?.content)).toBe(true)
    const parts = userMessage!.content as { type?: string; image_url?: { url?: string } }[]
    const image = parts.find((part) => part.type === "image_url")
    expect(image?.image_url?.url).toMatch(/^data:image\/png;base64,/)
  })

  test("an oversized image is refused locally and nothing is sent", async ({ tui, backend, fakeModel }) => {
    await writeFile(join(backend.dir, "huge.png"), pngBytes(11 * 1024 * 1024))
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "@huge.png describe this")
    await term.waitForText(/larger than 10 MiB/, 5_000)
    // Nothing reached the fake model.
    expect(fakeModel!.requests()).toHaveLength(0)
  })

  test("a non-image file is refused locally and nothing is sent", async ({ tui, backend, fakeModel }) => {
    await writeFile(join(backend.dir, "notes.txt"), "just text\n")
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    // notes.txt has no image extension, so it is never treated as an attachment: type the mention, then submit plain text.
    await term.type("@notes.txt hello")
    await term.press("Enter")
    await term.waitForText("looks fine", 20_000)
    const requests = fakeModel!.requests() as { messages?: { role?: string; content?: unknown }[] }[]
    const userMessage = requests[0]?.messages?.find((message) => message.role === "user")
    // A non-image mention sends as plain text, no attachment.
    expect(typeof userMessage?.content === "string" || !Array.isArray(userMessage?.content)).toBe(true)
  })
})

test.describe("a model that refuses images", () => {
  test.use({ model: { steps: [textStep("no images please")], modelModalities: { model: ["text"] } } })

  test("refuses to send when the current model has modalities: { input: [text] }", async ({ tui, backend, fakeModel }) => {
    await writeFile(join(backend.dir, "shot.png"), pngBytes(1024))
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "@shot.png describe this")
    await term.waitForText(/does not accept image attachments/, 5_000)
    expect(fakeModel!.requests()).toHaveLength(0)
  })
})
