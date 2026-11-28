/**
 * In-memory `testRender` coverage for messages, streaming, tool cards, status
 * & working indicators, compaction dividers, undo/redo/fork revert, empty
 * reasoning, startup resources, tui.toml config, reload, theme switching,
 * terminal title state, and notification gating.
 *
 * Covers UI invariants downgraded from 12 PTY specs:
 * `messages`, `streaming`, `tools`, `status`, `revert`, `empty-reasoning`,
 * `startup-resources`, `config`, `reload`, `theme`, `title`, and `notifications`.
 */
import { afterAll, afterEach, beforeAll, describe, expect, test } from "bun:test"
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { titleFromState } from "../src/title"
import { themes } from "../src/theme"
import { errorResponse, loadBasicComponents, renderApp, resetBasicComponents, type RenderedApp } from "./support/appHarness"

let app: RenderedApp | undefined
const temps: string[] = []

beforeAll(async () => {
  await loadBasicComponents()
})

afterEach(async () => {
  await app?.dispose()
  app = undefined
  for (const dir of temps.splice(0)) rmSync(dir, { recursive: true, force: true })
})

afterAll(async () => {
  await resetBasicComponents()
})

function tempDir(prefix: string): string {
  const dir = mkdtempSync(join(tmpdir(), prefix))
  temps.push(dir)
  return dir
}

async function runCmd(rendered: RenderedApp, text: string): Promise<void> {
  rendered.controller.ui.command?.open(text)
  await rendered.render()
  await rendered.press("RETURN")
  await rendered.waitForGone("Commands")
}

