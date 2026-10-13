import { Terminal } from "@xterm/headless"
import type { Subprocess } from "bun"
import { mkdir, writeFile } from "node:fs/promises"
import { join } from "node:path"
import type { Cell } from "../e2e/harness"
import { encodeKey, encodeMouse, type MouseAction, type MouseModifiers, type MousePoint } from "./input"

export type TerminalOptions = {
  cols?: number
  rows?: number
  cwd?: string
  env?: Record<string, string>
  /** Match the WebUI's optional Shift+Enter -> LF mapping; default is CR. */
  shiftEnterLf?: boolean
}

function dimensions(cols: number, rows: number): void {
  if (![cols, rows].every((value) => Number.isInteger(value) && value > 0 && value <= 4096)) throw new Error("Terminal size must be 1..4096 cells")
}

/** A screen model and a real PTY. No browser, DOM, WebSocket, or app mocks. */
export class PtyTerminal {
  readonly screen: Terminal
  readonly startedAt = performance.now()
  readonly osc52: string[] = []
  private proc?: Subprocess
  private settledExitCode: number | null = null
  private pending = Promise.resolve()
  private replies: string[] = []
  private raw: string[] = []
  private rawSize = 0
  private sgrMouse = false
  private pixelMouse = false
  private focusReporting = false
  private closed = false
  private ptyClosed = false
  private finishPty: () => void = () => {}
  private readonly ptyEnded = new Promise<void>((resolve) => { this.finishPty = resolve })
  private frames: { ms: number; text: string }[] = []

  private constructor(readonly options: TerminalOptions) {
    const cols = options.cols ?? 120
    const rows = options.rows ?? 36
    dimensions(cols, rows)
    this.screen = new Terminal({ cols, rows, allowProposedApi: true, scrollback: 1000 })
    // DA/DSR responses produced by xterm must go back to the child.
    this.screen.onData((data: string) => {
      if (!this.proc) this.replies.push(data)
      else if (this.proc.exitCode === null && this.proc.terminal && !this.proc.terminal.closed) this.write(data)
    })
    for (const final of ["h", "l"]) {
      this.screen.parser.registerCsiHandler({ prefix: "?", final }, (params: (number | number[])[]) => {
        if (params.includes(1004)) this.focusReporting = final === "h"
        if (params.includes(1006)) this.sgrMouse = final === "h"
        if (params.includes(1016)) this.pixelMouse = final === "h"
        return false // Keep xterm's own mode handling.
      })
    }
    this.screen.parser.registerOscHandler(52, (payload: string) => {
      this.osc52.push(payload)
      return true
    })
  }

  static launch(command: string[], options: TerminalOptions = {}): PtyTerminal {
    if (!command.length) throw new Error("A terminal command is required")
    const terminal = new PtyTerminal(options)
    try {
      terminal.proc = Bun.spawn(command, {
        cwd: options.cwd,
        env: { ...process.env, TERM: "xterm-256color", COLORTERM: "truecolor", ...options.env },
        terminal: {
          cols: terminal.screen.cols,
          rows: terminal.screen.rows,
          data(_pty, bytes) { terminal.receive(bytes) },
          exit() { terminal.ptyClosed = true; terminal.finishPty() },
        },
      })
      // Bun keeps Subprocess.exitCode null after a signal; exited resolves
      // the shell-style status (128 + signal) for both exit paths.
      void terminal.proc.exited.then((code) => { terminal.settledExitCode = code })
      for (const reply of terminal.replies.splice(0)) terminal.write(reply)
      return terminal
    } catch (error) {
      terminal.screen.dispose()
      throw error
    }
  }

  get pid(): number { return this.proc!.pid }
  get exitCode(): number | null { return this.settledExitCode ?? this.proc?.exitCode ?? null }

  private receive(bytes: Uint8Array): void {
    const copy = Uint8Array.from(bytes)
    const encoded = Buffer.from(copy).toString("base64")
    this.raw.push(encoded)
    this.rawSize += encoded.length
    while (this.rawSize > 1_000_000 && this.raw.length > 1) this.rawSize -= this.raw.shift()!.length
    this.pending = this.pending.then(() => new Promise<void>((resolve) => {
      this.screen.write(copy, () => {
        this.frames.push({ ms: performance.now() - this.startedAt, text: this.text() })
        if (this.frames.length > 100) this.frames.shift()
        resolve()
      })
    }))
  }

  /** Complete writes already received; it does not declare the application idle. */
  async flush(): Promise<void> {
    let observed: Promise<void>
    do { observed = this.pending; await observed } while (observed !== this.pending)
  }

  lines(): string[] {
    const buffer = this.screen.buffer.active
    return Array.from({ length: this.screen.rows }, (_, row) => buffer.getLine(buffer.viewportY + row)?.translateToString(true, 0, this.screen.cols) ?? "")
  }
  text(): string { return this.lines().join("\n") }
  size(): { cols: number; rows: number } { return { cols: this.screen.cols, rows: this.screen.rows } }
  find(needle: string): MousePoint | null {
    // Search terminal cells, not UTF-16 offsets, so wide glyphs stay aligned.
    for (let row = 0; row < this.screen.rows; row++) {
      let text = ""
      const columns: number[] = []
      for (let col = 0; col < this.screen.cols; col++) {
        const char = this.cell(row, col)?.char || " "
        if (this.cell(row, col)?.width === 0) continue
        text += char
        for (let index = 0; index < char.length; index++) columns.push(col)
      }
      const index = text.indexOf(needle)
      if (index !== -1) return { row, col: columns[index]! }
    }
    return null
  }
  cell(row: number, col: number): Cell | null {
    if (row < 0 || row >= this.screen.rows || col < 0 || col >= this.screen.cols) return null
    const buffer = this.screen.buffer.active
    const cell = buffer.getLine(buffer.viewportY + row)?.getCell(col)
    if (!cell) return null
    const color = (rgb: boolean, palette: boolean, value: number) => rgb ? `#${value.toString(16).padStart(6, "0")}` : palette ? `palette:${value}` : "default"
    return {
      char: cell.getChars(), width: cell.getWidth(),
      fg: color(cell.isFgRGB(), cell.isFgPalette(), cell.getFgColor()),
      bg: color(cell.isBgRGB(), cell.isBgPalette(), cell.getBgColor()),
      bold: cell.isBold() !== 0, italic: cell.isItalic() !== 0,
      underline: cell.isUnderline() !== 0, inverse: cell.isInverse() !== 0,
    }
  }

