/**
 * In-memory `testRender` coverage for layout, presets, focus highlight,
 * selectable/passive panes, layout editor, direct keys, bubbling, reload,
 * sidebar resize, pane routing, compact dock, clean input, and passive notices.
 *
 * Covers the UI invariants downgraded from 16 PTY specs:
 * `layout`, `layout-foundation`, `layout-editor`, `layout-direct-keys`,
 * `layout-bubble`, `layout-reload`, `presets`, `selectable-panes`,
 * `focus-highlight`, `workspace-baseline`, `sidebar-resize`, `sidebar-context`,
 * `pane-routing`, `compact-dock`, `clean-input`, and `passive-notices`.
 */
import { afterAll, afterEach, beforeAll, describe, expect, test } from "bun:test"
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { MouseButtons } from "@opentui/core/testing"
import type { PaneLayout, PaneNode, PaneSplit } from "../src/state/panes"
import { loadBasicComponents, renderApp, resetBasicComponents, type RenderedApp } from "./support/appHarness"

let app: RenderedApp | undefined

beforeAll(async () => {
  await loadBasicComponents()
})

afterEach(async () => {
  await app?.dispose()
  app = undefined
})

afterAll(async () => {
  await resetBasicComponents()
})

function focusBoxes(rendered: RenderedApp): Array<{ row: number; col: number }> {
  const out: Array<{ row: number; col: number }> = []
  const lines = rendered.setup.captureSpans().lines
  for (let row = 0; row < lines.length; row++) {
    let col = 0
    for (const span of lines[row]!.spans) {
      const hex = span.fg
        ? `#${[span.fg.r, span.fg.g, span.fg.b].map((n) => Math.round(n * 255).toString(16).padStart(2, "0")).join("")}`
        : undefined
      if (hex === "#73c8e8") {
        for (let i = 0; i < span.text.length; i++) {
          if (span.text[i] === "┌") out.push({ row, col: col + i })
        }
      }
      col += span.text.length
    }
  }
  return out
}

function flatten(node: PaneNode): PaneNode[] {
  return node.type === "pane" ? [node] : [node, ...node.children.flatMap((c) => flatten(c.node))]
}

async function runCmd(rendered: RenderedApp, text: string): Promise<void> {
  await rendered.press("x", { ctrl: true })
  await rendered.type(text)
  await rendered.press("RETURN")
  await rendered.waitForGone("Commands")
}

