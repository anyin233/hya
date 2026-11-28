/**
 * In-memory `testRender` coverage for minimal `hya --bare` TUI mode (ADR-0039,
 * CHR-21): booting cleanly when the backend advertises zero TUI bundles and
 * omits domain bundle endpoints (`hya/workflow`, `hya/basic-tui-components`).
 */
import { afterEach, describe, expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { extensionManager } from "../src/extensions/manager"
import { chatPaneLayout, defaultPaneLayout, splitPane, visiblePaneLayout } from "../src/state/panes"
import { statusLineShown } from "../src/state/statusline"
import { renderApp, type RenderedApp } from "./support/appHarness"

let app: RenderedApp | undefined

afterEach(async () => {
  await app?.dispose()
  app = undefined
  await extensionManager.reset()
})

async function runCmd(rendered: RenderedApp, text: string): Promise<void> {
  rendered.controller.ui.command?.open(text)
  await rendered.render()
  await rendered.press("RETURN")
  await rendered.waitForGone("Commands")
}

async function resolveWithin(promise: Promise<void>, timeoutMs = 200): Promise<void> {
  let timer: Timer | undefined
  try {
    await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error(`Timed out after ${timeoutMs}ms`)), timeoutMs)
      }),
    ])
  } finally {
    if (timer) clearTimeout(timer)
  }
}

describe("bare TUI startup without TUI bundles", () => {
  test("zero-bundle default startup settles immediately, uses chatPaneLayout + status line, and renders zero Loading… or missing-pane errors", async () => {
    const waitingBeforeLoad = extensionManager.settled()
    app = await renderApp({
      width: 156,
      height: 41,
      loadExtensions: true,
      seed: {
        bundles: [],
        tuiExtensions: [],
        workflowsUnavailable: true,
        autoReply: "bare mode reply",
        sessions: [{
          id: "hysec_bare_1",
          agent: "hya-main",
          workdir: "/workspace",
          projectId: "prj_work",
          model: { providerId: "fake", modelId: "model" },
          permissionMode: "manual",
        }],
      },
    })

    // Both the pre-load waiter and subsequent settled() calls resolve immediately.
    await resolveWithin(waitingBeforeLoad)
    await resolveWithin(extensionManager.settled())
    expect(extensionManager.bare()).toBe(true)
    expect(extensionManager.list()).toEqual([])
    expect(app.store.state.ready).toBe(true)
    expect(app.store.state.paneLayout).toEqual(chatPaneLayout())
    expect(statusLineShown(app.store.state, 156)).toBe(true)

    const screen = app.frame()
    expect(screen).not.toContain("Loading…")
    expect(screen).not.toContain("needs hya/basic-tui-components")
    expect(screen).not.toContain("is not available")
    expect(screen).not.toContain("─Sessions─")
    expect(screen).not.toContain("─Projects─")
    expect(screen).toContain("Message, !shell")
    expect(screen).toContain("model · manual")

    // Chat turn, activity line, and status line work end to end in bare mode.
    await app.submit("hello bare")
    await app.waitForText("bare mode reply")
    expect(app.frame()).not.toContain("Loading…")
    expect(app.frame()).not.toContain("needs hya/basic-tui-components")
  })

  test("workspace preset and saved sidebar/extension layouts gracefully fall back to chatPaneLayout + status line at wide widths", async () => {
    const savedWithExt = splitPane(defaultPaneLayout(), "vertical", "extension", false, "acme/git#summary")
    app = await renderApp({
      width: 190,
      height: 41,
      preset: "workspace",
      loadExtensions: true,
      preferences: { paneLayout: savedWithExt },
      seed: {
        bundles: [],
        tuiExtensions: [],
        workflowsUnavailable: true,
        sessions: [{
          id: "hysec_bare_ws",
          agent: "hya-main",
          workdir: "/workspace",
          projectId: "prj_work",
          model: { providerId: "fake", modelId: "model" },
          permissionMode: "manual",
        }],
        todos: {
          hysec_bare_ws: [{ id: "t1", status: "in_progress", content: "verify bare layout" }],
        },
      },
    })

    await resolveWithin(extensionManager.settled())
    expect(extensionManager.bare()).toBe(true)

    // Visible layout collapses cleanly to chatPaneLayout() without mutating saved tui.json.
    const visible = visiblePaneLayout(
      app.store.state.paneLayout,
      190,
      app.store.state.sidebar,
      app.store.state.projectsSidebar,
      extensionManager.visiblePaneOptions(),
    )
    expect(visible).toEqual(chatPaneLayout())
    expect(statusLineShown(app.store.state, 190)).toBe(true)

    const screen = app.frame()
    expect(screen).not.toContain("Loading…")
    expect(screen).not.toContain("needs hya/basic-tui-components")
    expect(screen).not.toContain("is not available")
    expect(screen).not.toContain("─Sessions─")
    expect(screen).not.toContain("─Projects─")
    expect(screen).toContain("Message, !shell")
    expect(screen).toContain("model · manual")
    // Workspace todos fall back into the dock because the Todos sidebar pane is absent.
    expect(screen).toContain("verify bare layout")
    // User's saved workspace layout in tui.json stays intact for full mode.
    expect(JSON.parse(readFileSync(app.preferencesPath, "utf8")).paneLayout).toEqual(savedWithExt)

    // Sidebar/project actions fail gracefully with clear status notices instead of broken panes.
    await runCmd(app, "/sidebar on")
    expect(app.store.state.status).toContain("no TUI bundles loaded")
    await runCmd(app, "/projects-sidebar on")
    expect(app.store.state.status).toContain("no TUI bundles loaded")
    await runCmd(app, "/project")
    await app.waitFor(() => app!.store.state.status.includes("basic-tui-components"), "project status notice")
    expect(app.store.state.extensionOverlay).toBeUndefined()
    expect(app.frame()).not.toContain("Loading…")

    // Narrow 80-column resize also stays clean and within bounds.
    await app.resize(80, 37)
    for (const line of app.lines()) expect(line.length).toBeLessThanOrEqual(80)
    expect(app.frame()).not.toContain("Loading…")
  })
})

