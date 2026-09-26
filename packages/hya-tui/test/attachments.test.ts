import { expect, test } from "bun:test"
import {
  attachmentLabel,
  exceedsTurnBudget,
  formatBytes,
  imageMentionPaths,
  isImagePath,
  mimeForPath,
  pastedImagePath,
  validateAttachmentBytes,
} from "../src/composer/attachments"

test("isImagePath/mimeForPath accept png, jpg, jpeg, gif, webp only, case-insensitively", () => {
  expect(isImagePath("shot.png")).toBe(true)
  expect(isImagePath("a/b/shot.JPG")).toBe(true)
  expect(isImagePath("shot.jpeg")).toBe(true)
  expect(isImagePath("shot.gif")).toBe(true)
  expect(isImagePath("shot.webp")).toBe(true)
  expect(isImagePath("shot.txt")).toBe(false)
  expect(isImagePath("noext")).toBe(false)
  expect(mimeForPath("a.PNG")).toBe("image/png")
  expect(mimeForPath("a.jpg")).toBe("image/jpeg")
  expect(mimeForPath("a.bmp")).toBeUndefined()
})

test("imageMentionPaths finds @path mentions that resolve to image files, de-duplicated, in order", () => {
  expect(imageMentionPaths("describe @shot.png please")).toEqual(["shot.png"])
  expect(imageMentionPaths("@a.png and @b.txt and @a.png again")).toEqual(["a.png"])
  expect(imageMentionPaths("no mention here")).toEqual([])
  // An @ that is not preceded by start-of-text or whitespace is not a mention.
  expect(imageMentionPaths("email@shot.png")).toEqual([])
  // Nested paths keep their directory.
  expect(imageMentionPaths("@dir/sub/shot.png")).toEqual(["dir/sub/shot.png"])
})

test("pastedImagePath recognizes a bare, quoted, or escaped image path and nothing else", () => {
  expect(pastedImagePath("/tmp/shot.png")).toBe("/tmp/shot.png")
  expect(pastedImagePath("  /tmp/shot.png  \n")).toBe("/tmp/shot.png")
  expect(pastedImagePath("'/tmp/my shot.png'")).toBe("/tmp/my shot.png")
  expect(pastedImagePath('"/tmp/my shot.png"')).toBe("/tmp/my shot.png")
  expect(pastedImagePath("/tmp/my\\ shot.png")).toBe("/tmp/my shot.png")
  // Not a path to an image: falls back to a normal text paste.
  expect(pastedImagePath("hello world")).toBeUndefined()
  expect(pastedImagePath("/tmp/notes.txt")).toBeUndefined()
  // A multi-line paste is never a single dragged path.
  expect(pastedImagePath("/tmp/a.png\n/tmp/b.png")).toBeUndefined()
  // Unescaped, unquoted spaces: ambiguous with prose, so not treated as a path.
  expect(pastedImagePath("look at /tmp/my shot.png")).toBeUndefined()
})

test("validateAttachmentBytes rejects empty files and files over 10 MiB", () => {
  expect(validateAttachmentBytes("a.png", 0)).toMatch(/empty/)
  expect(validateAttachmentBytes("a.png", 10 * 1024 * 1024 + 1)).toMatch(/10 MiB/)
  expect(validateAttachmentBytes("a.png", 1024)).toBeUndefined()
})

test("exceedsTurnBudget caps the running total at 20 MiB", () => {
  const twelveMib = 12 * 1024 * 1024
  expect(exceedsTurnBudget([twelveMib], twelveMib)).toBe(true)
  expect(exceedsTurnBudget([twelveMib], 1024)).toBe(false)
})

test("formatBytes and attachmentLabel", () => {
  expect(formatBytes(512)).toBe("512 B")
  expect(formatBytes(240 * 1024)).toBe("240 KB")
  expect(formatBytes(1.4 * 1024 * 1024)).toBe("1.4 MB")
  expect(attachmentLabel({ name: "shot.png", size: 240 * 1024 })).toBe("[image] shot.png · 240 KB")
  expect(attachmentLabel({ name: "shot.png", error: "larger than 10 MiB" })).toBe("[image] shot.png · larger than 10 MiB")
})