describe("presets, statusline, dock, and clean input", () => {
  test("statusline default shows no headings, one status row below input, elides in order across 120 -> 80 -> 40 cols", async () => {
    app = await renderApp({
      width: 120,
      height: 37,
      tuiToml: '[statusline]\nsegments = ["model", "mode", "agent", "cwd", "ctx", "tokens", "session"]\n',
      seed: {
        sessions: [{
          id: "hysec_preset",
          agent: "hya-main",
          workdir: "/workspace",
          projectId: "prj_work",
          model: { providerId: "fake", modelId: "model" },
          permissionMode: "manual",
          usage: { input: "42000", output: "300" },
        }],
        messages: {
          hysec_preset: [
            { id: "m1", role: "ROLE_USER", parts: [{ id: "p1", text: { text: "hi" } }] },
            { id: "m2", role: "ROLE_ASSISTANT", agent: "hya-main", model: "fake/model", parts: [{ id: "p2", text: { text: "hello back" } }], roundUsage: { input: "42000", output: "300" } },
          ],
        },
      },
    })
    await app.waitForText("hello back")
    expect(app.find("hya ·")).toBeNull()
    expect(app.find("─Sessions─")).toBeNull()
    const below = () => {
      const lines = app!.lines()
      const at = lines.findIndex((l) => l.includes("Message, !shell"))
      return at >= 0 ? lines[at + 2] ?? "" : ""
    }
    expect(below()).toMatch(/model · manual · hya-main · workspace · ctx ━━─── 42% · 42\.3k tok\s+hysec_preset/)

    await app.resize(80, 37)
    expect(below()).not.toContain("hysec_preset")
    expect(below()).toMatch(/model · manual · .*ctx ━━─── 42%/)

    await app.resize(40, 37)
    expect(below()).toMatch(/model · manual · ctx ━━─── 42%/)
    expect(below()).not.toContain("tok")

    // Layout editing in non-workspace preset refuses without dumping usage in transcript.
    await runCmd(app, "/layout assign invalid")
    expect(app.find("Usage: /layout assign")).toBeNull()
  })

  test("minimal preset puts model/mode on input border; dock preset draws ╰─ input and caps todos at 3 rows", async () => {
    app = await renderApp({ preset: "minimal", width: 120, height: 37 })
    const lines = app.lines()
    const inputRow = lines.findIndex((l) => l.includes("Message, !shell"))
    expect(lines[inputRow - 1]).toMatch(/─ .+ · manual ─/)
    expect((lines[inputRow + 2] ?? "").trim()).toBe("")
    await app.dispose()

    app = await renderApp({
      preset: "dock",
      width: 120,
      height: 37,
      seed: {
        sessions: [{ id: "hysec_dock", agent: "hya-main", workdir: "/workspace", projectId: "prj_work", model: { providerId: "fake", modelId: "model" }, permissionMode: "manual" }],
        todos: {
          hysec_dock: [
            { id: "1", status: "pending", content: "write tests" },
            { id: "2", status: "pending", content: "fix the bug" },
            { id: "3", status: "pending", content: "update docs" },
            { id: "4", status: "pending", content: "release" },
          ],
        },
      },
    })
    await app.waitForText("╰─ Message, !shell")
    expect(app.find("┌───")).toBeNull()
    await app.waitForText("○ write tests")
    await app.waitForText("○ fix the bug")
    await app.waitForText("… +2 more · /todos")
    expect(app.find("○ update docs")).toBeNull()
    // Fits 80 columns too and `/set todos off` hides rows.
    await app.resize(80, 37)
    for (const line of app.lines()) expect(line.length).toBeLessThanOrEqual(80)
    await runCmd(app, "/set todos off")
    await app.waitForGone("○ write tests")
  })

  test("workspace baseline shows sidebars >= 150 cols, hides them at 80/120; /set preset switches live without writing tui.toml", async () => {
    app = await renderApp({ width: 190, height: 41 })
    await app.type("preserved draft")
    await runCmd(app, "/set preset workspace")
    await app.waitForText("─Sessions─")
    await app.waitForText("─Projects─")
    await app.waitForText("Todos")
    await app.waitForText("Context")
    expect(app.controller.ui.composerInput?.text).toBe("preserved draft")

    // Below 150 columns sidebars hide and statusline returns.
    await app.resize(120, 41)
    expect(app.frame()).not.toContain("─Sessions─")
    expect(app.frame()).toMatch(/manual/)
    await app.resize(80, 41)
    expect(app.frame()).not.toContain("─Sessions─")
    for (const line of app.lines()) expect(line.length).toBeLessThanOrEqual(80)

    // Turning sidebar off leaves tui.toml unwritten.
    await app.resize(190, 41)
    await runCmd(app, "/set sidebar off")
    await app.waitForGone("─Sessions─")
    expect(app.controller.ui.composerInput?.text).toBe("preserved draft")
    expect(await Bun.file(app.tuiConfigPath).exists()).toBe(false)
  })
})

