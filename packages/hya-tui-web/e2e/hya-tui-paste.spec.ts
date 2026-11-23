// Paste chips and pasted paths (docs/tui.md "Paste and attachments"): a long
// paste becomes one `[paste #N · L lines]` token with an attachment row,
// Backspace removes it whole, and the fake model receives the text wrapped in
// an `<attachment>` block while the transcript shows it folded; pasted image
// paths become image chips sent as image parts; a path inside the workspace
// becomes an `@path` mention; a missing one stays text with a notice.

import { writeFile } from "node:fs/promises"
import { join } from "node:path"
import { expect, hyaTui, test, textStep } from "./hya"
import type { Tui } from "./harness"

const input = "Message, !shell, or @file · / commands"
const lines = (count: number) => Array.from({ length: count }, (_, i) => `const value${i + 1} = compute(${i + 1})`).join("\n")

function pngBytes(size = 2048): Buffer {
  const signature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])
  return Buffer.concat([signature, Buffer.alloc(size - signature.length, 1)])
}

type Request = { messages?: { role?: string; content?: unknown }[] }
const userContent = (requests: unknown[]) => (requests.at(-1) as Request | undefined)?.messages?.findLast((message) => message.role === "user")?.content
const userText = (content: unknown): string => typeof content === "string" ? content : Array.isArray(content) ? content.map((part: { text?: string }) => part.text ?? "").join("") : ""
const imageParts = (content: unknown): number => Array.isArray(content) ? content.filter((part: { type?: string }) => part.type === "image_url").length : 0

/** The text row of the input box that holds `anchor`. */
async function inputRow(term: Tui, anchor: string): Promise<number> {
  const rows = await term.lines()
  return rows.findLastIndex((row) => row.includes(anchor))
}

test.describe("paste chips", () => {
  test.use({ model: { steps: [textStep("got the paste"), textStep("second reply")] } })

  test("a 30-line paste is one chip: Backspace removes it whole; sent wrapped and shown folded", async ({ tui, backend, fakeModel }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText(input)
    await term.paste(lines(30))
    await term.waitForText("[paste #1 · 30 lines]")
    await term.waitForText("[paste #1] 30 lines · const value1 = compute(1)")
    // The input stays one row: the 30 lines are not in it.
    expect(await term.find("const value30")).toBeNull()
    await term.attach(testInfo, "paste-chip")
    // The first Backspace takes the space after the token, the second the whole token and its row.
    await term.press("Backspace")
    await term.press("Backspace")
    await expect.poll(() => term.find("[paste #1")).toBeNull()
    await term.waitForText(input)

    await term.type("explain ")
    await term.paste(lines(30))
    await term.waitForText("[paste #2 · 30 lines]")
    await term.press("Enter")
    await term.waitForText("got the paste", 20_000)
    const sent = userText(userContent(fakeModel!.requests()))
    expect(sent).toContain('explain <attachment kind="paste" id="1" lines="30">\nconst value1 = compute(1)\n')
    expect(sent).toContain("const value30 = compute(30)\n</attachment>")
    // The transcript folds the block back to a label.
    await term.waitForText("explain [paste #1 · 30 lines]")
    expect(await term.find("const value30")).toBeNull()
    await term.attach(testInfo, "paste-folded")
  })

  test("a short paste is inserted as text; /paste expand turns a chip back into text", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText(input)
    await term.paste("two\nlines")
    await term.waitForText("two")
    expect(await term.find("[paste #")).toBeNull()
    await term.press("Control+a")
    await term.press("Control+k")
    await term.paste(lines(12))
    await term.waitForText("[paste #1 · 12 lines]")
    await term.press("Control+x")
    await term.type("/paste expand")
    await term.press("Enter")
    await term.waitForText("const value12 = compute(12)")
    expect(await term.find("[paste #1")).toBeNull()
  })
})