describe("core overlays remain operational in bare mode", () => {
  test("ProviderView, McpView, SkillsView, BundlesView, RulesView, and DiffView open, interact, and close cleanly without TUI bundles", async () => {
    const sampleDiff = [
      "diff --git a/src/a.ts b/src/a.ts",
      "--- a/src/a.ts",
      "+++ b/src/a.ts",
      "@@ -1 +1,2 @@",
      " const a = 1",
      "+const b = 2",
      "diff --git a/src/c.ts b/src/c.ts",
      "--- /dev/null",
      "+++ b/src/c.ts",
      "@@ -0,0 +1 @@",
      "+export const c = 3",
      "",
    ].join("\n")

    app = await renderApp({
      width: 120,
      height: 41,
      loadExtensions: true,
      seed: {
        bundles: [],
        tuiExtensions: [],
        workflowsUnavailable: true,
        providers: [
          { id: "fake", kind: "openai-compatible", baseUrl: "http://127.0.0.1:1/v1", auth: "AUTH_STATUS_CREDENTIALED", keySource: "config", modelCount: 1, result: "models" },
          { id: "hya", auth: "AUTH_STATUS_NOT_APPLICABLE", keySource: "none", modelCount: 1 },
        ],
        models: [
          { id: "fake/model", providerId: "fake", modelId: "model", displayName: "Fake Model", contextLimit: "100000", source: "config" },
          { id: "hya/offline", providerId: "hya", modelId: "offline", source: "offline" },
        ],
        mcpServers: [
          { name: "memory", state: "MCP_SERVER_STATE_CONNECTED", tools: ["mcp__memory__read_graph", "mcp__memory__create_entities"] },
        ],
        skills: [
          { name: "bare-review", description: "Review code in bare mode", source: "project", location: "/workspace/.hya/skills/bare-review/SKILL.md", content: "# Bare Review\n\nBARE_SKILL_BODY" },
        ],
        savedRules: [
          { id: "rule_bare_1", permission: "RULE_PERMISSION_ALLOW", tool: "bash", pattern: "bun test", projectId: "global", timeCreated: new Date().toISOString() },
        ],
        vcs: { branch: "feat/bare" },
        diff: sampleDiff,
      },
    })

    await resolveWithin(extensionManager.settled())
    expect(extensionManager.bare()).toBe(true)

    // 1. ProviderView (/key)
    await runCmd(app, "/key")
    await app.waitForText("Providers")
    await app.waitForText(/fake\s+openai-compatib.*config key\s+ready\s+1 model/)
    await app.press("RETURN")
    await app.waitForText("Providers › fake")
    await app.press("t")
    await app.waitForText(/✓ fake\/model replied/)
    await app.press("ESCAPE")
    await app.press("ESCAPE")
    await app.waitForGone("Providers")

    // 2. McpView (/mcp)
    await runCmd(app, "/mcp")
    await app.waitForText("MCP servers")
    await app.waitForText(/memory\s+connected\s+2 tools/)
    await app.press("RETURN")
    await app.waitForText("MCP › memory")
    await app.waitForText("▸ read_graph")
    await app.press("ESCAPE")
    await app.press("ESCAPE")
    await app.waitForGone("MCP servers")

    // 3. SkillsView (/skills)
    await runCmd(app, "/skills")
    await app.waitForText("── project")
    await app.waitForText(/bare-review/)
    await app.press("RETURN")
    await app.waitForText("BARE_SKILL_BODY")
    await app.press("ESCAPE")
    await app.press("u")
    await app.waitForGone("── project")
    expect(app.controller.ui.composerInput?.text).toBe("/skill:bare-review ")
    await app.press("c", { ctrl: true })

    // 4. BundlesView (/bundles) — clean zero-bundle state
    await runCmd(app, "/bundles")
    await app.waitForText("0 bundles · 0 enabled · 0 with a TUI extension")
    await app.waitForText("No bundles")
    await app.press("r")
    await app.waitForText("Bundles refreshed")
    await app.press("e")
    await app.waitForText("No bundle selected")
    await app.press("ESCAPE")
    await app.waitForGone("0 bundles")

    // 5. RulesView (/rules)
    await runCmd(app, "/rules")
    await app.waitForText("Saved Rules")
    await app.waitForText(/allow\s+bash\s+bun test/)
    await app.press("d")
    await app.waitForText("Enter confirms · Esc cancels")
    await app.press("RETURN")
    await app.waitForText("Deleted rule")
    await app.press("ESCAPE")
    await app.waitForGone("Saved Rules")

    // 6. DiffView (/diff)
    await runCmd(app, "/diff")
    await app.waitForText("2 files changed")
    await app.waitForText("Diff › src/a.ts")
    await app.waitForText("+ const b = 2")
    await app.press("n")
    await app.waitForText("Diff › src/c.ts")
    await app.waitForText("+ export const c = 3")
    await app.press("ESCAPE")
    await app.waitForGone("2 files changed")

    // Back in bare chat layout with zero placeholders or broken panes.
    expect(app.frame()).not.toContain("Loading…")
    expect(app.frame()).not.toContain("needs hya/basic-tui-components")
    expect(app.frame()).toContain("Message, !shell")
  })
})
