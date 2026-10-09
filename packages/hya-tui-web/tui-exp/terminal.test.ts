import { expect, test } from "bun:test"
import { fileURLToPath } from "node:url"
import { PtyTerminal } from "./terminal"

test("a real PTY renders overwritten cells, truecolor, wide glyphs and terminal replies", async () => {
  const script = `process.stdin.setRawMode(true);
    process.stdout.write("\\x1b[2J\\x1b[Hobsolete\\r\\x1b[2K\\x1b[38;2;115;200;232m你好|\\x1b[0m\\x1b[6n");
    process.stdin.on("data", b => { process.stdout.write("\\r\\nreply:" + b.toString("hex")); });`
  const terminal = PtyTerminal.launch(["bun", "-e", script], { cols: 80, rows: 24 })
  try {
    await terminal.waitForText("reply:1b5b313b3652") // DSR: row 1, column 6.
    expect(terminal.text()).not.toContain("obsolete")
    expect(terminal.cell(0, 0)).toMatchObject({ char: "你", width: 2, fg: "#73c8e8" })
    expect(terminal.cell(0, 2)).toMatchObject({ char: "好", width: 2 })
    expect(terminal.find("|")).toEqual({ row: 0, col: 4 })
  } finally { await terminal.close() }
})

test("keyboard, bracketed paste, mouse click/drag/wheel reach the raw child", async () => {
  const terminal = PtyTerminal.launch(["bun", fileURLToPath(new URL("./protocol-probe.ts", import.meta.url))], { cols: 200, rows: 24 })
  try {
    await terminal.waitForText("ready")
    await terminal.press("ArrowUp")
    await terminal.paste("hello")
    await terminal.click({ col: 4, row: 2 })
    await terminal.drag({ col: 0, row: 0 }, { col: 1, row: 0 })
    await terminal.mouse("wheel-down", { col: 4, row: 2 })
    const wire = "\x1bOA\x1b[200~hello\x1b[201~\x1b[<0;5;3M\x1b[<0;5;3m\x1b[<0;1;1M\x1b[<32;2;1M\x1b[<0;2;1m\x1b[<65;5;3M"
    await terminal.waitForText(`input:${Buffer.from(wire).toString("hex")}`)
    await terminal.press("Control+C")
    expect(await terminal.waitForExit()).toBe(0)
  } finally { await terminal.close() }
})

test("OpenTUI renders at cell dimensions and handles real key input and resize", async () => {
  const terminal = PtyTerminal.launch(["bun", fileURLToPath(new URL("../e2e/fixtures/opentui-probe.ts", import.meta.url))], { cols: 100, rows: 30 })
  try {
    await terminal.waitForText("size 100x30")
    const accent = terminal.find("accent")!
    expect(terminal.cell(accent.row, accent.col)?.fg).toBe("#73c8e8")
    await terminal.type("headless works")
    await terminal.press("Enter")
    await terminal.waitForText("echo:headless works")
    await terminal.resize(80, 24)
    await terminal.waitForText("size 80x24")
    await terminal.press("Control+C")
    expect(await terminal.waitForExit()).toBe(0)
  } finally { await terminal.close() }
})

test("exit codes are from the real process and a hung child is cleaned up", async () => {
  const exited = PtyTerminal.launch(["sh", "-c", "printf 'done'; exit 3"])
  try { expect(await exited.waitForExit()).toBe(3); expect(exited.text()).toContain("done") }
  finally { await exited.close() }
  const hung = PtyTerminal.launch(["sh", "-c", "trap '' TERM; printf ready; while :; do sleep 1; done"])
  await hung.waitForText("ready")
  await hung.close()
  expect(hung.exitCode).toBe(128 + 9)
  expect(await hung.waitForExit()).toBe(128 + 9)
  expect(() => process.kill(hung.pid, 0)).toThrow()
})
