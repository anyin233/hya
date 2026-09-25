import { expect, test } from "bun:test"
import { initialVimState, vimKey, type VimBuffer, type VimState } from "../src/composer/vim"

/** One key the way OpenTUI reports it: `<Esc>`, `<CR>`, `<C-r>`, `<BS>`, `<Del>`, `<Left>`, `<Tab>`, or one printable character. */
function parseKeys(keys: string) {
  const out: { name: string; ctrl: boolean; meta: boolean; shift: boolean; sequence: string }[] = []
  for (let i = 0; i < keys.length;) {
    const special = /^<(Esc|CR|C-[a-z]|BS|Del|Left|Tab)>/.exec(keys.slice(i))
    if (special) {
      const tag = special[1]!
      i += special[0].length
      if (tag === "Esc") out.push({ name: "escape", ctrl: false, meta: false, shift: false, sequence: "\x1b" })
      else if (tag === "CR") out.push({ name: "return", ctrl: false, meta: false, shift: false, sequence: "\r" })
      else if (tag === "BS") out.push({ name: "backspace", ctrl: false, meta: false, shift: false, sequence: "\x7f" })
      else if (tag === "Del") out.push({ name: "delete", ctrl: false, meta: false, shift: false, sequence: "\x1b[3~" })
      else if (tag === "Left") out.push({ name: "left", ctrl: false, meta: false, shift: false, sequence: "\x1b[D" })
      else if (tag === "Tab") out.push({ name: "tab", ctrl: false, meta: false, shift: false, sequence: "\t" })
      else out.push({ name: tag.slice(2), ctrl: true, meta: false, shift: false, sequence: String.fromCharCode(tag.charCodeAt(2) - 96) })
      continue
    }
    const char = keys[i]!
    i++
    const upper = char !== char.toLowerCase()
    out.push({ name: upper ? char.toLowerCase() : char, ctrl: false, meta: false, shift: upper, sequence: char })
  }
  return out
}

/** `text` with `|` marking the cursor. */
function buffer(marked: string): VimBuffer {
  const cursor = marked.indexOf("|")
  return { text: marked.replace("|", ""), cursor }
}

function mark(value: VimBuffer): string {
  return `${value.text.slice(0, value.cursor)}|${value.text.slice(value.cursor)}`
}

interface Run {
  text: string
  state: VimState
  commands: string[]
  passed: string[]
}

/** Feed `keys` to the state machine, applying its edits and cursor moves like the composer adapter does. */
function run(marked: string, keys: string, start: VimState = { ...initialVimState(), mode: "normal" }): Run {
  let state = start
  let current = buffer(marked)
  const commands: string[] = []
  const passed: string[] = []
  for (const key of parseKeys(keys)) {
    const result = vimKey(state, current, key)
    if (result.type === "pass") {
      passed.push(key.name)
      continue
    }
    state = result.state
    if (result.edit) current = result.edit
    else if (result.cursor !== undefined) current = { ...current, cursor: result.cursor }
    if (result.command) commands.push(result.command)
  }
  return { text: mark(current), state, commands, passed }
}

const insert = (): VimState => initialVimState()

test("the machine starts in insert mode and passes every key but Esc to the editor", () => {
  expect(initialVimState().mode).toBe("insert")
  const typed = run("ab|", "xyz<CR><BS><C-r>", insert())
  expect(typed.passed).toEqual(["x", "y", "z", "return", "backspace", "r"])
  expect(typed.state.mode).toBe("insert")
})

test("Esc in insert mode switches to normal and steps the cursor back onto the last character", () => {
  expect(run("abc|", "<Esc>", insert())).toMatchObject({ text: "ab|c", state: { mode: "normal" } })
  expect(run("abc\n|def", "<Esc>", insert()).text).toBe("abc\n|def")
  expect(run("|", "<Esc>", insert()).text).toBe("|")
})

test("Esc in normal mode with nothing pending passes on (the composer's usual Esc)", () => {
  const result = run("a|bc", "<Esc>")
  expect(result.passed).toEqual(["escape"])
  expect(result.state.mode).toBe("normal")
})

