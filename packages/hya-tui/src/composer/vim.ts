/**
 * Vim mode for the composer (docs/tui.md "Vim mode"): a pure state machine
 * over the input's text and cursor. The composer (components/Composer.tsx)
 * feeds it each key while vim mode is on and applies the result to the
 * OpenTUI textarea: an `edit` replaces the text through the textarea's undo
 * history (one undo step per edit), a `cursor` moves the cursor, and a
 * `command` asks the editor to undo, redo, or submit.
 *
 * Insert mode passes every key on except Esc, which switches to normal mode.
 * Normal mode handles the keys below and swallows other printable keys;
 * Ctrl/Alt keys (except Ctrl+R), arrows, Tab, PgUp/PgDn, and Esc with nothing
 * pending pass on, so the composer's bindings keep working.
 *
 * - Motions (with a count): h l j k (Backspace = h), w b e, 0 ^ $, gg G
 *   (a count picks the line).
 * - Insert: i a I A o O. Edits: x (Delete = x), dd, D, d{motion}, cc, C,
 *   c{motion} (cw = ce), s, S, yy, y{motion}, p, P (one internal register).
 * - u undo, Ctrl+R redo (the textarea's history), Enter submits.
 *
 * Offsets are string indexes (UTF-16 code units), like the composer's.
 */
import type { KeyLike } from "../keys/bindings"

export type VimMode = "insert" | "normal"

export interface VimRegister {
  text: string
  /** Whole lines (dd, yy, cc): p/P put them below/above the cursor's line. */
  linewise: boolean
}

export interface VimState {
  readonly mode: VimMode
  /** Digits typed before a command (`3` of `3w`). */
  readonly count: string
  /** The operator waiting for its motion (`d` of `dw`). */
  readonly operator?: "d" | "c" | "y"
  /** The count typed before the operator (`2` of `2dw`). */
  readonly operatorCount: string
  /** `g` of `gg`. */
  readonly prefix?: "g"
  /** Column j/k aim for (kept across a run of j/k). */
  readonly want?: number
  readonly register?: VimRegister
  /** What is typed but not finished (`2d`, `g`), for the status bar; "" when nothing. */
  readonly pending: string
}

export interface VimBuffer {
  text: string
  cursor: number
}

export type VimCommand = "undo" | "redo" | "submit"

export type VimResult =
  /** Not handled: the key goes on to the composer's usual handling. */
  | { type: "pass" }
  | { type: "handled"; state: VimState; edit?: VimBuffer; cursor?: number; command?: VimCommand }

export function initialVimState(): VimState {
  return { mode: "insert", count: "", operatorCount: "", pending: "" }
}

// ---- text geometry ----

function lineStart(text: string, pos: number): number {
  return pos <= 0 ? 0 : text.lastIndexOf("\n", pos - 1) + 1
}

function lineEnd(text: string, pos: number): number {
  const end = text.indexOf("\n", pos)
  return end < 0 ? text.length : end
}

/** The last character of the line (normal mode never rests on the line break). */
function lastChar(text: string, pos: number): number {
  return Math.max(lineStart(text, pos), lineEnd(text, pos) - 1)
}

function clampNormal(text: string, pos: number): number {
  const bounded = Math.max(0, Math.min(pos, text.length))
  return Math.min(bounded, lastChar(text, bounded))
}

function firstNonBlank(text: string, pos: number): number {
  let i = lineStart(text, pos)
  const end = lineEnd(text, pos)
  while (i < end && (text[i] === " " || text[i] === "\t")) i++
  return clampNormal(text, i)
}

function lineIndex(text: string, pos: number): number {
  let lines = 0
  for (let i = text.indexOf("\n"); i >= 0 && i < pos; i = text.indexOf("\n", i + 1)) lines++
  return lines
}

function lineCount(text: string): number {
  return lineIndex(text, text.length) + 1
}

/** Offset of the start of line `line` (0-based, clamped). */
function lineOffset(text: string, line: number): number {
  let pos = 0
  for (let i = 0; i < line; i++) {
    const next = text.indexOf("\n", pos)
    if (next < 0) break
    pos = next + 1
  }
  return pos
}

/** 0 blank, 1 word character, 2 punctuation. */
function charClass(char: string | undefined): number {
  if (char === undefined || /\s/.test(char)) return 0
  return /\w/.test(char) ? 1 : 2
}

