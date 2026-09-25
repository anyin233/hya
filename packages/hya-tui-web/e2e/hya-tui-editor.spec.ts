// External editor (docs/tui.md "External editor"): Ctrl+X Ctrl+E or
// `/editor` suspends the TUI, runs $VISUAL / $EDITOR on a temp file holding
// the input, then puts the edited text back into the input without sending
// it. The editor here is a shell script: it prints a line (visible while the
// TUI is suspended), waits for the spec's go-file, and rewrites the file.

import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import type { Tui } from "./harness"
import { expect, hyaTui, test } from "./hya"

let dir: string
test.beforeEach(async () => { dir = await mkdtemp(join(tmpdir(), "hya-tui-editor-")) })
test.afterEach(async () => { await rm(dir, { recursive: true, force: true }) })

/** An editor script that shows `editing <file>`, waits for `<dir>/go`, then runs `edit` (sh, file in $1). */
async function editorScript(edit: string): Promise<string> {
  const path = join(dir, "editor.sh")
  await writeFile(path, `#!/bin/sh\necho "editing $1"\nwhile [ ! -f "${dir}/go" ]; do sleep 0.05; done\n${edit}\n`)
  await chmod(path, 0o755)
  return path
}

/** The composer's box rows (between its top and bottom borders). */
async function composerText(term: Tui): Promise<string> {
  const lines = await term.lines()
  const bottom = lines.findLastIndex((line) => line.startsWith("└"))
  const top = lines.slice(0, bottom).findLastIndex((line) => line.startsWith("┌"))
  // The sidebar may share these rows: cut at the composer's right border.
  const right = lines[bottom]!.indexOf("┘")
  return lines.slice(top + 1, bottom).map((line) => line.slice(1, right).trim()).join("\n").trim()
}

test("Ctrl+X Ctrl+E edits the input in $EDITOR and puts the result back, unsent", async ({ tui, backend }, testInfo) => {
  const editor = await editorScript(`printf 'edited: %s\\nsecond line' "$(cat "$1")" > "$1"`)
  const term = await tui(hyaTui(backend), { env: { EDITOR: editor, VISUAL: "" } })
  await term.waitForText("Connected to hya")
  await term.type("draft words")
  await term.press("Control+x")
  await term.waitForText("Ctrl+X · Ctrl+E opens the external editor")
  await term.press("Control+e")
  // The TUI hands the terminal to the editor: its output is on screen.
  await term.waitForText(/editing \S+prompt\.md/)
  await term.attach(testInfo, "editor-running")
  await writeFile(join(dir, "go"), "")
  await term.waitForText("Edited in the external editor · Enter sends")
  await expect.poll(() => composerText(term)).toBe("edited: draft words\nsecond line")
  // Not sent: no session was created, the transcript is still empty.
  expect(await term.find("No messages yet")).not.toBeNull()
  // Enter sends the edited text as usual.
  await term.press("Enter")
  await term.waitForText("edited: draft words")
  await expect.poll(() => composerText(term)).toBe("Message, /command, !shell, or @file")
})

test("/editor with $VISUAL (arguments allowed) wins over $EDITOR", async ({ tui, backend }) => {
  const editor = await editorScript(`[ "$1" = "--flag" ] && printf 'visual with %s' "$1" > "$2"`)
  const term = await tui(hyaTui(backend), { env: { VISUAL: `${editor} --flag`, EDITOR: "/no/such/editor" } })
  await term.waitForText("Connected to hya")
  await writeFile(join(dir, "go"), "")
  await term.type("/editor")
  await term.press("Enter")
  await term.waitForText("Edited in the external editor")
  await expect.poll(() => composerText(term)).toBe("visual with --flag")
})

test("a failing or missing editor keeps the input and says why", async ({ tui, backend }) => {
  const failing = await editorScript("exit 3")
  await writeFile(join(dir, "go"), "")
  let term = await tui(hyaTui(backend), { env: { EDITOR: failing, VISUAL: "" } })
  await term.waitForText("Connected to hya")
  await term.type("keep this")
  await term.press("Control+x")
  await term.press("Control+e")
  await term.waitForText("Editor editor.sh exited with status 3 · input unchanged")
  expect(await composerText(term)).toBe("keep this")

  term = await tui(hyaTui(backend), { env: { EDITOR: "/no/such/editor-hya", VISUAL: "" } })
  await term.waitForText("Connected to hya")
  await term.type("still here")
  await term.press("Control+x")
  await term.press("e")
  await term.waitForText("Editor editor-hya not found · input unchanged")
  expect(await composerText(term)).toBe("still here")
})

test("Ctrl+X then another key drops the chord; that key works as usual; help lists the chord", async ({ tui, backend }) => {
  const term = await tui(hyaTui(backend), { viewport: { width: 690, height: 640 } })
  await term.waitForText("Connected to hya")
  expect((await term.size()).cols).toBeLessThanOrEqual(84)
  await term.press("Control+x")
  await term.type("ab")
  await expect.poll(() => composerText(term)).toBe("ab")
  await expect.poll(() => term.find("Ctrl+X · Ctrl+E opens")).toBeNull()
  await term.press("Control+u")
  await term.press("?")
  await term.waitForText("Help · keys and commands")
  await term.type("editor")
  await term.waitForText("Ctrl+X Ctrl+E")
  await term.waitForText("/editor")
})