test("h and l move within the line and stop at its ends", () => {
  expect(run("ab|c", "h").text).toBe("a|bc")
  expect(run("ab|c", "hhhh").text).toBe("|abc")
  expect(run("|abc", "lllll").text).toBe("ab|c")
  expect(run("ab\n|cd", "h").text).toBe("ab\n|cd")
  expect(run("a|b\ncd", "ll").text).toBe("a|b\ncd")
  expect(run("abc|", "<BS>").text).toBe("ab|c")
})

test("j and k keep the column, clamped to the target line", () => {
  expect(run("ab|cd\nxy\nlonger", "j").text).toBe("abcd\nx|y\nlonger")
  expect(run("ab|cd\nxy\nlonger", "jj").text).toBe("abcd\nxy\nlo|nger")
  expect(run("abcd\nxy\nlon|ger", "k").text).toBe("abcd\nx|y\nlonger")
  expect(run("a|b", "j").text).toBe("a|b")
  expect(run("a\n\n|c", "k").text).toBe("a\n|\nc")
})

test("w, b, and e move by words (letters/digits/_ vs punctuation vs blanks)", () => {
  expect(run("|foo bar.baz", "w").text).toBe("foo |bar.baz")
  expect(run("|foo bar.baz", "ww").text).toBe("foo bar|.baz")
  expect(run("|foo bar.baz", "www").text).toBe("foo bar.|baz")
  expect(run("|foo bar", "wwww").text).toBe("foo ba|r")
  expect(run("foo bar.ba|z", "b").text).toBe("foo bar.|baz")
  expect(run("foo bar.ba|z", "bbb").text).toBe("foo |bar.baz")
  expect(run("|foo bar", "e").text).toBe("fo|o bar")
  expect(run("|foo bar", "ee").text).toBe("foo ba|r")
  expect(run("one\n|two", "b").text).toBe("|one\ntwo")
  expect(run("on|e\ntwo", "w").text).toBe("one\n|two")
})

test("0, ^, and $ go to the line start, first non-blank, and last character", () => {
  expect(run("x\n  ab|cd", "0").text).toBe("x\n|  abcd")
  expect(run("x\n  ab|cd", "^").text).toBe("x\n  |abcd")
  expect(run("x\n  |abcd\ny", "$").text).toBe("x\n  abc|d\ny")
})

test("gg and G go to the first / last line (a count picks the line)", () => {
  expect(run("one\n two\nthr|ee", "gg").text).toBe("|one\n two\nthree")
  expect(run("o|ne\n two\nthree", "G").text).toBe("one\n two\n|three")
  expect(run("o|ne\n two\nthree", "2G").text).toBe("one\n |two\nthree")
  expect(run("one\n two\nthr|ee", "2gg").text).toBe("one\n |two\nthree")
})

test("counts repeat motions", () => {
  expect(run("|a b c d e", "3w").text).toBe("a b c |d e")
  expect(run("|abcdef", "4l").text).toBe("abcd|ef")
  expect(run("|1\n2\n3\n4", "2j").text).toBe("1\n2\n|3\n4")
  expect(run("|abcdefghijkl", "10l").text).toBe("abcdefghij|kl")
})

test("i, a, I, A enter insert mode at the cursor, after it, at the first non-blank, at the line end", () => {
  expect(run("ab|c", "i")).toMatchObject({ text: "ab|c", state: { mode: "insert" } })
  expect(run("ab|c", "a")).toMatchObject({ text: "abc|", state: { mode: "insert" } })
  expect(run("|", "a").text).toBe("|")
  expect(run("  ab|c\nx", "I").text).toBe("  |abc\nx")
  expect(run("a|bc\nx", "A").text).toBe("abc|\nx")
})

test("o and O open a line below / above and enter insert mode", () => {
  expect(run("a|b\ncd", "o")).toMatchObject({ text: "ab\n|\ncd", state: { mode: "insert" } })
  expect(run("ab\nc|d", "O")).toMatchObject({ text: "ab\n|\ncd", state: { mode: "insert" } })
  expect(run("a|b", "O").text).toBe("|\nab")
})

test("x deletes the character under the cursor (with a count), and the cursor stays on the line", () => {
  expect(run("a|bc", "x").text).toBe("a|c")
  expect(run("ab|c", "x").text).toBe("a|b")
  expect(run("a|bcdef", "3x").text).toBe("a|ef")
  expect(run("a|bc\nd", "9x").text).toBe("|a\nd")
  expect(run("ab\n|\ncd", "x").text).toBe("ab\n|\ncd")
  expect(run("a|bc", "<Del>").text).toBe("a|c")
})