function wordForward(text: string, pos: number): number {
  let i = pos
  const kind = charClass(text[i])
  if (kind > 0) while (i < text.length && charClass(text[i]) === kind) i++
  while (i < text.length && charClass(text[i]) === 0) i++
  return i
}

function wordEnd(text: string, pos: number): number {
  let i = pos + 1
  while (i < text.length && charClass(text[i]) === 0) i++
  if (i >= text.length) return Math.max(0, text.length - 1)
  const kind = charClass(text[i])
  while (i + 1 < text.length && charClass(text[i + 1]) === kind) i++
  return i
}

function wordBackward(text: string, pos: number): number {
  let i = pos - 1
  while (i > 0 && charClass(text[i]) === 0) i--
  if (i <= 0) return 0
  const kind = charClass(text[i])
  while (i > 0 && charClass(text[i - 1]) === kind) i--
  return i
}

function repeat(times: number, from: number, step: (pos: number) => number): number {
  let pos = from
  for (let i = 0; i < times; i++) pos = step(pos)
  return pos
}

/** Move `count` lines down (negative: up), aiming for column `want`. */
function verticalTarget(text: string, pos: number, count: number, want: number): number {
  const target = Math.max(0, Math.min(lineCount(text) - 1, lineIndex(text, pos) + count))
  const start = lineOffset(text, target)
  return Math.min(start + want, lastChar(text, start))
}

// ---- motions ----

type MotionKey = "h" | "l" | "w" | "b" | "e" | "0" | "^" | "$" | "j" | "k"

const motionKeys = new Set<string>(["h", "l", "w", "b", "e", "0", "^", "$", "j", "k"])

/** Where a motion lands; `operator` lets `l` reach past the last character (dl on it deletes it). */
function motionTarget(text: string, pos: number, key: MotionKey, count: number, operator: boolean): number {
  switch (key) {
    case "h": return Math.max(lineStart(text, pos), pos - count)
    case "l": return Math.min(operator ? lineEnd(text, pos) : lastChar(text, pos), pos + count)
    case "w": return repeat(count, pos, (at) => wordForward(text, at))
    case "b": return repeat(count, pos, (at) => wordBackward(text, at))
    case "e": return repeat(count, pos, (at) => wordEnd(text, at))
    case "0": return lineStart(text, pos)
    case "^": return firstNonBlank(text, pos)
    case "$": return lastChar(text, lineOffset(text, lineIndex(text, pos) + count - 1))
    case "j": return verticalTarget(text, pos, count, pos - lineStart(text, pos))
    case "k": return verticalTarget(text, pos, -count, pos - lineStart(text, pos))
  }
}

// ---- results ----

function clearPending(state: VimState): VimState {
  const { operator: _operator, prefix: _prefix, want: _want, ...rest } = state
  return { ...rest, count: "", operatorCount: "", pending: "" }
}

function done(state: VimState, extra: { edit?: VimBuffer; cursor?: number; command?: VimCommand; mode?: VimMode; register?: VimRegister; want?: number } = {}): VimResult {
  let next = clearPending(state)
  if (extra.mode) next = { ...next, mode: extra.mode }
  if (extra.register) next = { ...next, register: extra.register }
  if (extra.want !== undefined) next = { ...next, want: extra.want }
  const result: VimResult = { type: "handled", state: next }
  if (extra.edit) result.edit = extra.edit
  if (extra.cursor !== undefined) result.cursor = extra.cursor
  if (extra.command) result.command = extra.command
  return result
}

function waiting(state: VimState): VimResult {
  const pending = `${state.operatorCount}${state.operator ?? ""}${state.count}${state.prefix ?? ""}`
  return { type: "handled", state: { ...state, pending } }
}

/** Replace `[from, to)` with `insert`; the cursor goes to `cursor` (normal mode clamps it onto the line). */
function splice(text: string, from: number, to: number, insert: string, cursor: number, mode: VimMode): VimBuffer {
  const next = text.slice(0, from) + insert + text.slice(to)
  return { text: next, cursor: mode === "normal" ? clampNormal(next, cursor) : Math.max(0, Math.min(cursor, next.length)) }
}

