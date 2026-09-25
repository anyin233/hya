// Playwright fixture that runs a terminal program behind the tui-web host and
// drives it through xterm.js in Chromium. Assertions read the xterm buffer
// (text and per-cell style); screenshots are attached for visual review.

import { spawn, type ChildProcess } from "node:child_process"
import { writeFile } from "node:fs/promises"
import { fileURLToPath } from "node:url"
import { test as base, expect, type Page, type TestInfo } from "@playwright/test"

const packageDir = fileURLToPath(new URL("..", import.meta.url))

export type Cell = {
  char: string
  /** `#rrggbb` for truecolor, `palette:N` for indexed, `default` otherwise. */
  fg: string
  bg: string
  bold: boolean
  italic: boolean
  underline: boolean
  inverse: boolean
  width: number
}

export type LaunchOptions = {
  cwd?: string
  env?: Record<string, string>
  /** Browser viewport in CSS pixels (default: the project viewport). */
  viewport?: { width: number; height: number }
}

export class Tui {
  constructor(
    readonly page: Page,
    readonly url: string,
  ) {}

  /** Visible screen rows, right-trimmed. */
  async lines(): Promise<string[]> {
    return this.page.evaluate(() => {
      const buffer = window.hyaTerm.term.buffer.active
      const rows: string[] = []
      for (let y = 0; y < window.hyaTerm.term.rows; y++) {
        rows.push(buffer.getLine(buffer.viewportY + y)?.translateToString(true) ?? "")
      }
      return rows
    })
  }

  async text(): Promise<string> {
    return (await this.lines()).join("\n")
  }

  async waitForText(pattern: string | RegExp, timeout = 10_000): Promise<void> {
    await expect
      .poll(async () => {
        const text = await this.text()
        return typeof pattern === "string" ? text.includes(pattern) : pattern.test(text)
      }, { timeout, message: `screen never showed ${pattern}` })
      .toBe(true)
  }

  /** Row/column of the first occurrence of `needle` on screen, or null. */
  async find(needle: string): Promise<{ row: number; col: number } | null> {
    const lines = await this.lines()
    for (let row = 0; row < lines.length; row++) {
      const col = lines[row]!.indexOf(needle)
      if (col !== -1) return { row, col }
    }
    return null
  }

  /** Style of one screen cell. `col` is a terminal column, not a string index. */
  async cell(row: number, col: number): Promise<Cell | null> {
    return this.page.evaluate(
      ([row, col]) => {
        const buffer = window.hyaTerm.term.buffer.active
        const cell = buffer.getLine(buffer.viewportY + row)?.getCell(col)
        if (!cell) return null
        const color = (rgb: boolean, palette: boolean, value: number) =>
          rgb ? `#${value.toString(16).padStart(6, "0")}` : palette ? `palette:${value}` : "default"
        return {
          char: cell.getChars(),
          fg: color(cell.isFgRGB(), cell.isFgPalette(), cell.getFgColor()),
          bg: color(cell.isBgRGB(), cell.isBgPalette(), cell.getBgColor()),
          bold: cell.isBold() !== 0,
          italic: cell.isItalic() !== 0,
          underline: cell.isUnderline() !== 0,
          inverse: cell.isInverse() !== 0,
          width: cell.getWidth(),
        }
      },
      [row, col] as const,
    )
  }

  async size(): Promise<{ cols: number; rows: number }> {
    return this.page.evaluate(() => ({ cols: window.hyaTerm.term.cols, rows: window.hyaTerm.term.rows }))
  }

  async type(text: string): Promise<void> {
    await this.page.keyboard.type(text)
  }

  async press(key: string): Promise<void> {
    await this.page.keyboard.press(key)
  }

  /** Resize the browser viewport; xterm refits and the PTY gets SIGWINCH. */
  async resize(width: number, height: number): Promise<{ cols: number; rows: number }> {
    const before = await this.size()
    await this.page.setViewportSize({ width, height })
    await expect.poll(async () => JSON.stringify(await this.size())).not.toBe(JSON.stringify(before))
    return this.size()
  }

  async waitForExit(timeout = 10_000): Promise<number> {
    await expect.poll(() => this.page.evaluate(() => window.hyaTerm.exitCode), { timeout }).not.toBeNull()
    return (await this.page.evaluate(() => window.hyaTerm.exitCode))!
  }

  /** Write `<name>.png` and `<name>.txt` into the test's output dir and attach both. */
  async attach(testInfo: TestInfo, name: string): Promise<void> {
    const png = testInfo.outputPath(`${name}.png`)
    const txt = testInfo.outputPath(`${name}.txt`)
    await this.page.screenshot({ path: png })
    await writeFile(txt, await this.text())
    await testInfo.attach(`${name}.png`, { path: png, contentType: "image/png" })
    await testInfo.attach(`${name}.txt`, { path: txt, contentType: "text/plain" })
  }
}

function startHost(command: string[], options: LaunchOptions): Promise<{ child: ChildProcess; url: string }> {
  const args = ["src/main.ts", "--port", "0", ...(options.cwd ? ["--cwd", options.cwd] : []), "--", ...command]
  const child = spawn("bun", args, { cwd: packageDir, env: { ...process.env, ...options.env }, stdio: ["ignore", "pipe", "pipe"] })
  return new Promise((resolve, reject) => {
    let output = ""
    const onData = (chunk: Buffer) => {
      output += chunk.toString()
      const match = /listening on (\S+)/.exec(output)
      if (match) resolve({ child, url: match[1]! })
    }
    child.stdout!.on("data", onData)
    child.stderr!.on("data", onData)
    child.once("exit", (code) => reject(new Error(`tui-web host exited (${code}): ${output}`)))
  })
}

export const test = base.extend<{ tui: (command: string[], options?: LaunchOptions) => Promise<Tui> }>({
  tui: async ({ page }, use, testInfo) => {
    const children: ChildProcess[] = []
    let last: Tui | undefined
    await use(async (command, options = {}) => {
      const { child, url } = await startHost(command, options)
      children.push(child)
      if (options.viewport) await page.setViewportSize(options.viewport)
      await page.goto(url)
      await expect.poll(() => page.evaluate(() => window.hyaTerm?.connected ?? false)).toBe(true)
      last = new Tui(page, url)
      return last
    })
    // A derived fixture may already have captured the final screen.
    const captured = testInfo.attachments.some((attachment) => attachment.name === "final-screen.png")
    if (last && !captured) await last.attach(testInfo, "final-screen").catch(() => {})
    for (const child of children) child.kill("SIGTERM")
  },
})

export { expect }

/** argv that runs a fixture TUI under Bun, e.g. `fixture("opentui-probe.ts")`. */
export function fixture(name: string): string[] {
  return ["bun", fileURLToPath(new URL(`./fixtures/${name}`, import.meta.url))]
}