test("dd deletes whole lines (count), and the deleted lines go to the register", () => {
  expect(run("one\nt|wo\nthree", "dd").text).toBe("one\n|three")
  expect(run("one\ntwo\nthr|ee", "dd").text).toBe("one\n|two")
  expect(run("on|ly", "dd").text).toBe("|")
  expect(run("o|ne\ntwo\nthree", "2dd").text).toBe("|three")
  const cut = run("one\nt|wo\nthree", "ddp")
  expect(cut.text).toBe("one\nthree\n|two")
  expect(run("one\nt|wo\nthree", "ddkP").text).toBe("|two\none\nthree")
})

test("D and d$ delete to the end of the line; C also enters insert mode", () => {
  expect(run("ab|cd\nx", "D").text).toBe("a|b\nx")
  expect(run("ab|cd\nx", "d$").text).toBe("a|b\nx")
  expect(run("ab|cd\nx", "C")).toMatchObject({ text: "ab|\nx", state: { mode: "insert" } })
})

test("dw, de, db delete by word; dw stops at the end of the line", () => {
  expect(run("|foo bar baz", "dw").text).toBe("|bar baz")
  expect(run("|foo bar baz", "2dw").text).toBe("|baz")
  expect(run("|foo bar baz", "d2w").text).toBe("|baz")
  expect(run("|foo bar", "de").text).toBe("| bar")
  expect(run("foo b|ar", "db").text).toBe("foo |ar")
  expect(run("foo |bar\nnext", "dw").text).toBe("foo| \nnext")
  expect(run("a|bcd", "d0").text).toBe("|bcd")
  expect(run("  abc|d", "d^").text).toBe("  |d")
  expect(run("ab|cd", "dl").text).toBe("ab|d")
  expect(run("ab|cd", "dh").text).toBe("a|cd")
})

test("cw changes to the end of the word (like ce), cc clears the line, both enter insert mode", () => {
  expect(run("|foo bar", "cw")).toMatchObject({ text: "| bar", state: { mode: "insert" } })
  expect(run("x\n  f|oo bar\ny", "cc")).toMatchObject({ text: "x\n|\ny", state: { mode: "insert" } })
  expect(run("foo |bar", "cb").text).toBe("|bar")
})

test("yy/yw yank without changing the text; p/P put after / before", () => {
  expect(run("a|bc\nd", "yyp").text).toBe("abc\n|abc\nd")
  expect(run("a|bc\nd", "yyP").text).toBe("|abc\nabc\nd")
  expect(run("|foo bar", "ywP").text).toBe("foo| foo bar")
  expect(run("|foo bar", "ywp").text).toBe("ffoo| oo bar")
  expect(run("a|bc", "xp").text).toBe("ac|b")
  expect(run("a|bc", "2yy").text).toBe("a|bc")
  expect(run("|a\nb\nc", "2yyGp").text).toBe("a\nb\nc\n|a\nb")
})

test("p with an empty register does nothing", () => {
  expect(run("a|bc", "p").text).toBe("a|bc")
})

test("u and Ctrl+R ask the editor to undo and redo", () => {
  expect(run("a|bc", "u<C-r>").commands).toEqual(["undo", "redo"])
})

test("Enter in normal mode submits; other Ctrl keys and non-printing keys pass on", () => {
  expect(run("a|bc", "<CR>").commands).toEqual(["submit"])
  const passed = run("a|bc", "<C-c><C-d><Left><Tab>")
  expect(passed.passed).toEqual(["c", "d", "left", "tab"])
})

test("unknown printable keys are swallowed in normal mode; Esc cancels a pending operator", () => {
  const swallowed = run("a|bc", "Z!")
  expect(swallowed.text).toBe("a|bc")
  expect(swallowed.passed).toEqual([])
  const cancelled = run("a|bc", "d<Esc>x")
  expect(cancelled.text).toBe("a|c")
  expect(cancelled.passed).toEqual([])
  expect(run("a|bc", "dZx").text).toBe("a|c")
})

test("a pending operator or count is reported (for the status bar)", () => {
  expect(run("a|bc", "2d").state.pending).toBe("2d")
  expect(run("a|bc", "g").state.pending).toBe("g")
  expect(run("a|bc", "2dw").state.pending).toBe("")
})