describe("messages, streaming, reasoning, errors, and scrolling", () => {
  test("renders user bar, assistant header, reasoning toggle, empty reasoning, errors/length/cancel notices, and streaming queue", async () => {
    app = await renderApp({
      width: 110,
      height: 37,
      seed: {
        sessions: [{ id: "hysec_msg", agent: "hya-main", workdir: "/workspace", projectId: "prj_work", model: { providerId: "fake", modelId: "model" } }],
        messages: {
          hysec_msg: [
            { id: "u1", role: "ROLE_USER", parts: [{ id: "u1p", text: { text: "think about it" } }] },
            {
              id: "a1", role: "ROLE_ASSISTANT", agent: "hya-main", model: "fake/model",
              parts: [
                { id: "a1r", reasoning: { text: "private chain of thought about apples" } },
                { id: "a1e", reasoning: {} },
                { id: "a1t", text: { text: "The final answer is 7." } },
              ],
              finish: "FINISH_REASON_STOP",
            },
            {
              id: "a2", role: "ROLE_ASSISTANT", agent: "hya-main", model: "fake/model",
              parts: [{ id: "a2t", text: { text: "cut off mid" } }],
              finish: "FINISH_REASON_LENGTH",
            },
            {
              id: "a3", role: "ROLE_ASSISTANT", agent: "hya-main", model: "fake/model",
              error: { code: "provider_error", message: "http status 400" },
              finish: "FINISH_REASON_ERROR",
            },
          ],
        },
        onTurn: (_id, body) => ({ id: `u_${String((body.prompt as { text?: string })?.text)}`, state: "TURN_STATE_RUNNING" }),
      },
    })

    // User bar + assistant header (no raw `user · stop` or `assistant · stop`).
    await app.waitForText("┃ think about it")
    await app.waitForText("● hya-main · fake/model")
    expect(app.frame()).not.toContain("user · stop")
    expect(app.frame()).not.toContain("assistant · stop")

    // Reasoning collapsed by default (including empty reasoning block); `/thinking` expands & collapses; click toggles.
    await app.waitForText("▸ Thinking · 6 words")
    expect(app.find("private chain of thought")).toBeNull()
    await runCmd(app, "/thinking")
    await app.waitForText("▾ Thinking · 6 words")
    await app.waitForText("private chain of thought about apples")
    await app.clickText("▾ Thinking · 6 words")
    await app.waitForText("▸ Thinking · 6 words")
    expect(app.find("private chain of thought")).toBeNull()

    // Error line in error color, length notice in warning color.
    await app.waitForText("✗ provider_error: http status 400")
    expect(app.span("✗ provider_error")?.fg).toBe(themes.hya.colors.error)
    await app.waitForText("! Reply stopped at the output length limit")
    expect(app.span("! Reply stopped")?.fg).toBe(themes.hya.colors.warning)

    // Streaming chunks + prompt queuing while a turn runs.
    await app.submit("first live")
    await app.waitForText("Esc to interrupt")
    app.backend.emitSession("hysec_msg", { messageStarted: { message: "live_a", role: "ROLE_ASSISTANT" } })
    app.backend.emitSession("hysec_msg", { partStarted: { message: "live_a", part: "live_p", kind: "text" } })
    app.backend.emitSession("hysec_msg", { partAppended: { message: "live_a", part: "live_p", textDelta: "### Streamed " } })
    await app.waitForText("Streamed")
    // Raw markdown heading markers are never leaked while streaming.
    expect(app.find("### Streamed")).toBeNull()

    // Submitting while running queues the second prompt until the first turn finishes.
    await app.submit("queued second")
    await app.waitFor(() => app!.store.state.queued.length === 1, "prompt queued")
    app.backend.emitSession("hysec_msg", { partAppended: { message: "live_a", part: "live_p", textDelta: "Heading" } })
    app.backend.emitSession("hysec_msg", { messageFinished: { message: "live_a", finish: "FINISH_REASON_STOP" } })
    await app.waitFor(() => app!.backend.calls.some((c) => c.path.endsWith("/turns") && JSON.stringify(c.body).includes("queued second")), "queued prompt sent")
  })

  test("PgUp/PgDn/End scroll transcript; new content below scrolled-up view shows hint and stays put on session list refreshes", async () => {
    const longText = Array.from({ length: 80 }, (_, i) => `row ${String(i + 1).padStart(2, "0")} of the long reply`).join("\n\n")
    app = await renderApp({
      width: 100,
      height: 30,
      seed: {
        sessions: [{ id: "hysec_scr", agent: "hya-main", workdir: "/workspace", projectId: "prj_work", model: { providerId: "fake", modelId: "model" } }],
        messages: {
          hysec_scr: [
            { id: "m1", role: "ROLE_USER", parts: [{ id: "p1", text: { text: "long" } }] },
            { id: "m2", role: "ROLE_ASSISTANT", agent: "hya-main", model: "fake/model", parts: [{ id: "p2", text: { text: longText } }], finish: "FINISH_REASON_STOP" },
          ],
        },
      },
    })
    await app.waitForText("row 80 of the long reply")
    // Scroll using the transcript scrollbox directly via keyboard PgUp (`\x1b[5~`)
    app.setup.mockInput.pressKey("\u001b[5~")
    await app.render()
    await app.waitForGone("row 80 of the long reply")

    // Emit new content while scrolled up -> hint appears without jumping to bottom.
    app.backend.emitSession("hysec_scr", { messageStarted: { message: "m3", role: "ROLE_ASSISTANT" } })
    app.backend.emitSession("hysec_scr", { partStarted: { message: "m3", part: "p3", kind: "text" } })
    app.backend.emitSession("hysec_scr", { partAppended: { message: "m3", part: "p3", textDelta: "brand new bottom line" } })
    await app.waitForText("↓ New messages below")
    expect(app.find("brand new bottom line")).toBeNull()

    // Session list refresh does not reset scroll position.
    app.backend.emitGlobal({ sessionUpdated: { busy: true }, session: "hysec_scr" })
    await app.render()
    expect(app.find("brand new bottom line")).toBeNull()

    // End jumps to bottom and clears hint.
    await app.press("END")
    await app.waitForText("brand new bottom line")
    await app.waitForGone("New messages below")
  })
})

