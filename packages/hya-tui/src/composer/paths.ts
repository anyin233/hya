/**
 * Paths in a paste (docs/tui.md "Paste and attachments"): a terminal pastes
 * files dragged into it as their paths — bare, quoted, with backslash-escaped
 * spaces, as `file://` URLs, one or several, separated by spaces or line
 * breaks. A paste is a path list only when every word is an explicit path
 * (`/`, `~/`, `./`, `../`, or `file://`), so prose that mentions a path stays
 * text. One exception keeps an unescaped macOS screenshot name: the whole
 * paste is one line that starts with `/` and ends in an image extension.
 */
import { isImagePath } from "./attachments"

export interface PastedPath {
  /** The word as pasted (quotes and escapes included). */
  raw: string
  /** The path on this machine: unquoted, unescaped, `~` and `file://` resolved. */
  path: string
}

const explicit = /^(?:\/|~\/|~$|\.\.?\/|file:\/\/)/

/** Split `text` into shell-like words: blanks separate, '…' and "…" quote, a backslash escapes. `undefined` for an unterminated quote. */
export function shellWords(text: string): { raw: string; word: string }[] | undefined {
  const words: { raw: string; word: string }[] = []
  let word = "", raw = "", started = false
  let quote: "'" | '"' | undefined
  for (let i = 0; i < text.length; i++) {
    const char = text[i]!
    if (quote) {
      raw += char
      if (char === quote) quote = undefined
      else if (char === "\\" && quote === '"' && i + 1 < text.length && /["\\$`]/.test(text[i + 1]!)) { word += text[++i]; raw += text[i] }
      else word += char
      continue
    }
    if (char === "'" || char === '"') {
      quote = char
      raw += char
      started = true
    } else if (char === "\\" && i + 1 < text.length) {
      raw += char + text[i + 1]
      word += text[++i]
      started = true
    } else if (/\s/.test(char)) {
      if (started) words.push({ raw, word })
      word = ""
      raw = ""
      started = false
    } else {
      word += char
      raw += char
      started = true
    }
  }
  if (quote) return undefined
  if (started) words.push({ raw, word })
  return words
}

/** `~` → `home`; `file:///a%20b` → `/a b`. */
export function resolvePastedPath(word: string, home: string | undefined): string {
  if (word.startsWith("file://")) {
    const rest = word.slice("file://".length).replace(/^localhost(?=\/)/, "")
    try { return decodeURIComponent(rest) } catch { return rest }
  }
  if (home && (word === "~" || word.startsWith("~/"))) return home.replace(/\/+$/, "") + word.slice(1)
  return word
}

/** The paths of a paste that is only paths, or `undefined` for anything else. */
export function pastedPaths(text: string, home: string | undefined = process.env.HOME): PastedPath[] | undefined {
  const trimmed = text.trim()
  if (!trimmed) return undefined
  const words = shellWords(trimmed)
  if (words?.length && words.every((item) => explicit.test(item.word))) {
    return words.map((item) => ({ raw: item.raw, path: resolvePastedPath(item.word, home) }))
  }
  // An unescaped macOS screenshot name: one line, one absolute image path.
  if (!/\n/.test(trimmed) && trimmed.startsWith("/") && isImagePath(trimmed)) return [{ raw: trimmed, path: trimmed }]
  return undefined
}

/** Whether `path` is `root` or below it. */
export function inside(path: string, root: string): boolean {
  const base = root.replace(/\/+$/, "")
  return path === base || path.startsWith(`${base}/`)
}

/** The image types the server accepts, by file header. `undefined` for anything else. */
export function sniffImage(bytes: Uint8Array): string | undefined {
  const starts = (...values: number[]) => values.every((value, index) => bytes[index] === value)
  if (starts(0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a)) return "image/png"
  if (starts(0xff, 0xd8, 0xff)) return "image/jpeg"
  if (starts(0x47, 0x49, 0x46, 0x38)) return "image/gif"
  if (starts(0x52, 0x49, 0x46, 0x46) && bytes[8] === 0x57 && bytes[9] === 0x45 && bytes[10] === 0x42 && bytes[11] === 0x50) return "image/webp"
  return undefined
}

/** Whether `bytes` looks like text: no NUL in the first 8 KiB and valid UTF-8. */
export function looksLikeText(bytes: Uint8Array): boolean {
  if (bytes.subarray(0, 8192).includes(0)) return false
  try {
    new TextDecoder("utf-8", { fatal: true }).decode(bytes)
    return true
  } catch {
    return false
  }
}