/** Lines `first..last` (0-based, inclusive) as a range that removes them with one line break. */
function lineRange(text: string, first: number, last: number): { from: number; to: number; content: string } {
  const start = lineOffset(text, first)
  const end = lineEnd(text, lineOffset(text, last))
  const content = text.slice(start, end)
  if (end < text.length) return { from: start, to: end + 1, content }
  if (start > 0) return { from: start - 1, to: end, content }
  return { from: 0, to: end, content }
}

function linewise(state: VimState, buffer: VimBuffer, operator: "d" | "c" | "y", count: number, lastLine?: number): VimResult {
  const { text, cursor } = buffer
  const here = lineIndex(text, cursor)
  const other = lastLine ?? Math.min(lineCount(text) - 1, here + count - 1)
  const first = Math.min(here, other)
  const last = Math.max(here, other)
  const range = lineRange(text, first, last)
  const register: VimRegister = { text: range.content, linewise: true }
  // A yank leaves the cursor where it is, or moves it up to the first line of an upward range (yk).
  if (operator === "y") return done(state, { register, cursor: first === here ? cursor : verticalTarget(text, cursor, first - here, cursor - lineStart(text, cursor)) })
  if (operator === "c") {
    const start = lineOffset(text, first)
    const end = lineEnd(text, lineOffset(text, last))
    return done(state, { register, mode: "insert", edit: splice(text, start, end, "", start, "insert") })
  }
  const next = text.slice(0, range.from) + text.slice(range.to)
  const line = Math.min(first, lineCount(next) - 1)
  return done(state, { register, edit: { text: next, cursor: firstNonBlank(next, lineOffset(next, line)) } })
}

function applyOperator(state: VimState, buffer: VimBuffer, operator: "d" | "c" | "y", key: MotionKey, count: number): VimResult {
  const { text, cursor } = buffer
  if (key === "j" || key === "k") {
    const target = lineIndex(text, motionTarget(text, cursor, key, count, true))
    return linewise(state, buffer, operator, count, target)
  }
  // cw on a word changes to its end (like ce); on blanks it acts like cl.
  const motion: MotionKey = operator === "c" && key === "w" && charClass(text[cursor]) !== 0 ? "e" : key
  const target = motionTarget(text, cursor, motion, count, true)
  let from = Math.min(cursor, target)
  let to = Math.max(cursor, target)
  if (motion === "e") to = Math.min(to + 1, text.length)
  if (motion === "$") to = lineEnd(text, target)
  if (motion === "w") {
    // dw on the last word of a line stops at the line end.
    const lastBreak = text.lastIndexOf("\n", to - 1)
    if (lastBreak >= cursor && text.slice(lastBreak, to).trim() === "") to = lastBreak
  }
  if (from === to) return done(state)
  const removed = text.slice(from, to)
  const register: VimRegister = { text: removed, linewise: false }
  if (operator === "y") return done(state, { register, cursor: clampNormal(text, from) })
  if (operator === "c") return done(state, { register, mode: "insert", edit: splice(text, from, to, "", from, "insert") })
  from = Math.max(0, from)
  return done(state, { register, edit: splice(text, from, to, "", from, "normal") })
}

function put(state: VimState, buffer: VimBuffer, before: boolean, count: number): VimResult {
  const register = state.register
  if (!register || !register.text && !register.linewise) return done(state)
  const { text, cursor } = buffer
  if (register.linewise) {
    const block = Array.from({ length: count }, () => register.text).join("\n")
    if (before) {
      const at = lineStart(text, cursor)
      const next = text.slice(0, at) + block + "\n" + text.slice(at)
      return done(state, { edit: { text: next, cursor: firstNonBlank(next, at) } })
    }
    const at = lineEnd(text, cursor)
    const next = text.slice(0, at) + "\n" + block + text.slice(at)
    return done(state, { edit: { text: next, cursor: firstNonBlank(next, at + 1) } })
  }
  const block = register.text.repeat(count)
  const at = before || lineEnd(text, cursor) === cursor ? cursor : cursor + 1
  const next = text.slice(0, at) + block + text.slice(at)
  return done(state, { edit: { text: next, cursor: at + block.length - 1 } })
}

const printable = (key: KeyLike): boolean => !key.ctrl && !key.meta && key.sequence.length === 1 && key.sequence >= " " && key.sequence !== "\x7f"