describe("tool cards, status, todos, context usage, and compaction history", () => {
  test("read/bash/edit/write/failed/running tool cards, /tools expand, task card read-only open, and 80-col fit", async () => {
    app = await renderApp({
      width: 110,
      height: 41,
      seed: {
        sessions: [
          {
            id: "hysec_tools", agent: "hya-main", workdir: "/workspace", projectId: "prj_work",
            model: { providerId: "fake", modelId: "model" },
          },
          { id: "hysec_sub", parent: "hysec_tools", agent: "hya-task", workdir: "/workspace", busy: true },
        ],
        messages: {
          hysec_tools: [
            {
              id: "a1", role: "ROLE_ASSISTANT", agent: "hya-main", model: "fake/model",
              parts: [
                { id: "t_read", toolCall: { tool: "read", state: "TOOL_EXECUTION_STATE_OK", inputJson: '{"path":"notes.txt","offset":1,"limit":2}', outputJson: '{"content":"1  alpha line\\n2  beta line\\n"}', durationMs: "12" } },
                { id: "t_edit", toolCall: { tool: "edit", state: "TOOL_EXECUTION_STATE_OK", inputJson: '{"path":"poem.txt","oldString":"two","newString":"TWO"}', outputJson: '{"diff":"@@ -1,3 +1,3 @@\\n one\\n-two\\n+TWO\\n three\\n"}', durationMs: "8" } },
                { id: "t_err", toolCall: { tool: "read", state: "TOOL_EXECUTION_STATE_ERROR", inputJson: '{"path":"missing.txt"}', errorCode: "not_found", errorMessage: "File not found" } },
                { id: "t_task", toolCall: { tool: "task", callId: "call_1", state: "TOOL_EXECUTION_STATE_OK", inputJson: '{"description":"survey repo","subagent_type":"hya-task"}', outputJson: '{"child_session_id":"hysec_sub"}' } },
              ],
              finish: "FINISH_REASON_STOP",
            },
          ],
          hysec_sub: [{ id: "sm1", role: "ROLE_USER", parts: [{ id: "sp1", text: { text: "inspect repo files" } }] }],
        },
      },
    })
    await app.waitForText("notes.txt")
    await app.waitForText("✓")
    await app.waitForText("✗")
    expect(app.find("alpha line")).toBeNull() // collapsed by default
    expect(app.find("File not found")).toBeNull()

    // `/tools on` expands diffs and errors with semantic colors.
    await runCmd(app, "/tools on")
    await app.waitForText("alpha line")
    await app.waitForText("File not found")
    expect(app.span("File not found")?.fg).toBe(themes.hya.colors.error)
    await app.waitForText("- two")
    await app.waitForText("+ TWO")
    expect(app.span("- two")?.fg).toBe(themes.hya.diffColors.remove)
    expect(app.span("+ TWO")?.fg).toBe(themes.hya.diffColors.add)

    // Fits 80 columns cleanly.
    await app.resize(80, 41)
    for (const line of app.lines()) expect(line.length).toBeLessThanOrEqual(80)
    await app.resize(110, 41)

    // `/tools off` collapses; `/open hysec_sub` opens subagent read-only; Esc returns to parent.
    await runCmd(app, "/tools off")
    await app.waitForGone("alpha line")
    await runCmd(app, "/open hysec_sub")
    await app.waitForText("Viewing subagent hya-task · Esc returns")
    await app.waitForText("inspect repo files")
    await app.press("ESCAPE")
    await app.waitForGone("Viewing subagent")
    await app.waitForText("notes.txt")
  })

  test("live todos, context % warning color at 85%, and compaction history divider", async () => {
    app = await renderApp({
      preset: "workspace",
      width: 156,
      height: 41,
      seed: {
        sessions: [{
          id: "hysec_st", agent: "hya-main", workdir: "/workspace", projectId: "prj_work",
          model: { providerId: "fake", modelId: "model" }, permissionMode: "manual",
          usage: { input: "85000", output: "100" },
        }],
        messages: {
          hysec_st: [
            { id: "m_comp", role: "ROLE_SYSTEM", parts: [{ id: "pc", text: { text: "HYA_COMPACTED_CONTEXT\nEarlier context was removed by the harness." } }] },
            { id: "m_after", role: "ROLE_ASSISTANT", agent: "hya-main", model: "fake/model", parts: [{ id: "pa", text: { text: "Kept reply." } }], roundUsage: { input: "85000", output: "100" }, finish: "FINISH_REASON_STOP" },
          ],
        },
        vcs: { branch: "feature-ui" },
      },
    })
    await app.waitForText("Kept reply.")
    // History compaction divider renders once above the rewrite marker.
    await app.waitForText("── context compacted ──")
    expect(app.frame().match(/context compacted/g)).toHaveLength(1)
    // Context sidebar shows 85% (warning) + git branch.
    await app.waitForText("85%")
    await app.waitForText("feature-ui")
    expect(app.span("85%")?.fg).toBe(themes.hya.colors.warning)

    // Live todoUpdated frame updates Todos panel without polling GET /todo again.
    const todoGetsBefore = app.backend.calls.filter((c) => c.path.endsWith("/todo")).length
    app.backend.emitSession("hysec_st", { todoUpdated: { items: [{ id: "1", status: "pending", content: "write tests" }] } })
    await app.waitForText("write tests")
    expect(app.backend.calls.filter((c) => c.path.endsWith("/todo")).length).toBe(todoGetsBefore)
  })
})

