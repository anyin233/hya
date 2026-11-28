/**
 * In-memory `testRender` coverage for the command pane, command overlay,
 * command dropdown height, core navigation commands, and the `?` / `/help`
 * overlay (migrated from `hya-tui-commands-menu`, `hya-tui-commands`,
 * `hya-tui-command-overlay`, `hya-tui-command-height`, and `hya-tui-help`).
 */
import { afterEach, describe, expect, test } from "bun:test"
import { renderApp, type RenderedApp } from "./support/appHarness"

let app: RenderedApp | undefined

afterEach(async () => {
  await app?.dispose()
  app = undefined
})

function commandBox(rendered: RenderedApp): { top: number; left: number; rows: string[] } | undefined {
  const lines = rendered.lines()
  const top = lines.findIndex((line) => line.includes("┌") && line.includes("Commands"))
  if (top < 0) return undefined
  const left = lines[top]!.lastIndexOf("┌", lines[top]!.indexOf("Commands"))
  let bottom = top + 1
  while (bottom < lines.length && lines[bottom]![left] !== "└") bottom++
  const rows = lines.slice(top + 1, bottom).map((line) => {
    const end = line.indexOf("│", left + 1)
    return line.slice(left + 1, end < 0 ? undefined : end).trim()
  })
  return { top, left, rows }
}

