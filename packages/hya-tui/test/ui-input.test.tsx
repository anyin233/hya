/**
 * In-memory `testRender` coverage for composer, keybindings, vim mode,
 * external editor, paste chips/paths/SSH, attachments, and clipboard UI
 * (migrated from `composer`, `keybindings`, `vim`, `editor`, `paste`,
 * `attachments`, and UI parts of `clipboard`).
 */
import { afterEach, describe, expect, test } from "bun:test"
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { renderApp, type RenderedApp } from "./support/appHarness"

let app: RenderedApp | undefined
const temps: string[] = []

afterEach(async () => {
  await app?.dispose()
  app = undefined
  for (const dir of temps.splice(0)) rmSync(dir, { recursive: true, force: true })
})

function tempDir(prefix: string): string {
  const dir = mkdtempSync(join(tmpdir(), prefix))
  temps.push(dir)
  return dir
}

function pngBytes(size = 2048): Buffer {
  const sig = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])
  return Buffer.concat([sig, Buffer.alloc(Math.max(0, size - sig.length), 1)])
}

function bmpBytes(): Buffer {
  const bytes = Buffer.alloc(54 + 16)
  bytes.fill(0x7f, 54)
  bytes.write("BM", 0, "latin1")
  bytes.writeUInt32LE(bytes.length, 2)
  bytes.writeUInt32LE(54, 10)
  bytes.writeUInt32LE(40, 14)
  bytes.writeInt32LE(2, 18)
  bytes.writeInt32LE(2, 22)
  bytes.writeUInt16LE(1, 26)
  bytes.writeUInt16LE(24, 28)
  bytes.writeUInt32LE(16, 34)
  return bytes
}

function composerRows(rendered: RenderedApp): { top: number; rows: string[]; title: string } {
  const lines = rendered.lines()
  let bottom = lines.length - 1
  while (bottom >= 0 && !lines[bottom]!.startsWith("└")) bottom--
  let top = bottom - 1
  while (top >= 0 && !lines[top]!.startsWith("┌")) top--
  const rows = lines.slice(top + 1, bottom).map((line) => {
    const end = line.indexOf("│", 1)
    return line.slice(1, end < 0 ? undefined : end).trim()
  })
  const title = lines[top]!.slice(1, lines[top]!.indexOf("┐")).replace(/─/g, " ").trim()
  return { top, rows, title }
}

async function runCmd(rendered: RenderedApp, text: string): Promise<void> {
  await rendered.press("x", { ctrl: true })
  await rendered.type(text)
  await rendered.press("RETURN")
  await rendered.waitForGone("Commands")
}