describe("undo, redo, fork, startup resources, config, reload, theme, title, and notifications", () => {
  test("/undo refills input with revert line, /redo restores, busy /undo returns 409 safely, and /fork picker works at 80 cols", async () => {
    let blockRevert = false
    app = await renderApp({
      width: 80,
      height: 37,
      seed: {
        sessions: [{ id: "hysec_rev", agent: "hya-main", workdir: "/workspace", projectId: "prj_work", model: { providerId: "fake", modelId: "model" } }],
        messages: {
          hysec_rev: [
            { id: "u1", role: "ROLE_USER", parts: [{ id: "u1p", text: { text: "alpha question" } }] },
            { id: "a1", role: "ROLE_ASSISTANT", agent: "hya-main", model: "fake/model", parts: [{ id: "a1p", text: { text: "First reply." } }], finish: "FINISH_REASON_STOP" },
            { id: "u2", role: "ROLE_USER", parts: [{ id: "u2p", text: { text: "beta question" } }] },
            { id: "a2", role: "ROLE_ASSISTANT", agent: "hya-main", model: "fake/model", parts: [{ id: "a2p", text: { text: "Second reply." } }], finish: "FINISH_REASON_STOP" },
          ],
        },
        onRequest: (method, path) => {
          if (blockRevert && method === "POST" && path.endsWith("/revert")) return errorResponse(409, "session_busy", "turn running")
          return undefined
        },
      },
    })
    await app.waitForText("Second reply.")

    // Busy revert returns 409 without showing pending revert marker.
    blockRevert = true
    await runCmd(app, "/undo")
    expect(app.find("↶")).toBeNull()
    blockRevert = false

    // `/undo` shows pending revert marker and prefills input; `/redo` clears it.
    await runCmd(app, "/undo")
    await app.waitForText(/↶ \d+ messages reverted/)
    expect(app.controller.ui.composerInput?.text).toBe("beta question")
    await runCmd(app, "/redo")
    await app.waitForGone("↶")
    await app.waitForText("Second reply.")

    // `/fork` opens picker at 80 cols, forks before `beta question` and prefills it.
    await runCmd(app, "/fork")
    await app.waitForText("Fork at the latest message")
    await app.waitForText(/beta question\s+\[#2\]/)
    for (const line of app.lines()) expect(line.length).toBeLessThanOrEqual(80)
    await app.press("ARROW_DOWN")
    await app.press("RETURN")
    await app.waitForGone("Fork at the latest message")
    await app.waitForGone("Second reply.")
    await app.waitForText("First reply.")
    expect(app.find("Second reply.")).toBeNull()
    expect(app.controller.ui.composerInput?.text).toBe("beta question")
  })

  test("startup resources, quiet mode, /resources, tui.toml warnings, /config reload/init, and /reload", async () => {
    const configDir = tempDir("hya-cfg-")
    app = await renderApp({
      width: 120,
      height: 41,
      configDir,
      tuiToml: '[statusline]\ncolour = "red"\nenabled = "yes"\nsegments = ["mode", "nope"]\n[theme]\nname = "ember"\n',
      seed: {
        skills: [{ name: "greet", description: "Say hello", source: "project", location: "/workspace/.hya/skills/greet/SKILL.md" }],
        contextFiles: [{ path: "/workspace/AGENTS.md", scope: "project", bytes: 18 }],
        bundles: [{ id: "hya/base-tools", version: "1.0.0", scope: "first_party", state: "active", enabled: true }],
        mcpServers: [{ name: "broken", state: "MCP_SERVER_STATE_FAILED", error: "spawn ENOENT" }],
      },
    })
    await app.waitForText(/\[Config\] .*4 warnings/)
    await app.waitForText("Unknown key statusline.colour; ignored")
    await app.waitForText(/\[Context\] .*AGENTS\.md/)
    await app.waitForText(/\[Skills\] greet/)

    // `/config` shows sources and warnings; `/config reload` applies edits while keeping draft.
    await app.type("kept draft")
    await runCmd(app, "/config")
    await app.waitForText("TUI config")
    await app.waitForText("Warnings (4)")
    writeFileSync(app.tuiConfigPath, 'preset = "minimal"\n')
    await runCmd(app, "/config reload")
    await app.waitForText(/Preset\s+minimal \(tui\.toml\)/)
    expect(app.controller.ui.composerInput?.text).toBe("kept draft")

    // `/reload` picks up newly added skills/commands; `/reload-context` and `/resources` work.
    app.backend.skills.push({ name: "fresh-skill", description: "Added later", source: "project" })
    app.backend.commands.push({ name: "fresh-command", description: "Added cmd", source: "command", scope: "project" })
    await runCmd(app, "/reload")
    await app.waitForText(/Reloaded tui\.toml/)
    await runCmd(app, "/reload-context")
    await app.waitForText("Reloaded context (re-recorded at the next turn)")
    await runCmd(app, "/resources")
    await app.waitForText(/AGENTS\.md · 18 B/)

    // Quiet startup shows only problems (failed MCP server), hiding `[Skills]`.
    await app.dispose()
    app = await renderApp({
      width: 120,
      height: 41,
      tuiToml: "[startup]\nquiet = true\n",
      seed: {
        skills: [{ name: "greet", description: "Say hello", source: "project" }],
        mcpServers: [{ name: "broken", state: "MCP_SERVER_STATE_FAILED", error: "spawn ENOENT" }],
      },
    })
    await app.waitForText(/\[MCP\] broken: failed/)
    expect(app.find("[Skills]")).toBeNull()

    // `/config init` writes template once.
    await app.dispose()
    const freshDir = tempDir("hya-cfg-init-")
    app = await renderApp({ width: 120, height: 41, configDir: freshDir })
    await runCmd(app, "/config init")
    const template = readFileSync(app.tuiConfigPath, "utf8")
    expect(template).toContain("version = 1")
    await runCmd(app, "/config init")
    expect(readFileSync(app.tuiConfigPath, "utf8")).toBe(template)
  })

  test("/theme live preview, Esc restore, Enter save & restart, title state markers, and unfocused notifications gating", async () => {
    const configDir = tempDir("hya-theme-")
    app = await renderApp({
      width: 80,
      height: 37,
      configDir,
      seed: {
        sessions: [{ id: "hysec_th", agent: "hya-main", workdir: "/workspace", projectId: "prj_work", model: { providerId: "fake", modelId: "model" } }],
        messages: {
          hysec_th: [
            { id: "m1", role: "ROLE_USER", parts: [{ id: "p1", text: { text: "show me text" } }] },
            { id: "m2", role: "ROLE_ASSISTANT", agent: "hya-main", model: "fake/model", parts: [{ id: "p2", text: { text: "Done." } }], finish: "FINISH_REASON_STOP" },
          ],
        },
        autoReply: "Notified reply.",
      },
    })
    await app.waitForText("Done.")
    expect(app.span("show me text")?.fg).toBe(themes.hya.colors.fg)

    // Preview Light -> Esc restores hya without writing tui.json.
    await runCmd(app, "/theme")
    await app.waitForText(/▸ ● Auto/)
    await app.press("ARROW_DOWN")
    await app.press("ARROW_DOWN")
    await app.waitForText(/▸ +Light\s+\[light\]/)
    expect(app.span("▸")?.fg).toBe(themes.light.colors.focus)
    await app.press("ESCAPE")
    await app.waitForGone("─Theme")
    expect(app.span("show me text")?.fg).toBe(themes.hya.colors.fg)

    // Enter saves Light and repaints existing messages.
    await runCmd(app, "/theme")
    await app.press("ARROW_DOWN")
    await app.press("ARROW_DOWN")
    await app.press("RETURN")
    await app.waitForGone("─Theme")
    expect(app.span("show me text")?.fg).toBe(themes.light.colors.fg)
    expect(JSON.parse(readFileSync(app.preferencesPath, "utf8")).theme).toBe("light")

    // Notifications & title markers: focused emits nothing; unfocused emits OSC 9/777 & `✓ ` title until refocused; `/notifications off` suppresses.
    app.terminal.setFocused(true)
    await app.submit("while focused")
    await app.waitForText("Notified reply.")
    expect(app.terminal.notifications).toEqual([])
    expect(titleFromState(app.store.state, "/workspace")).not.toMatch(/^[●?✓✗] /)

    app.terminal.setFocused(false)
    await app.submit("while blurred")
    await app.waitFor(() => app!.terminal.notifications.length === 1, "unfocused notification")
    expect(app.terminal.notifications[0]).toContain("Turn finished")
    expect(titleFromState(app.store.state, "/workspace")).toMatch(/^✓ /)
    app.terminal.setFocused(true)
    expect(titleFromState(app.store.state, "/workspace")).not.toMatch(/^✓ /)

    await runCmd(app, "/notifications off")
    app.terminal.setFocused(false)
    app.terminal.notifications.length = 0
    await app.submit("with notifications off")
    await app.waitFor(() => !app!.store.state.running, "turn done")
    expect(app.terminal.notifications).toEqual([])
  })
})