describe("command pane, overlay, and height", () => {
  test("floats above workspace, preserves drafts across resize, Backspace closes at wide & narrow widths", async () => {
    app = await renderApp({ preset: "workspace", width: 128, height: 37 })
    await app.submit("/layout split left jobs")
    await app.waitForText("Jobs")
    await app.press("ARROW_RIGHT", { meta: true })
    await app.type("keep this draft")
    expect(app.controller.ui.composerInput?.text).toBe("keep this draft")

    // Open command pane from another pane, verify floating geometry and draft preservation across resize.
    await app.press("ARROW_LEFT", { meta: true })
    await app.waitForText("Jobs")
    await app.type("/layout split")
    await app.waitForText("Commands")
    const boxWide = commandBox(app)!
    expect(boxWide.top).toBe(2)
    expect(boxWide.left).toBe(Math.max(0, Math.floor((128 - Math.min(96, 128 - 4)) / 2)))

    await app.resize(80, 37)
    await app.waitForText("Commands")
    expect(commandBox(app)!.top).toBe(2)
    await app.press("ESCAPE")
    await app.waitForGone("Commands")
    expect(app.controller.ui.composerInput?.text).toBe("keep this draft")

    // Reopening via Ctrl+X / restores command draft; Ctrl+C closes it.
    await app.press("ARROW_RIGHT", { meta: true })
    await app.press("x", { ctrl: true })
    await app.type("/")
    await app.waitForText("/layout split")
    await app.press("c", { ctrl: true })
    await app.waitForGone("Commands")

    // Backspace deleting the final `/` closes at narrow width too; Home+Backspace stays open.
    app.controller.ui.command?.open("/h")
    await app.render()
    await app.waitForText("Commands")
    await app.press("BACKSPACE")
    expect(commandBox(app)?.rows[0]).toBe("/")
    await app.press("HOME")
    await app.press("BACKSPACE")
    expect(commandBox(app)?.rows[0]).toBe("/")
    await app.press("END")
    await app.press("BACKSPACE")
    await app.waitForGone("Commands")
    expect(app.controller.ui.composerInput?.text).toBe("keep this draft")
  })

  test("dropdown caps at 12 visible rows, shrinks for short terminals, and wraps full list", async () => {
    app = await renderApp({
      preset: "workspace",
      width: 128,
      height: 37,
      seed: {
        commands: [
          { name: "review", description: "Review code", source: "command", scope: "builtin" },
          { name: "zz-navigation-last", description: "Last navigation choice", source: "command", scope: "project" },
        ],
      },
    })
    await app.type("/")
    await app.waitForText("▸ /agent")
    // 12 choice rows + input row + hint row = 14 inner rows
    expect(commandBox(app)!.rows.length - 2).toBe(12)

    // ArrowUp wraps to the final command; ArrowDown wraps back to /agent.
    await app.press("ARROW_UP")
    await app.waitForText("▸ /zz-navigation-last")
    await app.press("ARROW_DOWN")
    await app.waitForText("▸ /agent")

    // Stepping down reaches commands past the first 12 rows without early wrap.
    const visited = new Set<string>()
    let selected = commandBox(app)?.rows.find((row) => row.startsWith("▸ ")) ?? ""
    visited.add(selected)
    for (let step = 0; step < 45 && !selected.startsWith("▸ /layout "); step++) {
      await app.press("ARROW_DOWN")
      selected = commandBox(app)?.rows.find((row) => row.startsWith("▸ ")) ?? ""
      expect(visited.has(selected)).toBe(false)
      visited.add(selected)
      expect(commandBox(app)!.rows.length - 2).toBeLessThanOrEqual(12)
    }
    expect(selected).toMatch(/^▸ \/layout /)

    // Resize to a short terminal: choices shrink to rows - 6.
    await app.resize(80, 13)
    await app.waitForText("Commands")
    expect(commandBox(app)!.rows.length - 2).toBe(Math.max(1, 13 - 6))
    await app.press("ESCAPE")
    await app.waitForGone("Commands")
  })

  test("nested layout & API completion, filtering with source tags, history, paste, and argument hints", async () => {
    app = await renderApp({
      preset: "workspace",
      width: 128,
      height: 37,
      seed: {
        autoReply: "echoed",
        commands: [
          { name: "review", description: "Review changes", source: "command", scope: "builtin" },
          { name: "skill:greet", description: "Say hello", source: "skill", scope: "project" },
        ],
      },
    })

    // Filtering + source tags
    await app.type("/rev")
    await app.waitForText("▸ /review")
    expect(commandBox(app)?.rows.some((r) => r.includes("/agent"))).toBe(false)
    await app.press("ESCAPE")
    await app.waitForGone("Commands")

    // Skill tag + unknown command suggestion
    await app.press("x", { ctrl: true })
    await app.type("/")
    await app.waitForText("Commands")
    // Reopen restores previous command draft `/rev`.
    expect(commandBox(app)?.rows[0]).toBe("/rev")
    await app.press("ESCAPE")
    await app.waitForGone("Commands")
    app.controller.ui.command?.open("/skill:gre")
    await app.render()
    await app.waitForText("▸ /skill:greet")
    expect(commandBox(app)?.rows.find((r) => r.includes("/skill:greet"))).toContain("[skill · project]")
    app.controller.ui.command?.open("/greet world")
    await app.render()
    await app.press("RETURN")
    await app.waitForText("Unknown command /greet · did you mean /skill:greet?")

    // Nested /layout completion with Tab + Enter
    await app.type("/layout s")
    await app.waitForText("▸ /layout show")
    await app.press("ARROW_DOWN")
    await app.waitForText("▸ /layout split")
    await app.press("TAB")
    await app.waitForText("▸ /layout split left")
    await app.press("ARROW_DOWN")
    await app.waitForText("▸ /layout split up")
    await app.press("TAB")
    await app.type("jo")
    await app.waitForText("▸ /layout split up jobs")
    await app.press("TAB")
    await app.press("RETURN")
    await app.waitForText("Jobs")
    // Return focus to composer (`pane-1`) so later paste & composer assertions target the composer.
    app.controller.ui.command?.open("/layout focus pane-1")
    await app.render()
    await app.press("RETURN")
    await app.waitFor(() => app!.store.state.paneLayout.active === "pane-1", "composer focused")

    // Nested /api completion
    await app.type("/api GET /v2/hea")
    await app.waitForText("/api GET /v2/health")
    app.controller.ui.command?.open("/open")
    await app.render()
    await app.waitForText("▸ /open")
    await app.press("RETURN")
    expect(commandBox(app)?.rows[0]).toBe("/open")
    app.controller.ui.command?.open("/status")
    await app.render()
    await app.press("RETURN")
    await app.waitForText(/Server\s+http:\/\/127\.0\.0\.1:9999/)
    await app.type("/")
    await app.press("ARROW_UP", { shift: true })
    expect(commandBox(app)?.rows[0]).toBe("/status")
    await app.press("ESCAPE")

    // Pasted `/help` stays a message, never opens Help.
    await app.paste("/help")
    await app.waitForText("│ /help")
    await app.press("RETURN")
    await app.waitForText("┃ /help")
    expect(app.find("Help · keys and commands")).toBeNull()
  })

  test("/compact, /rename + /open completion, /status, /models, /api, narrow layout, and Ctrl+C twice quit", async () => {
    app = await renderApp({
      preset: "workspace",
      width: 156,
      height: 41,
      seed: { autoReply: "hi there" },
    })

    // Rename + /open completion by title
    await app.submit("/rename Bug fix session")
    await app.waitFor(() => app!.store.state.selected?.title === "Bug fix session", "renamed")
    await app.submit("/status")
    await app.waitForText(/Session\s+Bug fix session/)
    await app.waitForText(/Agent\s+hya-main/)
    await app.waitForText(/Mode\s+manual/)

    await app.type("/open bug")
    await app.waitForText(/▸ \/open Bug fix session \(hysec_\w+\)/)
    await app.press("TAB")
    expect(commandBox(app)?.rows[0]).toMatch(/^\/open hysec_\w+$/)
    await app.press("c", { ctrl: true })
    await app.waitForGone("Commands")

    // Sending a prompt from /status switches back to transcript and displays reply.
    await app.submit("hello from status")
    await app.waitForText("hi there")
    expect(app.find("Server      http://")).toBeNull()

    // /compact triggers manual rewrite compaction divider.
    app.controller.ui.command?.open("/compact")
    await app.render()
    await app.press("RETURN")
    await app.waitForText(/context compacted.*rewrite/)

    // /models and /api switch the main panel.
    app.controller.ui.command?.open("/models")
    await app.render()
    await app.press("RETURN")
    await app.waitForText("Models")
    await app.waitForText("fake/model")
    app.controller.ui.command?.open("/api")
    await app.render()
    await app.press("RETURN")
    await app.waitForText("API commands")
    await app.waitForText("/v2/health")

    // Narrow viewport keeps sidebars hidden even after /sidebar toggle.
    await app.resize(80, 37)
    expect(app.frame()).not.toContain("Sessions")
    app.controller.ui.command?.open("/sidebar")
    await app.render()
    await app.press("RETURN")
    expect(app.frame()).not.toContain("Sessions")

    // Ctrl+C once does not quit; twice quits with archive mode.
    await app.press("c", { ctrl: true })
    expect(app.quitCalls).toEqual([])
    await app.press("c", { ctrl: true })
    expect(app.quitCalls).toEqual(["archive"])
  })

  test("help overlay opens from ? or /help, filters by group and source, ignores ? in text, fits 80 cols", async () => {
    app = await renderApp({
      width: 80,
      height: 37,
      seed: {
        commands: [{ name: "review", description: "Review code", source: "command", scope: "builtin" }],
      },
    })
    // `?` inside text types `?` instead of opening help.
    await app.type("why?")
    expect(app.controller.ui.composerInput?.text).toBe("why?")
    expect(app.find("Help · keys and commands")).toBeNull()
    await app.press("ESCAPE")

    // `?` on empty input opens help overlay and fits 80 columns.
    await app.type("?")
    await app.waitForText("Help · keys and commands")
    for (const line of app.lines()) expect(line.length).toBeLessThanOrEqual(80)
    await app.waitForText(/Enter \/ Keypad Enter\s+\[composer\]/)

    await app.type("views")
    await app.waitForText(/\?\s+\[views\]/)
    expect(app.find("[composer]")).toBeNull()
    await app.press("ESCAPE")
    await app.waitForGone("Help · keys and commands")

    // `/help` opens same overlay with `[local]` and `[server]` commands.
    await app.submit("/help")
    await app.waitForText("Help · keys and commands")
    await app.type("/review")
    await app.waitForText(/\/review.*\[server\]/)
    await app.press("ESCAPE")
    await app.waitForGone("Help · keys and commands")
  })
})
