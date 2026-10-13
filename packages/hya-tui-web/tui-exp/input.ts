// Terminal input encoding, independent of the PTY and screen implementation.
export type KeyboardModes = { applicationCursor: boolean; shiftEnterLf: boolean }

export function encodeKey(shortcut: string, modes: KeyboardModes): string {
  const parts = shortcut.split("+")
  const key = parts.pop()!
  const modifiers = new Set(parts.map((part) => part.toLowerCase()))
  for (const modifier of modifiers) {
    if (!["control", "ctrl", "alt", "shift"].includes(modifier)) throw new Error(`Unsupported modifier: ${modifier}`)
  }
  const ctrl = modifiers.has("control") || modifiers.has("ctrl")
  const alt = modifiers.has("alt")
  const shift = modifiers.has("shift")
  const modifier = 1 + Number(shift) + 2 * Number(alt) + 4 * Number(ctrl)
  const arrows: Record<string, string> = {
    ArrowUp: "A", Up: "A", ArrowDown: "B", Down: "B", ArrowRight: "C", Right: "C", ArrowLeft: "D", Left: "D", Home: "H", End: "F",
  }
  if (arrows[key]) return modifier > 1 ? `\x1b[1;${modifier}${arrows[key]}` : `\x1b${modes.applicationCursor ? "O" : "["}${arrows[key]}`
  const functionKey = /^F([1-9]|1[0-2])$/.exec(key)
  if (functionKey) {
    const n = Number(functionKey[1])
    if (n <= 4) return modifier === 1 ? `\x1bO${"PQRS"[n - 1]}` : `\x1b[1;${modifier}${"PQRS"[n - 1]}`
    const code = [15, 17, 18, 19, 20, 21, 23, 24][n - 5]
    return `\x1b[${code}${modifier > 1 ? `;${modifier}` : ""}~`
  }
  const numbered: Record<string, number> = { Insert: 2, Delete: 3, PageUp: 5, PageDown: 6 }
  if (numbered[key]) return `\x1b[${numbered[key]}${modifier > 1 ? `;${modifier}` : ""}~`
  if (key === "Tab" && !ctrl && !alt) return shift ? "\x1b[Z" : "\t"
  if (key === "Enter" && !ctrl) return `${alt ? "\x1b" : ""}${shift && modes.shiftEnterLf ? "\n" : "\r"}`
  if ((key === "Escape" || key === "Esc") && modifier === 1) return "\x1b"
  if (key === "Backspace" && !ctrl && !shift) return `${alt ? "\x1b" : ""}\x7f`
  const character = key === "Space" ? " " : key
  if ([...character].length === 1) {
    let value = shift ? character.toUpperCase() : character
    if (ctrl) {
      // Legacy terminal encoding cannot distinguish Ctrl+Shift+letter.
      if (shift) throw new Error(`Unsupported legacy key combination: ${shortcut}`)
      const code = character.toUpperCase().charCodeAt(0)
      if (character === " ") value = "\0"
      else if (code >= 64 && code <= 95) value = String.fromCharCode(code & 31)
      else throw new Error(`Unsupported control key: ${shortcut}`)
    }
    return `${alt ? "\x1b" : ""}${value}`
  }
  throw new Error(`Unsupported key: ${shortcut}`)
}

export type MousePoint = { col: number; row: number }
export type MouseModifiers = { shift?: boolean; alt?: boolean; ctrl?: boolean }
export type MouseAction = "down" | "up" | "move" | "wheel-up" | "wheel-down"

export function encodeMouse(
  action: MouseAction, point: MousePoint, sgr: boolean, modifiers: MouseModifiers = {}, button = 0,
): string {
  if (!Number.isInteger(point.col) || !Number.isInteger(point.row) || point.col < 0 || point.row < 0) throw new Error("Mouse coordinates must be nonnegative integer cells")
  const wheel = action.startsWith("wheel-")
  let code = wheel ? (action === "wheel-up" ? 64 : 65) : button
  if (action === "move") code += 32
  code += (modifiers.shift ? 4 : 0) + (modifiers.alt ? 8 : 0) + (modifiers.ctrl ? 16 : 0)
  if (sgr) return `\x1b[<${code};${point.col + 1};${point.row + 1}${action === "up" ? "m" : "M"}`
  if (point.col > 222 || point.row > 222) throw new Error("Legacy mouse coordinates exceed the protocol range")
  if (action === "up") code = 3 + (modifiers.shift ? 4 : 0) + (modifiers.alt ? 8 : 0) + (modifiers.ctrl ? 16 : 0)
  return `\x1b[M${String.fromCharCode(code + 32, point.col + 33, point.row + 33)}`
}