describe("composer multiline, history, shell, @file, Esc, and quit", () => {
  test("multiline (Ctrl+J, Alt+Enter, paste), 8-row scroll cap, cursor keys, history, and Esc/Ctrl+C/Ctrl+D", async () => {
    app = await renderApp({
      width: 120,
      height: 37,
      seed: { autoReply: "replied" },
    })
    expect(composerRows(app).rows).toHaveLength(1)
    await app.type("line one")
    await app.press("j", { ctrl: true })
    await app.type("line two")
    await app.press("RETURN", { meta: true })
    await app.type("line three")
    expect(composerRows(app).rows).toEqual(["line one", "line two", "line three"])
    await app.press("RETURN")
    await app.waitForText("┃ line one")
    await app.waitForText("┃ line three")
    expect(app.controller.ui.composerInput?.text).toBe("")
    expect(composerRows(app).rows).toHaveLength(1)

    // 11 lines cap composer box height at 8 rows and scroll.
    for (let i = 1; i <= 11; i++) {
      await app.type(`row ${i}`)
      if (i < 11) await app.press("j", { ctrl: true })
    }
    const capped = composerRows(app)
    expect(capped.rows).toHaveLength(8)
    expect(capped.rows.at(-1)).toBe("row 11")
    await app.press("c", { ctrl: true })
    expect(app.controller.ui.composerInput?.text).toBe("")
    expect(app.quitCalls).toEqual([])

    // Cursor editing: Home/End/Left/Backspace/Delete
    await app.type("abc")
    await app.press("j", { ctrl: true })
    await app.type("xyz")
    await app.press("HOME")
    await app.type(">")
    await app.press("END")
    await app.press("BACKSPACE")
    await app.press("ARROW_UP")
    await app.press("END")
    await app.press("ARROW_LEFT")
    await app.press("DELETE")
    expect(app.controller.ui.composerInput?.text).toBe("ab\n>xy")
    await app.press("ESCAPE")
    await app.waitFor(() => (app!.controller.ui.composerInput?.text ?? "") === "", "esc cleared draft")

    // History navigation (Up/Down) restores draft.
    await app.submit("first prompt")
    await app.waitForText("┃ first prompt")
    await app.submit("second prompt")
    await app.waitForText("┃ second prompt")
    await app.type("my draft")
    await app.press("ARROW_UP")
    expect(app.controller.ui.composerInput?.text).toBe("second prompt")
    await app.press("ARROW_UP")
    expect(app.controller.ui.composerInput?.text).toBe("first prompt")
    await app.press("ARROW_DOWN")
    expect(app.controller.ui.composerInput?.text).toBe("second prompt")
    await app.press("ARROW_DOWN")
    expect(app.controller.ui.composerInput?.text).toBe("my draft")

    // Ctrl+D on non-empty input deletes forward instead of quitting; on empty quits with background.
    await app.press("HOME")
    await app.press("d", { ctrl: true })
    expect(app.quitCalls).toEqual([])
    expect(app.controller.ui.composerInput?.text).toBe("y draft")
    await app.press("c", { ctrl: true })
    await app.press("d", { ctrl: true })
    expect(app.quitCalls).toEqual(["background"])
  })

  test("!shell turns, /approve, @file completion at 80 cols, and Esc interrupt while turn runs", async () => {
    app = await renderApp({
      width: 80,
      height: 37,
      seed: {
        files: { "notes-alpha.md": "alpha\n", "notes-beta.md": "beta\n", "other.txt": "other\n" },
        onTurn: (sessionId, body, backend) => {
          if (body.shell) {
            const shell = body.shell as { command?: string }
            const userId = "msg_shell_u_1"
            const msgId = "msg_shell_1"
            const inputJson = JSON.stringify({ command: shell.command })
            const outputJson = JSON.stringify({ stdout: "hello\n" })
            const list = backend.messages.get(sessionId) ?? []
            list.push(
              { id: userId, role: "ROLE_USER", parts: [{ id: `${userId}_t`, text: { text: "The following tool was executed by the user" } }], finish: "FINISH_REASON_STOP" },
              {
                id: msgId, role: "ROLE_ASSISTANT", agent: "hya-main",
                parts: [{ id: `${msgId}_p`, toolCall: { tool: "bash", state: "TOOL_EXECUTION_STATE_OK", inputJson, outputJson, durationMs: "5" } }],
                finish: "FINISH_REASON_STOP",
              },
            )
            backend.messages.set(sessionId, list)
            backend.emitSession(sessionId, { messageStarted: { message: userId, role: "ROLE_USER" } })
            backend.emitSession(sessionId, { partStarted: { message: userId, part: `${userId}_t`, kind: "text" } })
            backend.emitSession(sessionId, { partAppended: { message: userId, part: `${userId}_t`, textDelta: "The following tool was executed by the user" } })
            backend.emitSession(sessionId, { messageFinished: { message: userId, finish: "FINISH_REASON_STOP" } })
            backend.emitSession(sessionId, { messageStarted: { message: msgId, role: "ROLE_ASSISTANT" } })
            backend.emitSession(sessionId, { partStarted: { message: msgId, part: `${msgId}_p`, kind: "tool_call", tool: "bash" } })
            backend.emitSession(sessionId, { toolStateChanged: { message: msgId, part: `${msgId}_p`, tool: "bash", state: "TOOL_EXECUTION_STATE_OK", inputJson, outputJson, durationMs: "5" } })
            backend.emitSession(sessionId, { messageFinished: { message: msgId, finish: "FINISH_REASON_STOP" } })
            return { id: msgId, state: "TURN_STATE_FINISHED", finish: "FINISH_REASON_STOP" }
          }
          return { id: "msg_hang", state: "TURN_STATE_RUNNING" }
        },
      },
    })
    // `!command` switches title to `! shell`, submits without permission ask, and shows output.
    await app.type("!echo hello")
    expect(composerRows(app).title).toBe("! shell")
    await app.press("RETURN")
    await app.waitForText("┃ !echo hello")
    expect(composerRows(app).title).toBe("")

    // `@file` completion filters, ArrowDown selects, Tab/Enter inserts without submitting.
    await app.type("read @notes")
    await app.waitForText("▸ notes-beta.md")
    expect(app.frame()).not.toContain("other.txt")
    await app.press("ARROW_DOWN")
    await app.waitForText("▸ notes-alpha.md")
    await app.press("TAB")
    expect(app.controller.ui.composerInput?.text).toBe("read @notes-alpha.md ")
    for (const line of app.lines()) expect(line.length).toBeLessThanOrEqual(80)
    await app.press("c", { ctrl: true })

    // Running turn: Esc interrupts without clearing typed draft text.
    await app.submit("hang please")
    await app.waitForText("Esc to interrupt")
    await app.type("next")
    await app.press("ESCAPE")
    await app.waitForText("! Cancelled")
    expect(app.controller.ui.composerInput?.text).toBe("next")
  })
})

