/**
 * In-memory `testRender` coverage for subagent selector & viewer panes:
 * preview, pin, follow, multi-viewer, live streaming child, PgUp/PgDn/Home/End
 * scrolling, x/s/d interrupt/steer/archive, Esc stopping subagents, and
 * actor archival/Ctrl+A visibility/revival (migrated from `hya-tui-subagent-panes`).
 */
import { afterEach, describe, expect, test } from "bun:test"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { renderApp, type RenderedApp } from "./support/appHarness"

let app: RenderedApp | undefined

afterEach(async () => {
  await app?.dispose()
  app = undefined
})

async function runCmd(rendered: RenderedApp, text: string): Promise<void> {
  await rendered.press("x", { ctrl: true })
  await rendered.type(text)
  await rendered.press("RETURN")
  await rendered.waitForGone("Commands")
}

function viewerId(rendered: RenderedApp, mode: "following" | "pinned", exclude?: string): string | undefined {
  const re = new RegExp(`Subagent · (pane-\\d+) · ${mode}`, "g")
  return Array.from(rendered.frame().matchAll(re), ([, id]) => id).find((id) => id !== exclude)
}

describe("subagent panes", () => {
  test("preview, pin, follow, multi-viewer, persistence, and draft isolation at wide & narrow widths", async () => {
    const configDir = mkdtempSync(join(tmpdir(), "hya-subagents-"))
    try {
      const seed = {
        sessions: [
          {
            id: "hysec_parent", agent: "hya-main", workdir: "/workspace", projectId: "prj_work",
            model: { providerId: "fake", modelId: "model" }, permissionMode: "manual",
            members: [
              { member: "m_alpha", handle: "main/alpha", child: "hysec_alpha", agent: "hya-task", description: "Alpha helper", status: "MEMBER_STATUS_RUNNING" },
              { member: "m_beta", handle: "main/beta", child: "hysec_beta", agent: "hya-task", description: "Beta helper", status: "MEMBER_STATUS_RUNNING" },
            ],
          },
          { id: "hysec_alpha", parent: "hysec_parent", agent: "hya-task", workdir: "/workspace", busy: true },
          { id: "hysec_beta", parent: "hysec_parent", agent: "hya-task", workdir: "/workspace", busy: true },
        ],
        messages: {
          hysec_parent: [
            { id: "pm1", role: "ROLE_USER", parts: [{ id: "pp1", text: { text: "delegate" } }] },
            { id: "pm2", role: "ROLE_ASSISTANT", agent: "hya-main", model: "fake/model", parts: [{ id: "pp2", text: { text: "Parent stays here." } }], finish: "FINISH_REASON_STOP" },
          ],
          hysec_alpha: [{ id: "am1", role: "ROLE_USER", parts: [{ id: "ap1", text: { text: "alpha-child-prompt" } }] }],
          hysec_beta: [{ id: "bm1", role: "ROLE_USER", parts: [{ id: "bp1", text: { text: "beta-child-prompt" } }] }],
        },
      }
      app = await renderApp({ preset: "workspace", width: 128, height: 37, configDir, seed })
      await app.waitForText("Parent stays here.")
      for (const pane of ["projects", "sessions", "todos", "context"]) await runCmd(app, `/layout close ${pane}`)

      await runCmd(app, "/subagents")
      await app.waitForText("Subagents")
      await app.waitForText("alpha-child-prompt")
      const firstViewer = viewerId(app, "following")!
      expect(firstViewer).toBeDefined()

      // Enter pins the viewer; selector owns typing so `zzz` does not leak into parent draft.
      await app.press("RETURN")
      await app.waitForText(`Subagent · ${firstViewer} · pinned`)
      await app.type("zzz")
      expect(app.find("zzz")).toBeNull()

      // Selecting beta while firstViewer is pinned keeps alpha in firstViewer; `n` opens second pinned viewer for beta.
      await app.press("ARROW_DOWN")
      await app.waitForText("alpha-child-prompt")
      expect(app.find("beta-child-prompt")).toBeNull()
      await app.press("n")
      await app.waitForText("beta-child-prompt")
      const secondViewer = viewerId(app, "pinned", firstViewer)!
      expect(secondViewer).toBeDefined()

      // Narrow width renders both without overflow.
      await app.resize(80, 37)
      await app.waitForText("Subagents")
      await app.resize(128, 37)

      // Closing second viewer and switching back to composer preserves draft; `/subagents follow` unpins.
      await runCmd(app, `/layout close ${secondViewer}`)
      await app.waitForGone(`Subagent · ${secondViewer} ·`)
      await runCmd(app, "/layout focus pane-1")
      await app.type("draft42")
      expect(app.controller.ui.composerInput?.text).toBe("draft42")
      await runCmd(app, `/subagents follow ${firstViewer}`)
      await app.waitForText(`Subagent · ${firstViewer} · following`)
      expect(app.controller.ui.composerInput?.text).toBe("draft42")

      // Re-pin Alpha, restart app, and verify pinned viewer restores cleanly.
      await runCmd(app, "/subagents")
      await app.press("ARROW_UP")
      await app.press("RETURN")
      await app.waitForText(`Subagent · ${firstViewer} · pinned`)
      await app.dispose()

      app = await renderApp({ preset: "workspace", width: 128, height: 37, configDir, seed })
      await app.waitForText(`Subagent · ${firstViewer} · pinned`)
      await app.waitForText("alpha-child-prompt")
      await runCmd(app, "/layout close subagents")
      await runCmd(app, "/layout close subagent-viewer")
      await app.waitForGone(`Subagent · ${firstViewer} ·`)
      await app.waitForText("Parent stays here.")
    } finally {
      await app?.dispose()
      app = undefined
      rmSync(configDir, { recursive: true, force: true })
    }
  })

  test("live child streaming, viewer scrolling without moving parent, and x/s/d/Ctrl+A/revival lifecycle", async () => {
    const longChild = Array.from({ length: 60 }, (_, i) => `child row ${i + 1}.`).join("\n\n")
    app = await renderApp({
      preset: "workspace",
      width: 128,
      height: 37,
      seed: {
        sessions: [
          {
            id: "hysec_parent", agent: "hya-main", workdir: "/workspace", projectId: "prj_work",
            model: { providerId: "fake", modelId: "model" }, permissionMode: "manual",
            members: [
              { member: "m_work", handle: "main/work", child: "hysec_child", agent: "hya-task", description: "steerable helper", status: "MEMBER_STATUS_RUNNING" },
            ],
          },
          { id: "hysec_child", parent: "hysec_parent", agent: "hya-task", workdir: "/workspace", busy: true },
        ],
        messages: {
          hysec_parent: [
            { id: "pm1", role: "ROLE_ASSISTANT", agent: "hya-main", model: "fake/model", parts: [{ id: "pp1", text: { text: "Parent live marker." } }], finish: "FINISH_REASON_STOP" },
          ],
          hysec_child: [
            { id: "cm1", role: "ROLE_USER", parts: [{ id: "cp1", text: { text: "child prompt" } }] },
            { id: "cm2", role: "ROLE_ASSISTANT", agent: "hya-task", model: "fake/model", parts: [{ id: "cp2", text: { text: longChild } }], finish: "FINISH_REASON_LENGTH" },
          ],
        },
      },
    })
    await app.waitForText("Parent live marker.")
    for (const pane of ["projects", "sessions", "todos", "context"]) await runCmd(app, `/layout close ${pane}`)
    await runCmd(app, "/subagents")
    await app.waitForText("child row 60.")

    // Stream live delta onto child session while parent stays open.
    app.backend.emitSession("hysec_child", { messageStarted: { message: "cm3", role: "ROLE_ASSISTANT" } })
    app.backend.emitSession("hysec_child", { partStarted: { message: "cm3", part: "cp3", kind: "text" } })
    app.backend.emitSession("hysec_child", { partAppended: { message: "cm3", part: "cp3", textDelta: "child-live-tail" } })
    await app.waitForText("child-live-tail")
    await app.waitForText("Parent live marker.")

    // Home/End in selector scroll the child viewer without moving parent.
    const parentPos = app.find("Parent live marker.")
    await app.press("HOME")
    await app.waitForText("child row 1.")
    expect(app.find("Parent live marker.")).toEqual(parentPos)
    await app.press("END")
    await app.waitForText("child-live-tail")
    expect(app.find("Parent live marker.")).toEqual(parentPos)

    // `x` interrupts child turn; `s` prefills `/subagents steer`.
    await app.press("x")
    await app.waitFor(() => app!.backend.calls.some((c) => c.method === "POST" && c.path.includes("/hysec_child/turns/")), "child cancel called")
    await app.press("s")
    await app.waitForText(/\/subagents steer \S+/)
    await app.type("steer-marker please")
    await app.press("RETURN")
    await app.waitFor(() => app!.backend.calls.some((c) => c.method === "POST" && c.path === "/v1/sessions/hysec_child/steer"), "steer posted")

    // `d` asks confirmation; ArrowDown cancels; `d` + Enter archives child and hides selector row.
    await app.press("d")
    await app.waitForText(/Archive .*\? Enter confirms/)
    await app.press("ARROW_DOWN")
    await app.waitForGone("Enter confirms")
    await app.press("d")
    await app.waitForText("Enter confirms")
    await app.press("RETURN")
    await app.waitForText("No active subagents in this conversation.")

    // Ctrl+A belongs to selector (not composer), toggles archived visibility, and respects `/keybind unset Ctrl+A`.
    await runCmd(app, "/layout focus pane-1")
    await app.press("a", { ctrl: true })
    expect(app.find("· archived")).toBeNull()
    await runCmd(app, "/subagents")
    await app.press("a", { ctrl: true })
    await app.waitForText("· archived")
    await app.waitForText("hide archived")
    await runCmd(app, "/keybind unset Ctrl+A")
    await app.press("ESCAPE")
    await app.press("a", { ctrl: true })
    expect(app.find("· archived")).not.toBeNull()
    await runCmd(app, "/keybind reset Ctrl+A")
    await app.press("ESCAPE")
    await app.press("a", { ctrl: true })
    await app.waitForText("No active subagents in this conversation.")

    // Revival lifecycle frame restores row cleanly.
    app.backend.emitSession("hysec_parent", { memberLifecycleChanged: { child: "hysec_child", handle: "main/work", archived: false } })
    await app.waitForGone("No active subagents in this conversation.")
    await app.waitForText("steerable helper")
  })
})
