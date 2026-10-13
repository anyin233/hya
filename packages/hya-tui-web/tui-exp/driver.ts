import type { TestInfo } from "@playwright/test"
import { Tui, type InspectionState, type LaunchOptions, type Pointer } from "../e2e/harness"
import type { Terminal } from "@xterm/headless"
import { PtyTerminal } from "./terminal"

// Browser fixtures use 14px DejaVu Sans Mono: about 8.43 x 17 cell pixels.
// Convert historical viewport cases to deterministic cell dimensions.
export function viewportCells(width: number, height: number) {
  return { cols: Math.max(2, Math.floor((width - 15) / 8.43)), rows: Math.max(1, Math.floor(height / 17)) }
}

export class PtyTui extends Tui {
  readonly terminal: PtyTerminal
  private state: InspectionState = {}
  private point = { col: 0, row: 0 }
  private button: number | undefined
  constructor(command: string[], options: LaunchOptions = {}) {
    super(undefined, "pty:")
    const { width = 1100, height = 640 } = options.viewport ?? {}
    const { cols, rows } = viewportCells(width, height)
    const args = options.hostArgs ?? []
    if (args.some((arg) => arg !== "--shift-enter-lf")) throw new Error(`Unsupported PTY host flags: ${args.join(" ")}`)
    this.terminal = PtyTerminal.launch(command, { ...options, cols, rows, shiftEnterLf: args.includes("--shift-enter-lf") })
  }
  override async lines() { await this.terminal.flush(); return this.terminal.lines() }
  override async text() { return (await this.lines()).join("\n") }
  override async find(needle: string) { await this.terminal.flush(); return this.terminal.find(needle) }
  override async cell(row: number, col: number) { await this.terminal.flush(); return this.terminal.cell(row, col) }
  override async size() { return this.terminal.size() }
  override async type(text: string) { await this.terminal.type(text) }
  override async press(key: string) { await this.terminal.press(key) }
  override async focus(focused: boolean) { await this.terminal.focus(focused) }
  override async paste(text: string) { await this.terminal.paste(text) }
  override async exitStatus() { return this.terminal.exitCode }
  override async waitForText(pattern: string | RegExp, timeout = 10_000) { await this.terminal.waitForText(pattern, timeout) }
  override async waitForExit(timeout = 10_000) { return this.terminal.waitForExit(timeout) }
  override async resize(width: number, height: number) {
    const { cols, rows } = viewportCells(width, height)
    await this.terminal.resize(cols, rows)
    return { cols, rows }
  }
  override async screenBox() { const { cols, rows } = this.terminal.size(); return { x: 0, y: 0, width: cols, height: rows } }
  override async inspect<R, A = undefined>(fn: (terminal: Terminal, state: InspectionState, arg: A) => R, arg?: A): Promise<R> {
    await this.terminal.flush()
    return fn(this.terminal.screen, this.state, arg as A)
  }
  override async pause(ms: number) { await Bun.sleep(ms) }
  override async disconnect() { process.kill(-this.terminal.pid, "SIGHUP"); await this.terminal.waitForExit() }
  override async attach(info: TestInfo, name: string) {
    const directory = info.outputPath(name)
    await this.terminal.save(directory)
    await info.attach(`${name}.txt`, { path: `${directory}/final-screen.txt`, contentType: "text/plain" })
  }
  async close() { await this.terminal.close() }
  override get mouse(): Pointer {
    const move = async (x: number, y: number, options?: { steps?: number }) => {
      const next = { col: Math.floor(x), row: Math.floor(y) }
      const from = this.point
      const steps = options?.steps ?? 1
      for (let n = 1; n <= steps; n++) {
        this.point = { col: Math.round(from.col + (next.col - from.col) * n / steps), row: Math.round(from.row + (next.row - from.row) * n / steps) }
        if (this.button !== undefined) await this.terminal.mouse("move", this.point, {}, this.button)
      }
    }
    const down = async (options?: { button?: string }) => {
      this.button = options?.button === "right" ? 2 : options?.button === "middle" ? 1 : 0
      await this.terminal.mouse("down", this.point, {}, this.button)
    }
    const up = async () => { await this.terminal.mouse("up", this.point, {}, this.button); this.button = undefined }
    return {
      move, down, up,
      click: async (x: number, y: number, options?: { button?: string }) => { await move(x, y); await down(options); await up() },
      wheel: async (_dx: number, dy: number) => {
        for (let n = 0; n < Math.ceil(Math.abs(dy) / 100); n++) await this.terminal.mouse(dy < 0 ? "wheel-up" : "wheel-down", this.point)
      },
    }
  }
}