  async waitFor(check: () => boolean | Promise<boolean>, description: string, timeout = 10_000): Promise<void> {
    const deadline = performance.now() + timeout
    while (true) {
      await this.flush()
      if (await check()) return
      if (this.ptyClosed && this.exitCode !== null) throw new Error(`Process exited (${this.exitCode}) while waiting for ${description}\n${this.text()}`)
      if (performance.now() >= deadline) throw new Error(`Timed out waiting for ${description}\n${this.text()}`)
      await Bun.sleep(10)
    }
  }
  async waitForText(pattern: string | RegExp, timeout = 10_000): Promise<void> {
    await this.waitFor(() => typeof pattern === "string" ? this.text().includes(pattern) : new RegExp(pattern.source, pattern.flags.replace(/[gy]/g, "")).test(this.text()), String(pattern), timeout)
  }
  private write(data: string): void {
    if (!this.proc?.terminal || this.proc.terminal.closed) throw new Error("PTY is closed")
    this.proc.terminal.write(data)
  }
  async focus(focused: boolean): Promise<void> {
    if (!this.focusReporting) throw new Error("Application has not enabled terminal focus reporting")
    this.write(focused ? "\x1b[I" : "\x1b[O")
    await this.flush()
  }
  async type(text: string): Promise<void> { this.write(text); await this.flush() }
  async press(shortcut: string): Promise<void> {
    this.write(encodeKey(shortcut, { applicationCursor: this.screen.modes.applicationCursorKeysMode, shiftEnterLf: this.options.shiftEnterLf ?? false }))
    await this.flush()
  }
  async paste(text: string): Promise<void> {
    this.write(this.screen.modes.bracketedPasteMode ? `\x1b[200~${text}\x1b[201~` : text)
    await this.flush()
  }
  async resize(cols: number, rows: number): Promise<void> {
    dimensions(cols, rows)
    this.screen.resize(cols, rows)
    this.proc!.terminal!.resize(cols, rows)
    await this.flush()
  }
  async mouse(action: MouseAction, point: MousePoint, modifiers: MouseModifiers = {}, button = 0): Promise<void> {
    const mode = this.screen.modes.mouseTrackingMode
    if (mode === "none") throw new Error("Application has not enabled mouse reporting")
    if (this.pixelMouse) throw new Error("Pixel mouse mode is not supported by this cell-based harness")
    if (point.col >= this.screen.cols || point.row >= this.screen.rows) throw new Error("Mouse point is outside the screen")
    if (action === "move" && mode !== "drag" && mode !== "any") return
    if (action === "up" && mode === "x10") return
    this.write(encodeMouse(action, point, this.sgrMouse, modifiers, button))
    await this.flush()
  }
  async click(point: MousePoint): Promise<void> { await this.mouse("down", point); await this.mouse("up", point) }
  async drag(from: MousePoint, to: MousePoint): Promise<void> {
    await this.mouse("down", from)
    const steps = Math.max(Math.abs(to.col - from.col), Math.abs(to.row - from.row))
    for (let step = 1; step <= steps; step++) await this.mouse("move", { col: Math.round(from.col + (to.col - from.col) * step / steps), row: Math.round(from.row + (to.row - from.row) * step / steps) })
    await this.mouse("up", to)
  }
  async waitForExit(timeout = 10_000): Promise<number> {
    let timer: ReturnType<typeof setTimeout> | undefined
    try {
      // Process exit can precede the final PTY read. Await EOF too, so the
      // last rendered output is included without an arbitrary drain sleep.
      const code = await Promise.race([Promise.all([this.proc!.exited, this.ptyEnded]).then(([code]) => code), new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new Error("Timed out waiting for exit")), timeout) })])
      await this.flush()
      return code
    } finally { clearTimeout(timer) }
  }
  async save(directory: string): Promise<void> {
    await this.flush()
    await mkdir(directory, { recursive: true })
    await Promise.all([
      writeFile(join(directory, "final-screen.txt"), this.text()),
      writeFile(join(directory, "frames.json"), JSON.stringify(this.frames)),
      writeFile(join(directory, "output.base64.txt"), this.raw.join("\n")),
    ])
  }
  async close(): Promise<void> {
    if (this.closed) return
    this.closed = true
    try {
      if (this.proc && !this.ptyClosed) {
        try { process.kill(-this.pid, "SIGTERM") } catch { if (this.proc.exitCode === null) this.proc.kill("SIGTERM") }
        try { await this.waitForExit(1000) } catch {
          // Bun PTYs create a session/process group, as in the WebUI host.
          try { process.kill(-this.pid, "SIGKILL") } catch { this.proc.kill("SIGKILL") }
          await this.waitForExit(1000)
        }
      }
      await this.flush()
    } finally {
      this.proc?.terminal?.close()
      this.screen.dispose()
    }
  }
}