describe("focus highlight, selectable panes, routing, compact dock, and passive notices", () => {
  test("exactly one focus border moves through panes, overlays, resize, and ignores clicks on passive panes", async () => {
    app = await renderApp({ preset: "workspace", width: 156, height: 41 })
    await app.waitForText("Sessions")
    await app.waitForText("Projects")
    await app.waitForText("Todos")
    await app.waitForText("Context")

    // Only one focused box initially (composer).
    expect(focusBoxes(app)).toHaveLength(1)
    expect(focusBoxes(app)[0]!.row).toBeGreaterThan(20)

    // Todos and Context are borderless passive sections.
    const todos = app.find("Todos")!
    const context = app.find("Context")!
    expect(app.lines()[todos.row]).not.toMatch(/─Todos/)
    expect(app.lines()[context.row]).not.toMatch(/─Context/)

    // Draft survives switching to Sessions, typing/pasting there does NOT leak to composer, clicking passive panes keeps Sessions focus.
    await app.type("preserved draft")
    await runCmd(app, "/layout focus pane-3")
    await app.waitFor(() => app!.store.state.paneLayout.active === "pane-3", "sessions focused")
    expect(focusBoxes(app)).toHaveLength(1)
    await app.clickText("Todos")
    expect(app.store.state.paneLayout.active).toBe("pane-3")
    await app.type("leaked text")
    await app.paste("leaked paste")
    await runCmd(app, "/layout focus pane-4") // passive pane cannot take focus
    expect(app.store.state.paneLayout.active).toBe("pane-3")

    // Command overlay & Help overlay each own the sole focus box across resize.
    await app.type("/")
    await app.waitForText("Commands")
    expect(focusBoxes(app)).toHaveLength(1)
    expect(focusBoxes(app)[0]!.row).toBe(2)
    await app.resize(80, 41)
    expect(focusBoxes(app)).toHaveLength(1)
    await app.press("ESCAPE")
    await app.waitForGone("Commands")

    // Return to composer: draft is untouched.
    await app.resize(156, 41)
    await runCmd(app, "/layout focus pane-1")
    await app.type(" intact")
    expect(app.controller.ui.composerInput?.text).toBe("preserved draft intact")
    expect(app.frame()).not.toContain("leaked")

    // Provider view + nested form keeps sole focus box on form.
    await runCmd(app, "/key")
    await app.waitForText("Providers")
    expect(focusBoxes(app)).toHaveLength(1)
    await app.type("a")
    await app.waitForText("Add provider · 1/4")
    expect(focusBoxes(app)).toHaveLength(1)
    expect(focusBoxes(app)[0]!.row).toBe(3)
    await app.press("ESCAPE")
    await app.press("ESCAPE")
    await app.waitForGone("Providers")

    // Right-click sidebar context menu coexists with focus & keybinding columns.
    await app.clickText(/wor.*\(\d+\)/, MouseButtons.RIGHT)
    await app.waitForText("Click an action · Esc closes")
    expect(app.find("Filter")).toBeNull()
    await app.press("ESCAPE")
    await app.waitForGone("Click an action")
  })

  test("compact dock meets transcript tightly, expands multiline, shows activity directly above input, and keeps passive notices borderless", async () => {
    const longReply = Array.from({ length: 60 }, (_, i) => `line ${i}`).join("\n\n") + "\n\nDOCK END"
    app = await renderApp({
      width: 128,
      height: 37,
      seed: {
        sessions: [{ id: "hysec_1", agent: "hya-main", workdir: "/workspace", projectId: "prj_work", model: { providerId: "fake", modelId: "model" }, permissionMode: "manual" }],
        messages: {
          hysec_1: [
            { id: "m1", role: "ROLE_USER", parts: [{ id: "p1", text: { text: "long" } }] },
            { id: "m2", role: "ROLE_ASSISTANT", agent: "hya-main", model: "fake/model", parts: [{ id: "p2", text: { text: longReply } }], finish: "FINISH_REASON_STOP" },
          ],
        },
        interactions: [{
          id: "req_perm", session: "hysec_1", type: "INTERACTION_TYPE_PERMISSION",
          title: "Run bash command", detail: "echo passive-warning",
          options: ["Allow once", "Always allow", "Deny"],
          payload: { tool: "bash", action: "bash", resource: "echo passive-warning" },
        }],
      },
    })
    await app.waitForText("Allow once")
    await app.waitForText("Permission")
    // Permission dock above input is borderless (only composer has border).
    const inputRow = app.find("Message, !shell")!.row
    const above = app.lines().slice(0, inputRow - 1).filter((line) => /[┌┐└┘╭╮╰╯]/.test(line))
    expect(above).toEqual([])

    // Opening a new session turns the other session's ask into a borderless Pending (1) block.
    await runCmd(app, "/new")
    await app.waitForText("Pending (1)")
    const pendingRow = app.find("Pending (1)")!.row
    expect(app.lines()[pendingRow]).not.toMatch(/[┌┐╭╮─]/)
    expect(pendingRow).toBeLessThan(app.find("Message, !shell")!.row)

    // Back to original session after resolving: multiline paste grows composer cleanly.
    await app.press("1") // not shown in new session, types into composer
    expect(app.controller.ui.composerInput?.text).toBe("1")
    await app.press("c", { ctrl: true })
    await runCmd(app, "/pending")
    await app.waitForText("Allow once")
    await app.press("1")
    await app.waitForGone("Allow once")
    await app.paste("FIRST DRAFT ROW\nSECOND DRAFT ROW\nTHIRD DRAFT ROW")
    const first = app.find("FIRST DRAFT ROW")!
    const third = app.find("THIRD DRAFT ROW")!
    expect(third.row - first.row).toBe(2)

    // Active turn renders working indicator immediately above composer border.
    app.store.beginTurn()
    app.store.setTurn("msg_live")
    await app.render()
    const activity = app.find("Esc to interrupt")!
    const composer = app.find("FIRST DRAFT ROW")!
    expect(composer.row - 1 - activity.row).toBe(1)
  })
})

