import { expect, test } from "bun:test"
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { ChipTable, defaultPasteOptions, firstLine, foldAttachments, foldedAttachments, foldsPaste, sanitizePaste } from "../src/composer/chips"
import { chipRows } from "../src/composer/chipRows"
import { inside, looksLikeText, pastedPaths, resolvePastedPath, shellWords, sniffImage } from "../src/composer/paths"
import { resolvePastedPaths } from "../src/app/paste"

const png = (size = 64) => Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), Buffer.alloc(size - 8, 1)])
const lines = (count: number) => Array.from({ length: count }, (_, i) => `const value${i + 1} = compute(${i + 1})`).join("\n")

test("a paste folds above 10 lines or 1000 characters; 0 never folds", () => {
  expect(foldsPaste(lines(10), defaultPasteOptions)).toBe(false)
  expect(foldsPaste(lines(11), defaultPasteOptions)).toBe(true)
  expect(foldsPaste(`${lines(10)}\n`, defaultPasteOptions)).toBe(false)
  expect(foldsPaste("x".repeat(1001), defaultPasteOptions)).toBe(true)
  expect(foldsPaste(lines(500), { ...defaultPasteOptions, collapse_lines: 0 })).toBe(false)
})

test("sanitizing drops escape sequences and control characters, turns CR into LF, and keeps tabs", () => {
  expect(sanitizePaste("\x1b[31mred\x1b[0m\r\nnext\rlast")).toBe("red\nnext\nlast")
  expect(sanitizePaste("a\tb\x00c\x07d")).toBe("a\tbcd")
  expect(sanitizePaste("\x1b]0;title\x07text")).toBe("text")
  expect(sanitizePaste("cafe\u0301")).toBe("café")
  expect(firstLine("\n\n   first line\nsecond")).toBe("first line")
  expect(firstLine("x".repeat(80), 10)).toBe("xxxxxxxxx…")
})

test("chips: tokens, presence, atomic ranges, and expansion numbered per kind in order", () => {
  const chips = new ChipTable()
  const paste = chips.addPaste(lines(30))
  const file = chips.addFile("/tmp/notes.md", "one\ntwo\n")
  const image = chips.addImage("/tmp/shot 1.png", { mime: "image/png", data: "AAAA", size: 3 })
  expect(paste.token).toBe("[paste #1 · 30 lines]")
  expect(file.token).toBe("[file #2 notes.md]")
  expect(image.token).toBe("[image #3 shot 1.png]")
  const text = `see ${image.token} and ${paste.token} then ${file.token} `
  expect(chips.present(text).map((chip) => chip.id)).toEqual([3, 1, 2])
  // Backspace at the end of a token, Delete at its start.
  const end = text.indexOf(paste.token) + paste.token.length
  expect(chips.tokenEndingAt(text, end)).toEqual({ start: end - paste.token.length, end })
  expect(chips.tokenEndingAt(text, end + 1)).toBeUndefined()
  expect(chips.tokenStartingAt(text, text.indexOf(file.token))).toEqual({ start: text.indexOf(file.token), end: text.indexOf(file.token) + file.token.length })
  expect(chips.tokenAround(text, text.indexOf(file.token) + 2)?.chip.id).toBe(2)
  const expanded = chips.expand(text, { wrap: true })
  expect(expanded.text).toBe(`see [image #1: shot 1.png] and <attachment kind="paste" id="1" lines="30">\n${lines(30)}\n</attachment> then <attachment kind="file" name="notes.md" lines="2">\none\ntwo\n</attachment> `)
  expect(expanded.images).toEqual([{ name: "shot 1.png", mime: "image/png", data: "AAAA", path: "/tmp/shot 1.png" }])
  expect(chips.expand(`${paste.token}`, { wrap: false }).text).toBe(lines(30))
  // A cut token is no longer a chip: its attachment is dropped.
  expect(chips.present("[paste #1 · 30 li").length).toBe(0)
  // Ids stay unique for history entries.
  expect(chips.addPaste("x".repeat(2000)).token).toBe("[paste #4 · 2000 chars]")
})

test("sent attachment blocks fold back to labels in the transcript", () => {
  const chips = new ChipTable()
  const paste = chips.addPaste(lines(12))
  const file = chips.addFile("/tmp/a\"b.md", "x\n")
  const sent = chips.expand(`look ${paste.token} and ${file.token}`, { wrap: true }).text
  expect(foldAttachments(sent)).toBe('look [paste #1 · 12 lines] and [file a"b.md · 1 line]')
  expect(foldedAttachments(sent).map((fold) => fold.kind)).toEqual(["paste", "file"])
  expect(foldAttachments("no blocks here")).toBe("no blocks here")
})

