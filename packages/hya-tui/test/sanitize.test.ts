import { expect, test } from "bun:test"
import { stripTerminalControls } from "../src/sanitize"

test("plain text and tabs are kept", () => {
  expect(stripTerminalControls("hya bridge: relay online\tok · ünïcödé")).toBe("hya bridge: relay online\tok · ünïcödé")
})

test("C0 controls, CR/LF, DEL, and C1 controls are removed", () => {
  expect(stripTerminalControls("a\x00b\x07c\x08d\re\nf\x7fg\x85h\x9ai\x9f")).toBe("abcdefghi")
})

test("whole ESC / CSI / OSC sequences are removed", () => {
  expect(stripTerminalControls("\x1b[31mred\x1b[0m")).toBe("red")
  expect(stripTerminalControls("\x1b[?1049h\x1b[2Jwiped")).toBe("wiped")
  expect(stripTerminalControls("\x1b]0;evil title\x07after")).toBe("after")
  expect(stripTerminalControls("\x1b]8;;https://evil.example\x1b\\link\x1b]8;;\x1b\\")).toBe("link")
  expect(stripTerminalControls("\x1b]52;c;ZXZpbA==\x07clip")).toBe("clip")
  expect(stripTerminalControls("\x1bPq#0;2;0;0;0\x1b\\dcs")).toBe("dcs")
  expect(stripTerminalControls("\x1b7saved\x1b8")).toBe("saved")
  expect(stripTerminalControls("\x1b(Bcharset")).toBe("charset")
  // 8-bit CSI / OSC forms.
  expect(stripTerminalControls("\x9b31mred")).toBe("red")
  expect(stripTerminalControls("\x9d0;title\x9cafter")).toBe("after")
  // An unterminated OSC swallows the rest of the line; a lone ESC goes.
  expect(stripTerminalControls("ok\x1b]0;never ends")).toBe("ok")
  expect(stripTerminalControls("end\x1b")).toBe("end")
})