test.describe("pasted paths", () => {
  test.use({ model: { steps: [textStep("got the images"), textStep("got the mention")] } })

  test("two image paths, one with a space, become image chips and image parts", async ({ tui, backend, fakeModel }, testInfo) => {
    await writeFile(join(backend.dir, "shot 1.png"), pngBytes())
    await writeFile(join(backend.dir, "b.png"), pngBytes(4096))
    const term = await tui(hyaTui(backend))
    await term.waitForText(input)
    await term.paste(`${backend.dir}/shot\\ 1.png '${backend.dir}/b.png'`)
    await term.waitForText("[image #1 shot 1.png] [image #2 b.png]")
    await term.waitForText("[image #1] shot 1.png · 2 KB")
    await term.waitForText("[image #2] b.png · 4 KB")
    await term.attach(testInfo, "image-chips")
    await term.type("compare")
    await term.press("Enter")
    await term.waitForText("got the images", 20_000)
    const content = userContent(fakeModel!.requests())
    expect(imageParts(content)).toBe(2)
    expect(userText(content)).toBe("[image #1: shot 1.png] [image #2: b.png] compare")
  })

  test("a path inside the workspace is a mention; a missing one stays text with a notice", async ({ tui, backend }) => {
    await writeFile(join(backend.dir, "notes.txt"), "notes\n")
    const term = await tui(hyaTui(backend))
    await term.waitForText(input)
    await term.paste(`${backend.dir}/notes.txt`)
    await term.waitForText("@notes.txt")
    await term.paste(` ${backend.dir}/missing.txt`)
    await term.waitForText("missing.txt: not found on this machine; pasted as text")
    await term.waitForText(`@notes.txt ${backend.dir}/missing.txt`.slice(0, 40))
    // The next edit clears the notice.
    await term.type("x")
    await expect.poll(() => term.find("not found on this machine")).toBeNull()
  })

  test("a paste and Enter in one go send the chip", async ({ tui, backend, fakeModel }) => {
    await writeFile(join(backend.dir, "quick.png"), pngBytes())
    const term = await tui(hyaTui(backend))
    await term.waitForText(input)
    await term.paste(`${backend.dir}/quick.png`)
    await term.press("Enter")
    await term.waitForText("got the images", 20_000)
    expect(imageParts(userContent(fakeModel!.requests()))).toBe(1)
    expect(await inputRow(term, input)).toBeGreaterThan(0)
  })
})

/** A 2×2 24-bit BMP, which `image_resize` converts to PNG. */
function bmpBytes(): Buffer {
  const bytes = Buffer.alloc(54 + 16)
  bytes.fill(0x7f, 54)
  bytes.write("BM", 0, "latin1")
  bytes.writeUInt32LE(bytes.length, 2)
  bytes.writeUInt32LE(54, 10)
  bytes.writeUInt32LE(40, 14)
  bytes.writeInt32LE(2, 18)
  bytes.writeInt32LE(2, 22)
  bytes.writeUInt16LE(1, 26)
  bytes.writeUInt16LE(24, 28)
  bytes.writeUInt32LE(16, 34)
  return bytes
}

test.describe("clipboard and SSH", () => {
  test.use({ model: { steps: [textStep("got the bmp")] } })

  test("Ctrl+V without a display server says so; the draft is untouched", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { env: { DISPLAY: "", WAYLAND_DISPLAY: "" } })
    await term.waitForText(input)
    await term.type("keep me")
    await term.press("Control+v")
    await term.waitForText(/No system clipboard here|The clipboard is empty/)
    await term.waitForText("keep me")
  })

  test("over SSH, Ctrl+V and /paste explain the clipboard is the host's; a missing pasted path is on your machine", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { env: { SSH_CONNECTION: "10.0.0.2 50000 10.0.0.1 22" } })
    await term.waitForText(input)
    await term.press("Control+v")
    await term.waitForText("The clipboard here belongs to the SSH host")
    await term.type("x")
    await expect.poll(() => term.find("belongs to the SSH host")).toBeNull()
    await term.press("Control+x")
    await term.type("/paste")
    await term.press("Enter")
    await term.waitForText("The clipboard here belongs to the SSH host")
    await term.paste(" /Users/me/Desktop/shot.png")
    await term.waitForText("/Users/me/Desktop/shot.png is on your machine, not on the SSH host")
    await term.attach(testInfo, "ssh-hints")
  })

  test("a pasted BMP is converted to PNG and sent", async ({ tui, backend, fakeModel }) => {
    await writeFile(join(backend.dir, "tiny.bmp"), bmpBytes())
    const term = await tui(hyaTui(backend))
    await term.waitForText(input)
    await term.paste(`${backend.dir}/tiny.bmp`)
    await term.waitForText("[image #1 tiny.bmp]")
    await term.waitForText(/\[image #1\] tiny\.bmp · 2×2 · \d+ B/)
    await term.press("Enter")
    await term.waitForText("got the bmp", 20_000)
    const content = userContent(fakeModel!.requests()) as { type?: string; image_url?: { url?: string } }[]
    expect(content.find((part) => part.type === "image_url")?.image_url?.url).toMatch(/^data:image\/png;base64,/)
  })
})