test("attachment rows: one per chip and pending image, capped with +N more", () => {
  const chips = new ChipTable()
  const rows = chipRows([chips.addPaste(lines(150)), chips.addFile("/tmp/notes.md", "a\nb\n"), chips.addImage("/x/b.bmp", undefined, "not a PNG, JPEG, GIF, or WebP image")], [{ path: "s.png", name: "s.png", size: 2048 }])
  expect(rows.map((row) => `${row.tag} ${row.detail}`)).toEqual([
    "[paste #1] 150 lines · const value1 = compute(1)",
    "[file #2] notes.md · 2 lines · 4 B",
    "[image #3] b.bmp · not a PNG, JPEG, GIF, or WebP image",
    "[image] s.png · 2 KB",
  ])
  expect(rows[2]!.error).toBe(true)
  const many = chipRows(Array.from({ length: 6 }, () => chips.addPaste(lines(20))), [], 4)
  expect(many.map((row) => row.tag)).toEqual(["[paste #4]", "[paste #5]", "[paste #6]", "+3 more"])
})

test("path pastes: quotes, escapes, ~, file://, several paths; prose stays text", () => {
  expect(shellWords(`'/a b/c.png' "/d \\"e\\".png" /f\\ g.png`)?.map((word) => word.word)).toEqual(["/a b/c.png", '/d "e".png', "/f g.png"])
  expect(shellWords("'open")).toBeUndefined()
  expect(pastedPaths("/tmp/shot\\ 1.png", "/home/u")).toEqual([{ raw: "/tmp/shot\\ 1.png", path: "/tmp/shot 1.png" }])
  expect(pastedPaths("~/a.png file:///tmp/b%20c.png\n./d.txt", "/home/u")?.map((item) => item.path)).toEqual(["/home/u/a.png", "/tmp/b c.png", "./d.txt"])
  expect(pastedPaths("/Users/me/Desktop/Screenshot 2026-06-25 at 1.23.45 PM.png")?.[0]?.path).toBe("/Users/me/Desktop/Screenshot 2026-06-25 at 1.23.45 PM.png")
  expect(pastedPaths("look at /tmp/a.png")).toBeUndefined()
  expect(pastedPaths("/tmp/a b.txt")).toBeUndefined()
  expect(pastedPaths("")).toBeUndefined()
  expect(resolvePastedPath("file://localhost/x", undefined)).toBe("/x")
  expect(inside("/w/a", "/w")).toBe(true)
  expect(inside("/wx", "/w")).toBe(false)
  expect(sniffImage(png())).toBe("image/png")
  expect(sniffImage(new Uint8Array([0xff, 0xd8, 0xff, 0]))).toBe("image/jpeg")
  expect(sniffImage(new Uint8Array([0x42, 0x4d, 0, 0]))).toBeUndefined()
  expect(looksLikeText(Buffer.from("hello\n"))).toBe(true)
  expect(looksLikeText(new Uint8Array([104, 0, 105]))).toBe(false)
})

test("pasted paths become image chips, mentions, file chips, or text with a notice", async () => {
  const root = mkdtempSync(join(tmpdir(), "tui-paste-"))
  try {
    const work = join(root, "work"), outside = join(root, "outside")
    mkdirSync(work)
    mkdirSync(outside)
    writeFileSync(join(work, "shot 1.png"), png())
    writeFileSync(join(work, "notes.md"), "inside\n")
    writeFileSync(join(outside, "notes.md"), "outside\nfile\n")
    writeFileSync(join(outside, "blob.bin"), Buffer.from([1, 0, 2]))
    writeFileSync(join(outside, "fake.png"), "not an image")
    const chips = new ChipTable()
    const context = { chips, remote: false, workdir: work, roots: [], existsOnBackend: async () => false }
    const paths = pastedPaths([join(work, "shot 1.png"), join(work, "notes.md"), join(outside, "notes.md"), join(outside, "blob.bin"), join(outside, "fake.png"), join(root, "missing.txt"), outside].map((path) => `'${path}'`).join(" "))!
    const result = await resolvePastedPaths(paths, context)
    expect(result.insert).toBe(`[image #1 shot 1.png] @notes.md [file #2 notes.md] '${join(outside, "blob.bin")}' [image #3 fake.png] '${join(root, "missing.txt")}' '${outside}' `)
    expect(chips.get(1)?.image?.mime).toBe("image/png")
    expect(chips.get(2)?.text).toBe("outside\nfile\n")
    expect(chips.get(3)?.error).toBe("not an image this TUI can read")
    expect(result.notices).toEqual([
      "blob.bin: not a text file; pasted its path",
      expect.stringContaining("missing.txt: not found on this machine; pasted as text"),
      expect.stringContaining("a directory outside the workspace; pasted its path"),
    ])
    // Remote mode: nothing local is backend-readable; a backend-only path is a mention.
    const remote = await resolvePastedPaths(pastedPaths(`${join(work, "notes.md")} /srv/repo/a.txt`)!, { ...context, remote: true, existsOnBackend: async (path) => path === "/srv/repo/a.txt" })
    expect(remote.insert).toBe("[file #4 notes.md] @/srv/repo/a.txt ")
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})