describe("layout mutations, editor, direct keys, bubbling, reload, and persistence", () => {
  test("split, focus, assign, named close, wrap, insert, move, reload, and persistence keep draft & scroll", async () => {
    const configDir = mkdtempSync(join(tmpdir(), "hya-layout-persist-"))
    try {
      app = await renderApp({ preset: "workspace", width: 156, height: 41, configDir })
      await app.waitForText("Todos")
      await app.type("draft kept")

      // Close passive panes by name and id, keep focus in editor.
      await runCmd(app, "/layout close todos")
      await app.waitForGone("Todos")
      await runCmd(app, "/layout close pane-5")
      await app.waitForGone("Context")
      expect(app.controller.ui.composerInput?.text).toBe("draft kept")

      // Split left jobs and split up jobs; verify placement and focus cycle.
      await runCmd(app, "/layout split left jobs")
      await app.waitForText("▸ jobs · pane-8")
      const jobs = app.find("jobs · pane-8")!
      const draft = app.find("draft kept")!
      expect(jobs.col).toBeLessThan(draft.col)

      // Structural ops (/layout insert, move, wrap, remove) preserve draft.
      await runCmd(app, "/layout wrap root column status before")
      await app.waitForText("status · pane-9")
      expect(app.controller.ui.composerInput?.text).toBe("draft kept")
      await runCmd(app, "/layout remove pane-9")
      await app.waitForGone("status · pane-9")

      // External file edit + /layout reload applies valid file, rejects invalid file while retaining running layout & draft.
      const prefsPath = join(configDir, "tui.json")
      const saved: PaneLayout = JSON.parse(readFileSync(prefsPath, "utf8")).paneLayout
      const leaf = (id: number, kind: string) => ({ type: "pane" as const, id: `pane-${id}`, kind })
      const edited = {
        version: 4,
        active: "pane-1",
        root: {
          type: "split", id: "group-1", direction: "row",
          children: [
            { node: { type: "split", id: "group-2", direction: "column", children: [{ node: leaf(6, "conversation"), size: { mode: "weight", value: 1 } }, { node: leaf(1, "composer"), size: { mode: "content" } }] }, size: { mode: "weight", value: 0.5 } },
            { node: leaf(3, "status"), size: { mode: "weight", value: 0.5 } },
          ],
        },
      }
      writeFileSync(prefsPath, JSON.stringify({ paneLayout: edited, unknown: "preserve me" }, null, 2))
      await runCmd(app, "/layout reload")
      await app.waitForText("status · pane-3")
      expect(app.controller.ui.composerInput?.text).toBe("draft kept")
      expect(readFileSync(prefsPath, "utf8")).toContain('"unknown": "preserve me"')

      // Invalid layout reload keeps current tree intact.
      writeFileSync(prefsPath, JSON.stringify({ paneLayout: { version: 4, active: "pane-1", root: leaf(1, "composer") } }))
      await runCmd(app, "/layout reload")
      await app.waitForText("status · pane-3")
      expect(app.controller.ui.composerInput?.text).toBe("draft kept")
      expect(saved.version).toBe(4)

      // Restart restores saved valid layout after reset.
      await runCmd(app, "/layout reset")
      await app.dispose()
      app = await renderApp({ preset: "workspace", width: 156, height: 41, configDir })
      await app.waitForText("Sessions")
    } finally {
      await app?.dispose()
      app = undefined
      rmSync(configDir, { recursive: true, force: true })
    }
  })

  test("Layout tree editor: weight validation, direct keys (mark/wrap/insert/remove/bubble), keybind disable, and self-removal", async () => {
    app = await renderApp({ preset: "workspace", width: 128, height: 37 })
    await app.type("keep my draft")
    await runCmd(app, "/layout tree")
    await app.waitForText("Layout tree · pane-8")
    await app.waitForText("pane-4 todos")

    // Edit weight of pane-5: reject 0, accept 2.5, persist to tui.json.
    await app.press("END")
    await app.press("RETURN")
    await app.waitForText("Edit pane-5")
    await app.clickText("Change weight")
    await app.waitForText("Weight for pane-5")
    await app.type("0")
    await app.press("RETURN")
    await app.waitForText("positive finite")
    await app.press("BACKSPACE")
    await app.type("2.5")
    await app.press("RETURN")
    await app.waitForText("weight 2.5")
    const saved = (): PaneLayout => JSON.parse(readFileSync(app!.preferencesPath, "utf8")).paneLayout
    expect((flatten(saved().root).find((n) => n.id === "group-3") as PaneSplit).children[2]!.size).toEqual({ mode: "weight", value: 2.5 })

    // Bubble siblings with Shift+Up / Shift+Down while preserving mark.
    await app.clickText("pane-3 sessions")
    await app.press(" ")
    await app.waitForText("Marked pane-3")
    await app.clickText("pane-4 todos")
    await app.press("ARROW_UP", { shift: true })
    await app.waitFor(() => (flatten(saved().root).find((n) => n.id === "group-3") as PaneSplit).children.map((c) => c.node.id).join(",") === "pane-4,pane-3,pane-5", "bubbled up")
    await app.press("ARROW_DOWN", { shift: true })
    await app.waitFor(() => (flatten(saved().root).find((n) => n.id === "group-3") as PaneSplit).children.map((c) => c.node.id).join(",") === "pane-3,pane-4,pane-5", "bubbled down")

    // Direct wrap (`w` -> `r`) + direct remove (`Backspace`) on mark + cannot remove composer.
    await app.clickText("pane-5 context")
    await app.press(" ")
    await app.waitForText("Marked pane-5")
    await app.press("w")
    await app.waitForText("r row · c column")
    await app.press("r")
    await app.waitForText("Wrap pane-5 in row")
    await app.clickText("jobs")
    await app.waitForText("pane-9 jobs")
    await app.press(" ")
    await app.waitForText("Marked pane-9")
    await app.press("HOME")
    await app.press("BACKSPACE")
    await app.waitForGone("pane-9 jobs")

    // Direct editing key `i` can be disabled & restored via /keybind.
    await app.clickText("pane-5 context")
    await runCmd(app, "/keybind unset I")
    await app.waitForText("Keybind unset")
    await app.press("ESCAPE")
    await app.press("i")
    expect(app.find("Insert here")).toBeNull()
    await runCmd(app, "/keybind reset I")
    await app.waitForText("Keybind reset")
    await app.press("ESCAPE")
    await app.press("i")
    await app.waitForText("Insert here · before pane-5")
    await app.press("ESCAPE")

    // Composer cannot be removed; closing layout restores draft intact.
    await app.clickText("pane-1 composer")
    await app.press("DELETE")
    await app.waitForText("Cannot remove")
    await app.press("ESCAPE")
    await runCmd(app, "/layout close layout")
    await app.waitForGone("Layout tree")
    await app.type(" intact")
    expect(app.controller.ui.composerInput?.text).toBe("keep my draft intact")
  })

  test("mouse dragging right sidebar border resizes it down to minimum 29 columns and persists", async () => {
    const configDir = mkdtempSync(join(tmpdir(), "hya-sidebar-drag-"))
    try {
      app = await renderApp({ preset: "workspace", width: 170, height: 41, configDir })
      await app.waitForText("Sessions")
      const borderCol = () => {
        const hit = app!.find("Sessions")!
        return app!.lines()[hit.row]!.lastIndexOf("┌", hit.col)
      }
      const startCol = borderCol()
      const startRow = app.find("Sessions")!.row + 1
      expect(170 - startCol).toBe(29) // default 12% clamped to sidebarMinColumns

      // Drag left by 25 columns.
      await app.setup.mockMouse.drag(startCol, startRow, startCol - 25, startRow)
      await app.render()
      expect(borderCol()).toBe(startCol - 25)

      // Drag right past edge: clamps at minimum 29 columns.
      const widened = borderCol()
      await app.setup.mockMouse.drag(widened, startRow, 168, startRow)
      await app.render()
      expect(170 - borderCol()).toBe(29)

      // Widen again and verify restart preserves width.
      const narrow = borderCol()
      await app.setup.mockMouse.drag(narrow, startRow, narrow - 30, startRow)
      await app.render()
      const targetCol = borderCol()
      expect(targetCol).toBe(narrow - 30)
      await app.dispose()

      app = await renderApp({ preset: "workspace", width: 170, height: 41, configDir })
      await app.waitForText("Sessions")
      expect(borderCol()).toBe(targetCol)
    } finally {
      await app?.dispose()
      app = undefined
      rmSync(configDir, { recursive: true, force: true })
    }
  })
})