describe("keybindings customization and persistence", () => {
  test("list/show/set/unset/reset, aligned columns at 80 cols, scope gating, and persistence across restart", async () => {
    const configDir = tempDir("hya-keybind-")
    app = await renderApp({ preset: "workspace", width: 156, height: 41, configDir })
    await app.type("draft survives")

    // Inspect quit without quitting.
    await runCmd(app, "/keybind show quit")
    await app.waitForText("Keybinding · quit")
    await app.waitForText("Command: /exit")
    await app.press("ESCAPE")
    await app.waitForGone("Keybinding · quit")
    expect(app.quitCalls).toEqual([])

    // Invalid show target & invalid set command show errors cleanly.
    await runCmd(app, "/keybind show")
    await app.waitForText("Usage: /keybind show")
    await app.press("ESCAPE")
    await runCmd(app, "/keybind set F7 invalid")
    await app.waitForText("single slash command")
    await app.press("ESCAPE")

    // Set F6 to focus pane-3, verify aligned columns at 80 cols, and verify persistence.
    await runCmd(app, "/keybind set F6 /layout focus pane-3")
    await app.waitForText("Keybind saved · F6")
    await app.press("ESCAPE")
    await app.waitForGone("Keybind saved · F6")
    expect(JSON.parse(readFileSync(app.preferencesPath, "utf8")).keybindings).toEqual({
      F6: { command: "/layout focus pane-3", scope: "workspace" },
    })

    await app.resize(80, 37)
    await runCmd(app, "/keybind list")
    await app.waitForText(/F6\s+\/layout focus pane-3\s+workspace/)
    for (const line of app.lines()) expect(line.length).toBeLessThanOrEqual(80)
    await app.press("ESCAPE")

    // Unset Ctrl+C suppresses default exit & clear; reset restores it.
    await runCmd(app, "/keybind unset Ctrl+C")
    await app.waitForText("Keybind unset")
    await app.press("ESCAPE")
    await app.press("c", { ctrl: true })
    await app.press("c", { ctrl: true })
    expect(app.quitCalls).toEqual([])
    expect(app.controller.ui.composerInput?.text).toBe("draft survives")

    await runCmd(app, "/keybind reset Ctrl+C")
    await app.waitForText("Keybind reset")
    await app.press("ESCAPE")
    await app.press("c", { ctrl: true })
    expect(app.controller.ui.composerInput?.text).toBe("")

    // Restart preserves F6 shortcut.
    await app.dispose()
    app = await renderApp({ preset: "workspace", width: 156, height: 41, configDir })
    await app.press("F6")
    await app.waitFor(() => app!.store.state.paneLayout.active === "pane-3", "F6 focused pane-3 after restart")
  })
})

