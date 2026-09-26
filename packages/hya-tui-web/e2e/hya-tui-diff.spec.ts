// The Diff view (`/diff`): the working tree diff split per file, with
// +/- counts, colored lines, file switching, and reload
// (state/diff.ts, `GET /v1/vcs/diff`).

import { execFileSync } from "node:child_process"
import { writeFile } from "node:fs/promises"
import { join } from "node:path"
import { hyaTui, initGitRepo, test } from "./hya"

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
})
