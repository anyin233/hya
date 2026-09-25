import { afterEach, expect, test } from "bun:test"
import { chmodSync, existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { editText, editorCommand, splitCommand } from "../src/composer/editor"

const dirs: string[] = []
function temp(): string {
  const dir = mkdtempSync(join(tmpdir(), "hya-tui-editor-"))
  dirs.push(dir)
  return dir
}
afterEach(() => { for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true }) })

/** A shell script used as the editor: `body` runs with the file path in `$1`. */
function script(body: string): string {
  const path = join(temp(), "edit.sh")
  writeFileSync(path, `#!/bin/sh\n${body}\n`)
  chmodSync(path, 0o755)
  return path
}

test("the editor is $VISUAL, else $EDITOR, else vi; empty values are skipped", () => {
  expect(editorCommand({ VISUAL: "nvim", EDITOR: "nano" })).toEqual(["nvim"])
  expect(editorCommand({ VISUAL: "", EDITOR: "nano" })).toEqual(["nano"])
  expect(editorCommand({ VISUAL: "  ", EDITOR: undefined })).toEqual(["vi"])
  expect(editorCommand({})).toEqual(["vi"])
})

test("an editor command with arguments is split like a shell word list", () => {
  expect(splitCommand("code -w")).toEqual(["code", "-w"])
  expect(splitCommand("  emacsclient   -t  ")).toEqual(["emacsclient", "-t"])
  expect(splitCommand(`"/Applications/My Editor/bin/ed" --wait`)).toEqual(["/Applications/My Editor/bin/ed", "--wait"])
  expect(splitCommand(`vim -c 'set tw=72'`)).toEqual(["vim", "-c", "set tw=72"])
  expect(splitCommand(String.raw`my\ editor -x`)).toEqual(["my editor", "-x"])
})

test("editText suspends the renderer, runs the editor on a temp file with the text, and returns the edited text", async () => {
  const calls: string[] = []
  const editor = script(`cat "$1" > "$(dirname "$0")/seen"; printf 'edited\\nlines\\n' > "$1"`)
  const result = await editText("draft text", {
    env: { EDITOR: editor },
    suspend: () => calls.push("suspend"),
    resume: () => calls.push("resume"),
  })
  expect(result).toEqual({ ok: true, text: "edited\nlines" })
  expect(calls).toEqual(["suspend", "resume"])
  expect(await Bun.file(join(editor, "..", "seen")).text()).toBe("draft text")
})

test("the editor gets its arguments before the file path", async () => {
  const editor = script(`[ "$1" = "-w" ] && printf 'with %s' "$1" > "$2"`)
  const result = await editText("x", { env: { VISUAL: `${editor} -w` }, suspend() {}, resume() {} })
  expect(result).toEqual({ ok: true, text: "with -w" })
})

test("one trailing newline the editor adds is dropped; the input's own is kept", async () => {
  const touch = script(`printf '%s\\n' "$(cat "$1")" > "$1"`)
  expect(await editText("one", { env: { EDITOR: touch }, suspend() {}, resume() {} })).toEqual({ ok: true, text: "one" })
  const keep = script(`true`)
  expect(await editText("two\n", { env: { EDITOR: keep }, suspend() {}, resume() {} })).toEqual({ ok: true, text: "two\n" })
})

test("a failing editor or a missing binary is an error naming the program; the renderer is resumed and the temp file removed", async () => {
  const calls: string[] = []
  let file = ""
  const failing = script(`echo "$1" > "$(dirname "$0")/path"; exit 3`)
  const failed = await editText("keep me", { env: { EDITOR: failing }, suspend: () => calls.push("suspend"), resume: () => calls.push("resume") })
  expect(failed).toEqual({ ok: false, error: "Editor edit.sh exited with status 3" })
  expect(calls).toEqual(["suspend", "resume"])
  file = (await Bun.file(join(failing, "..", "path")).text()).trim()
  expect(existsSync(file)).toBe(false)

  const missing = await editText("keep me", { env: { EDITOR: "/no/such/editor-hya" }, suspend() {}, resume() {} })
  expect(missing).toEqual({ ok: false, error: "Editor editor-hya not found" })
})