describe("vim mode, external editor, and clipboard UI", () => {
  test("/vim toggles & persists, motions/dd/undo/redo/A/o work, and Esc precedence (insert->normal->cancel->clear)", async () => {
    const configDir = tempDir("hya-vim-")
    app = await renderApp({
      width: 80,
      height: 37,
      configDir,
      seed: { onTurn: () => ({ id: "msg_vim_hang", state: "TURN_STATE_RUNNING" }) },
    })
    await runCmd(app, "/vim")
    expect(app.store.state.vim).toBe(true)
    expect(JSON.parse(readFileSync(app.preferencesPath, "utf8")).vim).toBe(true)

    await app.type("alpha beta gamma")
    await app.press("ESCAPE")
    expect(app.store.state.vimMode).toBe("normal")
    expect(app.controller.ui.composerInput?.text).toBe("alpha beta gamma")

    // Normal mode motions & edits: 0wdw -> u -> Ctrl+R -> dd -> u
    for (const ch of "0wdw") await app.press(ch)
    expect(app.controller.ui.composerInput?.text).toBe("alpha gamma")
    await app.press("u")
    expect(app.controller.ui.composerInput?.text).toBe("alpha beta gamma")
    await app.press("r", { ctrl: true })
    expect(app.controller.ui.composerInput?.text).toBe("alpha gamma")
    await app.press("d")
    await app.press("d")
    expect(app.controller.ui.composerInput?.text).toBe("")
    await app.press("u")
    expect(app.controller.ui.composerInput?.text).toBe("alpha gamma")

    // Running turn + Esc precedence: 1st Esc insert->normal, 2nd Esc cancels turn, 3rd Esc clears draft.
    await app.press("RETURN") // Enter in normal mode sends!
    await app.waitForText("Esc to interrupt")
    expect(app.store.state.vimMode).toBe("insert")
    await app.type("draft")
    await app.press("ESCAPE")
    expect(app.store.state.vimMode).toBe("normal")
    expect(app.store.state.running).toBe(true)
    await app.press("ESCAPE")
    await app.waitForText("! Cancelled")
    expect(app.controller.ui.composerInput?.text).toBe("draft")
    await app.press("ESCAPE")
    expect(app.controller.ui.composerInput?.text).toBe("")

    // Restart preserves vim: true; `/vim off` disables it.
    await app.dispose()
    app = await renderApp({ width: 80, height: 37, configDir })
    expect(app.store.state.vim).toBe(true)
    await runCmd(app, "/vim off")
    expect(app.store.state.vim).toBe(false)
    await app.type("hjkl")
    expect(app.controller.ui.composerInput?.text).toBe("hjkl")
  })

  test("/editor runs $VISUAL/$EDITOR, preserves input on failure, and /copy sends reply to terminal.copy", async () => {
    const dir = tempDir("hya-editor-")
    const okScript = join(dir, "ok.sh")
    writeFileSync(okScript, `#!/bin/sh\nprintf 'edited: %s\\nsecond line' "$(cat "$1")" > "$1"\n`)
    chmodSync(okScript, 0o755)
    const failScript = join(dir, "fail.sh")
    writeFileSync(failScript, "#!/bin/sh\nexit 3\n")
    chmodSync(failScript, 0o755)

    app = await renderApp({
      width: 100,
      height: 37,
      env: { ...process.env, VISUAL: "", EDITOR: okScript },
      seed: {
        sessions: [{ id: "hysec_copy", agent: "hya-main", workdir: "/workspace", projectId: "prj_work", model: { providerId: "fake", modelId: "model" } }],
        messages: {
          hysec_copy: [
            { id: "m1", role: "ROLE_USER", parts: [{ id: "p1", text: { text: "hi" } }] },
            { id: "m2", role: "ROLE_ASSISTANT", agent: "hya-main", model: "fake/model", parts: [{ id: "p2", text: { text: "copy-target-reply" } }], finish: "FINISH_REASON_STOP" },
          ],
        },
      },
    })
    await app.type("draft words")
    await runCmd(app, "/editor")
    await app.waitFor(() => app!.controller.ui.composerInput?.text === "edited: draft words\nsecond line", "editor output")
    expect(app.terminal.suspended).toBe(1)
    expect(app.terminal.resumed).toBe(1)

    // /copy calls terminal.copy with the last assistant reply.
    await runCmd(app, "/copy")
    expect(app.terminal.copied).toEqual(["copy-target-reply"])

    // Failing editor preserves existing input.
    await app.dispose()
    app = await renderApp({ width: 100, height: 37, env: { ...process.env, VISUAL: "", EDITOR: failScript } })
    await app.type("keep this")
    await runCmd(app, "/editor")
    await app.waitFor(() => app!.terminal.resumed === 1, "failed editor resumed")
    expect(app.controller.ui.composerInput?.text).toBe("keep this")
  })
})

