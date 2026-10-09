import { expect, test } from "bun:test"
import { encodeKey, encodeMouse } from "./input"

const modes = { applicationCursor: false, shiftEnterLf: false }

test("keys follow cursor mode and xterm modifiers", () => {
  expect(encodeKey("ArrowUp", modes)).toBe("\x1b[A")
  expect(encodeKey("ArrowUp", { ...modes, applicationCursor: true })).toBe("\x1bOA")
  expect(encodeKey("Alt+ArrowRight", modes)).toBe("\x1b[1;3C")
  expect(encodeKey("F2", modes)).toBe("\x1bOQ")
  expect(encodeKey("F6", modes)).toBe("\x1b[17~")
  expect(encodeKey("Shift+F12", modes)).toBe("\x1b[24;2~")
  expect(encodeKey("Control+C", modes)).toBe("\x03")
  expect(encodeKey("Shift+Tab", modes)).toBe("\x1b[Z")
  expect(encodeKey("Shift+Enter", modes)).toBe("\r")
  expect(encodeKey("Shift+Enter", { ...modes, shiftEnterLf: true })).toBe("\n")
  expect(() => encodeKey("Control+Shift+C", modes)).toThrow("Unsupported legacy")
  expect(() => encodeKey("Meta+Q", modes)).toThrow("Unsupported modifier")
})

test("mouse coordinates are one-based on the wire and preserve release, drag, wheel and modifiers", () => {
  expect(encodeMouse("down", { col: 4, row: 2 }, true)).toBe("\x1b[<0;5;3M")
  expect(encodeMouse("up", { col: 4, row: 2 }, true)).toBe("\x1b[<0;5;3m")
  expect(encodeMouse("move", { col: 4, row: 2 }, true, { alt: true })).toBe("\x1b[<40;5;3M")
  expect(encodeMouse("wheel-down", { col: 4, row: 2 }, true)).toBe("\x1b[<65;5;3M")
  expect(encodeMouse("up", { col: 0, row: 0 }, false)).toBe("\x1b[M#!!")
  expect(() => encodeMouse("down", { col: -1, row: 2 }, true)).toThrow("nonnegative")
})
