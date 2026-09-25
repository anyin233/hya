import { expect, test } from "bun:test"
import { escapeAction } from "../src/composer/escape"
import { InputHistory } from "../src/composer/history"
import { findPattern, insertMention, mentionAt, rankPaths } from "../src/composer/mention"
import { createQuitGuard } from "../src/composer/quit"
import { isShellInput, shellCommand } from "../src/composer/shell"

test("history walks older entries from the draft and back down to it", () => {
  const history = new InputHistory()
  history.push("first")
  history.push("second\nline")
  expect(history.navigating).toBe(false)
  expect(history.previous("draft")).toBe("second\nline")
  expect(history.navigating).toBe(true)
  expect(history.previous("ignored")).toBe("first")
  // Nothing older: stay on the oldest entry.
  expect(history.previous("ignored")).toBeUndefined()
  expect(history.next()).toBe("second\nline")
  // Leaving the newest entry restores the draft and ends navigation.
  expect(history.next()).toBe("draft")
  expect(history.navigating).toBe(false)
  expect(history.next()).toBeUndefined()
})

test("history skips empty and repeated entries, caps its size, and resets on push", () => {
  const history = new InputHistory(3)
  for (const text of ["a", "", "  ", "a", "b", "c", "d"]) history.push(text)
  expect(history.entries).toEqual(["b", "c", "d"])
  expect(history.previous("")).toBe("d")
  history.push("e")
  expect(history.navigating).toBe(false)
  expect(history.previous("")).toBe("e")
  expect(new InputHistory().previous("x")).toBeUndefined()
})

test("Ctrl+C clears or hints first, and quits on a second press within the window", () => {
  let now = 1_000
  const guard = createQuitGuard({ windowMs: 2_000, now: () => now })
  expect(guard.press(false)).toBe("clear")
  now += 500
  expect(guard.press(true)).toBe("quit")

  const idle = createQuitGuard({ windowMs: 2_000, now: () => now })
  expect(idle.press(true)).toBe("hint")
  now += 2_500
  // Too late: the window expired, so this press arms again.
  expect(idle.press(true)).toBe("hint")
  now += 100
  expect(idle.press(true)).toBe("quit")
  expect(idle.armed()).toBe(false)
})

test("the quit guard disarms on demand", () => {
  let now = 0
  const guard = createQuitGuard({ windowMs: 2_000, now: () => now })
  guard.press(true)
  expect(guard.armed()).toBe(true)
  guard.disarm()
  now += 10
  expect(guard.press(true)).toBe("hint")
})

test("! input is a shell command", () => {
  expect(shellCommand("!echo hello")).toBe("echo hello")
  expect(shellCommand("  ! ls -la  ")).toBe("ls -la")
  expect(shellCommand("!")).toBe("")
  expect(shellCommand("echo !x")).toBeUndefined()
  expect(shellCommand("/help")).toBeUndefined()
  expect(isShellInput("!ls")).toBe(true)
  expect(isShellInput("  !")).toBe(true)
  expect(isShellInput("ls")).toBe(false)
})

test("Esc closes the file menu, then cancels a running turn, then clears the input", () => {
  expect(escapeAction({ menuOpen: true, running: true, inputEmpty: false })).toBe("closeMenu")
  expect(escapeAction({ menuOpen: false, running: true, inputEmpty: false })).toBe("cancelTurn")
  expect(escapeAction({ menuOpen: false, running: true, inputEmpty: true })).toBe("cancelTurn")
  expect(escapeAction({ menuOpen: false, running: false, inputEmpty: false })).toBe("clearInput")
  expect(escapeAction({ menuOpen: false, running: false, inputEmpty: true })).toBe("none")
})

test("an @ token at the cursor is a file mention", () => {
  expect(mentionAt("look at @src/ma", 15)).toEqual({ start: 8, end: 15, query: "src/ma" })
  // The token runs past the cursor up to the next white space.
  expect(mentionAt("see @main.ts now", 7)).toEqual({ start: 4, end: 12, query: "ma" })
  expect(mentionAt("@r", 2)).toEqual({ start: 0, end: 2, query: "r" })
  expect(mentionAt("line one\n@x", 11)).toEqual({ start: 9, end: 11, query: "x" })
  // A bare @, an e-mail address, a token before the cursor, and slash commands do not count.
  expect(mentionAt("hi @", 4)).toBeUndefined()
  expect(mentionAt("mail me@host", 12)).toBeUndefined()
  expect(mentionAt("@abc def", 8)).toBeUndefined()
  expect(mentionAt("/api GET @x", 11)).toBeUndefined()
})

test("inserting a mention keeps the @ and adds one trailing space", () => {
  const token = { start: 8, end: 15, query: "src/ma" }
  expect(insertMention("look at @src/ma", token, "src/main.ts")).toEqual({ text: "look at @src/main.ts ", cursor: 21 })
  expect(insertMention("see @ma now", { start: 4, end: 7, query: "ma" }, "main.ts")).toEqual({ text: "see @main.ts now", cursor: 12 })
})

test("file queries become FindFiles globs and results rank by name", () => {
  expect(findPattern("main")).toBe("**/*main*")
  expect(findPattern("src/ma")).toBe("**/*src/ma*")
  expect(rankPaths(["lib/domain.ts", "src/main.ts", "main.ts", "docs/x/remain.md", "Main.md"], "main"))
    .toEqual(["Main.md", "main.ts", "src/main.ts", "lib/domain.ts", "docs/x/remain.md"])
  expect(rankPaths(["a1", "a2", "a3"], "a", 2)).toEqual(["a1", "a2"])
})
