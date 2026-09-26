import { expect, test } from "bun:test"
import type { KeyLike } from "../src/keys/bindings"
import {
  currentDiffFile,
  diffFileWindow,
  diffViewHint,
  diffViewKey,
  fileLine,
  initialDiffView,
  parseDiff,
  settleDiffView,
  type DiffFile,
  type DiffViewState,
} from "../src/state/diff"

const key = (name: string, extra: Partial<KeyLike> = {}): KeyLike => ({
  name, ctrl: false, meta: false, shift: false, sequence: name.length === 1 ? name : "", ...extra,
})

const trackedDiff = [
  "diff --git a/src/a.ts b/src/a.ts",
  "index 111..222 100644",
  "--- a/src/a.ts",
  "+++ b/src/a.ts",
  "@@ -1,2 +1,3 @@",
  " unchanged",
  "-old line",
  "+new line",
  "+second new line",
].join("\n")

const untrackedDiff = [
  "diff --git a/dev/null b/src/b.ts",
  "new file mode 100644",
  "index 0000000..333",
  "--- /dev/null",
  "+++ b/src/b.ts",
  "@@ -0,0 +1,2 @@",
  "+line one",
  "+line two",
].join("\n")

test("parseDiff returns [] for an empty or blank diff", () => {
  expect(parseDiff("")).toEqual([])
  expect(parseDiff("   \n")).toEqual([])
})

test("parseDiff splits a tracked-file section and counts +/-", () => {
  const files = parseDiff(trackedDiff)
  expect(files).toHaveLength(1)
  expect(files[0]!.path).toBe("src/a.ts")
  expect(files[0]!.additions).toBe(2)
  expect(files[0]!.deletions).toBe(1)
  expect(files[0]!.lines.some((line) => line.tone === "hunk")).toBe(true)
})

test("parseDiff splits an untracked file's --no-index section (dev/null side skipped)", () => {
  const files = parseDiff(untrackedDiff)
  expect(files).toHaveLength(1)
  expect(files[0]!.path).toBe("src/b.ts")
  expect(files[0]!.additions).toBe(2)
  expect(files[0]!.deletions).toBe(0)
})

test("parseDiff joins multiple sections (tracked + untracked) in order", () => {
  const files = parseDiff(`${trackedDiff}\n${untrackedDiff}`)
  expect(files.map((file) => file.path)).toEqual(["src/a.ts", "src/b.ts"])
})

test("parseDiff relativizes an absolute path under directory", () => {
  const abs = untrackedDiff.replace(/src\/b\.ts/g, "/work/repo/src/b.ts")
  const files = parseDiff(abs, "/work/repo")
  expect(files[0]!.path).toBe("src/b.ts")
})

test("parseDiff relativizes git's own --no-index header, which drops the leading slash in a/ b/", () => {
  // `git diff --no-index -- /dev/null <abs>` writes `+++ b/<abs without the leading />`.
  const gitStyle = untrackedDiff.replace(/src\/b\.ts/g, "work/repo/src/b.ts")
  const files = parseDiff(gitStyle, "/work/repo")
  expect(files[0]!.path).toBe("src/b.ts")
})

test("initialDiffView opens the first file; currentDiffFile finds it", () => {
  const files = parseDiff(`${trackedDiff}\n${untrackedDiff}`)
  const view = initialDiffView(files)
  expect(view.current).toBe("src/a.ts")
  expect(currentDiffFile(view)?.path).toBe("src/a.ts")
})

test("settleDiffView keeps the open file when it still exists, else the first", () => {
  const files = parseDiff(`${trackedDiff}\n${untrackedDiff}`)
  const view: DiffViewState = { files, current: "src/b.ts" }
  expect(settleDiffView(view, files).current).toBe("src/b.ts")
  const onlyA = parseDiff(trackedDiff)
  expect(settleDiffView(view, onlyA).current).toBe("src/a.ts")
  expect(settleDiffView(view, []).current).toBeUndefined()
})

