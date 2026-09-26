// The Diff view (`/diff`): the working tree diff split per file, with
// +/- counts, colored lines, file switching, and reload
// (state/diff.ts, `GET /v1/vcs/diff`).

import { execFileSync } from "node:child_process"
import { writeFile } from "node:fs/promises"
import { join } from "node:path"
import { expect, hyaTui, initGitRepo, test } from "./hya"
import type { Tui } from "./harness"

async function at(term: Tui, needle: string) {
  const found = await term.find(needle)
  expect(found, `screen shows ${needle}`).not.toBeNull()
  return found!
}

/** The diff's last line (`+ added 80`) is on screen, on its own row above the key hint. */
async function expectLastLineVisible(term: Tui) {
  await term.waitForText("+ added 80")
  const last = await at(term, "+ added 80")
  const hint = await at(term, "↑↓ scroll")
  expect(last.row, "the last diff line sits above the hint").toBeLessThan(hint.row)
}

test.describe("hya TUI Diff view", () => {
  test("not a git repository: the empty state", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await term.type("/diff")
    await term.press("Enter")
    await term.waitForText("Diff")
    await term.waitForText("0 files changed")
    await term.waitForText("Not a git repository")
    await term.press("Escape")
    await term.waitForText("Enter a prompt · /new creates a session")
  })

  test("a tracked edit and an untracked file: per-file split, counts, n/p switch, r reload, Esc closes", async ({ tui, backend }) => {
    await initGitRepo(backend.dir)
    await writeFile(join(backend.dir, "tracked.txt"), "one\n")
    // Commit the tracked file first so the later edit is a real diff.
    execFileSync("git", ["-C", backend.dir, "add", "tracked.txt"])
    execFileSync("git", ["-C", backend.dir, "-c", "user.email=e2e@hya.test", "-c", "user.name=e2e", "commit", "-q", "-m", "add tracked"])
    await writeFile(join(backend.dir, "tracked.txt"), "one\ntwo\n")
    await writeFile(join(backend.dir, "untracked.txt"), "new file\n")

    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await term.type("/diff")
    await term.press("Enter")
    await term.waitForText("Diff › tracked.txt")
    await term.waitForText("2 files changed")
    await term.waitForText(/tracked\.txt\s+\+1 -0/)
    await term.waitForText(/untracked\.txt\s+\+1 -0/)
    await term.waitForText("+ two")
    await term.waitForText("↑↓ scroll · n/p file · r reload · Esc close")

    await term.press("n")
    await term.waitForText("Diff › untracked.txt")
    await term.waitForText("+ new file")
    await term.press("p")
    await term.waitForText("Diff › tracked.txt")

    await writeFile(join(backend.dir, "tracked.txt"), "one\ntwo\nthree\n")
    await term.press("r")
    await term.waitForText(/tracked\.txt\s+\+2 -0/)

    await term.press("Escape")
    await term.waitForText("Enter a prompt · /new creates a session")
  })

  test("a diff long enough to scroll: PgDn/PgUp/Home/End and the mouse wheel move the visible lines", async ({ tui, backend }) => {
    await initGitRepo(backend.dir)
    // A small tracked file, committed, then 80 new lines appended: a
    // pure-addition diff (no matching "-" lines to confuse the assertions)
    // long enough to overflow the scrollbox.
    await writeFile(join(backend.dir, "long.txt"), "intro\n")
    execFileSync("git", ["-C", backend.dir, "add", "long.txt"])
    execFileSync("git", ["-C", backend.dir, "-c", "user.email=e2e@hya.test", "-c", "user.name=e2e", "commit", "-q", "-m", "add long"])
    const added = Array.from({ length: 80 }, (_, index) => `added ${String(index + 1).padStart(2, "0")}`)
    await writeFile(join(backend.dir, "long.txt"), `intro\n${added.join("\n")}\n`)

    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await term.type("/diff")
    await term.press("Enter")
    await term.waitForText("Diff › long.txt")
    await term.waitForText(/long\.txt\s+\+80 -0/)
    // The top of the diff is visible first.
    await term.waitForText("+ added 01")

    await term.press("PageDown")
    await expect.poll(async () => (await term.text()).includes("+ added 01")).toBe(false)
    await term.waitForText(/\+ added \d\d/)

    await term.press("End")
    // End reaches the true last line of the file, fully visible above the
    // key hint (the hint used to overlap the scrollbox's last row).
    await expectLastLineVisible(term)
    await expect.poll(async () => (await term.text()).includes("+ added 01")).toBe(false)

    await term.press("Home")
    await term.waitForText("+ added 01")
    await expect.poll(async () => (await term.text()).includes("+ added 79")).toBe(false)

    // Paging down all the way stops on the last line too.
    for (let page = 0; page < 8; page++) await term.press("PageDown")
    await expectLastLineVisible(term)

    await term.press("Home")
    await term.waitForText("+ added 01")
    // One page down from the top: a view in the middle of the file (a single
    // key, so the screen read below cannot catch an intermediate frame).
    await term.press("PageDown")
    await expect.poll(async () => (await term.text()).includes("+ added 01")).toBe(false)
    const midMatch = (await term.text()).match(/\+ added \d\d/)!
    const midRow = await at(term, midMatch[0])
    const box = (await term.page.locator(".xterm-screen").boundingBox())!
    const size = await term.size()
    await term.page.mouse.move(box.x + box.width / 2, box.y + ((midRow.row + 0.5) / size.rows) * box.height)
    await term.page.mouse.wheel(0, -600)
    // Scrolling up with the wheel moves the view: the first visible line is
    // an earlier one (a longer poll: the wheel event's round trip through
    // the PTY can lag under heavy parallel load).
    const firstShown = async () => (await term.text()).match(/\+ added \d\d/)?.[0] ?? ""
    await expect.poll(async () => (await firstShown()) < midMatch[0], { timeout: 15_000, message: "the wheel scrolled up" }).toBe(true)

    // Wheel-scrolling down to the end shows the last line as well.
    // One wheel event moves a few rows, so keep wheeling until the end shows.
    await expect.poll(async () => {
      if ((await term.text()).includes("+ added 80")) return true
      await term.page.mouse.wheel(0, 600)
      return false
    }, { timeout: 15_000, intervals: [50] }).toBe(true)
    await expectLastLineVisible(term)

    await term.press("Escape")
    await term.waitForText("Enter a prompt · /new creates a session")
  })

  test("~60 changed files: the file list windows around the open file with a N more indicator (T1c)", async ({ tui, backend }) => {
    await initGitRepo(backend.dir)
    const fileCount = 60
    for (let index = 0; index < fileCount; index++) {
      await writeFile(join(backend.dir, `file${String(index).padStart(2, "0")}.txt`), `one ${index}\n`)
    }

    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await term.type("/diff")
    await term.press("Enter")
    await term.waitForText(`${fileCount} files changed`)
    // The list opens on the first file: nothing hidden above, a marker below.
    await term.waitForText("file00.txt")
    await term.waitForText(/↓ \d+ more/)
    await expect((await term.text())).not.toContain("file59.txt")
    // The hint below the file list stays visible (T1b's flexShrink lesson).
    await term.waitForText("n/p file")

    // Walk to the last file: the window follows the selection all the way
    // down, and the last file becomes visible with a "more above" marker.
    for (let index = 0; index < fileCount - 1; index++) await term.press("n")
    await term.waitForText("Diff › file59.txt")
    await term.waitForText("file59.txt")
    await term.waitForText(/↑ \d+ more/)
    await term.waitForText("n/p file")
    // The file list should fill the panel down to its border, not leave a
    // blank band: the last file sits on the row right above the panel's
    // bottom border, or within one row of it.
    {
      const lines = await term.lines()
      const last = lines.findIndex((line) => line.includes("▸ file59.txt"))
      const border = lines.findIndex((line, index) => index > last && /└/.test(line))
      expect(border - last).toBeLessThanOrEqual(2)
    }

    await term.press("p")
    await term.waitForText("Diff › file58.txt")

    await term.press("Escape")
    await term.waitForText("Enter a prompt · /new creates a session")
  })
})