/** One key: the next state and what the composer should do. */
export function vimKey(state: VimState, buffer: VimBuffer, key: KeyLike): VimResult {
  const { text, cursor } = buffer
  const escape = key.name === "escape" && !key.ctrl && !key.meta && !key.shift

  if (state.mode === "insert") {
    if (!escape) return { type: "pass" }
    const back = cursor > lineStart(text, cursor) ? cursor - 1 : cursor
    return done(state, { mode: "normal", cursor: clampNormal(text, back) })
  }

  const busy = state.pending !== "" || state.operator !== undefined || state.prefix !== undefined || state.count !== ""
  if (escape) return busy ? done(state) : { type: "pass" }
  if (key.ctrl && !key.meta && key.name === "r") return done(state, { command: "redo" })
  if (key.name === "return" || key.name === "kpenter") return done(state, { command: "submit" })

  // Keys that stand for a printable command.
  let char = printable(key) ? key.sequence : undefined
  if (!char && key.name === "backspace" && !key.ctrl && !key.meta) char = "h"
  if (!char && key.name === "delete" && !key.ctrl && !key.meta) char = "x"
  if (!char) return { type: "pass" }

  // Counts: 1-9, then any digit (a lone 0 is the line-start motion).
  if (/[0-9]/.test(char) && (char !== "0" || state.count !== "")) {
    return waiting({ ...state, count: `${state.count}${char}`.slice(0, 4) })
  }
  const count = Math.max(1, Number(state.operatorCount || "1") * Number(state.count || "1"))
  const counted = state.operatorCount !== "" || state.count !== ""

  if (state.prefix === "g") {
    if (char === "g" && state.operator === undefined) {
      const line = counted ? count - 1 : 0
      return done(state, { cursor: firstNonBlank(text, lineOffset(text, Math.min(line, lineCount(text) - 1))) })
    }
    return done(state)
  }

  if (state.operator) {
    const operator = state.operator
    if (char === operator) return linewise(state, buffer, operator, count)
    if (motionKeys.has(char)) return applyOperator(state, buffer, operator, char as MotionKey, count)
    return done(state)
  }

  if (motionKeys.has(char)) {
    if (char === "j" || char === "k") {
      const want = state.want ?? cursor - lineStart(text, cursor)
      return done(state, { cursor: verticalTarget(text, cursor, char === "j" ? count : -count, want), want })
    }
    return done(state, { cursor: clampNormal(text, motionTarget(text, cursor, char as MotionKey, count, false)) })
  }

  switch (char) {
    case "g": return waiting({ ...state, prefix: "g" })
    case "G": {
      const line = counted ? count - 1 : lineCount(text) - 1
      return done(state, { cursor: firstNonBlank(text, lineOffset(text, Math.min(line, lineCount(text) - 1))) })
    }
    case "d": case "c": case "y":
      return waiting({ ...state, operator: char, operatorCount: state.count, count: "" })
    case "i": return done(state, { mode: "insert", cursor })
    case "a": return done(state, { mode: "insert", cursor: cursor < lineEnd(text, cursor) ? cursor + 1 : cursor })
    case "I": return done(state, { mode: "insert", cursor: firstNonBlank(text, cursor) })
    case "A": return done(state, { mode: "insert", cursor: lineEnd(text, cursor) })
    case "o": {
      const at = lineEnd(text, cursor)
      return done(state, { mode: "insert", edit: splice(text, at, at, "\n", at + 1, "insert") })
    }
    case "O": {
      const at = lineStart(text, cursor)
      return done(state, { mode: "insert", edit: splice(text, at, at, "\n", at, "insert") })
    }
    case "x": {
      const end = Math.min(lineEnd(text, cursor), cursor + count)
      if (end <= cursor) return done(state)
      return done(state, { register: { text: text.slice(cursor, end), linewise: false }, edit: splice(text, cursor, end, "", cursor, "normal") })
    }
    case "s": return applyOperator(state, buffer, "c", "l", count)
    case "S": return linewise(state, buffer, "c", count)
    case "D": return applyOperator(state, buffer, "d", "$", count)
    case "C": return applyOperator(state, buffer, "c", "$", count)
    case "p": return put(state, buffer, false, count)
    case "P": return put(state, buffer, true, count)
    case "u": return done(state, { command: "undo" })
  }
  // Any other printable key is swallowed: normal mode never types.
  return done(state)
}
