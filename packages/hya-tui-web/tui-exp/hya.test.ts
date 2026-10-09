// Representative scenarios also exercised by e2e/hya-tui-*.spec.ts.
// Keep isolated backend setup shared; only the terminal driver differs.
import { expect, test } from "bun:test"
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises"
import { homedir, tmpdir } from "node:os"
import { dirname, join } from "node:path"
import type { ChildProcess } from "node:child_process"
import { api, hyaTui, startBackend } from "../e2e/backend"
import { startFakeModel, textStep, type FakeModel, type Step } from "../e2e/fake-model"
import { PtyTerminal } from "./terminal"

const outputRoot = process.env.HYA_TUI_EXP_OUTPUT_DIR ?? join(homedir(), "data/hya-rust/tmp/tui-exp-results")

async function stopBackend(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null) return
  let timer: ReturnType<typeof setTimeout> | undefined
  try {
    const exited = new Promise<void>((resolve) => child.once("exit", () => resolve()))
    child.kill("SIGTERM")
    timer = setTimeout(() => child.kill("SIGKILL"), 3000)
    await exited
  } finally { clearTimeout(timer) }
}

async function withHya(name: string, body: (terminal: PtyTerminal) => Promise<void>, steps?: Step[]): Promise<void> {
  const output = join(outputRoot, name)
  await mkdir(output, { recursive: true })
  const root = await mkdtemp(join(tmpdir(), "hya-tui-exp-"))
  const started = performance.now()
  const timings: Record<string, number> = {}
  let fake: FakeModel | undefined
  let child: ChildProcess | undefined
  let terminal: PtyTerminal | undefined
  let passed = false
  try {
    if (steps) fake = await startFakeModel(steps)
    const startedBackend = await startBackend(root, { fakeModel: fake, protocol: "chat", permission: "default", bundles: undefined })
    child = startedBackend.child
    const backend = startedBackend.backend
    timings.backendSetupMs = performance.now() - started
    const launch = performance.now()
    terminal = PtyTerminal.launch(hyaTui(backend), {
      cols: 180, rows: 40,
      env: { HYA_TUI_CONFIG: join(dirname(backend.dir), "config/hya/tui.json") },
    })
    await terminal.waitForText("Message, !shell, or @file · / commands", 20_000)
    await terminal.waitFor(async () => ((await api<{ sessions?: unknown[] }>(backend, "GET", "/v1/sessions")).sessions?.length ?? 0) > 0, "initial session")
    timings.launchToReadyMs = performance.now() - launch
    const actions = performance.now()
    await body(terminal)
    timings.actionsMs = performance.now() - actions
    passed = true
  } finally {
    const teardown = performance.now()
    try { await terminal?.save(output) }
    finally {
      try { await terminal?.close() }
      finally {
        try { if (child) await stopBackend(child) }
        finally { try { await fake?.stop() } finally { await rm(root, { recursive: true, force: true }) } }
      }
    }
    timings.teardownMs = performance.now() - teardown
    timings.totalMs = performance.now() - started
    await writeFile(join(output, "timings.json"), JSON.stringify({ name, passed, ...timings }, null, 2))
    console.log(`tui-exp ${name}: ${JSON.stringify(timings)}`)
  }
}

test("real TUI: command overlay preserves its input across narrow resize", async () => {
  await withHya("command-resize", async (terminal) => {
    await terminal.type("/layout split")
    await terminal.waitForText("Commands")
    await terminal.waitForText("/layout split")
    expect(terminal.find("Commands")?.row).toBe(2)
    await terminal.resize(80, 24)
    await terminal.waitForText("/layout split")
    expect(terminal.find("Commands")?.row).toBe(2)
    await terminal.press("Escape")
    await terminal.waitFor(() => terminal.find("Commands") === null, "command overlay closed")
    await terminal.type("draft after resize")
    await terminal.waitForText("draft after resize")
  })
})

test("real TUI: mouse focus routes typing and paste to Sessions and preserves the composer draft", async () => {
  await withHya("mouse-focus", async (terminal) => {
    await terminal.type("preserved draft")
    await terminal.waitForText("preserved draft")
    await terminal.waitForText("Sessions")
    const sessions = terminal.find("Sessions")!
    await terminal.click(sessions)
    await terminal.waitFor(() => terminal.cell(sessions.row, sessions.col - 1)?.fg === "#73c8e8", "Sessions focus highlight")
    await terminal.type("leaked text")
    await terminal.paste("leaked paste")
    // A round trip through the command overlay fences the preceding inputs.
    await terminal.type("/")
    await terminal.waitForText("Commands")
    await terminal.press("Escape")
    await terminal.waitFor(() => terminal.find("Commands") === null, "command overlay closed")
    await terminal.press("Alt+ArrowRight")
    await terminal.waitFor(() => terminal.cell(sessions.row, sessions.col - 1)?.fg === "#405366", "Sessions no longer focused")
    await terminal.type(" continues")
    await terminal.waitForText("preserved draft continues")
    expect(terminal.text()).not.toContain("leaked")
  })
})

test("real TUI: partial streamed reply becomes one final answer", async () => {
  const reply = "alpha bravo charlie delta echo foxtrot"
  await withHya("streaming", async (terminal) => {
    await terminal.type("stream please")
    await terminal.press("Enter")
    await terminal.waitForText("alpha br", 20_000)
    expect(terminal.text()).not.toContain(reply)
    expect(terminal.text()).toContain("Esc to interrupt")
    await terminal.waitForText(reply, 20_000)
    await terminal.waitFor(() => !terminal.text().includes("Esc to interrupt"), "turn idle")
    expect(terminal.text().split(reply).length - 1).toBe(1)
  }, [textStep(reply, { chunkSize: 4, delayMs: 150 })])
})