describe("paste chips, pasted paths, SSH hints, and image attachments", () => {
  test("30-line paste chip, Backspace deletion, /paste expand, image chips/BMP->PNG, SSH hints, and model modality rejection", async () => {
    const work = tempDir("hya-paste-work-")
    writeFileSync(join(work, "shot 1.png"), pngBytes(2048))
    writeFileSync(join(work, "tiny.bmp"), bmpBytes())
    writeFileSync(join(work, "notes.txt"), "notes\n")
    writeFileSync(join(work, "huge.png"), pngBytes(11 * 1024 * 1024))

    app = await renderApp({
      width: 120,
      height: 37,
      seed: {
        directory: work,
        autoReply: "got it",
        files: { "shot.png": pngBytes(2048), "notes.txt": "notes\n" },
      },
    })

    // 30-line paste -> chip -> 2 Backspaces remove whole chip.
    const thirty = Array.from({ length: 30 }, (_, i) => `const value${i + 1} = compute(${i + 1})`).join("\n")
    await app.paste(thirty)
    await app.waitForText("[paste #1 · 30 lines]")
    expect(app.find("const value30")).toBeNull()
    await app.press("BACKSPACE")
    await app.press("BACKSPACE")
    await app.waitForGone("[paste #1")

    // /paste expand restores chip text inline.
    await app.paste(Array.from({ length: 12 }, (_, i) => `row-${i + 1}`).join("\n"))
    await app.waitForText("[paste #2 · 12 lines]")
    await runCmd(app, "/paste expand")
    await app.waitForText("row-12")
    expect(app.find("[paste #2")).toBeNull()
    await app.press("c", { ctrl: true })

    // Pasted workspace text file -> @mention; image path with escaped space + BMP -> image chips sent as PNG.
    await app.paste(`${work}/notes.txt`)
    await app.waitForText("@notes.txt")
    await app.press("ESCAPE")
    await app.paste(`${work}/shot\\ 1.png ${work}/tiny.bmp`)
    await app.waitForText(/\[image #\d+ shot 1\.png\]/)
    await app.waitForText(/\[image #\d+ tiny\.bmp\]/)
    await app.type("compare")
    await app.press("RETURN")
    await app.waitForText("got it")
    const turnCall = app.backend.calls.findLast((c) => c.method === "POST" && c.path.endsWith("/turns"))
    const promptBody = (turnCall?.body as { prompt?: { attachments?: Array<{ name: string; mime?: string }> } })?.prompt
    expect(promptBody?.attachments?.map((a) => [a.name, a.mime])).toEqual([
      ["shot 1.png", "image/png"],
      ["tiny.bmp", "image/png"],
    ])

    // Unquoted spaced image path and oversized image are refused locally without posting a turn.
    const beforeCount = app.backend.calls.filter((c) => c.path.endsWith("/turns")).length
    await app.type("@shot 1.png describe")
    await app.waitForText("[image] shot 1.png · has a space")
    await app.press("RETURN")
    await app.press("c", { ctrl: true })
    await app.submit("@huge.png describe")
    expect(app.backend.calls.filter((c) => c.path.endsWith("/turns")).length).toBe(beforeCount)

    // SSH hints + text-only model refusal.
    await app.dispose()
    app = await renderApp({
      width: 120,
      height: 37,
      env: { ...process.env, SSH_CONNECTION: "10.0.0.2 50000 10.0.0.1 22" },
      seed: {
        directory: work,
        models: [{ id: "fake/textonly", providerId: "fake", modelId: "textonly", imageInput: false, source: "config" }],
        sessions: [{ id: "hysec_txt", agent: "hya-main", workdir: work, projectId: "prj_work", model: { providerId: "fake", modelId: "textonly" } }],
      },
    })
    await app.press("v", { ctrl: true })
    await app.waitForText("The clipboard here belongs to the SSH host")
    await app.paste("/Users/me/Desktop/shot.png")
    await app.waitForText("is on your machine, not on the SSH host")
    await app.press("c", { ctrl: true })
    await app.submit("@shot 1.png describe")
    expect(app.backend.calls.filter((c) => c.path.endsWith("/turns"))).toHaveLength(0)
  })
})