test("n/p and ]/[ switch files with wrap-around", () => {
  const files = parseDiff(`${trackedDiff}\n${untrackedDiff}`)
  const view: DiffViewState = { files, current: "src/a.ts" }
  expect(diffViewKey(view, key("n", { sequence: "n" }))).toEqual({ type: "update", view: { ...view, current: "src/b.ts" } })
  expect(diffViewKey({ ...view, current: "src/b.ts" }, key("]", { sequence: "]" }))).toEqual({ type: "update", view: { ...view, current: "src/a.ts" } })
  expect(diffViewKey(view, key("p", { sequence: "p" }))).toEqual({ type: "update", view: { ...view, current: "src/b.ts" } })
  expect(diffViewKey(view, key("[", { sequence: "[" }))).toEqual({ type: "update", view: { ...view, current: "src/b.ts" } })
})

test("Up/Down/PgUp/PgDn/Home/End produce scroll outcomes, not highlight moves", () => {
  const view: DiffViewState = { files: [], current: undefined }
  expect(diffViewKey(view, key("up"))).toEqual({ type: "scroll", action: "line-up" })
  expect(diffViewKey(view, key("down"))).toEqual({ type: "scroll", action: "line-down" })
  expect(diffViewKey(view, key("pageup"))).toEqual({ type: "scroll", action: "page-up" })
  expect(diffViewKey(view, key("pagedown"))).toEqual({ type: "scroll", action: "page-down" })
  expect(diffViewKey(view, key("home"))).toEqual({ type: "scroll", action: "top" })
  expect(diffViewKey(view, key("end"))).toEqual({ type: "scroll", action: "bottom" })
})

test("r reloads, Esc closes", () => {
  const view: DiffViewState = { files: [], current: undefined }
  expect(diffViewKey(view, key("r", { sequence: "r" }))).toEqual({ type: "reload" })
  expect(diffViewKey(view, key("escape"))).toEqual({ type: "close" })
})

test("Esc while busy cancels instead of closing", () => {
  const view: DiffViewState = { files: [], current: undefined, busy: { label: "Reloading", startedAt: 0 } }
  expect(diffViewKey(view, key("escape"))).toEqual({ type: "cancelBusy" })
  expect(diffViewKey(view, key("n", { sequence: "n" }))).toEqual({ type: "none" })
})

test("fileLine shows the marker and +/- counts and fits the width", () => {
  const file = { path: "src/a.ts", additions: 3, deletions: 1, lines: [] }
  const line = fileLine(file, true, 40)
  expect(line.startsWith("▸ src/a.ts")).toBe(true)
  expect(line).toContain("+3 -1")
  expect(Bun.stringWidth(fileLine(file, false, 40))).toBeLessThanOrEqual(40)
})

test("diffFileWindow keeps the open file in view and reports the more-above/below counts", () => {
  const files: DiffFile[] = Array.from({ length: 20 }, (_, i) => ({ path: `f${i}.ts`, additions: 0, deletions: 0, lines: [] }))
  expect(diffFileWindow(files, "f0.ts", 5)).toEqual({ start: 0, end: 5, moreAbove: 0, moreBelow: 15 })
  expect(diffFileWindow(files, "f19.ts", 5)).toEqual({ start: 15, end: 20, moreAbove: 15, moreBelow: 0 })
  expect(diffFileWindow(files, "f10.ts", 5)).toEqual({ start: 6, end: 11, moreAbove: 6, moreBelow: 9 })
  expect(diffFileWindow(files, undefined, 5)).toEqual({ start: 0, end: 5, moreAbove: 0, moreBelow: 15 })
})

test("diffFileWindow shows every file (no more indicator) when they all fit", () => {
  const files: DiffFile[] = Array.from({ length: 3 }, (_, i) => ({ path: `f${i}.ts`, additions: 0, deletions: 0, lines: [] }))
  expect(diffFileWindow(files, "f1.ts", 5)).toEqual({ start: 0, end: 3, moreAbove: 0, moreBelow: 0 })
})

test("diffViewHint reflects busy, notice, and the default hint", () => {
  expect(diffViewHint({ files: [], current: undefined, busy: { label: "Reloading", startedAt: 0 } })).toContain("Esc cancels")
  expect(diffViewHint({ files: [], current: undefined, notice: { tone: "error", text: "boom" } })).toBe("boom")
  expect(diffViewHint({ files: [], current: undefined })).toContain("Esc close")
})
