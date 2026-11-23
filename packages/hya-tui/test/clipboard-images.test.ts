import { expect, test } from "bun:test"
import { inSshSession, noClipboardHint, readClipboard, sshClipboardHint, sshPathHint } from "../src/composer/hostClipboard"
import { isPastedImagePath, prepareImage } from "../src/composer/images"

/** A 2×2 24-bit BMP: a type the server refuses, which `image_resize` converts to PNG. */
function bmp(): Uint8Array {
  const width = 2, height = 2, row = 8, pixels = row * height
  const bytes = new Uint8Array(54 + pixels)
  const view = new DataView(bytes.buffer)
  bytes.set([0x42, 0x4d])
  view.setUint32(2, bytes.length, true)
  view.setUint32(10, 54, true)
  view.setUint32(14, 40, true)
  view.setInt32(18, width, true)
  view.setInt32(22, height, true)
  view.setUint16(26, 1, true)
  view.setUint16(28, 24, true)
  view.setUint32(34, pixels, true)
  bytes.fill(0x7f, 54)
  return bytes
}

test("SSH sessions never read the clipboard; other results map to a paste or a reason", async () => {
  expect(inSshSession({ SSH_CONNECTION: "1.2.3.4 22 5.6.7.8 22" })).toBe(true)
  expect(inSshSession({ SSH_TTY: "/dev/pts/1" })).toBe(true)
  expect(inSshSession({})).toBe(false)
  let asked = false
  expect(await readClipboard({ SSH_CLIENT: "x" }, async () => { asked = true; return { status: "empty" } })).toEqual({ kind: "none", reason: sshClipboardHint })
  expect(asked).toBe(false)
  expect(await readClipboard({}, async () => ({ status: "unsupported" }))).toEqual({ kind: "none", reason: noClipboardHint })
  expect(await readClipboard({}, async () => ({ status: "empty" }))).toEqual({ kind: "none", reason: "The clipboard is empty" })
  const png = new Uint8Array([0x89, 0x50, 0x4e, 0x47])
  expect(await readClipboard({}, async (types) => {
    expect(types[0]).toBe("image/png")
    expect(types.at(-1)).toBe("text/plain")
    return { status: "read", representation: { mimeType: "image/png", bytes: png } }
  })).toEqual({ kind: "image", mime: "image/png", bytes: png })
  expect(await readClipboard({}, async () => ({ status: "read", representation: { mimeType: "text/plain", bytes: new TextEncoder().encode("hi") } }))).toEqual({ kind: "text", text: "hi" })
  expect((await readClipboard({}, async () => { throw new Error("boom") })).kind).toBe("none")
  expect(sshPathHint("~/shot.png")).toContain("~/shot.png is on your machine, not on the SSH host")
})

test("pasted image paths include types that are converted before sending", () => {
  expect(isPastedImagePath("/a/b.PNG")).toBe(true)
  expect(isPastedImagePath("/a/b.bmp")).toBe(true)
  expect(isPastedImagePath("/a/b.heic")).toBe(true)
  expect(isPastedImagePath("/a/b.txt")).toBe(false)
})

test("a BMP becomes a PNG with its size in pixels; without image_resize it is refused", async () => {
  const converted = await prepareImage(bmp(), { resize: true })
  expect(converted.error).toBeUndefined()
  expect(converted.image?.mime).toBe("image/png")
  expect(converted.image?.width).toBe(2)
  expect(converted.image?.height).toBe(2)
  expect(Buffer.from(converted.image!.data, "base64").subarray(0, 4)).toEqual(Buffer.from([0x89, 0x50, 0x4e, 0x47]))
  expect(await prepareImage(bmp(), { resize: false })).toEqual({ error: "not a PNG, JPEG, GIF, or WebP image" })
  expect(await prepareImage(new Uint8Array(), { resize: true })).toEqual({ error: "empty image" })
  // A supported image passes through with its pixel size.
  const png = Buffer.from(converted.image!.data, "base64")
  const same = await prepareImage(png, { resize: true })
  expect(same.image?.data).toBe(converted.image!.data)
  expect(same.image?.width).toBe(2)
})
