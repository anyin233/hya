/**
 * External editor for the composer (docs/tui.md "External editor"): `/editor`
 * or Ctrl+X Ctrl+E writes the input to a temporary file, suspends the
 * renderer (the editor gets the terminal), runs `$VISUAL`, else `$EDITOR`,
 * else `vi` on it, resumes, and returns the edited text. The caller puts it
 * back in the input (it is not sent). A non-zero exit or a missing binary is
 * an error and the caller keeps the original text.
 *
 * The editor value is split like a shell word list (`code -w`,
 * `"/path with spaces/ed" --wait`, `vim -c 'set tw=72'`) but not run
 * through a shell; the file path is the last argument.
 */
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { basename, join } from "node:path"

/** Split `value` into words: blanks separate, '…' and "…" quote, a backslash escapes the next character. */
export function splitCommand(value: string): string[] {
  const words: string[] = []
  let word = ""
  let started = false
  let quote: "'" | '"' | undefined
  for (let i = 0; i < value.length; i++) {
    const char = value[i]!
    if (quote) {
      if (char === quote) quote = undefined
      else if (char === "\\" && quote === '"' && i + 1 < value.length) word += value[++i]
      else word += char
      continue
    }
    if (char === "'" || char === '"') {
      quote = char
      started = true
    } else if (char === "\\" && i + 1 < value.length) {
      word += value[++i]
      started = true
    } else if (/\s/.test(char)) {
      if (started) words.push(word)
      word = ""
      started = false
    } else {
      word += char
      started = true
    }
  }
  if (started) words.push(word)
  return words
}

/** The editor argv: `$VISUAL`, else `$EDITOR`, else `vi` (blank values are skipped). */
export function editorCommand(env: Record<string, string | undefined>): string[] {
  for (const name of ["VISUAL", "EDITOR"]) {
    const words = splitCommand(env[name] ?? "")
    if (words.length) return words
  }
  return ["vi"]
}

export interface EditTextOptions {
  env: Record<string, string | undefined>
  /** Hand the terminal to the editor (CliRenderer.suspend). */
  suspend(): void
  /** Take it back (CliRenderer.resume). */
  resume(): void
  /** Run the editor with the terminal attached; resolves to its exit status. Default: Bun.spawn with inherited stdio. */
  spawn?(argv: string[]): Promise<number>
}

export type EditResult = { ok: true; text: string } | { ok: false; error: string }

async function spawnInherited(argv: string[]): Promise<number> {
  const child = Bun.spawn(argv, { stdin: "inherit", stdout: "inherit", stderr: "inherit" })
  return child.exited
}

/** Edit `text` in the external editor; never throws. */
export async function editText(text: string, options: EditTextOptions): Promise<EditResult> {
  const argv = editorCommand(options.env)
  // Messages name the program, not its full path (the status line is one row).
  const name = basename(argv[0]!)
  const dir = mkdtempSync(join(tmpdir(), "hya-prompt-"))
  const file = join(dir, "prompt.md")
  try {
    writeFileSync(file, text)
    let status: number
    options.suspend()
    try {
      status = await (options.spawn ?? spawnInherited)([...argv, file])
    } catch (error) {
      const code = (error as NodeJS.ErrnoException).code
      return { ok: false, error: code === "ENOENT" || /ENOENT|no such file/i.test(String(error)) ? `Editor ${name} not found` : `Editor ${name} failed: ${error instanceof Error ? error.message : String(error)}` }
    } finally {
      options.resume()
    }
    if (status !== 0) return { ok: false, error: `Editor ${name} exited with status ${status}` }
    let edited = readFileSync(file, "utf8")
    // Most editors end the file with a newline; drop it unless the input had one.
    if (!text.endsWith("\n") && edited.endsWith("\n")) edited = edited.slice(0, edited.endsWith("\r\n") ? -2 : -1)
    return { ok: true, text: edited }
  } catch (error) {
    return { ok: false, error: `Editor failed: ${error instanceof Error ? error.message : String(error)}` }
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}
