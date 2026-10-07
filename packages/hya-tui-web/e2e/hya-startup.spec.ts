// Real PTY through the browser; timings are process phase marks, not polling latency.
import { readFile } from "node:fs/promises"
import { join, resolve } from "node:path"
import { daemon, daemonStatus, expect, hyaBin, launchTest as test, test as backendTest, tuiMain, statusSessionId, textStep } from "./hya"

test("records cold and warm all-bundle startup and accepts input", async ({ tui, workspace }, info) => {
  test.setTimeout(60_000)
  for (const mode of ["cold", "warm", "cold_cached"] as const) {
    if (mode === "cold_cached") expect((await daemon(workspace, ["stop"])).code).toBe(0)
    expect(Boolean(await daemonStatus(workspace))).toBe(mode === "warm")
    const trace = join(workspace.dir, `${mode}.jsonl`)
    const term = await tui(["bun", resolve("e2e/fixtures/startup-launch.ts"), hyaBin, "--port", "0"], {
      cwd: workspace.dir,
      env: {
        ...workspace.env,
        HYA_TUI_DIR: resolve("../hya-tui"),
        HYA_TUI_WEB_DIR: resolve("."),
        HYA_STARTUP_TRACE_FILE: trace,
      },
    })
    await term.waitForText("Message, !shell, or @file · / commands", 60_000)
    await expect.poll(async () => await readFile(trace, "utf8").catch(() => ""), { timeout: 30_000 }).toMatch(/tui_extensions_(loaded|failed)/)
    expect(await readFile(trace, "utf8")).not.toContain("tui_extensions_failed")
    const marks = (await readFile(trace, "utf8")).trim().split("\n").map((line) => JSON.parse(line) as { mark: string; wall_ms: number })
    const start = marks.find((mark) => mark.mark === "frontend_spawn")!.wall_ms
    expect(JSON.stringify(marks)).not.toContain("Connection failed")
    const ready = marks.find((mark) => mark.mark === "tui_controller_ready")!.wall_ms - start
    const budget = Number(process.env[`HYA_STARTUP_BUDGET_${mode === "warm" ? "WARM" : "COLD"}_MS`])
    if (budget > 0) expect.soft(ready, `${mode} interactive startup`).toBeLessThanOrEqual(budget)
    console.log(mode, marks.map((mark) => `${mark.mark}=${(mark.wall_ms - start).toFixed(1)}ms`).join(" "))
    await info.attach(`${mode}-startup`, { body: JSON.stringify(marks, null, 2), contentType: "application/json" })
    await term.type("/help")
    await term.press("Enter")
    await term.waitForText("Help · keys and commands")
    await term.attach(info, mode)
    await term.press("Escape")
    await expect.poll(async () => (await term.text()).includes("Help · keys and commands")).toBe(false)
    await term.press("Control+d")
    expect(await term.waitForExit()).toBe(0)
  }
})

backendTest("compiled gRPC startup resolves bundled definitions in a narrow browser", async ({ tui, backend }, info) => {
  const term = await tui(["bun", tuiMain, "--grpc", new URL(backend.url).host, "--dir", backend.dir], { viewport: { width: 700, height: 640 } })
  await term.waitForText("Message, !shell, or @file · / commands")
  await term.type("hello from gRPC")
  await term.press("Enter")
  await term.waitForText("No live provider is available", 20_000)
  await term.attach(info, "narrow-grpc")
})


test.describe("native TUI supervision", () => {
  test.use({ model: { steps: [textStep("Before native reload."), textStep("After native reload.")] } })
  test("bare hya reloads the same session and unsent draft without a Bun supervisor", async ({ tui, workspace }, info) => {
    const trace = join(workspace.dir, "reload.jsonl")
    const term = await tui([hyaBin, "--port", "0"], {
      cwd: workspace.dir,
      env: { ...workspace.env, HYA_TUI_DIR: resolve("../hya-tui"), HYA_TUI_WEB_DIR: resolve("."), HYA_STARTUP_TRACE_FILE: trace },
    })
    await term.waitForText("Message, !shell, or @file · / commands")
    await term.type("first prompt")
    await term.press("Enter")
    await term.waitForText("Before native reload.")
    await term.waitForIdle()
    const session = await statusSessionId(term)
    await term.type("preserved draft")
    await term.waitForText("preserved draft")
    expect((await daemon(workspace, ["restart", "--json"])).code).toBe(0)
    await expect.poll(async () => (await readFile(trace, "utf8")).match(/tui_app_entry/g)?.length, { timeout: 30_000 }).toBe(2)
    expect(await readFile(trace, "utf8")).not.toContain("tui_supervisor_entry")
    await term.waitForText("preserved draft")
    await term.waitForText("Before native reload.")
    await term.press("Enter")
    await term.waitForText("After native reload.")
    expect(await statusSessionId(term)).toBe(session)
    await term.attach(info, "native-reloaded")
    await term.press("Control+d")
    expect(await term.waitForExit()).toBe(0)
    expect(await daemonStatus(workspace)).toBeDefined()
  })
})
